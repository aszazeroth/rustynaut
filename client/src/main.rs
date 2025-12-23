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

use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::exit;

use base64::{engine::general_purpose, Engine as _};

use std::path::Path;
use std::sync::Mutex;

use lazy_static::lazy_static;

lazy_static! {
    static ref CLIPBOARD: Mutex<arboard::Clipboard> = Mutex::new(get_current_clipboard());
    /// Tracks recently applied clipboard content from the network (for echo suppression)
    /// We keep a few recent values since multiple clips can arrive in quick succession
    static ref RECENT_APPLIED_CLIPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
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
        .merge(rx.map(Result::<String, LinesCodecError>::Ok));

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
                    // Display the file offer to the user
                    let display_msg = format!(
                        "INFO [{room}] {username} offers file: {filename} ({size_display})"
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
                    // Display the file offer to the user
                    let display_msg = format!(
                        "INFO [{room}] {username} offers file: {filename} ({size_display})"
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
