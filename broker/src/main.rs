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

/// Maximum line length for the wire protocol (2MB to handle large base64 clipboard payloads)
/// For files larger than ~1.5MB raw, use the sideband FILE_OFFER protocol instead.
const MAX_LINE_LENGTH: usize = 2 * 1024 * 1024;

use tokio::io::{self, AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_stream::StreamExt;
use tokio_util::codec::{Framed, LinesCodec};

use futures::SinkExt;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

const BANNER: &str = r#"
██████  ██████   ██████  ██   ██ ███████ ██████  
██   ██ ██   ██ ██    ██ ██  ██  ██      ██   ██ 
██████  ██████  ██    ██ █████   █████   ██████  
██   ██ ██   ██ ██    ██ ██  ██  ██      ██   ██ 
██████  ██   ██  ██████  ██   ██ ███████ ██   ██ 
                                                 
https://github.com/aszazeroth/rustynaut                                                 
"#;

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
    println!("{BANNER}");

    // Args: broker [--verbose|-v] [--no-tls] [--cert-dir <path>] [--regenerate-token] [addr]
    let args = parse_args(env::args().skip(1))?;

    use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};
    // Configure a `tracing` subscriber that logs traces emitted by the chat
    // server.
    let default_directive = if args.verbose {
        "chat=debug"
    } else {
        "chat=info"
    };
    tracing_subscriber::fmt()
        // Filter what traces are displayed based on the RUST_LOG environment
        // variable.
        //
        // Traces emitted by the example code will always be displayed. You
        // can set `RUST_LOG=tokio=trace` to enable additional traces emitted by
        // Tokio itself.
        .with_env_filter(EnvFilter::from_default_env().add_directive(default_directive.parse()?))
        // Log events when `tracing` spans are created, entered, exited, or
        // closed. When Tokio's internal tracing support is enabled (as
        // described above), this can be used to track the lifecycle of spawned
        // tasks on the Tokio runtime.
        .with_span_events(FmtSpan::FULL)
        // Set this subscriber as the default, to collect all traces emitted by
        // the program.
        .init();

    // Initialize TLS (enabled by default)
    let tls_config = if !args.tls_disabled {
        // Extract hostname/IP from addr for certificate SANs
        let server_names = extract_server_names(&args.addr);
        Some(Arc::new(
            tls::init_tls(&args.cert_dir, &server_names, args.regenerate_token)
                .map_err(|e| -> Box<dyn Error> { e })?,
        ))
    } else {
        eprintln!("WARNING: TLS disabled, connections will not be encrypted!");
        None
    };

    // Create the shared state. This is how all the peers communicate.
    //
    // The server task will hold a handle to this. For every new client, the
    // `state` handle is cloned and passed into the task that processes the
    // client connection.
    let state = Arc::new(Mutex::new(Shared::new()));

    // Shutdown signal broadcast channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    println!("broker started on : {}", &args.addr);
    if !args.tls_disabled {
        println!("TLS enabled");
    }
    println!("Type /help for broker commands, /quit to shutdown");

    // Bind a TCP listener to the socket address.
    //
    // Note that this is the Tokio TcpListener, which is fully async.
    let listener = TcpListener::bind(&args.addr).await?;

    tracing::info!("server running on {}", args.addr);

    // Spawn stdin reader for broker commands
    let stdin_state = Arc::clone(&state);
    let stdin_shutdown = shutdown_tx.clone();
    tokio::spawn(async move {
        let stdin = BufReader::new(io::stdin());
        let mut lines = stdin.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            match trimmed {
                "/quit" | "/exit" | "/shutdown" => {
                    println!("Shutting down broker...");
                    {
                        let state = stdin_state.lock().await;
                        for peer in state.peers.values() {
                            let _ = peer.tx.send("INFO broker shutting down".to_string());
                        }
                    }
                    let _ = stdin_shutdown.send(());
                    break;
                }
                "/status" => {
                    let state = stdin_state.lock().await;
                    let peer_count = state.peers.len();
                    let mut rooms: Vec<_> = state.peers.values().map(|p| p.room.as_str()).collect();
                    rooms.sort_unstable();
                    rooms.dedup();
                    println!("Connected clients: {}", peer_count);
                    println!("Active rooms: {}", rooms.join(", "));
                }
                "/help" => {
                    println!("Broker commands:");
                    println!("  /status   - Show connected clients and rooms");
                    println!("  /quit     - Gracefully shutdown the broker");
                }
                "" => {}
                _ => {
                    println!("Unknown command: {trimmed}. Type /help for commands.");
                }
            }
        }
    });

    // Set up signal handler for Ctrl+C
    let signal_state = Arc::clone(&state);
    let signal_shutdown = shutdown_tx.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            println!("\nReceived Ctrl+C, shutting down...");
            {
                let state = signal_state.lock().await;
                for peer in state.peers.values() {
                    let _ = peer.tx.send("INFO broker shutting down".to_string());
                }
            }
            let _ = signal_shutdown.send(());
        }
    });

    let mut shutdown_rx = shutdown_tx.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, addr) = result?;

                // Clone a handle to the `Shared` state for the new connection.
                let state = Arc::clone(&state);
                let tls_config = tls_config.clone();

                // Spawn our handler to be run asynchronously.
                tokio::spawn(async move {
                    tracing::debug!("accepted connection");
                    let result = if let Some(ref tls_cfg) = tls_config {
                        // TLS connection
                        match tls_cfg.acceptor.accept(stream).await {
                            Ok(tls_stream) => {
                                process_tls(state, tls_stream, addr, tls_cfg.clone()).await
                            }
                            Err(e) => {
                                tracing::warn!("TLS handshake failed: {:?}", e);
                                Ok(())
                            }
                        }
                    } else {
                        // Plain TCP connection
                        process(state, stream, addr, None).await
                    };
                    if let Err(e) = result {
                        tracing::info!("an error occurred; error = {:?}", e);
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                println!("Broker shutdown complete.");
                break;
            }
        }
    }

    Ok(())
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

    eprintln!("Certificate SANs: {:?}", names);

    names
}

