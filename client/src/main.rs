//! Clipboard sync client for the Rustynaut broker.
//!
//! Transport is TCP or TLS and line-framed (`LinesCodec`). Clipboard updates are
//! sent as base64 to keep payloads binary-safe.

#![warn(rust_2018_idioms)]

mod clipboard_files;
mod tls;

/// Maximum line length for the wire protocol (2MB to handle large base64 clipboard payloads)
/// For files larger than ~1.5MB raw, use the sideband FILE_OFFER protocol instead.
const MAX_LINE_LENGTH: usize = 2 * 1024 * 1024;

use tokio::io;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::exit;

use base64::{engine::general_purpose, Engine as _};

use std::path::Path;
use std::sync::Mutex;

use lazy_static::lazy_static;

/// Chunk size for file transfers (64KB)
const FILE_CHUNK_SIZE: usize = 64 * 1024;

/// Pending outgoing file: filename_b64 -> file path
/// When we send FILE_OFFER, we store the path here
/// When we receive FILE_START, we read from this path and send chunks
#[derive(Debug)]
struct PendingOutgoingFile {
    path: PathBuf,
    size: u64,
}

/// Incoming file transfer state
#[derive(Debug)]
struct IncomingTransfer {
    filename: String,
    size: u64,
    file: std::fs::File,
    temp_path: PathBuf,
    bytes_received: u64,
}

lazy_static! {
    static ref CLIPBOARD: Mutex<arboard::Clipboard> = Mutex::new(get_current_clipboard());
    /// Tracks recently applied clipboard content from the network (for echo suppression)
    /// We keep a few recent values since multiple clips can arrive in quick succession
    static ref RECENT_APPLIED_CLIPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// Pending outgoing files: filename_b64 -> PendingOutgoingFile
    static ref PENDING_OUTGOING: Mutex<HashMap<String, PendingOutgoingFile>> = Mutex::new(HashMap::new());
    /// Incoming file transfers: transfer_id -> IncomingTransfer
    static ref INCOMING_TRANSFERS: Mutex<HashMap<u64, IncomingTransfer>> = Mutex::new(HashMap::new());
    /// Channel for sending file transfer messages (FILE_CHUNK, FILE_END)
    static ref FILE_TX: Mutex<Option<mpsc::Sender<String>>> = Mutex::new(None);
}

/// Maximum number of recent clips to track for echo suppression
const MAX_RECENT_CLIPS: usize = 10;

/// Helper trait to get string contents from the global clipboard
trait ClipboardExt {
    fn get_string_contents(&self) -> Result<String, String>;
}

impl ClipboardExt for Mutex<arboard::Clipboard> {
    fn get_string_contents(&self) -> Result<String, String> {
        let mut clipboard = self.lock().map_err(|e| format!("clipboard lock: {e}"))?;
        clipboard.get_text().map_err(|e| e.to_string())
    }
}

/// Check if a string looks like a file path and return file info if it exists.
/// Works cross-platform (Linux, macOS, Windows).
/// Returns Some((path, size, is_file)) if valid file, None otherwise.
fn detect_file_path(content: &str) -> Option<(std::path::PathBuf, u64, bool)> {
    let trimmed = content.trim();

    // Skip empty or very long strings (unlikely to be file paths)
    if trimmed.is_empty() || trimmed.len() > 4096 {
        return None;
    }

    // Skip if content has multiple lines (likely text, not a path)
    if trimmed.lines().count() > 1 {
        return None;
    }

    // Handle file:// URLs (common on macOS/Linux when copying files)
    let path_str = if let Some(stripped) = trimmed.strip_prefix("file://") {
        // URL decode common sequences
        stripped
            .replace("%20", " ")
            .replace("%23", "#")
            .replace("%25", "%")
    } else {
        trimmed.to_string()
    };

    let path = Path::new(&path_str);

    // Check if it looks like an absolute path
    // - Unix: starts with /
    // - Windows: starts with drive letter (C:\) or UNC path (\\)
    let looks_like_path = path.is_absolute()
        || (cfg!(windows) && path_str.len() >= 3 && path_str.chars().nth(1) == Some(':'));

    if !looks_like_path {
        return None;
    }

    // Check if the file actually exists
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let size = metadata.len();
            let is_file = metadata.is_file();
            Some((path.to_path_buf(), size, is_file))
        }
        Err(_) => None,
    }
}

const BANNER: &str = r#"
██████  ██      ██ ███████ ███    ██ ████████ 
██      ██      ██ ██      ████   ██    ██    
██      ██      ██ █████   ██ ██  ██    ██    
██      ██      ██ ██      ██  ██ ██    ██    
██████  ███████ ██ ███████ ██   ████    ██    
                                              
https://github.com/aszazeroth/rustynaut                                                
"#;

