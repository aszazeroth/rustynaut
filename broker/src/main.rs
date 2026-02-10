//! Rustynaut Broker - A room-based chat and clipboard sync server.
//!
//! Clients connect via TCP and communicate using a line-framed protocol.
//! Each line is a command (USER, JOIN, CLIP, CMD, SAY) or response (INFO, ERR, CLIP).
//!
//! ## Usage
//!
//! Start the broker:
//!
//!     cargo run --release -- [--verbose] [--tls] [addr]
//!
//! Default address is 127.0.0.1:4242.
//!
//! Connect clients using the rustynaut client or any line-based TCP client.

#![warn(rust_2018_idioms)]

mod tls;
mod tui;

/// Maximum line length for the wire protocol (2MB to handle large base64 clipboard payloads)
/// For files larger than ~1.5MB raw, use the sideband FILE_OFFER protocol instead.
const MAX_LINE_LENGTH: usize = 2 * 1024 * 1024;

use tokio::io::{self, AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_stream::StreamExt;
use tokio_util::codec::{Framed, LinesCodec};

use futures::SinkExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

/// Parsed command-line arguments
struct Args {
    verbose: bool,
    addr: String,
    tls_disabled: bool,
    cert_dir: PathBuf,
    regenerate_token: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Args: broker [--verbose|-v] [--no-tls] [--cert-dir <path>] [--regenerate-token] [addr]
    let args = parse_args(env::args().skip(1))?;

    // NOTE: Tracing to stdout is disabled when TUI is active to avoid screen corruption.
    // Important events are routed through the TUI message channel instead.
    use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};
    let default_directive = if args.verbose {
        "chat=debug"
    } else {
        "chat=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(default_directive.parse()?))
        .with_span_events(FmtSpan::FULL)
        // Use std::io::sink() to prevent stdout/stderr writes that break the TUI
        .with_writer(std::io::sink)
        .init();

    let tls_config = if !args.tls_disabled {
        let server_names = extract_server_names(&args.addr);
        Some(Arc::new(
            tls::init_tls(&args.cert_dir, &server_names, args.regenerate_token)
                .map_err(|e| -> Box<dyn Error> { e })?,
        ))
    } else {
        None
    };

    let state = Arc::new(Mutex::new(Shared::new()));
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let listener = TcpListener::bind(&args.addr).await?;
    tracing::info!("server running on {}", args.addr);

    let (ui_tx, ui_rx) = mpsc::channel::<tui::Message>(100);

    let mut terminal = tui::setup_terminal()?;
    let mut app = tui::App::new(args.addr.clone(), !args.tls_disabled);
    app.add_info(format!("Broker started on {}", &args.addr));
    if !args.tls_disabled {
        app.add_info("TLS enabled");
        if let Some(ref tls_cfg) = tls_config {
            app.add_info(format!("Enrollment token: {}", tls_cfg.enrollment_token));
            app.add_info(format!("CA cert: {:?}", tls_cfg.ca_cert_path));
        }
    } else {
        app.add_error("TLS disabled, connections will not be encrypted");
    }
    app.add_info("Type /help for broker commands, /quit to shutdown");

    let server_handle = tokio::spawn(run_server(
        listener,
        Arc::clone(&state),
        tls_config.clone(),
        shutdown_tx.clone(),
        ui_tx.clone(),
    ));

    let signal_state = Arc::clone(&state);
    let signal_shutdown = shutdown_tx.clone();
    let signal_ui = ui_tx.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            request_shutdown(&signal_state, &signal_shutdown, &signal_ui).await;
        }
    });

    let result = run_tui_loop(
        &mut terminal,
        &mut app,
        Arc::clone(&state),
        shutdown_tx.clone(),
        ui_tx.clone(),
        ui_rx,
    )
    .await;

    tui::restore_terminal()?;
    let _ = shutdown_tx.send(());
    let _ = server_handle.await;

    result
}

async fn run_server(
    listener: TcpListener,
    state: Arc<Mutex<Shared>>,
    tls_config: Option<Arc<tls::TlsConfig>>,
    shutdown_tx: broadcast::Sender<()>,
    ui_tx: mpsc::Sender<tui::Message>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut shutdown_rx = shutdown_tx.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, addr) = result?;
                let state = Arc::clone(&state);
                let tls_config = tls_config.clone();
                let ui_tx = ui_tx.clone();

                tokio::spawn(async move {
                    let _ = ui_info(&ui_tx, format!("Accepted connection from {addr}")).await;
                    let result = if let Some(ref tls_cfg) = tls_config {
                        tracing::debug!("TLS: Starting handshake with {}...", addr);
                        match tls_cfg.acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                tracing::debug!("TLS: Handshake complete with {}", addr);
                                process_tls(state, tls_stream, addr, tls_cfg.clone(), ui_tx.clone()).await
                            }
                            Err(e) => {
                                tracing::warn!("TLS handshake failed from {}: {:?}", addr, e);
                                let _ = ui_error(&ui_tx, format!("TLS handshake failed from {addr}: {e:?}")).await;
                                Ok(())
                            }
                        }
                    } else {
                        process(state, stream, addr, None, ui_tx.clone()).await
                    };

                    if let Err(e) = result {
                        tracing::info!("an error occurred; error = {:?}", e);
                        let _ = ui_error(&ui_tx, format!("Connection error: {e}"))
                            .await;
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                let _ = ui_info(&ui_tx, "Broker shutdown complete.").await;
                break;
            }
        }
    }

    Ok(())
}