/// Shorthand for the transmit half of the message channel.
type Tx = mpsc::UnboundedSender<String>;

/// Shorthand for the receive half of the message channel.
type Rx = mpsc::UnboundedReceiver<String>;

/// Data that is shared between all peers in the chat server.
///
/// This is the set of `Tx` handles for all connected clients. Whenever a
/// message is received from a client, it is broadcasted to all peers by
/// iterating over the `peers` entries and sending a copy of the message on each
/// `Tx`.
struct Shared {
    peers: HashMap<SocketAddr, PeerInfo>,
    next_clip_id: u64,
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
        }
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
) -> Result<(), Box<dyn Error + Send + Sync>> {
    process(state, stream, addr, Some(tls_config)).await
}

/// Process an individual chat client
async fn process<S>(
    state: Arc<Mutex<Shared>>,
    stream: S,
    addr: SocketAddr,
    tls_config: Option<Arc<tls::TlsConfig>>,
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

                        let mut state = state.lock().await;
                        if let Some(peer_info) = state.peers.get_mut(&addr) {
                            peer_info.room = room.clone();
                        }

                        peer.lines.send(format!("INFO joined {room}")).await?;
                        continue;
                    }

                    // CMD /...
                    if let Some(cmd) = parse_cmd(&msg) {
                        match cmd {
                            "/help" => {
                                        peer.lines
                                            .send("INFO commands: /help /rooms /who".to_string())
                                            .await?;
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
                            _ => {
                                peer.lines.send(format!("ERR unknown command: {cmd}")).await?;
                            }
                        }
                        continue;
                    }

                    // CLIP <room> <b64>
                    if let Some((_wire_room, b64)) = parse_clip(&msg) {
                        let mut state = state.lock().await;
                        state.next_clip_id += 1;
                        let id = state.next_clip_id;
                        let out = format!("CLIP {room} {b64} {id}");
                        state.broadcast_to_room(addr, &room, &out).await;
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