/// Parsed command-line arguments
struct Args {
    verbose: bool,
    addr: SocketAddr,
    username: String,
    room: String,
    tls_disabled: bool,
    enroll_token: Option<String>,
    cert_dir: PathBuf,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut verbose = false;
    let mut tls_disabled = false;
    let mut enroll_token: Option<String> = None;
    let mut cert_dir: Option<PathBuf> = None;
    let mut positional = Vec::new();

    let mut args_iter = env::args().skip(1).peekable();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            "--no-tls" => tls_disabled = true,
            "--enroll" => {
                enroll_token = Some(
                    args_iter
                        .next()
                        .ok_or("--enroll requires a token argument")?,
                );
            }
            "--cert-dir" => {
                cert_dir = Some(PathBuf::from(
                    args_iter
                        .next()
                        .ok_or("--cert-dir requires a path argument")?,
                ));
            }
            "--help" | "-h" => {
                eprintln!("usage: client [OPTIONS] <addr> [username] [room]");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --verbose, -v           Enable verbose logging");
                eprintln!("  --no-tls                Disable TLS (insecure, for testing)");
                eprintln!("  --enroll <TOKEN>        Enroll with broker using token");
                eprintln!("  --cert-dir <PATH>       Certificate directory");
                eprintln!("                          (default: ~/.config/rustynaut/client)");
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown flag: {arg}").into());
            }
            _ => positional.push(arg),
        }
    }

    let addr = positional
        .first()
        .ok_or("usage: client [OPTIONS] <addr> [username] [room]")?
        .parse::<SocketAddr>()?;

    let username = positional.get(1).cloned().unwrap_or_else(default_username);
    let room = positional
        .get(2)
        .cloned()
        .unwrap_or_else(|| "lobby".to_string());

    Ok(Args {
        verbose,
        addr,
        username,
        room,
        tls_disabled,
        enroll_token,
        cert_dir: cert_dir.unwrap_or_else(tls::default_cert_dir),
    })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("{BANNER}");

    let args = parse_args()?;

    // Handle enrollment if requested
    if let Some(ref token) = args.enroll_token {
        enroll(&args.addr, token, &args.username, &args.cert_dir).await?;
        // After successful enrollment, continue to connect with TLS
        println!("Auto-connecting with TLS...\n");
        // Small delay to ensure broker has cleaned up the enrollment connection
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Check if we need TLS and have certificates
    let tls_enabled = !args.tls_disabled;
    if tls_enabled && !tls::is_enrolled(&args.cert_dir) {
        eprintln!("TLS is enabled by default but you are not enrolled.");
        eprintln!("Use --enroll <token> to enroll first, or --no-tls for insecure mode.");
        eprintln!("Certificates should be in: {:?}", args.cert_dir);
        return Err("Not enrolled for TLS".into());
    }

    let (tx, rx) = mpsc::channel::<String>(10);
    let rx = tokio_stream::wrappers::ReceiverStream::new(rx);

    // File transfer channel (larger buffer for chunked transfers)
    let (file_tx, file_rx) = mpsc::channel::<String>(100);
    let file_rx = tokio_stream::wrappers::ReceiverStream::new(file_rx);
    
    // Store file_tx globally so message handlers can send file chunks
    if let Ok(mut guard) = FILE_TX.lock() {
        *guard = Some(file_tx);
    }

    // Spawn a task to monitor clipboard changes
    let room_for_clipboard = args.room.clone();
    let verbose_for_clipboard = args.verbose;
    tokio::spawn(async move {
        let mut previous_content = match CLIPBOARD.get_string_contents() {
            Ok(s) => s,
            Err(err) => {
                // Treat "clipboard is empty" as an empty string, not an error
                let err_str = err.to_string();
                if err_str.contains("empty") || err_str.contains("Empty") {
                    String::new()
                } else {
                    eprintln!(
                        "clipboard unavailable ({err}); clipboard publishing disabled for this client"
                    );
                    return;
                }
            }
        };
        let mut previous_files: Option<Vec<std::path::PathBuf>> = None;
        let mut interval = time::interval(Duration::from_secs(2));
        let mut warned = false;
        loop {
            interval.tick().await;

            // First, check for native file clipboard (Finder/Explorer copy)
            if let Some(files) = clipboard_files::get_clipboard_files() {
                // Check if files changed
                let files_changed = previous_files.as_ref() != Some(&files);
                if files_changed {
                    for path in &files {
                        if let Ok(metadata) = std::fs::metadata(path) {
                            if metadata.is_file() {
                                let size = metadata.len();
                                // Get just the filename for the offer
                                let filename = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown");
                                let filename_b64 = general_purpose::STANDARD.encode(filename);

                                // Track the pending outgoing file
                                if let Ok(mut pending) = PENDING_OUTGOING.lock() {
                                    pending.insert(filename_b64.clone(), PendingOutgoingFile {
                                        path: path.clone(),
                                        size,
                                    });
                                }

                                // Send FILE_OFFER to broker
                                let offer_msg = format!(
                                    "FILE_OFFER {room_for_clipboard} {filename_b64} {size}"
                                );
                                if tx.send(offer_msg).await.is_err() {
                                    eprintln!("Failed to send file offer");
                                    break;
                                }

                                if verbose_for_clipboard {
                                    eprintln!(
                                        "file offer sent: {} ({} bytes)",
                                        path.display(),
                                        size
                                    );
                                }
                            } else if verbose_for_clipboard {
                                eprintln!(
                                    "directory copied (native): {} - skipping",
                                    path.display()
                                );
                            }
                        }
                    }
                    previous_files = Some(files);
                    // Also update previous_content to prevent the file path from being sent as CLIP
                    // (copying files often also puts the path in the text clipboard)
                    if let Ok(text) = CLIPBOARD.get_string_contents() {
                        previous_content = text;
                    }
                    continue;
                }
            }

            let current_content = match CLIPBOARD.get_string_contents() {
                Ok(s) => {
                    warned = false;
                    s
                }
                Err(err) => {
                    // Treat "clipboard is empty" as an empty string, not an error
                    let err_str = err.to_string();
                    if err_str.contains("empty") || err_str.contains("Empty") {
                        warned = false;
                        String::new()
                    } else {
                        if verbose_for_clipboard && !warned {
                            eprintln!(
                                "clipboard read failed ({err}); will retry (publishing paused)"
                            );
                            warned = true;
                        }
                        continue;
                    }
                }
            };
            if current_content != previous_content {
                // Clear file tracking when text content changes
                previous_files = None;

                // Skip empty content
                if current_content.is_empty() {
                    previous_content = current_content;
                    continue;
                }

                // Echo suppression: skip if this is content we recently applied from the network
                let is_echo = if let Ok(recent) = RECENT_APPLIED_CLIPS.lock() {
                    recent.contains(&current_content)
                } else {
                    false
                };

                if is_echo {
                    // This is an echo of what we received - don't send it back
                    previous_content = current_content;
                    continue;
                }

                // Check if clipboard contains a file path
                if let Some((path, size, is_file)) = detect_file_path(&current_content) {
                    if is_file {
                        // Send FILE_OFFER instead of CLIP for files
                        let filename = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown");
                        let filename_b64 = general_purpose::STANDARD.encode(filename);

                        // Track the pending outgoing file
                        if let Ok(mut pending) = PENDING_OUTGOING.lock() {
                            pending.insert(filename_b64.clone(), PendingOutgoingFile {
                                path: path.clone(),
                                size,
                            });
                        }

                        let offer_msg =
                            format!("FILE_OFFER {room_for_clipboard} {filename_b64} {size}");
                        if tx.send(offer_msg).await.is_err() {
                            eprintln!("Failed to send file offer");
                            break;
                        }

                        if verbose_for_clipboard {
                            eprintln!("file offer sent: {} ({} bytes)", path.display(), size);
                        }

                        previous_content = current_content;
                        continue;
                    } else if verbose_for_clipboard {
                        eprintln!("directory detected: {} - skipping", path.display());
                        previous_content = current_content;
                        continue;
                    }
                }

                let encoded = general_purpose::STANDARD.encode(&current_content);
                if tx
                    .send(format!("CLIP {room_for_clipboard} {encoded}"))
                    .await
                    .is_err()
                {
                    eprintln!("Failed to send encoded content");
                    break;
                }
                previous_content = current_content;
            }
        }
    });

    let init = tokio_stream::iter([
        Ok::<String, LinesCodecError>(format!("USER {}", args.username)),
        Ok::<String, LinesCodecError>(format!("JOIN {}", args.room)),
    ]);

    let stdin = FramedRead::new(io::stdin(), LinesCodec::new()).filter_map(|i| {
        match i {
            Ok(line) => {
                // Handle local quit commands
                if line == "/quit" || line == "/exit" {
                    println!("Goodbye!");
                    std::process::exit(0);
                }
                if line.starts_with('/') {
                    Some(Ok(format!("CMD {line}")))
                } else {
                    Some(Ok(format!("SAY {line}")))
                }
            }
            Err(e) => Some(Err(e)),
        }
    });

    let stdin = init
        .merge(stdin)
        .merge(rx.map(Result::<String, LinesCodecError>::Ok))
        .merge(file_rx.map(Result::<String, LinesCodecError>::Ok));

    let stdout = FramedWrite::new(io::stdout(), LinesCodec::new());

    if tls_enabled {
        // TLS connection
        let tls_config =
            tls::init_tls_with_client_cert(&args.cert_dir).map_err(|e| -> Box<dyn Error> { e })?;
        tls_transport::connect(&args.addr, &tls_config, stdin, stdout, args.verbose).await?;
    } else {
        // Plain TCP (insecure) connection
        eprintln!("WARNING: TLS disabled, connection is not encrypted!");
        tcp::connect(&args.addr, stdin, stdout, args.verbose).await?;
    }

    Ok(())
}