async fn run_tui_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::App,
    state: Arc<Mutex<Shared>>,
    shutdown_tx: broadcast::Sender<()>,
    ui_tx: mpsc::Sender<tui::Message>,
    mut ui_rx: mpsc::Receiver<tui::Message>,
) -> Result<(), Box<dyn Error>> {
    let mut shutdown_rx = shutdown_tx.subscribe();
    let mut shutdown_requested = false;

    loop {
        terminal.draw(|frame| app.draw(frame))?;

        while let Ok(msg) = ui_rx.try_recv() {
            app.handle_message(msg);
        }

        match shutdown_rx.try_recv() {
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {
                shutdown_requested = true;
                app.should_quit = true;
            }
            Err(broadcast::error::TryRecvError::Empty) => {}
            Err(broadcast::error::TryRecvError::Closed) => {
                shutdown_requested = true;
                app.should_quit = true;
            }
        }

        if crossterm::event::poll(std::time::Duration::from_millis(10))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    match key.code {
                        crossterm::event::KeyCode::Char('c')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            shutdown_requested = true;
                            request_shutdown(&state, &shutdown_tx, &ui_tx).await;
                            app.should_quit = true;
                        }
                        crossterm::event::KeyCode::Char(c) => {
                            if !app.current_completions.is_empty() {
                                app.cancel_completion();
                            }
                            if let Some(cmd) = app.handle_input(c) {
                                let should_quit =
                                    handle_broker_command(cmd, &state, &shutdown_tx, &ui_tx)
                                        .await?;
                                if should_quit {
                                    shutdown_requested = true;
                                    app.should_quit = true;
                                }
                            }
                        }
                        crossterm::event::KeyCode::Backspace => app.handle_backspace(),
                        crossterm::event::KeyCode::Delete => app.handle_delete(),
                        crossterm::event::KeyCode::Left => app.cursor_left(),
                        crossterm::event::KeyCode::Right => app.cursor_right(),
                        crossterm::event::KeyCode::Home => app.cursor_home(),
                        crossterm::event::KeyCode::End => app.cursor_end(),
                        crossterm::event::KeyCode::Up => app.history_previous(),
                        crossterm::event::KeyCode::Down => app.history_next(),
                        crossterm::event::KeyCode::PageUp => {
                            for _ in 0..5 {
                                app.scroll_up();
                            }
                        }
                        crossterm::event::KeyCode::PageDown => {
                            for _ in 0..5 {
                                app.scroll_down();
                            }
                        }
                        crossterm::event::KeyCode::Tab => app.handle_tab(),
                        crossterm::event::KeyCode::Enter => {
                            if !app.current_completions.is_empty() {
                                app.apply_completion();
                            } else if let Some(cmd) = app.handle_input('\n') {
                                let should_quit =
                                    handle_broker_command(cmd, &state, &shutdown_tx, &ui_tx)
                                        .await?;
                                if should_quit {
                                    shutdown_requested = true;
                                    app.should_quit = true;
                                }
                            }
                        }
                        crossterm::event::KeyCode::F(1) => app.toggle_sidebar(),
                        crossterm::event::KeyCode::Esc => {
                            if !app.current_completions.is_empty() {
                                app.cancel_completion();
                            } else {
                                app.should_quit = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    if !shutdown_requested {
        request_shutdown(&state, &shutdown_tx, &ui_tx).await;
    }

    Ok(())
}

async fn handle_broker_command(
    cmd: String,
    state: &Arc<Mutex<Shared>>,
    shutdown_tx: &broadcast::Sender<()>,
    ui_tx: &mpsc::Sender<tui::Message>,
) -> Result<bool, Box<dyn Error>> {
    let trimmed = cmd.trim();
    match trimmed {
        "/quit" | "/exit" | "/shutdown" => {
            request_shutdown(state, shutdown_tx, ui_tx).await;
            Ok(true)
        }
        "/status" => {
            let (peer_count, rooms) = collect_stats(state).await;
            let rooms_display = if rooms.is_empty() {
                "none".to_string()
            } else {
                rooms.join(", ")
            };
            ui_info(ui_tx, format!("Connected clients: {peer_count}")).await;
            ui_info(ui_tx, format!("Active rooms: {rooms_display}")).await;
            let _ = ui_tx
                .send(tui::Message::Stats { peer_count, rooms })
                .await;
            Ok(false)
        }
        "/help" => {
            ui_info(ui_tx, "Broker commands:").await;
            ui_info(ui_tx, "  /status   - Show connected clients and rooms").await;
            ui_info(ui_tx, "  /quit     - Gracefully shutdown the broker").await;
            Ok(false)
        }
        "" => Ok(false),
        _ => {
            ui_error(
                ui_tx,
                format!("Unknown command: {trimmed}. Type /help for commands."),
            )
            .await;
            Ok(false)
        }
    }
}

async fn request_shutdown(
    state: &Arc<Mutex<Shared>>,
    shutdown_tx: &broadcast::Sender<()>,
    ui_tx: &mpsc::Sender<tui::Message>,
) {
    ui_info(ui_tx, "Shutting down broker...").await;
    {
        let state = state.lock().await;
        for peer in state.peers.values() {
            let _ = peer.tx.send("INFO broker shutting down".to_string());
        }
    }
    let _ = shutdown_tx.send(());
}

async fn collect_stats(state: &Arc<Mutex<Shared>>) -> (usize, Vec<String>) {
    let state = state.lock().await;
    let peer_count = state.peers.len();
    let mut rooms: Vec<String> = state.peers.values().map(|p| p.room.clone()).collect();
    rooms.sort_unstable();
    rooms.dedup();
    (peer_count, rooms)
}

async fn send_stats(state: &Arc<Mutex<Shared>>, ui_tx: &mpsc::Sender<tui::Message>) {
    let (peer_count, rooms) = collect_stats(state).await;
    let _ = ui_tx
        .send(tui::Message::Stats { peer_count, rooms })
        .await;
}

async fn ui_info(ui_tx: &mpsc::Sender<tui::Message>, text: impl Into<String>) {
    let _ = ui_tx
        .send(tui::Message::Info {
            text: text.into(),
            timestamp: SystemTime::now(),
        })
        .await;
}

async fn ui_error(ui_tx: &mpsc::Sender<tui::Message>, text: impl Into<String>) {
    let _ = ui_tx
        .send(tui::Message::Error {
            text: text.into(),
            timestamp: SystemTime::now(),
        })
        .await;
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, Box<dyn Error>> {
    let mut verbose = false;
    let mut addr: Option<String> = None;
    let mut tls_disabled = false;
    let mut cert_dir: Option<PathBuf> = None;
    let mut regenerate_token = false;

    let mut args_iter = args.into_iter().peekable();

    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            "--no-tls" => tls_disabled = true,
            "--regenerate-token" => regenerate_token = true,
            "--cert-dir" => {
                cert_dir = Some(PathBuf::from(
                    args_iter
                        .next()
                        .ok_or("--cert-dir requires a path argument")?,
                ));
            }
            "--help" | "-h" => {
                eprintln!("usage: broker [OPTIONS] [addr]");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --verbose, -v       Enable verbose logging");
                eprintln!("  --no-tls            Disable TLS (insecure, for testing)");
                eprintln!(
                    "  --cert-dir <PATH>   Certificate directory (default: ~/.config/rustynaut)"
                );
                eprintln!("  --regenerate-token  Generate new enrollment token");
                eprintln!();
                eprintln!("  addr default: 127.0.0.1:4242");
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => {
                return Err(format!("unknown flag: {arg}").into());
            }
            _ => {
                if addr.is_none() {
                    addr = Some(arg);
                } else {
                    return Err(format!("unexpected extra arg: {arg}").into());
                }
            }
        }
    }

    Ok(Args {
        verbose,
        addr: addr.unwrap_or_else(|| "127.0.0.1:4242".to_string()),
        tls_disabled,
        cert_dir: cert_dir.unwrap_or_else(tls::default_cert_dir),
        regenerate_token,
    })
}

/// Detect local IP addresses from network interfaces using pure Rust
fn detect_local_ips() -> Vec<String> {
    let mut ips = Vec::new();

    // Use if-addrs crate for cross-platform network interface detection
    if let Ok(interfaces) = if_addrs::get_if_addrs() {
        for iface in interfaces {
            // Skip loopback interfaces
            if iface.is_loopback() {
                continue;
            }

            // Get the IP address
            let ip = iface.addr.ip();

            // Only include IPv4 addresses for now
            if ip.is_ipv4() {
                let ip_str = ip.to_string();
                if !ips.contains(&ip_str) {
                    ips.push(ip_str);
                }
            }
        }
    }

    ips
}

/// Extract hostnames/IPs from the bind address for certificate SANs
fn extract_server_names(addr: &str) -> Vec<String> {
    // Always include rustynaut.local as a stable DNS name for cross-network connections
    let mut names = vec!["rustynaut.local".to_string(), "localhost".to_string()];

    // Parse the address to extract host part
    if let Some(host) = addr.split(':').next() {
        if host != "0.0.0.0" && host != "::" && !names.contains(&host.to_string()) {
            names.push(host.to_string());
        }
    }

    // Always include common local addresses
    if !names.contains(&"127.0.0.1".to_string()) {
        names.push("127.0.0.1".to_string());
    }

    // Auto-detect local IPs from network interfaces
    for ip in detect_local_ips() {
        if !names.contains(&ip) {
            names.push(ip);
        }
    }

    names
}

/// Shorthand for the transmit half of the message channel.
type Tx = mpsc::UnboundedSender<String>;

/// Shorthand for the receive half of the message channel.
type Rx = mpsc::UnboundedReceiver<String>;

/// Maximum number of recent clip hashes to track per room for deduplication
const MAX_RECENT_CLIPS_PER_ROOM: usize = 20;

/// Maximum file size for transfer (1GB)
const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;

/// State of a file transfer
#[derive(Debug, Clone, PartialEq)]
enum TransferState {
    /// Offer sent, waiting for acceptors
    Offered,
    /// Transfer in progress
    Transferring,
}

/// A pending file offer (before any accepts)
#[derive(Debug, Clone)]
struct PendingOffer {
    sender: SocketAddr,
    sender_username: String,
    room: String,
    filename_b64: String,
    size: u64,
    created_at: Instant,
}

/// An active file transfer (after at least one accept)
#[derive(Debug)]
struct FileTransfer {
    sender: SocketAddr,
    sender_username: String,
    room: String,
    filename_b64: String,
    size: u64,
    acceptors: HashSet<SocketAddr>,
    state: TransferState,
}

/// Unique key for a pending offer (room + sender + filename)
type OfferKey = (String, String, String); // (room, sender_username, filename_b64)

/// Data that is shared between all peers in the chat server.
///
/// This is the set of `Tx` handles for all connected clients. Whenever a
/// message is received from a client, it is broadcasted to all peers by
/// iterating over the `peers` entries and sending a copy of the message on each
/// `Tx`.
struct Shared {
    peers: HashMap<SocketAddr, PeerInfo>,
    next_clip_id: u64,
    /// Recent clip content hashes per room for deduplication (room -> recent hashes)
    recent_clips: HashMap<String, VecDeque<u64>>,
    /// Recent file offer hashes per room for deduplication (room -> recent hashes)
    recent_file_offers: HashMap<String, VecDeque<u64>>,
    /// Pending file offers waiting for acceptors (key: room+sender+filename)
    pending_offers: HashMap<OfferKey, PendingOffer>,
    /// Active file transfers (key: transfer_id)
    active_transfers: HashMap<u64, FileTransfer>,
    /// Next transfer ID
    next_transfer_id: u64,
}

#[derive(Clone)]
struct PeerInfo {
    tx: Tx,
    username: String,
    room: String,
}

/// The state for each connected client.
struct Peer<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// The TCP socket wrapped with the `Lines` codec, defined below.
    ///
    /// This handles sending and receiving data on the socket. When using
    /// `Lines`, we can work at the line level instead of having to manage the
    /// raw byte operations.
    lines: Framed<S, LinesCodec>,

    /// Receive half of the message channel.
    ///
    /// This is used to receive messages from peers. When a message is received
    /// off of this `Rx`, it will be written to the socket.
    rx: Rx,
}

impl Shared {
    /// Create a new, empty, instance of `Shared`.
    fn new() -> Self {
        Shared {
            peers: HashMap::new(),
            next_clip_id: 0,
            recent_clips: HashMap::new(),
            recent_file_offers: HashMap::new(),
            pending_offers: HashMap::new(),
            active_transfers: HashMap::new(),
            next_transfer_id: 0,
        }
    }

    /// Register a file offer from a sender
    fn register_offer(
        &mut self,
        sender: SocketAddr,
        sender_username: &str,
        room: &str,
        filename_b64: &str,
        size: u64,
    ) {
        let key = (
            room.to_string(),
            sender_username.to_string(),
            filename_b64.to_string(),
        );
        self.pending_offers.insert(
            key,
            PendingOffer {
                sender,
                sender_username: sender_username.to_string(),
                room: room.to_string(),
                filename_b64: filename_b64.to_string(),
                size,
                created_at: Instant::now(),
            },
        );
    }

    /// Find the most recent offer from a user in a room
    /// Returns the filename_b64 of the offer if found
    fn find_latest_offer(&self, room: &str, sender_username: &str) -> Option<String> {
        self.pending_offers
            .values()
            .filter(|offer| offer.room == room && offer.sender_username == sender_username)
            .max_by_key(|offer| offer.created_at)
            .map(|offer| offer.filename_b64.clone())
    }

    /// List all pending offers in a room (for /offers command)
    fn list_offers(&self, room: &str) -> Vec<(&str, &str, u64)> {
        self.pending_offers
            .values()
            .filter(|offer| offer.room == room)
            .map(|offer| {
                (
                    offer.sender_username.as_str(),
                    offer.filename_b64.as_str(),
                    offer.size,
                )
            })
            .collect()
    }

    /// Accept a file offer and start a transfer
    /// Returns (transfer_id, sender_addr) if successful
    fn accept_offer(
        &mut self,
        acceptor: SocketAddr,
        room: &str,
        sender_username: &str,
        filename_b64: &str,
    ) -> Option<(u64, SocketAddr)> {
        let key = (
            room.to_string(),
            sender_username.to_string(),
            filename_b64.to_string(),
        );

        // Check if there's already an active transfer for this offer
        for (tid, transfer) in &mut self.active_transfers {
            if transfer.room == room
                && transfer.sender_username == sender_username
                && transfer.filename_b64 == filename_b64
                && transfer.state == TransferState::Offered
            {
                // Add this acceptor to existing transfer
                transfer.acceptors.insert(acceptor);
                return Some((*tid, transfer.sender));
            }
        }

        // Look for pending offer
        if let Some(offer) = self.pending_offers.remove(&key) {
            self.next_transfer_id += 1;
            let transfer_id = self.next_transfer_id;

            let mut acceptors = HashSet::new();
            acceptors.insert(acceptor);

            self.active_transfers.insert(
                transfer_id,
                FileTransfer {
                    sender: offer.sender,
                    sender_username: offer.sender_username,
                    room: offer.room,
                    filename_b64: offer.filename_b64,
                    size: offer.size,
                    acceptors,
                    state: TransferState::Offered,
                },
            );

            return Some((transfer_id, offer.sender));
        }

        None
    }

    /// Get transfer by ID
    fn get_transfer(&self, transfer_id: u64) -> Option<&FileTransfer> {
        self.active_transfers.get(&transfer_id)
    }

    /// Get mutable transfer by ID
    fn get_transfer_mut(&mut self, transfer_id: u64) -> Option<&mut FileTransfer> {
        self.active_transfers.get_mut(&transfer_id)
    }

    /// Send a message to a specific peer
    fn send_to_peer(&self, addr: SocketAddr, message: &str) {
        if let Some(peer) = self.peers.get(&addr) {
            let _ = peer.tx.send(message.to_string());
        }
    }

    /// Send a message to all acceptors of a transfer
    fn send_to_acceptors(&self, transfer_id: u64, message: &str) {
        if let Some(transfer) = self.active_transfers.get(&transfer_id) {
            for &acceptor in &transfer.acceptors {
                self.send_to_peer(acceptor, message);
            }
        }
    }

    /// Check if a clip is a duplicate and track it if not.
    /// Returns true if the clip is a duplicate (should be skipped).
    fn is_duplicate_clip(&mut self, room: &str, content: &str) -> bool {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Compute a simple hash of the content
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let hash = hasher.finish();

        // Get or create the recent clips queue for this room
        let recent = self.recent_clips.entry(room.to_string()).or_default();

        // Check if this hash is already in recent clips
        if recent.contains(&hash) {
            return true; // Duplicate
        }

        // Not a duplicate - add to recent clips
        if recent.len() >= MAX_RECENT_CLIPS_PER_ROOM {
            recent.pop_front();
        }
        recent.push_back(hash);

        false // Not a duplicate
    }

    /// Check if a file offer is a duplicate and track it if not.
    /// Returns true if the file offer is a duplicate (should be skipped).
    fn is_duplicate_file_offer(&mut self, room: &str, filename_b64: &str, size: &str) -> bool {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Compute a hash of filename + size
        let mut hasher = DefaultHasher::new();
        filename_b64.hash(&mut hasher);
        size.hash(&mut hasher);
        let hash = hasher.finish();

        // Get or create the recent file offers queue for this room
        let recent = self.recent_file_offers.entry(room.to_string()).or_default();

        // Check if this hash is already in recent file offers
        if recent.contains(&hash) {
            return true; // Duplicate
        }

        // Not a duplicate - add to recent file offers
        if recent.len() >= MAX_RECENT_CLIPS_PER_ROOM {
            recent.pop_front();
        }
        recent.push_back(hash);

        false // Not a duplicate
    }

    /// Send a `LineCodec` encoded message to every peer, except
    /// for the sender.
    async fn broadcast(&mut self, sender: SocketAddr, message: &str) {
        for (peer_addr, peer) in self.peers.iter_mut() {
            if *peer_addr != sender {
                let _ = peer.tx.send(message.into());
            }
        }
    }

    /// Send a message to peers in a specific room, except for the sender.
    async fn broadcast_to_room(&mut self, sender: SocketAddr, room: &str, message: &str) {
        for (peer_addr, peer) in self.peers.iter_mut() {
            if *peer_addr != sender && peer.room == room {
                let _ = peer.tx.send(message.into());
            }
        }
    }
}

impl<S> Peer<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Create a new instance of `Peer`.
    async fn new(
        state: Arc<Mutex<Shared>>,
        lines: Framed<S, LinesCodec>,
        addr: SocketAddr,
        username: String,
        room: String,
    ) -> io::Result<Peer<S>> {
        // Create a channel for this peer
        let (tx, rx) = mpsc::unbounded_channel();

        // Add an entry for this `Peer` in the shared state map.
        state
            .lock()
            .await
            .peers
            .insert(addr, PeerInfo { tx, username, room });

        Ok(Peer { lines, rx })
    }
}

fn parse_user(line: &str) -> Option<&str> {
    line.strip_prefix("USER ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn parse_join(line: &str) -> Option<&str> {
    line.strip_prefix("JOIN ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Validate a username or room name.
/// Allowed: alphanumeric, underscore, hyphen. Max 32 chars.
fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

fn parse_clip(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("CLIP", room, b64) => Some((room, b64)),
        _ => None,
    }
}

/// Parse FILE_OFFER command: FILE_OFFER <room> <filename_b64> <size>
/// Returns (room, filename_b64, size)
fn parse_file_offer(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_OFFER", room, filename_b64, size) => Some((room, filename_b64, size)),
        _ => None,
    }
}

/// Parse FILE_ACCEPT command: FILE_ACCEPT <room> <sender_username> <filename_b64>
/// Returns (room, sender_username, filename_b64)
fn parse_file_accept(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_ACCEPT", room, sender_username, filename_b64) => {
            Some((room, sender_username, filename_b64))
        }
        _ => None,
    }
}

