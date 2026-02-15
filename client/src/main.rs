//! Clipboard sync client for the Rustynaut broker.
//!
//! Transport is TCP or TLS and line-framed (`LinesCodec`). Clipboard updates are
//! sent as base64 to keep payloads binary-safe.

#![warn(rust_2018_idioms)]

mod clipboard_files;
mod tls;
mod tui;
mod completion;
mod reconnect;

use tokio::sync::{mpsc, broadcast};
use tokio::time::{self, Duration};

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::exit;

use rustynaut_common::config::{ClientConfig, ConfigLoader};
use rustynaut_common::constants::{FILE_CHUNK_SIZE, MAX_LINE_LENGTH};
use rustynaut_common::utils::{decode_base64, encode_base64};
use reconnect::{ConnectionState, DisconnectReason, ReconnectContext, ReconnectionManager};
use std::path::Path;
use std::sync::Mutex;

use lazy_static::lazy_static;

fn resolve_download_dir() -> PathBuf {
    if let Some(downloads) = dirs::download_dir() {
        if std::fs::create_dir_all(&downloads).is_ok() {
            return downloads;
        }
    }

    if let Some(home) = dirs::home_dir() {
        let candidate = home.join("Downloads");
        if std::fs::create_dir_all(&candidate).is_ok() {
            return candidate;
        }
    }

    std::env::temp_dir()
}

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

const BANNER: &str = concat!(
    r#"
 ██████  ██      ██ ███████ ███    ██ ████████ 
 ██      ██      ██ ██      ████   ██    ██    
 ██      ██      ██ █████   ██ ██  ██    ██    
 ██      ██      ██ ██      ██  ██ ██    ██    
 ██████  ███████ ██ ███████ ██   ████    ██    
                                               
 https://github.com/aszazeroth/rustynaut v"#,
    env!("CARGO_PKG_VERSION"),
    "\n"
);

/// Parsed command-line arguments
#[allow(dead_code)]
struct Args {
    verbose: bool,
    addr: SocketAddr,
    username: String,
    room: String,
    enroll_token: Option<String>,
    cert_dir: PathBuf,
    config_path: Option<String>,
    dump_config: bool,
}

fn parse_args() -> Result<(Args, ClientConfig), Box<dyn Error>> {
    let mut verbose = false;
    let mut enroll_token: Option<String> = None;
    let mut cert_dir: Option<PathBuf> = None;
    let mut config_path: Option<String> = None;
    let mut dump_config = false;
    let mut positional = Vec::new();

    let mut args_iter = env::args().skip(1).peekable();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
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
            "--config" | "-c" => {
                config_path = Some(
                    args_iter
                        .next()
                        .ok_or("--config requires a path argument")?,
                );
            }
            "--dump-config" => dump_config = true,
            "--help" | "-h" => {
                eprintln!("usage: client [OPTIONS] <addr> [username] [room]");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --verbose, -v           Enable verbose logging");
                eprintln!("  --enroll <TOKEN>        Enroll with broker using token");
                eprintln!("  --cert-dir <PATH>       Certificate directory");
                eprintln!("                          (default: ~/.config/rustynaut/client)");
                eprintln!("  --config, -c <PATH>    Config file path");
                eprintln!("  --dump-config           Print effective config and exit");
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown flag: {arg}").into());
            }
            _ => positional.push(arg),
        }
    }

    // Load configuration (file + env vars)
    let config = ConfigLoader::load_client_config(config_path.as_deref())?;

    // Handle --dump-config before requiring addr
    if dump_config {
        let toml = ConfigLoader::client_to_toml_string(&config)?;
        println!("{}", toml);
        std::process::exit(0);
    }

    let addr = positional
        .first()
        .map(|s| s.as_str())
        .or(config.connection.broker_address.as_deref())
        .ok_or("usage: client [OPTIONS] <addr> [username] [room]")?
        .parse::<SocketAddr>()?;

    let username = positional
        .get(1)
        .cloned()
        .or_else(|| config.connection.default_username.clone())
        .unwrap_or_else(default_username);
    let room = positional
        .get(2)
        .cloned()
        .or_else(|| config.connection.default_room.clone())
        .unwrap_or_else(|| "lobby".to_string());

    // CLI cert_dir overrides config
    let cert_dir = cert_dir.unwrap_or(config.connection.tls.cert_dir.clone());

    // CLI verbose overrides config logging level
    let verbose = verbose || config.logging.level == "debug" || config.logging.level == "trace";

    let args = Args {
        verbose,
        addr,
        username,
        room,
        enroll_token,
        cert_dir,
        config_path,
        dump_config,
    };

    Ok((args, config))
}