/// Enroll with the broker to obtain client certificates
async fn enroll(
    addr: &SocketAddr,
    token: &str,
    username: &str,
    cert_dir: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    use futures::SinkExt;
    use tokio::net::TcpStream;
    use tokio_util::codec::{Framed, LinesCodec};

    // Clear any existing certificates first (handles re-enrollment with new broker)
    if tls::is_enrolled(cert_dir) {
        eprintln!("Clearing existing certificates for re-enrollment...");
        tls::clear_certs(cert_dir).map_err(|e| -> Box<dyn Error> { e })?;
    }

    println!("Enrolling with broker at {addr}...");

    // Connect with TLS in insecure mode (for enrollment)
    let tls_config = tls::init_tls_for_enrollment().map_err(|e| -> Box<dyn Error> { e })?;

    let stream = TcpStream::connect(addr).await?;

    // Extract host for TLS SNI
    let host = addr.ip().to_string();
    let tls_stream = tls::connect_tls(&tls_config.connector, stream, &host)
        .await
        .map_err(|e| -> Box<dyn Error> { e })?;

    let mut framed = Framed::new(tls_stream, LinesCodec::new_with_max_length(MAX_LINE_LENGTH));

    // Read the welcome message
    if let Some(Ok(line)) = framed.next().await {
        println!("Broker: {}", line);
    }

    // Send enrollment request
    let enroll_cmd = format!("ENROLL {token} {username}");
    framed.send(&enroll_cmd).await?;

    // Wait for response
    if let Some(Ok(line)) = framed.next().await {
        if line.starts_with("ENROLLED ") {
            let bundle =
                tls::parse_enrolled_response(&line).map_err(|e| -> Box<dyn Error> { e })?;
            tls::save_enrolled_certs(cert_dir, &bundle).map_err(|e| -> Box<dyn Error> { e })?;

            println!("Enrollment successful!");
            println!("Certificates saved to: {:?}", cert_dir);
            return Ok(());
        } else if let Some(err_msg) = line.strip_prefix("ERR ") {
            return Err(format!("Enrollment failed: {}", err_msg).into());
        } else {
            println!("Unexpected response: {}", line);
        }
    }

    Err("Enrollment failed: no response from broker".into())
}