/// Parse FILE_CANCEL command: FILE_CANCEL <transfer_id>
/// Returns transfer_id
fn parse_file_cancel(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("FILE_CANCEL ")?;
    rest.trim().parse().ok()
}

/// Parse FILE_CHUNK command: FILE_CHUNK <transfer_id> <offset> <chunk_b64>
/// Returns (transfer_id, offset, chunk_b64)
fn parse_file_chunk(line: &str) -> Option<(u64, u64, &str)> {
    let mut parts = line.splitn(4, ' ');
    match (parts.next()?, parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_CHUNK", tid, offset, chunk_b64) => {
            Some((tid.parse().ok()?, offset.parse().ok()?, chunk_b64))
        }
        _ => None,
    }
}

/// Parse FILE_END command: FILE_END <transfer_id> <sha256>
/// Returns (transfer_id, sha256)
fn parse_file_end(line: &str) -> Option<(u64, &str)> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next()?, parts.next()?, parts.next()?) {
        ("FILE_END", tid, sha256) => Some((tid.parse().ok()?, sha256)),
        _ => None,
    }
}

fn parse_cmd(line: &str) -> Option<&str> {
    line.strip_prefix("CMD ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn parse_say(line: &str) -> Option<&str> {
    line.strip_prefix("SAY ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Parse ENROLL command: ENROLL <token> <username>
fn parse_enroll(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("ENROLL ")?;
    let mut parts = rest.splitn(2, ' ');
    let token = parts.next()?.trim();
    let username = parts.next()?.trim();
    if token.is_empty() || username.is_empty() {
        return None;
    }
    Some((token, username))
}

/// Process a TLS connection
async fn process_tls(
    state: Arc<Mutex<Shared>>,
    stream: tokio_rustls::server::TlsStream<TcpStream>,
    addr: SocketAddr,
    tls_config: Arc<tls::TlsConfig>,
    ui_tx: mpsc::Sender<tui::Message>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    process(state, stream, addr, Some(tls_config), ui_tx).await
}

/// Process an individual chat client
async fn process<S>(
    state: Arc<Mutex<Shared>>,
    stream: S,
    addr: SocketAddr,
    tls_config: Option<Arc<tls::TlsConfig>>,
    ui_tx: mpsc::Sender<tui::Message>,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut lines = Framed::new(stream, LinesCodec::new_with_max_length(MAX_LINE_LENGTH));

    // Protocol note:
    // - New clients should send: `USER <name>` then optionally `JOIN <room>`.
    // - For legacy clients we still accept the first line as the username.
    // - For TLS enrollment: `ENROLL <token> <username>`
    if tls_config.is_some() {
        lines
            .send("INFO rustynaut broker (TLS): send 'ENROLL <token> <username>' or 'USER <name>' then 'JOIN <room>'")
            .await?;
    } else {
        lines
            .send(
                "INFO rustynaut broker: send 'USER <name>' then 'JOIN <room>' (defaults to lobby)",
            )
            .await?;
    }

    let first = match lines.next().await {
        Some(Ok(line)) => line,
        _ => {
            tracing::error!(
                "Failed to get first line from {}. Client disconnected.",
                addr
            );
            return Ok(());
        }
    };

    // Handle ENROLL command for TLS enrollment
    if let Some((token, enroll_username)) = parse_enroll(&first) {
        if let Some(ref tls_cfg) = tls_config {
            if token == tls_cfg.enrollment_token {
                if !is_valid_name(enroll_username) {
                    lines
                        .send("ERR enrollment failed: invalid username (alphanumeric, _, - only, max 32 chars)")
                        .await?;
                    return Ok(());
                }

                tracing::info!("Enrolling client '{}' from {}", enroll_username, addr);

                // Generate client certificate
                match tls::generate_client_cert(
                    &tls_cfg.ca_cert_pem,
                    &tls_cfg.ca_key,
                    enroll_username,
                ) {
                    Ok(bundle) => {
                        let response = tls::encode_enrolled_response(&bundle);
                        lines.send(&response).await?;
                        tracing::info!(
                            "Enrolled '{}' - client should reconnect with certificate",
                            enroll_username
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to generate client certificate: {:?}", e);
                        lines
                            .send("ERR enrollment failed: certificate generation error")
                            .await?;
                    }
                }
            } else {
                tracing::warn!("Invalid enrollment token from {}", addr);
                lines.send("ERR enrollment failed: invalid token").await?;
            }
        } else {
            lines
                .send("ERR enrollment not available (TLS not enabled)")
                .await?;
        }
        return Ok(());
    }

    let username = parse_user(&first).unwrap_or(first.trim()).to_string();
    if username.is_empty() {
        lines.send("ERR missing username").await?;
        return Ok(());
    }
    if !is_valid_name(&username) {
        lines
            .send("ERR invalid username (alphanumeric, _, - only, max 32 chars)")
            .await?;
        return Ok(());
    }

    let mut room = "lobby".to_string();

    // Register our peer with state which internally sets up some channels.
    let mut peer = Peer::new(state.clone(), lines, addr, username.clone(), room.clone()).await?;

    // A client has connected, let's let everyone know.
    {
        let mut state = state.lock().await;
        let msg = format!("INFO {username} joined");
        tracing::info!("{}", msg);
        state.broadcast(addr, &msg).await;
    }
    ui_info(&ui_tx, format!("{username} joined {room}")).await;
    send_stats(&state, &ui_tx).await;

    // Process incoming messages until our stream is exhausted by a disconnect.
    loop {
        tokio::select! {
            // A message was received from a peer. Send it to the current user.
            Some(msg) = peer.rx.recv() => {
                peer.lines.send(&msg).await?;
            }
            result = peer.lines.next() => match result {
                // A message was received from the current user, we should
                // broadcast this message to the other users.
                Some(Ok(msg)) => {
                    // JOIN <room>
                    if let Some(new_room) = parse_join(&msg) {
                        if !is_valid_name(new_room) {
                            peer.lines.send("ERR invalid room name (alphanumeric, _, - only, max 32 chars)").await?;
                            continue;
                        }
                        room = new_room.to_string();

                        {
                            let mut shared = state.lock().await;
                            if let Some(peer_info) = shared.peers.get_mut(&addr) {
                                peer_info.room = room.clone();
                            }
                        }

                        peer.lines.send(format!("INFO joined {room}")).await?;
                        ui_info(&ui_tx, format!("{username} joined {room}")).await;
                        send_stats(&state, &ui_tx).await;
                        continue;
                    }

                    // CMD /...
                    if let Some(cmd) = parse_cmd(&msg) {
                        match cmd {
                            "/help" => {
                                        peer.lines
                                            .send("INFO commands: /help /rooms /who /offers /accept <user>".to_string())
                                            .await?;
                            }
                            "/offers" => {
                                let state = state.lock().await;
                                let offers = state.list_offers(&room);
                                if offers.is_empty() {
                                    peer.lines.send(format!("INFO no pending file offers in {room}")).await?;
                                } else {
                                    use base64::{engine::general_purpose, Engine as _};
                                    let mut lines = vec![format!("INFO pending file offers in {room}:")];
                                    for (username, filename_b64, size) in offers {
                                        let filename = general_purpose::STANDARD
                                            .decode(filename_b64)
                                            .ok()
                                            .and_then(|b| String::from_utf8(b).ok())
                                            .unwrap_or_else(|| "<unknown>".to_string());
                                        let size_display = if size >= 1024 * 1024 {
                                            format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
                                        } else if size >= 1024 {
                                            format!("{:.1} KB", size as f64 / 1024.0)
                                        } else {
                                            format!("{} bytes", size)
                                        };
                                        lines.push(format!("INFO   {username}: {filename} ({size_display})"));
                                    }
                                    for line in lines {
                                        peer.lines.send(line).await?;
                                    }
                                }
                            }
                            "/rooms" => {
                                let state = state.lock().await;
                                let mut rooms = state
                                    .peers
                                    .values()
                                    .map(|p| p.room.as_str())
                                    .collect::<Vec<_>>();
                                rooms.sort_unstable();
                                rooms.dedup();
                                peer.lines
                                    .send(format!("INFO rooms: {}", rooms.join(", ")))
                                    .await?;
                            }
                            "/who" => {
                                let state = state.lock().await;
                                let mut users = state
                                    .peers
                                    .values()
                                    .filter(|p| p.room == room)
                                    .map(|p| p.username.as_str())
                                    .collect::<Vec<_>>();
                                users.sort_unstable();
                                users.dedup();

                                peer.lines
                                    .send(format!("INFO users in {room}: {}", users.join(", ")))
                                    .await?;
                            }
                            _ if cmd.starts_with("/accept ") => {
                                // Parse /accept <username> [filename]
                                // If filename is omitted, accept the most recent offer from that user
                                let rest = cmd.strip_prefix("/accept ").unwrap().trim();
                                let parts: Vec<&str> = rest.splitn(2, ' ').collect();
                                if parts.is_empty() || parts[0].is_empty() {
                                    peer.lines.send("ERR usage: /accept <username> [filename]").await?;
                                    continue;
                                }
                                let sender_username = parts[0];

                                use base64::{engine::general_purpose, Engine as _};

                                let mut state = state.lock().await;

                                // Get filename_b64 - either from argument or find latest offer
                                let filename_b64 = if parts.len() >= 2 {
                                    // Explicit filename provided - base64 encode it
                                    general_purpose::STANDARD.encode(parts[1])
                                } else {
                                    // No filename - find the most recent offer from this user
                                    match state.find_latest_offer(&room, sender_username) {
                                        Some(fb64) => fb64,
                                        None => {
                                            peer.lines.send(format!("ERR no pending offers from {sender_username}")).await?;
                                            continue;
                                        }
                                    }
                                };

                                // Try to accept the offer
                                if let Some((transfer_id, sender_addr)) = state.accept_offer(addr, &room, sender_username, &filename_b64) {
                                    let transfer = state.get_transfer(transfer_id).unwrap();
                                    let size = transfer.size;
                                    let acceptor_count = transfer.acceptors.len();

                                    // Notify the sender to start the transfer
                                    let start_msg = format!("FILE_START {transfer_id} {filename_b64} {acceptor_count}");
                                    state.send_to_peer(sender_addr, &start_msg);

                                    // Notify the acceptor that the file is incoming
                                    let incoming_msg = format!("FILE_INCOMING {transfer_id} {filename_b64} {size}");
                                    peer.lines.send(&incoming_msg).await?;

                                    tracing::info!(
                                        "file transfer {} started: {} -> {} ({} bytes)",
                                        transfer_id, sender_username, username, size
                                    );
                                } else {
                                    peer.lines.send("ERR no such file offer (or already accepted)").await?;
                                }
                            }
                            _ if cmd.starts_with("/cancel ") => {
                                // Parse /cancel <transfer_id>
                                let rest = cmd.strip_prefix("/cancel ").unwrap().trim();
                                if let Ok(transfer_id) = rest.parse::<u64>() {
                                    let mut state = state.lock().await;

                                    if let Some(transfer) = state.active_transfers.remove(&transfer_id) {
                                        // Notify all participants
                                        let cancel_msg = format!("FILE_CANCELLED {transfer_id} cancelled by {username}");
                                        state.send_to_peer(transfer.sender, &cancel_msg);
                                        for &acceptor in &transfer.acceptors {
                                            state.send_to_peer(acceptor, &cancel_msg);
                                        }
                                        tracing::info!("file transfer {} cancelled by {}", transfer_id, username);
                                    } else {
                                        peer.lines.send("ERR no such transfer").await?;
                                    }
                                } else {
                                    peer.lines.send("ERR usage: /cancel <transfer_id>").await?;
                                }
                            }
                            _ => {
                                peer.lines.send(format!("ERR unknown command: {cmd}")).await?;
                            }
                        }
                        continue;
                    }

                    // CLIP <room> <b64>
                    if let Some((_wire_room, b64)) = parse_clip(&msg) {
                        let mut state = state.lock().await;

                        // Check for duplicate clip (echo suppression)
                        if state.is_duplicate_clip(&room, b64) {
                            // Duplicate - skip broadcasting
                            tracing::debug!("duplicate clip from {} in room {}, skipping", username, room);
                            continue;
                        }

                        state.next_clip_id += 1;
                        let id = state.next_clip_id;
                        let out = format!("CLIP {room} {b64} {id}");
                        state.broadcast_to_room(addr, &room, &out).await;
                        continue;
                    }

                    // FILE_OFFER <room> <filename_b64> <size>
                    if let Some((_wire_room, filename_b64, size_str)) = parse_file_offer(&msg) {
                        let mut state = state.lock().await;

                        // Check for duplicate file offer (echo suppression)
                        if state.is_duplicate_file_offer(&room, filename_b64, size_str) {
                            tracing::debug!("duplicate file offer from {} in room {}, skipping", username, room);
                            continue;
                        }

                        // Parse and validate size
                        let size: u64 = match size_str.parse() {
                            Ok(s) => s,
                            Err(_) => {
                                peer.lines.send("ERR invalid file size").await?;
                                continue;
                            }
                        };

                        if size > MAX_FILE_SIZE {
                            peer.lines.send(format!("ERR file too large (max {} bytes)", MAX_FILE_SIZE)).await?;
                            continue;
                        }

                        // Register the offer for later acceptance
                        state.register_offer(addr, &username, &room, filename_b64, size);

                        // Relay file offer to room members with sender's username
                        let out = format!("FILE_OFFER {room} {username} {filename_b64} {size}");
                        tracing::info!("file offer from {} in room {}: {} bytes", username, room, size);
                        state.broadcast_to_room(addr, &room, &out).await;
                        continue;
                    }

                    // FILE_ACCEPT <room> <sender_username> <filename_b64>
                    if let Some((accept_room, sender_username, filename_b64)) = parse_file_accept(&msg) {
                        // Validate room matches current room
                        if accept_room != room {
                            peer.lines.send("ERR can only accept files in your current room").await?;
                            continue;
                        }

                        let mut state = state.lock().await;

                        // Try to accept the offer
                        if let Some((transfer_id, sender_addr)) = state.accept_offer(addr, &room, sender_username, filename_b64) {
                            let transfer = state.get_transfer(transfer_id).unwrap();
                            let size = transfer.size;
                            let acceptor_count = transfer.acceptors.len();

                            // Notify the sender to start the transfer
                            let start_msg = format!("FILE_START {transfer_id} {filename_b64} {acceptor_count}");
                            state.send_to_peer(sender_addr, &start_msg);

                            // Notify the acceptor that the file is incoming
                            let incoming_msg = format!("FILE_INCOMING {transfer_id} {filename_b64} {size}");
                            peer.lines.send(&incoming_msg).await?;

                            tracing::info!(
                                "file transfer {} started: {} -> {} ({} bytes)",
                                transfer_id, sender_username, username, size
                            );
                        } else {
                            peer.lines.send("ERR no such file offer").await?;
                        }
                        continue;
                    }

                    // FILE_CANCEL <transfer_id>
                    if let Some(transfer_id) = parse_file_cancel(&msg) {
                        let mut state = state.lock().await;

                        if let Some(transfer) = state.active_transfers.remove(&transfer_id) {
                            // Notify all participants
                            let cancel_msg = format!("FILE_CANCELLED {transfer_id} cancelled by {username}");
                            state.send_to_peer(transfer.sender, &cancel_msg);
                            for &acceptor in &transfer.acceptors {
                                state.send_to_peer(acceptor, &cancel_msg);
                            }
                            tracing::info!("file transfer {} cancelled by {}", transfer_id, username);
                        } else {
                            peer.lines.send("ERR no such transfer").await?;
                        }
                        continue;
                    }

                    // FILE_CHUNK <transfer_id> <offset> <chunk_b64>
                    if let Some((transfer_id, offset, chunk_b64)) = parse_file_chunk(&msg) {
                        let mut state = state.lock().await;

                        // Verify sender owns this transfer
                        if let Some(transfer) = state.get_transfer_mut(transfer_id) {
                            if transfer.sender != addr {
                                peer.lines.send("ERR not the sender of this transfer").await?;
                                continue;
                            }

                            // Update transfer state
                            transfer.state = TransferState::Transferring;

                            // Relay chunk to all acceptors
                            let chunk_msg = format!("FILE_CHUNK {transfer_id} {offset} {chunk_b64}");
                            state.send_to_acceptors(transfer_id, &chunk_msg);
                        } else {
                            peer.lines.send("ERR no such transfer").await?;
                        }
                        continue;
                    }

                    // FILE_END <transfer_id> <sha256>
                    if let Some((transfer_id, sha256)) = parse_file_end(&msg) {
                        let mut state = state.lock().await;

                        if let Some(transfer) = state.active_transfers.remove(&transfer_id) {
                            if transfer.sender != addr {
                                peer.lines.send("ERR not the sender of this transfer").await?;
                                // Put it back
                                state.active_transfers.insert(transfer_id, transfer);
                                continue;
                            }

                            // Notify acceptors that transfer is complete
                            let done_msg = format!("FILE_DONE {transfer_id} {sha256}");
                            for &acceptor in &transfer.acceptors {
                                state.send_to_peer(acceptor, &done_msg);
                            }

                            // Notify sender of success
                            let sent_msg = format!("FILE_SENT {transfer_id} {}", transfer.acceptors.len());
                            peer.lines.send(&sent_msg).await?;

                            tracing::info!(
                                "file transfer {} complete: {} receivers",
                                transfer_id, transfer.acceptors.len()
                            );
                        } else {
                            peer.lines.send("ERR no such transfer").await?;
                        }
                        continue;
                    }

                    // SAY <text>
                    let out = if let Some(text) = parse_say(&msg) {
                        format!("SAY {username} {text}")
                    } else {
                        // Legacy: treat the whole line as a chat message.
                        format!("SAY {username} {msg}")
                    };

                    let mut state = state.lock().await;
                    state.broadcast_to_room(addr, &room, &out).await;
                }
                // An error occurred.
                Some(Err(e)) => {
                    tracing::error!(
                        "an error occurred while processing messages for {}; error = {:?}",
                        username,
                        e
                    );
                }
                // The stream has been exhausted.
                None => break,
            },
        }
    }

    // If this section is reached it means that the client was disconnected!
    // Let's let everyone still connected know about it.
    {
        let mut state = state.lock().await;
        state.peers.remove(&addr);

        let msg = format!("INFO {username} left");
        tracing::info!("{}", msg);
        state.broadcast(addr, &msg).await;
    }
    ui_info(&ui_tx, format!("{username} left")).await;
    send_stats(&state, &ui_tx).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== parse_user tests ====================

    #[test]
    fn test_parse_user_valid() {
        assert_eq!(parse_user("USER alice"), Some("alice"));
        assert_eq!(parse_user("USER bob_123"), Some("bob_123"));
    }

    #[test]
    fn test_parse_user_with_whitespace() {
        assert_eq!(parse_user("USER   alice  "), Some("alice"));
    }

    #[test]
    fn test_parse_user_empty() {
        assert_eq!(parse_user("USER "), None);
        assert_eq!(parse_user("USER"), None);
    }

    #[test]
    fn test_parse_user_wrong_prefix() {
        assert_eq!(parse_user("JOIN alice"), None);
        assert_eq!(parse_user("alice"), None);
    }

    // ==================== parse_join tests ====================

    #[test]
    fn test_parse_join_valid() {
        assert_eq!(parse_join("JOIN lobby"), Some("lobby"));
        assert_eq!(parse_join("JOIN room-1"), Some("room-1"));
    }

    #[test]
    fn test_parse_join_with_whitespace() {
        assert_eq!(parse_join("JOIN   lobby  "), Some("lobby"));
    }

    #[test]
    fn test_parse_join_empty() {
        assert_eq!(parse_join("JOIN "), None);
        assert_eq!(parse_join("JOIN"), None);
    }

    #[test]
    fn test_parse_join_wrong_prefix() {
        assert_eq!(parse_join("USER lobby"), None);
    }

    // ==================== is_valid_name tests ====================

    #[test]
    fn test_is_valid_name_valid() {
        assert!(is_valid_name("alice"));
        assert!(is_valid_name("Bob_123"));
        assert!(is_valid_name("room-1"));
        assert!(is_valid_name("a"));
        assert!(is_valid_name("12345678901234567890123456789012")); // 32 chars
    }

    #[test]
    fn test_is_valid_name_invalid() {
        assert!(!is_valid_name("")); // empty
        assert!(!is_valid_name("alice bob")); // space
        assert!(!is_valid_name("alice@host")); // special char
        assert!(!is_valid_name("room/lobby")); // slash
        assert!(!is_valid_name("123456789012345678901234567890123")); // 33 chars
    }

    // ==================== parse_clip tests ====================

    #[test]
    fn test_parse_clip_valid() {
        assert_eq!(
            parse_clip("CLIP lobby SGVsbG8="),
            Some(("lobby", "SGVsbG8="))
        );
        assert_eq!(
            parse_clip("CLIP room-1 dGVzdA=="),
            Some(("room-1", "dGVzdA=="))
        );
    }

    #[test]
    fn test_parse_clip_with_extra_spaces_in_payload() {
        // The b64 part might have trailing content (like id) - we only take first 3 parts
        let result = parse_clip("CLIP lobby SGVsbG8= extra");
        assert_eq!(result, Some(("lobby", "SGVsbG8=")));
    }

    #[test]
    fn test_parse_clip_missing_parts() {
        assert_eq!(parse_clip("CLIP lobby"), None);
        assert_eq!(parse_clip("CLIP"), None);
    }

    #[test]
    fn test_parse_clip_wrong_prefix() {
        assert_eq!(parse_clip("JOIN lobby SGVsbG8="), None);
    }

    // ==================== parse_cmd tests ====================

    #[test]
    fn test_parse_cmd_valid() {
        assert_eq!(parse_cmd("CMD /help"), Some("/help"));
        assert_eq!(parse_cmd("CMD /rooms"), Some("/rooms"));
        assert_eq!(parse_cmd("CMD /who"), Some("/who"));
    }

    #[test]
    fn test_parse_cmd_with_args() {
        assert_eq!(parse_cmd("CMD /join lobby"), Some("/join lobby"));
    }

    #[test]
    fn test_parse_cmd_empty() {
        assert_eq!(parse_cmd("CMD "), None);
        assert_eq!(parse_cmd("CMD"), None);
    }

    #[test]
    fn test_parse_cmd_wrong_prefix() {
        assert_eq!(parse_cmd("/help"), None);
    }

    // ==================== parse_say tests ====================

    #[test]
    fn test_parse_say_valid() {
        assert_eq!(parse_say("SAY hello world"), Some("hello world"));
        assert_eq!(parse_say("SAY hi"), Some("hi"));
    }

    #[test]
    fn test_parse_say_empty() {
        assert_eq!(parse_say("SAY "), None);
        assert_eq!(parse_say("SAY"), None);
    }

    #[test]
    fn test_parse_say_wrong_prefix() {
        assert_eq!(parse_say("CMD hello"), None);
    }

    // ==================== parse_args tests ====================

    #[test]
    fn test_parse_args_defaults() {
        let args: Vec<String> = vec![];
        let result = parse_args(args).unwrap();
        assert!(!result.verbose);
        assert_eq!(result.addr, "127.0.0.1:4242");
        assert!(!result.tls_disabled); // TLS enabled by default
    }

    #[test]
    fn test_parse_args_with_address() {
        let args = vec!["0.0.0.0:8080".to_string()];
        let result = parse_args(args).unwrap();
        assert!(!result.verbose);
        assert_eq!(result.addr, "0.0.0.0:8080");
    }

    #[test]
    fn test_parse_args_verbose_short() {
        let args = vec!["-v".to_string()];
        let result = parse_args(args).unwrap();
        assert!(result.verbose);
        assert_eq!(result.addr, "127.0.0.1:4242");
    }

    #[test]
    fn test_parse_args_verbose_long() {
        let args = vec!["--verbose".to_string(), "0.0.0.0:9000".to_string()];
        let result = parse_args(args).unwrap();
        assert!(result.verbose);
        assert_eq!(result.addr, "0.0.0.0:9000");
    }

    #[test]
    fn test_parse_args_flags_anywhere() {
        let args = vec!["0.0.0.0:9000".to_string(), "-v".to_string()];
        let result = parse_args(args).unwrap();
        assert!(result.verbose);
        assert_eq!(result.addr, "0.0.0.0:9000");
    }

    #[test]
    fn test_parse_args_unknown_flag() {
        let args = vec!["--unknown".to_string()];
        assert!(parse_args(args).is_err());
    }

    #[test]
    fn test_parse_args_extra_arg() {
        let args = vec!["addr1".to_string(), "addr2".to_string()];
        assert!(parse_args(args).is_err());
    }

    #[test]
    fn test_parse_args_no_tls() {
        let args = vec!["--no-tls".to_string()];
        let result = parse_args(args).unwrap();
        assert!(result.tls_disabled);
        assert_eq!(result.addr, "127.0.0.1:4242");
    }

    #[test]
    fn test_parse_args_with_cert_dir() {
        let args = vec!["--cert-dir".to_string(), "/tmp/certs".to_string()];
        let result = parse_args(args).unwrap();
        assert!(!result.tls_disabled); // TLS still enabled by default
        assert_eq!(result.cert_dir, std::path::PathBuf::from("/tmp/certs"));
    }
}