/// Spawn a new network connection task
fn spawn_network_connection(
    addr: SocketAddr,
    tls_config: tls::TlsClientConfig,
    ui_tx: mpsc::Sender<tui::Message>,
    conn_status_tx: broadcast::Sender<ConnectionState>,
    verbose: bool,
    mut net_rx: mpsc::Receiver<String>,
) {
    tokio::spawn(async move {
        use futures::{SinkExt, StreamExt};
        use tokio::net::TcpStream;
        use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

        let tcp_stream = match TcpStream::connect(addr).await {
            Ok(s) => s,
            Err(_) => {
                let _ = conn_status_tx.send(ConnectionState::Disconnected);
                return;
            }
        };

        let host = addr.ip().to_string();
        let tls_stream = match tls::connect_tls(&tls_config.connector, tcp_stream, &host, false).await {
            Ok(s) => s,
            Err(_) => {
                let _ = conn_status_tx.send(ConnectionState::Disconnected);
                return;
            }
        };

        let (r, w) = tokio::io::split(tls_stream);
        let mut sink = FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_LINE_LENGTH));
        let mut stream_reader = FramedRead::new(
            r,
            LinesCodec::new_with_max_length(MAX_LINE_LENGTH),
        );

        // Notify that we're connected
        let _ = conn_status_tx.send(ConnectionState::Connected);

        loop {
            tokio::select! {
                msg = stream_reader.next() => {
                    match msg {
                        Some(Ok(message)) => {
                            if let Err(e) = tcp::handle_message(&message, &ui_tx, verbose).await {
                                let _ = ui_tx.send(tui::Message::Error { text: format!("Error handling message: {}", e), timestamp: std::time::SystemTime::now() }).await;
                            }
                        }
                        Some(Err(e)) => {
                            let _ = ui_tx.send(tui::Message::Error { text: format!("Read error: {}", e), timestamp: std::time::SystemTime::now() }).await;
                            break;
                        }
                        None => {
                            let _ = ui_tx.send(tui::Message::Info { text: "Connection closed".to_string(), timestamp: std::time::SystemTime::now() }).await;
                            break;
                        }
                    }
                }
                msg = net_rx.recv() => {
                    match msg {
                        Some(line) => {
                            if let Err(e) = sink.send(&line).await {
                                let _ = ui_tx.send(tui::Message::Error { text: format!("Write error: {}", e), timestamp: std::time::SystemTime::now() }).await;
                                break;
                            }
                        }
                        None => {
                            break;
                        }
                    }
                }
            }
        }

        let _ = conn_status_tx.send(ConnectionState::Disconnected);
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Parse args first (before TUI setup)
    let (args, _config) = parse_args()?;

    // Handle enrollment if requested (before TUI)
    if let Some(ref token) = args.enroll_token {
        println!("{BANNER}");
        enroll(&args.addr, token, &args.username, &args.cert_dir).await?;
        println!("Auto-connecting with TLS...\n");
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Check if we have certificates
    if !tls::is_enrolled(&args.cert_dir) {
        println!("{BANNER}");
        eprintln!("TLS is enabled by default but you are not enrolled.");
        eprintln!("Use --enroll <token> to enroll first.");
        eprintln!("Certificates should be in: {:?}", args.cert_dir);
        return Err("Not enrolled for TLS".into());
    }

    // Setup TUI
    let mut terminal = tui::setup_terminal()?;
    let mut app = tui::App::new(args.username.clone(), args.room.clone());
    app.handle_room_join(&args.room);
    app.completion_context_mut()
        .add_user(args.username.clone());

    // Add initial messages
    app.add_info("Welcome to Rustynaut!");
    app.add_info(format!("Connecting to {} as {}...", args.addr, args.username));
    app.add_info("Click/drag messages to select, drag in input to select text, 'y' to copy, ESC to deselect");

    // UI channel for receiving messages from network
    let (ui_tx, ui_rx) = mpsc::channel::<tui::Message>(100);
    
    // Channel for connection status (for reconnection handling)
    let (conn_status_tx, conn_status_rx) = broadcast::channel::<ConnectionState>(1);

    // Channel for clipboard to send messages to network (TUI loop forwards)
    let (clipboard_tx, clipboard_rx) = mpsc::channel::<String>(100);

    // Clone for clipboard task
    let room_for_clipboard = args.room.clone();
    let clipboard_tx_clone = clipboard_tx.clone();

    // Spawn clipboard monitoring task
    tokio::spawn(async move {
        let mut previous_content = match CLIPBOARD.get_string_contents() {
            Ok(s) => s,
            Err(err) => {
                let err_str = err.to_string();
                if err_str.contains("empty") || err_str.contains("Empty") {
                    String::new()
                } else {
                    return;
                }
            }
        };
        let mut previous_files: Option<Vec<std::path::PathBuf>> = None;
        let mut interval = time::interval(Duration::from_secs(2));
        
        loop {
            interval.tick().await;

            // Check for native file clipboard (Finder/Explorer copy)
            if let Some(files) = clipboard_files::get_clipboard_files() {
                let files_changed = previous_files.as_ref() != Some(&files);
                if files_changed {
                    for path in &files {
                        if let Ok(metadata) = std::fs::metadata(path) {
                            if metadata.is_file() {
                                let size = metadata.len();
                                let filename = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown");
                let filename_b64 = encode_base64(filename);

                                if let Ok(mut pending) = PENDING_OUTGOING.lock() {
                                    pending.insert(filename_b64.clone(), PendingOutgoingFile {
                                        path: path.clone(),
                                        size,
                                    });
                                }

                                let offer_msg = format!(
                                    "FILE_OFFER {room_for_clipboard} {filename_b64} {size}"
                                );
                                if clipboard_tx_clone.send(offer_msg).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    previous_files = Some(files);
                    if let Ok(text) = CLIPBOARD.get_string_contents() {
                        previous_content = text;
                    }
                    continue;
                }
            }

            let current_content = match CLIPBOARD.get_string_contents() {
                Ok(s) => s,
                Err(err) => {
                    let err_str = err.to_string();
                    if err_str.contains("empty") || err_str.contains("Empty") {
                        String::new()
                    } else {
                        continue;
                    }
                }
            };
            
            if current_content != previous_content {
                previous_files = None;

                if current_content.is_empty() {
                    previous_content = current_content;
                    continue;
                }

                let is_echo = if let Ok(recent) = RECENT_APPLIED_CLIPS.lock() {
                    recent.contains(&current_content)
                } else {
                    false
                };

                if is_echo {
                    previous_content = current_content;
                    continue;
                }

                if let Some((path, size, is_file)) = detect_file_path(&current_content) {
                    if is_file {
                        let filename = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown");
                        let filename_b64 = encode_base64(filename);

                        if let Ok(mut pending) = PENDING_OUTGOING.lock() {
                            pending.insert(filename_b64.clone(), PendingOutgoingFile {
                                path: path.clone(),
                                size,
                            });
                        }

                        let offer_msg =
                            format!("FILE_OFFER {room_for_clipboard} {filename_b64} {size}");
                        if clipboard_tx_clone.send(offer_msg).await.is_err() {
                            break;
                        }

                        previous_content = current_content;
                        continue;
                    }
                }

                let encoded = encode_base64(&current_content);
                if clipboard_tx_clone
                    .send(format!("CLIP {room_for_clipboard} {encoded}"))
                    .await
                    .is_err()
                {
                    break;
                }
                previous_content = current_content;
            }
        }
    });

    // Get TLS config for connections
    let tls_config = tls::init_tls_with_client_cert(&args.cert_dir)
        .map_err(|e| -> Box<dyn Error> { e })?;

    // Pass config and ui_tx to TUI loop
    let config = _config;

    // Main TUI event loop - handles connection, reconnection, and UI
    let result = run_tui_loop(
        &mut terminal,
        &mut app,
        ui_tx,
        ui_rx,
        conn_status_rx,
        config,
        args.addr.clone(),
        tls_config,
        args.username.clone(),
        args.room.clone(),
        args.verbose,
    ).await;

    // Restore terminal
    tui::restore_terminal()?;
    
    result
}

/// Main TUI event loop with reconnection support
async fn run_tui_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::App,
    ui_tx: mpsc::Sender<tui::Message>,
    mut ui_rx: mpsc::Receiver<tui::Message>,
    mut conn_status_rx: broadcast::Receiver<ConnectionState>,
    config: ClientConfig,
    broker_addr: SocketAddr,
    tls_config: tls::TlsClientConfig,
    username: String,
    room: String,
    verbose: bool,
) -> Result<(), Box<dyn Error>> {
    let mut last_tick = tokio::time::Instant::now();
    let tick_rate = tokio::time::Duration::from_millis(100);

    // Connection state - channels for network communication
    let (conn_status_tx, _) = broadcast::channel::<ConnectionState>(8);
    let (net_tx, mut net_rx) = mpsc::channel::<String>(100);
    let mut net_tx_opt: Option<mpsc::Sender<String>> = Some(net_tx);
    
    // Reconnection manager
    let reconnect_config = config.connection.reconnect.clone();
    let reconnect_ctx = ReconnectContext {
        broker_addr: broker_addr.to_string(),
        username: username.clone(),
        room: room.clone(),
    };
    let mut reconnect_mgr = ReconnectionManager::new(reconnect_config, reconnect_ctx);

    // Spawn initial network connection
    spawn_network_connection(
        broker_addr,
        tls_config.clone(),
        ui_tx.clone(),
        conn_status_tx.clone(),
        verbose,
        net_rx,
    );

    // Send initial USER and JOIN
    if let Some(ref tx) = net_tx_opt {
        let _ = tx.send(format!("USER {}", username)).await;
        let _ = tx.send(format!("JOIN {}", room)).await;
    }

    loop {
        // Draw UI
        terminal.draw(|frame| app.draw(frame))?;

        // Check for UI messages from network
        while let Ok(msg) = ui_rx.try_recv() {
            match msg {
                tui::Message::Info { text, .. } => app.handle_info(&text),
                tui::Message::Error { text, .. } => app.add_error(text),
                tui::Message::Chat { user, text, .. } => app.add_chat(user, text),
                tui::Message::FileOffer { room, user, filename, size, .. } => {
                    app.handle_file_offer(&room, &user, &filename, &size)
                }
                tui::Message::FileTransfer { text, .. } => app.handle_file_transfer(&text),
                tui::Message::Clip { room, preview, .. } => app.handle_clip(&room, &preview),
            }
        }

        // Check for connection status changes
        let mut reconnected = false;
        while let Ok(state) = conn_status_rx.try_recv() {
            match state {
                ConnectionState::Disconnected => {
                    app.add_info("Disconnected from broker");
                    reconnect_mgr.set_disconnected();
                }
                ConnectionState::Connected => {
                    app.add_info("Reconnected to broker");
                    reconnect_mgr.set_connected();
                    reconnected = true;
                }
                ConnectionState::Reconnecting => {
                    app.add_info("Attempting to reconnect...");
                }
            }
        }

        // If we just reconnected, re-send USER and JOIN
        if reconnected {
            reconnect_mgr.set_connected();
            if let Some(ref tx) = net_tx_opt {
                let _ = tx.send(format!("USER {}", username)).await;
                let _ = tx.send(format!("JOIN {}", room)).await;
            }
        }

        // Check if we need to reconnect
        if reconnect_mgr.state() == ConnectionState::Disconnected && reconnect_mgr.is_enabled() {
            if reconnect_mgr.should_reconnect(DisconnectReason::NetworkError) {
                let delay = reconnect_mgr.calculate_backoff();
                app.add_info(format!("Reconnecting in {}s... (attempt {}/{})", 
                    delay.as_secs(),
                    reconnect_mgr.attempt() + 1,
                    reconnect_mgr.max_attempts()));
                
                // Wait for backoff
                reconnect_mgr.wait_backoff().await;

                // Create new channel and spawn new network connection
                let (new_tx, new_rx) = mpsc::channel::<String>(100);
                let new_tx_for_sender = new_tx.clone();

                // Update global FILE_TX for file transfers
                if let Ok(mut guard) = FILE_TX.lock() {
                    *guard = Some(new_tx.clone());
                }

                // Update our sender reference
                net_tx_opt = Some(new_tx_for_sender);

                // Spawn new network task with new receiver
                spawn_network_connection(
                    broker_addr,
                    tls_config.clone(),
                    ui_tx.clone(),
                    conn_status_tx.clone(),
                    verbose,
                    new_rx,
                );

                // Re-send USER and JOIN using new_tx
                let _ = new_tx.send(format!("USER {}", username)).await;
                let _ = new_tx.send(format!("JOIN {}", room)).await;
            } else if reconnect_mgr.attempt() >= reconnect_mgr.max_attempts() {
                app.add_info("Reconnection failed. Press Enter to retry, Ctrl+C to exit");
            }
        }

        // Poll for events with timeout
        if crossterm::event::poll(std::time::Duration::from_millis(10))? {
            match crossterm::event::read()? {
                crossterm::event::Event::Key(key) => {
                    if key.kind == crossterm::event::KeyEventKind::Press {
                        match key.code {
                            crossterm::event::KeyCode::Char('c')
                                if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                            {
                                app.should_quit = true;
                            }
                            crossterm::event::KeyCode::Char('y') => {
                                app.copy_selected_message();
                            }
                            crossterm::event::KeyCode::Char(c) => {
                                if !app.current_completions.is_empty() {
                                    app.cancel_completion();
                                }
                                if let Some(cmd) = app.handle_input(c) {
                                    if cmd == "/quit" || cmd == "/exit" {
                                        app.should_quit = true;
                                    } else if let Err(e) = process_command(cmd, net_tx_opt.as_ref()).await {
                                        app.add_error(format!("Failed to send: {}", e));
                                    }
                                }
                            }
                            crossterm::event::KeyCode::Backspace => app.handle_backspace(),
                            crossterm::event::KeyCode::Left => app.cursor_left(),
                            crossterm::event::KeyCode::Right => app.cursor_right(),
                            crossterm::event::KeyCode::Up => {
                                if !app.current_completions.is_empty() {
                                    app.handle_tab();
                                } else {
                                    app.history_previous();
                                }
                            }
                            crossterm::event::KeyCode::Down => {
                                if !app.current_completions.is_empty() {
                                    app.handle_tab();
                                } else {
                                    app.history_next();
                                }
                            }
                            crossterm::event::KeyCode::Tab => {
                                app.handle_tab();
                            }
                            crossterm::event::KeyCode::Enter => {
                                if !app.current_completions.is_empty() {
                                    app.apply_completion();
                                } else if let Some(cmd) = app.handle_input('\n') {
                                    if cmd == "/quit" || cmd == "/exit" {
                                        app.should_quit = true;
                                    } else if let Err(e) = process_command(cmd, net_tx_opt.as_ref()).await {
                                        app.add_error(format!("Failed to send: {}", e));
                                    }
                                }
                            }
                            crossterm::event::KeyCode::F(1) => app.toggle_sidebar(),
                            crossterm::event::KeyCode::Esc => {
                                if app.text_selection.is_some() {
                                    app.clear_text_selection();
                                } else if !app.current_completions.is_empty() {
                                    app.cancel_completion();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                crossterm::event::Event::Mouse(mouse_event) => {
                    match mouse_event.kind {
                        crossterm::event::MouseEventKind::Down(button) => {
                            if button == crossterm::event::MouseButton::Left {
                                app.start_selection_drag(mouse_event.row, mouse_event.column);
                            }
                        }
                        crossterm::event::MouseEventKind::Drag(_button) => {
                            app.update_selection_drag(mouse_event.row, mouse_event.column);
                        }
                        crossterm::event::MouseEventKind::Up(_button) => {
                            app.end_selection_drag();
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = tokio::time::Instant::now();
        }

        if app.should_quit {
            let _ = conn_status_tx.send(ConnectionState::Disconnected);
            break;
        }
    }

    Ok(())
}

/// Process a user command and send to network
async fn process_command(cmd: String, net_tx: Option<&mpsc::Sender<String>>) -> Result<(), Box<dyn Error>> {
    let message = if cmd.starts_with('/') {
        format!("CMD {}", cmd)
    } else {
        format!("SAY {}", cmd)
    };
    
    if let Some(tx) = net_tx {
        tx.send(message).await.map_err(|e| e.into())
    } else {
        Err("Not connected".into())
    }
}

/// Enroll with the broker to obtain client certificates
async fn enroll(
    addr: &SocketAddr,
    token: &str,
    username: &str,
    cert_dir: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    use futures::{SinkExt, StreamExt};
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

    eprintln!("TCP: Connecting to {}...", addr);
    let stream = TcpStream::connect(addr).await?;
    eprintln!("TCP: Connected successfully to {}", addr);

    // Extract host for TLS SNI
    let host = addr.ip().to_string();
        let tls_stream = tls::connect_tls(&tls_config.connector, stream, &host, true)
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
                rustynaut_common::tls::parse_enrolled_response(&line)
                    .map_err(|e| -> Box<dyn Error> { e })?;
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
    let decoded = decode_base64(content)?;
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
        let chunk_b64 = encode_base64(chunk);
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
fn prepare_incoming_transfer(transfer_id: u64, filename: &str, _size: u64) -> Result<(), String> {
    let downloads = resolve_download_dir();
    let temp_path = downloads.join(format!(".rustynaut_incoming_{}", transfer_id));

    let file = std::fs::File::create(&temp_path)
        .map_err(|e| format!("Failed to create temp file: {}", e))?;

    let mut transfers = INCOMING_TRANSFERS.lock().map_err(|e| format!("Lock failed: {}", e))?;
    transfers.insert(transfer_id, IncomingTransfer {
        filename: filename.to_string(),
        file,
        temp_path,
        bytes_received: 0,
    });
    Ok(())
}

/// Handle incoming FILE_CHUNK - write bytes to temp file
fn handle_file_chunk(transfer_id: u64, _offset: u64, chunk_b64: &str) -> Result<(), String> {
    let chunk_data = decode_base64(chunk_b64)
        .map_err(|e| format!("Invalid base64: {}", e))?;

    let mut transfers = INCOMING_TRANSFERS.lock().map_err(|e| format!("Lock failed: {}", e))?;
    let transfer = transfers.get_mut(&transfer_id).ok_or_else(|| format!("Transfer {} not found", transfer_id))?;
    transfer.file.write_all(&chunk_data).map_err(|e| format!("Write failed: {}", e))?;
    transfer.bytes_received += chunk_data.len() as u64;
    Ok(())
}

/// Handle FILE_DONE - verify checksum and move file to final location
fn finalize_transfer(transfer_id: u64, _expected_sha256: &str) -> Result<String, String> {
    let mut transfers = INCOMING_TRANSFERS.lock().map_err(|e| format!("Lock failed: {}", e))?;
    let transfer_info = transfers.remove(&transfer_id).ok_or_else(|| "No such transfer".to_string())?;
    drop(transfers);

    // Close the file (drop it)
    drop(transfer_info.file);

    // Move to final location
    let downloads = resolve_download_dir();
    let mut final_path = downloads.join(&transfer_info.filename);

    // Handle file already exists - add number suffix
    let mut counter = 1;
    while final_path.exists() {
        let stem = Path::new(&transfer_info.filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = Path::new(&transfer_info.filename)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!(".{}", s))
            .unwrap_or_default();
        final_path = downloads.join(format!("{} ({}){}", stem, counter, ext));
        counter += 1;
    }

    std::fs::rename(&transfer_info.temp_path, &final_path)
        .map_err(|e| format!("Failed to move file: {}", e))?;

    Ok(final_path.display().to_string())
}

mod tcp {
    use futures::{future, Sink, SinkExt, Stream, StreamExt};
    use rustynaut_common::constants::MAX_LINE_LENGTH;
    use rustynaut_common::parsing::{
        parse_clip_fields, parse_file_cancelled_fields, parse_file_chunk_fields,
        parse_file_done_fields, parse_file_incoming_fields, parse_file_offer_fields,
        parse_file_sent_fields, parse_file_start_fields, parse_say_fields,
    };
    use rustynaut_common::utils::{decode_base64, format_size};
    use std::{error::Error, net::SocketAddr};
    use tokio::net::TcpStream;
    use tokio::sync::mpsc;
    use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

    /// Connect using channels for TUI integration
    #[allow(dead_code)]
    pub async fn connect_with_channels(
        addr: &SocketAddr,
        mut net_rx: mpsc::Receiver<String>,
        ui_tx: mpsc::Sender<crate::tui::Message>,
        verbose: bool,
    ) -> Result<(), Box<dyn Error>> {
        let mut stream = TcpStream::connect(addr).await?;
        let (r, w) = stream.split();
        
        let mut sink = FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_LINE_LENGTH));
        let mut stream_reader = FramedRead::new(
            r,
            LinesCodec::new_with_max_length(MAX_LINE_LENGTH),
        );

        // Send initial connection message
        let _ = ui_tx.send(crate::tui::Message::Info { text: format!("Connected to {}", addr), timestamp: std::time::SystemTime::now() }).await;

        loop {
            tokio::select! {
                // Read from network
                msg = stream_reader.next() => {
                    match msg {
                        Some(Ok(message)) => {
                            if let Err(e) = handle_message(&message, &ui_tx, verbose).await {
                                let _ = ui_tx.send(crate::tui::Message::Error { text: format!("Error handling message: {}", e), timestamp: std::time::SystemTime::now() }).await;
                            }
                        }
                        Some(Err(e)) => {
                            let _ = ui_tx.send(crate::tui::Message::Error { text: format!("Read error: {}", e), timestamp: std::time::SystemTime::now() }).await;
                            break;
                        }
                        None => {
                            let _ = ui_tx.send(crate::tui::Message::Info { text: "Connection closed".to_string(), timestamp: std::time::SystemTime::now() }).await;
                            break;
                        }
                    }
                }
                // Write to network
                msg = net_rx.recv() => {
                    match msg {
                        Some(line) => {
                            if let Err(e) = sink.send(&line).await {
                                let _ = ui_tx.send(crate::tui::Message::Error { text: format!("Write error: {}", e), timestamp: std::time::SystemTime::now() }).await;
                                break;
                            }
                        }
                        None => {
                            // Channel closed
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle a message from the network and send to UI
    pub async fn handle_message(
        message: &str,
        ui_tx: &mpsc::Sender<crate::tui::Message>,
        verbose: bool,
    ) -> Result<(), String> {
        // Apply clipboard updates silently (avoid printing base64 payloads).
        if let Some((room, clipboard_b64, id)) = parse_clip_fields(message) {
            let err_msg = if let Err(err) = crate::replace_clipboard_content(clipboard_b64) {
                Some(format!("could not replace the clipboard content, {}", err))
            } else {
                None
            };
            
            if let Some(msg) = err_msg {
                let _ = ui_tx.send(crate::tui::Message::Error { 
                    text: msg,
                    timestamp: std::time::SystemTime::now()
                }).await;
            }

            if verbose {
                let id_str = id.unwrap_or("?");
                let _ = ui_tx.send(crate::tui::Message::Info { 
                    text: format!("clip applied: room={} id={} (b64_len={})", room, id_str, clipboard_b64.len()),
                    timestamp: std::time::SystemTime::now()
                }).await;
            }
            return Ok(());
        }

        // Handle FILE_OFFER: display as a user-friendly message
        if let Some((room, username, filename_b64, size_str)) =
            parse_file_offer_fields(message)
        {
            let filename = decode_base64(filename_b64)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| "<unknown>".to_string());
            let size: u64 = size_str.parse().unwrap_or(0);
            let size_display = format_size(size);
            
            let _ = ui_tx.send(crate::tui::Message::FileOffer {
                room: room.to_string(),
                user: username.to_string(),
                filename,
                size: size_display,
                timestamp: std::time::SystemTime::now()
            }).await;
            return Ok(());
        }

        // Handle FILE_START: sender should begin transfer
        if let Some((transfer_id, filename_b64, acceptor_count)) =
            parse_file_start_fields(message)
        {
            let filename = decode_base64(filename_b64)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| "<unknown>".to_string());
            
            let _ = ui_tx.send(crate::tui::Message::FileTransfer {
                text: format!(
                    "File transfer started: {} (transfer_id={}, {} receiver(s))",
                    filename, transfer_id, acceptor_count
                ),
                timestamp: std::time::SystemTime::now()
            }).await;
            
            // Spawn a task to send file chunks
            let filename_b64_owned = filename_b64.to_string();
            let transfer_id_owned = transfer_id.to_string();
            tokio::spawn(async move {
                let messages = crate::generate_file_chunks(&filename_b64_owned, &transfer_id_owned);
                let tx = crate::FILE_TX.lock().ok().and_then(|guard| guard.clone());
                if let Some(tx) = tx {
                    for msg in messages {
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }
            });
            
            return Ok(());
        }

        // Handle FILE_INCOMING: receiver should prepare to receive
        if let Some((transfer_id, filename_b64, size_str)) =
            parse_file_incoming_fields(message)
        {
            let filename = decode_base64(filename_b64)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| "<unknown>".to_string());
            let size: u64 = size_str.parse().unwrap_or(0);
            
            let tid: u64 = transfer_id.parse().unwrap_or(0);
            if let Err(e) = crate::prepare_incoming_transfer(tid, &filename, size) {
                let _ = ui_tx.send(crate::tui::Message::Error { 
                    text: format!("Failed to prepare transfer: {}", e),
                    timestamp: std::time::SystemTime::now()
                }).await;
            }
            
            let _ = ui_tx.send(crate::tui::Message::FileTransfer {
                text: format!("Receiving file: {} ({} bytes)", filename, size),
                timestamp: std::time::SystemTime::now()
            }).await;
            return Ok(());
        }

        // Handle FILE_CHUNK: write chunk to temp file
        if let Some((transfer_id, offset, chunk_b64)) = parse_file_chunk_fields(message) {
            let tid: u64 = transfer_id.parse().unwrap_or(0);
            let off: u64 = offset.parse().unwrap_or(0);
            if let Err(e) = crate::handle_file_chunk(tid, off, chunk_b64) {
                let _ = ui_tx.send(crate::tui::Message::Error { text: format!("Chunk error: {}", e), timestamp: std::time::SystemTime::now() }).await;
            }
            return Ok(());
        }

        // Handle FILE_DONE: finalize the transfer
        if let Some((transfer_id, sha256)) = parse_file_done_fields(message) {
            let tid: u64 = transfer_id.parse().unwrap_or(0);
            match crate::finalize_transfer(tid, sha256) {
                Ok(final_path) => {
                    let _ = ui_tx.send(crate::tui::Message::FileTransfer {
                        text: format!("File received: {}", final_path),
                        timestamp: std::time::SystemTime::now()
                    }).await;
                }
                Err(e) => {
                    let _ = ui_tx.send(crate::tui::Message::Error {
                        text: format!("File transfer failed: {}", e),
                        timestamp: std::time::SystemTime::now()
                    }).await;
                }
            }
            return Ok(());
        }

        // Handle FILE_SENT: sender confirmation
        if let Some((transfer_id, count)) = parse_file_sent_fields(message) {
            let _ = ui_tx.send(crate::tui::Message::FileTransfer {
                text: format!(
                    "File sent successfully (transfer_id={}, {} receiver(s))",
                    transfer_id, count
                ),
                timestamp: std::time::SystemTime::now()
            }).await;
            return Ok(());
        }

        // Handle FILE_CANCELLED
        if let Some((transfer_id, reason)) = parse_file_cancelled_fields(message) {
            let _ = ui_tx.send(crate::tui::Message::FileTransfer {
                text: format!("File transfer {} cancelled: {}", transfer_id, reason),
                timestamp: std::time::SystemTime::now()
            }).await;
            return Ok(());
        }

         // Handle SAY messages
    if let Some((user, text)) = parse_say_fields(message) {
        let _ = ui_tx.send(crate::tui::Message::Chat {
            user: user.to_string(),
            text: text.to_string(),
            timestamp: std::time::SystemTime::now()
        }).await;
        return Ok(());
    }

    // Special handling for token commands - this catches the response from broker when /token sent
    if message.starts_with("INFO Token copied to clipboard") {
        // The command was successfully processed, just display a helpful message
        let _ = ui_tx.send(crate::tui::Message::Info { 
            text: "Token copied to clipboard".to_string(), 
            timestamp: std::time::SystemTime::now() 
        }).await;
        return Ok(());
    }
    
    // Default: treat as info message
         let _ = ui_tx.send(crate::tui::Message::Info { text: message.to_string(), timestamp: std::time::SystemTime::now() }).await;
         Ok(())
    }

    #[allow(dead_code)]
    pub async fn connect(
        addr: &SocketAddr,
        mut stdin: impl Stream<Item = Result<String, LinesCodecError>> + Unpin,
        mut stdout: impl Sink<String, Error = LinesCodecError> + Unpin,
        verbose: bool,
    ) -> Result<(), Box<dyn Error>> {
        let mut stream = TcpStream::connect(addr).await?;
        let (r, w) = stream.split();
        let mut sink = FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_LINE_LENGTH));
        let mut stream = FramedRead::new(
            r,
            LinesCodec::new_with_max_length(MAX_LINE_LENGTH),
        )
        .filter_map(|i| match i {
            Ok(message) => {
                // Apply clipboard updates silently (avoid printing base64 payloads).
                if let Some((room, clipboard_b64, id)) = parse_clip_fields(&message) {
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
                    parse_file_offer_fields(&message)
                {
                    // Decode filename from base64
                    let filename = decode_base64(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = format_size(size);
                    // Display the file offer to the user with accept hint
                    let display_msg = format!(
                        "INFO [{room}] {username} offers file: {filename} ({size_display}) - use /accept {username} {filename} to receive"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_START: sender should begin transfer
                if let Some((transfer_id, filename_b64, acceptor_count)) =
                    parse_file_start_fields(&message)
                {
                    let filename = decode_base64(filename_b64)
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
                    parse_file_incoming_fields(&message)
                {
                    let filename = decode_base64(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = format_size(size);
                    
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
                    parse_file_chunk_fields(&message)
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
                    parse_file_done_fields(&message)
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
                    parse_file_sent_fields(&message)
                {
                    let display_msg = format!(
                        "INFO File sent successfully (transfer_id={transfer_id}, {count} receiver(s))"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_CANCELLED
                if let Some((transfer_id, reason)) =
                    parse_file_cancelled_fields(&message)
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
    use futures::{future, Sink, SinkExt, Stream, StreamExt};
    use rustynaut_common::constants::MAX_LINE_LENGTH;
    use rustynaut_common::parsing::{
        parse_clip_fields, parse_file_cancelled_fields, parse_file_chunk_fields,
        parse_file_done_fields, parse_file_incoming_fields, parse_file_offer_fields,
        parse_file_sent_fields, parse_file_start_fields,
    };
    use rustynaut_common::utils::{decode_base64, format_size};
    use std::{error::Error, net::SocketAddr};
    use tokio::net::TcpStream;
    use tokio::sync::mpsc;
    use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

    /// Connect using channels for TUI integration
    pub async fn connect_with_channels(
        addr: &SocketAddr,
        tls_config: &crate::tls::TlsClientConfig,
        mut net_rx: mpsc::Receiver<String>,
        ui_tx: mpsc::Sender<crate::tui::Message>,
        verbose: bool,
    ) -> Result<(), Box<dyn Error>> {
        let tcp_stream = TcpStream::connect(addr).await?;
        
        // Extract host for TLS SNI
        let host = addr.ip().to_string();
        let tls_stream = crate::tls::connect_tls(&tls_config.connector, tcp_stream, &host, false)
            .await
            .map_err(|e| -> Box<dyn Error> { e })?;

        let (r, w) = tokio::io::split(tls_stream);
        let mut sink = FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_LINE_LENGTH));
        let mut stream_reader = FramedRead::new(
            r,
            LinesCodec::new_with_max_length(MAX_LINE_LENGTH),
        );

        // Send initial connection message
        let _ = ui_tx.send(crate::tui::Message::Info { text: format!("TLS connected to {}", addr), timestamp: std::time::SystemTime::now() }).await;

        loop {
            tokio::select! {
                // Read from network
                msg = stream_reader.next() => {
                    match msg {
                        Some(Ok(message)) => {
                            if let Err(e) = super::tcp::handle_message(&message, &ui_tx, verbose).await {
                                let _ = ui_tx.send(crate::tui::Message::Error { text: format!("Error handling message: {}", e), timestamp: std::time::SystemTime::now() }).await;
                            }
                        }
                        Some(Err(e)) => {
                            let _ = ui_tx.send(crate::tui::Message::Error { text: format!("Read error: {}", e), timestamp: std::time::SystemTime::now() }).await;
                            break;
                        }
                        None => {
                            let _ = ui_tx.send(crate::tui::Message::Info { text: "Connection closed".to_string(), timestamp: std::time::SystemTime::now() }).await;
                            break;
                        }
                    }
                }
                // Write to network
                msg = net_rx.recv() => {
                    match msg {
                        Some(line) => {
                            if let Err(e) = sink.send(&line).await {
                                let _ = ui_tx.send(crate::tui::Message::Error { text: format!("Write error: {}", e), timestamp: std::time::SystemTime::now() }).await;
                                break;
                            }
                        }
                        None => {
                            // Channel closed
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
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
        let tls_stream = crate::tls::connect_tls(&tls_config.connector, tcp_stream, &host, false)
            .await
            .map_err(|e| -> Box<dyn Error> { e })?;

        let (r, w) = tokio::io::split(tls_stream);
        let mut sink = FramedWrite::new(w, LinesCodec::new_with_max_length(MAX_LINE_LENGTH));
        let mut stream = FramedRead::new(
            r,
            LinesCodec::new_with_max_length(MAX_LINE_LENGTH),
        )
        .filter_map(|i| match i {
            Ok(message) => {
                // Apply clipboard updates silently (avoid printing base64 payloads).
                if let Some((room, clipboard_b64, id)) = parse_clip_fields(&message) {
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
                    parse_file_offer_fields(&message)
                {
                    // Decode filename from base64
                    let filename = decode_base64(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = format_size(size);
                    // Display the file offer to the user with accept hint
                    let display_msg = format!(
                        "INFO [{room}] {username} offers file: {filename} ({size_display}) - use /accept {username} {filename} to receive"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_START: sender should begin transfer
                if let Some((transfer_id, filename_b64, acceptor_count)) =
                    parse_file_start_fields(&message)
                {
                    let filename = decode_base64(filename_b64)
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
                    parse_file_incoming_fields(&message)
                {
                    let filename = decode_base64(filename_b64)
                        .ok()
                        .and_then(|bytes| String::from_utf8(bytes).ok())
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let size: u64 = size_str.parse().unwrap_or(0);
                    let size_display = format_size(size);
                    
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
                    parse_file_chunk_fields(&message)
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
                    parse_file_done_fields(&message)
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
                    parse_file_sent_fields(&message)
                {
                    let display_msg = format!(
                        "INFO File sent successfully (transfer_id={transfer_id}, {count} receiver(s))"
                    );
                    return future::ready(Some(Ok(display_msg)));
                }

                // Handle FILE_CANCELLED
                if let Some((transfer_id, reason)) =
                    parse_file_cancelled_fields(&message)
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