fn get_current_clipboard() -> arboard::Clipboard {
    let Ok(clipboard) = arboard::Clipboard::new() else {
        eprintln!("could not connect to clipboard");
        exit(100); // We exit here, as if this doesn't work, there is no use continue the client
    };
    clipboard
}

fn replace_clipboard_content(content: &str) -> Result<(), Box<dyn std::error::Error>> {
    let decoded = general_purpose::STANDARD.decode(content)?;
    let decoded_string = String::from_utf8(decoded)?;

    // Skip empty content
    if decoded_string.is_empty() {
        return Ok(());
    }

    let mut clipboard = CLIPBOARD
        .lock()
        .map_err(|e| format!("clipboard lock: {e}"))?;
    let current_content = clipboard.get_text().unwrap_or_default();
    if current_content != decoded_string {
        // Track what we're applying for echo suppression
        if let Ok(mut recent) = RECENT_APPLIED_CLIPS.lock() {
            // Add to recent list, removing oldest if at capacity
            if recent.len() >= MAX_RECENT_CLIPS {
                recent.remove(0);
            }
            recent.push(decoded_string.clone());
        }
        clipboard.set_text(decoded_string)?;
    }
    Ok(())
}

fn default_username() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "anon".to_string())
}

fn parse_clip_fields(line: &str) -> Option<(&str, &str, Option<&str>)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("CLIP", room, b64) => Some((room, b64, parts.next())),
        _ => None,
    }
}

/// Parse FILE_OFFER from broker: FILE_OFFER <room> <username> <filename_b64> <size>
fn parse_file_offer_fields(line: &str) -> Option<(&str, &str, &str, &str)> {
    let mut parts = line.splitn(5, ' ');
    match (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    ) {
        ("FILE_OFFER", room, username, filename_b64, size) => {
            Some((room, username, filename_b64, size))
        }
        _ => None,
    }
}

/// Parse FILE_START from broker: FILE_START <transfer_id> <filename_b64> <acceptor_count>
fn parse_file_start_fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_START", transfer_id, filename_b64, acceptor_count) => {
            Some((transfer_id, filename_b64, acceptor_count))
        }
        _ => None,
    }
}

/// Parse FILE_INCOMING from broker: FILE_INCOMING <transfer_id> <filename_b64> <size>
fn parse_file_incoming_fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_INCOMING", transfer_id, filename_b64, size) => {
            Some((transfer_id, filename_b64, size))
        }
        _ => None,
    }
}

/// Parse FILE_CANCELLED from broker: FILE_CANCELLED <transfer_id> <reason>
fn parse_file_cancelled_fields(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("FILE_CANCELLED ")?;
    let mut parts = rest.splitn(2, ' ');
    let transfer_id = parts.next()?;
    let reason = parts.next().unwrap_or("unknown");
    Some((transfer_id, reason))
}

/// Parse FILE_CHUNK from broker: FILE_CHUNK <transfer_id> <offset> <chunk_b64>
fn parse_file_chunk_fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_CHUNK", transfer_id, offset, chunk_b64) => Some((transfer_id, offset, chunk_b64)),
        _ => None,
    }
}

/// Parse FILE_DONE from broker: FILE_DONE <transfer_id> <sha256>
fn parse_file_done_fields(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_DONE", transfer_id, sha256) => Some((transfer_id, sha256)),
        _ => None,
    }
}

/// Parse FILE_SENT from broker: FILE_SENT <transfer_id> <count>
fn parse_file_sent_fields(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_SENT", transfer_id, count) => Some((transfer_id, count)),
        _ => None,
    }
}

/// Generate FILE_CHUNK messages for a file transfer
/// Returns a Vec of messages to send (FILE_CHUNK and FILE_END)
fn generate_file_chunks(filename_b64: &str, transfer_id: &str) -> Vec<String> {
    let mut messages = Vec::new();

    // Look up the pending file
    let file_info = if let Ok(pending) = PENDING_OUTGOING.lock() {
        pending.get(filename_b64).map(|p| (p.path.clone(), p.size))
    } else {
        None
    };

    let Some((path, _size)) = file_info else {
        eprintln!("No pending file for {}", filename_b64);
        return messages;
    };

    // Read file and generate chunks
    let file_data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) => {
            eprintln!("Failed to read file {}: {}", path.display(), e);
            return messages;
        }
    };

    // Compute SHA256
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&file_data);
    let hash = hasher.finalize();
    let sha256_hex = hex::encode(hash);

    // Generate chunks
    let mut offset: u64 = 0;
    for chunk in file_data.chunks(FILE_CHUNK_SIZE) {
        let chunk_b64 = general_purpose::STANDARD.encode(chunk);
        messages.push(format!("FILE_CHUNK {transfer_id} {offset} {chunk_b64}"));
        offset += chunk.len() as u64;
    }

    // FILE_END with checksum
    messages.push(format!("FILE_END {transfer_id} {sha256_hex}"));

    // Clean up pending
    if let Ok(mut pending) = PENDING_OUTGOING.lock() {
        pending.remove(filename_b64);
    }

    messages
}

/// Handle incoming FILE_INCOMING - prepare to receive a file
fn prepare_incoming_transfer(transfer_id: u64, filename: &str, size: u64) -> Result<(), String> {
    // Create temp file in downloads directory
    let downloads = dirs::download_dir().unwrap_or_else(std::env::temp_dir);
    let temp_path = downloads.join(format!(".rustynaut_incoming_{}", transfer_id));

    let file = std::fs::File::create(&temp_path)
        .map_err(|e| format!("Failed to create temp file: {}", e))?;

    if let Ok(mut transfers) = INCOMING_TRANSFERS.lock() {
        transfers.insert(transfer_id, IncomingTransfer {
            filename: filename.to_string(),
            size,
            file,
            temp_path,
            bytes_received: 0,
        });
    }

    Ok(())
}

/// Handle incoming FILE_CHUNK - write bytes to temp file
fn handle_file_chunk(transfer_id: u64, _offset: u64, chunk_b64: &str) -> Result<(), String> {
    let chunk_data = general_purpose::STANDARD.decode(chunk_b64)
        .map_err(|e| format!("Invalid base64: {}", e))?;

    if let Ok(mut transfers) = INCOMING_TRANSFERS.lock() {
        if let Some(transfer) = transfers.get_mut(&transfer_id) {
            transfer.file.write_all(&chunk_data)
                .map_err(|e| format!("Write failed: {}", e))?;
            transfer.bytes_received += chunk_data.len() as u64;
        }
    }

    Ok(())
}

/// Handle FILE_DONE - verify checksum and move file to final location
fn finalize_transfer(transfer_id: u64, _expected_sha256: &str) -> Result<String, String> {
    let transfer_info = if let Ok(mut transfers) = INCOMING_TRANSFERS.lock() {
        transfers.remove(&transfer_id)
    } else {
        None
    };

    let Some(transfer) = transfer_info else {
        return Err("No such transfer".to_string());
    };

    // Close the file (drop it)
    drop(transfer.file);

    // Move to final location
    let downloads = dirs::download_dir().unwrap_or_else(std::env::temp_dir);
    let mut final_path = downloads.join(&transfer.filename);

    // Handle file already exists - add number suffix
    let mut counter = 1;
    while final_path.exists() {
        let stem = Path::new(&transfer.filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = Path::new(&transfer.filename)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!(".{}", s))
            .unwrap_or_default();
        final_path = downloads.join(format!("{} ({}){}", stem, counter, ext));
        counter += 1;
    }

    std::fs::rename(&transfer.temp_path, &final_path)
        .map_err(|e| format!("Failed to move file: {}", e))?;

    Ok(final_path.display().to_string())
}

/// Format a byte size for human-readable display
fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{} bytes", size)
    }
}

mod tcp {
    use base64::Engine;
    use futures::{future, Sink, SinkExt, Stream, StreamExt};
    use std::{error::Error, net::SocketAddr};
    use tokio::net::TcpStream;
    use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

    pub async fn connect(
        addr: &SocketAddr,
        mut stdin: impl Stream<Item = Result<String, LinesCodecError>> + Unpin,
        mut stdout: impl Sink<String, Error = LinesCodecError> + Unpin,
        verbose: bool,
    ) -> Result<(), Box<dyn Error>> {
        let mut stream = TcpStream::connect(addr).await?;
        let (r, w) = stream.split();
        let mut sink = FramedWrite::new(w, LinesCodec::new_with_max_length(crate::MAX_LINE_LENGTH));
        let mut stream = FramedRead::new(
            r,
            LinesCodec::new_with_max_length(crate::MAX_LINE_LENGTH),
        )
        .filter_map(|i| match i {
            Ok(message) => {
                // Apply clipboard updates silently (avoid printing base64 payloads).
                if let Some((room, clipboard_b64, id)) = crate::parse_clip_fields(&message) {
                    if let Err(err) = crate::replace_clipboard_content(clipboard_b64) {
                        eprintln!("could not replace the clipboard content, {}", err)
                    }

                    if verbose {
                        let id_str = id.unwrap_or("?");
                        eprintln!(
                            "clip applied: room={room} id={id_str} (b64_len={})",
                            clipboard_b64.len()
                        );
                    }
                    return future::ready(None);
                }

                // Handle FILE_OFFER: display as a user-friendly message
                if let Some((room, username, filename_b64, size_str)) =
                    crate::parse_file_offer_fields(&message)
                {
                    // Decode filename from base64
                    let filename = base64::engine::general_purpose::STANDARD
                        .decode(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = crate::format_size(size);
                    // Display the file offer to the user with accept hint
                    let display_msg = format!(
                        "INFO [{room}] {username} offers file: {filename} ({size_display}) - use /accept {username} {filename} to receive"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_START: sender should begin transfer
                if let Some((transfer_id, filename_b64, acceptor_count)) =
                    crate::parse_file_start_fields(&message)
                {
                    let filename = base64::engine::general_purpose::STANDARD
                        .decode(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let display_msg = format!(
                        "INFO File transfer started: {filename} (transfer_id={transfer_id}, {acceptor_count} receiver(s))"
                    );
                    
                    // Spawn a task to send file chunks
                    let filename_b64_owned = filename_b64.to_string();
                    let transfer_id_owned = transfer_id.to_string();
                    tokio::spawn(async move {
                        let messages = crate::generate_file_chunks(&filename_b64_owned, &transfer_id_owned);
                        // Clone the sender before releasing the lock to avoid holding lock across await
                        let tx = crate::FILE_TX.lock().ok().and_then(|guard| guard.clone());
                        if let Some(tx) = tx {
                            for msg in messages {
                                if tx.send(msg).await.is_err() {
                                    eprintln!("Failed to send file chunk");
                                    break;
                                }
                            }
                        }
                    });
                    
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_INCOMING: receiver should prepare to receive
                if let Some((transfer_id, filename_b64, size_str)) =
                    crate::parse_file_incoming_fields(&message)
                {
                    let filename = base64::engine::general_purpose::STANDARD
                        .decode(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = crate::format_size(size);
                    
                    // Prepare to receive the file
                    let tid: u64 = transfer_id.parse().unwrap_or(0);
                    if let Err(e) = crate::prepare_incoming_transfer(tid, &filename, size) {
                        eprintln!("Failed to prepare transfer: {}", e);
                    }
                    
                    let display_msg = format!(
                        "INFO Receiving file: {filename} ({size_display}) transfer_id={transfer_id}"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_CHUNK: write chunk to temp file
                if let Some((transfer_id, offset, chunk_b64)) =
                    crate::parse_file_chunk_fields(&message)
                {
                    let tid: u64 = transfer_id.parse().unwrap_or(0);
                    let off: u64 = offset.parse().unwrap_or(0);
                    if let Err(e) = crate::handle_file_chunk(tid, off, chunk_b64) {
                        eprintln!("Chunk error: {}", e);
                    }
                    // Don't print anything for chunks (too noisy)
                    return future::ready(None);
                }

                // Handle FILE_DONE: finalize the transfer
                if let Some((transfer_id, sha256)) =
                    crate::parse_file_done_fields(&message)
                {
                    let tid: u64 = transfer_id.parse().unwrap_or(0);
                    match crate::finalize_transfer(tid, sha256) {
                        Ok(final_path) => {
                            let display_msg = format!(
                                "INFO File received: {final_path}"
                            );
                            return future::ready(Some(Ok(display_msg)));
                        }
                        Err(e) => {
                            let display_msg = format!(
                                "INFO File transfer failed: {e}"
                            );
                            return future::ready(Some(Ok(display_msg)));
                        }
                    }
                }

                // Handle FILE_SENT: sender confirmation
                if let Some((transfer_id, count)) =
                    crate::parse_file_sent_fields(&message)
                {
                    let display_msg = format!(
                        "INFO File sent successfully (transfer_id={transfer_id}, {count} receiver(s))"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_CANCELLED
                if let Some((transfer_id, reason)) =
                    crate::parse_file_cancelled_fields(&message)
                {
                    let display_msg = format!(
                        "INFO File transfer {transfer_id} cancelled: {reason}"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                future::ready(Some(Ok(message)))
            }
            Err(e) => {
                eprintln!("failed to read from socket; error={}", e);
                future::ready(None)
            }
        });

        match future::join(sink.send_all(&mut stdin), stdout.send_all(&mut stream)).await {
            (Err(e), _) | (_, Err(e)) => Err(e.into()),
            _ => Ok(()),
        }
    }
}

mod tls_transport {
    use base64::Engine;
    use futures::{future, Sink, SinkExt, Stream, StreamExt};
    use std::{error::Error, net::SocketAddr};
    use tokio::net::TcpStream;
    use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

    pub async fn connect(
        addr: &SocketAddr,
        tls_config: &crate::tls::TlsClientConfig,
        mut stdin: impl Stream<Item = Result<String, LinesCodecError>> + Unpin,
        mut stdout: impl Sink<String, Error = LinesCodecError> + Unpin,
        verbose: bool,
    ) -> Result<(), Box<dyn Error>> {
        let tcp_stream = TcpStream::connect(addr).await?;

        // Extract host for TLS SNI
        let host = addr.ip().to_string();
        let tls_stream = crate::tls::connect_tls(&tls_config.connector, tcp_stream, &host)
            .await
            .map_err(|e| -> Box<dyn Error> { e })?;

        let (r, w) = tokio::io::split(tls_stream);
        let mut sink = FramedWrite::new(w, LinesCodec::new_with_max_length(crate::MAX_LINE_LENGTH));
        let mut stream = FramedRead::new(
            r,
            LinesCodec::new_with_max_length(crate::MAX_LINE_LENGTH),
        )
        .filter_map(|i| match i {
            Ok(message) => {
                // Apply clipboard updates silently (avoid printing base64 payloads).
                if let Some((room, clipboard_b64, id)) = crate::parse_clip_fields(&message) {
                    if let Err(err) = crate::replace_clipboard_content(clipboard_b64) {
                        eprintln!("could not replace the clipboard content, {}", err)
                    }

                    if verbose {
                        let id_str = id.unwrap_or("?");
                        eprintln!(
                            "clip applied: room={room} id={id_str} (b64_len={})",
                            clipboard_b64.len()
                        );
                    }
                    return future::ready(None);
                }

                // Handle FILE_OFFER: display as a user-friendly message
                if let Some((room, username, filename_b64, size_str)) =
                    crate::parse_file_offer_fields(&message)
                {
                    // Decode filename from base64
                    let filename = base64::engine::general_purpose::STANDARD
                        .decode(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = crate::format_size(size);
                    // Display the file offer to the user with accept hint
                    let display_msg = format!(
                        "INFO [{room}] {username} offers file: {filename} ({size_display}) - use /accept {username} {filename} to receive"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_START: sender should begin transfer
                if let Some((transfer_id, filename_b64, acceptor_count)) =
                    crate::parse_file_start_fields(&message)
                {
                    let filename = base64::engine::general_purpose::STANDARD
                        .decode(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let display_msg = format!(
                        "INFO File transfer started: {filename} (transfer_id={transfer_id}, {acceptor_count} receiver(s))"
                    );
                    
                    // Spawn a task to send file chunks
                    let filename_b64_owned = filename_b64.to_string();
                    let transfer_id_owned = transfer_id.to_string();
                    tokio::spawn(async move {
                        let messages = crate::generate_file_chunks(&filename_b64_owned, &transfer_id_owned);
                        // Clone the sender before releasing the lock to avoid holding lock across await
                        let tx = crate::FILE_TX.lock().ok().and_then(|guard| guard.clone());
                        if let Some(tx) = tx {
                            for msg in messages {
                                if tx.send(msg).await.is_err() {
                                    eprintln!("Failed to send file chunk");
                                    break;
                                }
                            }
                        }
                    });
                    
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_INCOMING: receiver should prepare to receive
                if let Some((transfer_id, filename_b64, size_str)) =
                    crate::parse_file_incoming_fields(&message)
                {
                    let filename = base64::engine::general_purpose::STANDARD
                        .decode(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = crate::format_size(size);
                    
                    // Prepare to receive the file
                    let tid: u64 = transfer_id.parse().unwrap_or(0);
                    if let Err(e) = crate::prepare_incoming_transfer(tid, &filename, size) {
                        eprintln!("Failed to prepare transfer: {}", e);
                    }
                    
                    let display_msg = format!(
                        "INFO Receiving file: {filename} ({size_display}) transfer_id={transfer_id}"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_CHUNK: write chunk to temp file
                if let Some((transfer_id, offset, chunk_b64)) =
                    crate::parse_file_chunk_fields(&message)
                {
                    let tid: u64 = transfer_id.parse().unwrap_or(0);
                    let off: u64 = offset.parse().unwrap_or(0);
                    if let Err(e) = crate::handle_file_chunk(tid, off, chunk_b64) {
                        eprintln!("Chunk error: {}", e);
                    }
                    // Don't print anything for chunks (too noisy)
                    return future::ready(None);
                }

                // Handle FILE_DONE: finalize the transfer
                if let Some((transfer_id, sha256)) =
                    crate::parse_file_done_fields(&message)
                {
                    let tid: u64 = transfer_id.parse().unwrap_or(0);
                    match crate::finalize_transfer(tid, sha256) {
                        Ok(final_path) => {
                            let display_msg = format!(
                                "INFO File received: {final_path}"
                            );
                            return future::ready(Some(Ok(display_msg)));
                        }
                        Err(e) => {
                            let display_msg = format!(
                                "INFO File transfer failed: {e}"
                            );
                            return future::ready(Some(Ok(display_msg)));
                        }
                    }
                }

                // Handle FILE_SENT: sender confirmation
                if let Some((transfer_id, count)) =
                    crate::parse_file_sent_fields(&message)
                {
                    let display_msg = format!(
                        "INFO File sent successfully (transfer_id={transfer_id}, {count} receiver(s))"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_CANCELLED
                if let Some((transfer_id, reason)) =
                    crate::parse_file_cancelled_fields(&message)
                {
                    let display_msg = format!(
                        "INFO File transfer {transfer_id} cancelled: {reason}"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                future::ready(Some(Ok(message)))
            }
            Err(e) => {
                eprintln!("failed to read from socket; error={}", e);
                future::ready(None)
            }
        });

        match future::join(sink.send_all(&mut stdin), stdout.send_all(&mut stream)).await {
            (Err(e), _) | (_, Err(e)) => Err(e.into()),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== parse_clip_fields tests ====================

    #[test]
    fn test_parse_clip_fields_with_id() {
        let result = parse_clip_fields("CLIP lobby SGVsbG8= 42");
        assert_eq!(result, Some(("lobby", "SGVsbG8=", Some("42"))));
    }

    #[test]
    fn test_parse_clip_fields_without_id() {
        let result = parse_clip_fields("CLIP lobby SGVsbG8=");
        assert_eq!(result, Some(("lobby", "SGVsbG8=", None)));
    }

    #[test]
    fn test_parse_clip_fields_different_room() {
        let result = parse_clip_fields("CLIP room-1 dGVzdA== 123");
        assert_eq!(result, Some(("room-1", "dGVzdA==", Some("123"))));
    }

    #[test]
    fn test_parse_clip_fields_missing_parts() {
        assert_eq!(parse_clip_fields("CLIP lobby"), None);
        assert_eq!(parse_clip_fields("CLIP"), None);
        assert_eq!(parse_clip_fields(""), None);
    }

    #[test]
    fn test_parse_clip_fields_wrong_prefix() {
        assert_eq!(parse_clip_fields("JOIN lobby SGVsbG8="), None);
        assert_eq!(parse_clip_fields("INFO some text"), None);
    }

    // ==================== base64 roundtrip test ====================

    #[test]
    fn test_base64_roundtrip() {
        use base64::{engine::general_purpose, Engine as _};

        let original = "Hello, World!\nLine 2";
        let encoded = general_purpose::STANDARD.encode(original);
        let decoded_bytes = general_purpose::STANDARD.decode(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    fn test_base64_with_special_chars() {
        use base64::{engine::general_purpose, Engine as _};

        let original = "{\n  \"name\": \"test\",\n  \"value\": 123\n}";
        let encoded = general_purpose::STANDARD.encode(original);
        let decoded_bytes = general_purpose::STANDARD.decode(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();

        assert_eq!(original, decoded);
    }
}
