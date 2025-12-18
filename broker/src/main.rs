//! A chat server that broadcasts a message to all connections.
//!
//! This example is explicitly more verbose than it has to be. This is to
//! illustrate more concepts.
//!
//! A chat server for telnet clients. After a telnet client connects, the first
//! line should contain the client's name. After that, all lines sent by a
//! client are broadcasted to all other connected clients.
//!
//! Because the client is telnet, lines are delimited by "\r\n".
//!
//! You can test this out by running:
//!
//!     cargo run --example chat
//!
//! And then in another terminal run:
//!
//!     telnet localhost 6142
//!
//! You can run the `telnet` command in any number of additional windows.
//!
//! You can run the second command in multiple windows and then chat between the
//! two, seeing the messages from the other client as they're received. For all
//! connected clients they'll all join the same room and see everyone else's
//! messages.

#![warn(rust_2018_idioms)]

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::StreamExt;
use tokio_util::codec::{Framed, LinesCodec};

use futures::SinkExt;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

const BANNER: &str = r#"
██████  ██████   ██████  ██   ██ ███████ ██████  
██   ██ ██   ██ ██    ██ ██  ██  ██      ██   ██ 
██████  ██████  ██    ██ █████   █████   ██████  
██   ██ ██   ██ ██    ██ ██  ██  ██      ██   ██ 
██████  ██   ██  ██████  ██   ██ ███████ ██   ██ 
                                                 
https://github.com/aszazeroth/rustynaut                                                 
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("{BANNER}");

    // Args: broker [--verbose|-v] [addr]
    // Flags can appear anywhere.
    let (verbose, addr) = parse_args(env::args().skip(1))?;

    use tracing_subscriber::{fmt::format::FmtSpan, EnvFilter};
    // Configure a `tracing` subscriber that logs traces emitted by the chat
    // server.
    let default_directive = if verbose { "chat=debug" } else { "chat=info" };
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

    // Create the shared state. This is how all the peers communicate.
    //
    // The server task will hold a handle to this. For every new client, the
    // `state` handle is cloned and passed into the task that processes the
    // client connection.
    let state = Arc::new(Mutex::new(Shared::new()));

    println!("broker started on : {}", &addr);

    // Bind a TCP listener to the socket address.
    //
    // Note that this is the Tokio TcpListener, which is fully async.
    let listener = TcpListener::bind(&addr).await?;

    tracing::info!("server running on {}", addr); //#######################################

    loop {
        // Asynchronously wait for an inbound TcpStream.
        let (stream, addr) = listener.accept().await?;

        // Clone a handle to the `Shared` state for the new connection.
        let state = Arc::clone(&state);

        // Spawn our handler to be run asynchronously.
        tokio::spawn(async move {
            tracing::debug!("accepted connection");
            if let Err(e) = process(state, stream, addr).await {
                tracing::info!("an error occurred; error = {:?}", e);
            }
        });
    }
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<(bool, String), Box<dyn Error>> {
    let mut verbose = false;
    let mut addr: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            "--help" | "-h" => {
                eprintln!("usage: broker [--verbose|-v] [addr]");
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

    Ok((
        verbose,
        addr.unwrap_or_else(|| "127.0.0.1:4242".to_string()),
    ))
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
struct Peer {
    /// The TCP socket wrapped with the `Lines` codec, defined below.
    ///
    /// This handles sending and receiving data on the socket. When using
    /// `Lines`, we can work at the line level instead of having to manage the
    /// raw byte operations.
    lines: Framed<TcpStream, LinesCodec>,

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

impl Peer {
    /// Create a new instance of `Peer`.
    async fn new(
        state: Arc<Mutex<Shared>>,
        lines: Framed<TcpStream, LinesCodec>,
        username: String,
        room: String,
    ) -> io::Result<Peer> {
        // Get the client socket address
        let addr = lines.get_ref().peer_addr()?;

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

/// Process an individual chat client
async fn process(
    state: Arc<Mutex<Shared>>,
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn Error>> {
    let mut lines = Framed::new(stream, LinesCodec::new());

    // Protocol note:
    // - New clients should send: `USER <name>` then optionally `JOIN <room>`.
    // - For legacy clients we still accept the first line as the username.
    lines
        .send("INFO rustynaut broker: send 'USER <name>' then 'JOIN <room>' (defaults to lobby)")
        .await?;

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

    let username = parse_user(&first).unwrap_or(first.trim()).to_string();
    if username.is_empty() {
        lines.send("ERR missing username").await?;
        return Ok(());
    }

    let mut room = "lobby".to_string();

    // Register our peer with state which internally sets up some channels.
    let mut peer = Peer::new(state.clone(), lines, username.clone(), room.clone()).await?;

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
                //println!("peer->current-user::message: {:#?}",&msg);
                peer.lines.send(&msg).await?;
                /*
                //println!("peer->current-user::message: {:#?}",&msg);

                if let Some(captures) = RE_MESSAGE.captures(&msg) {
                    let username = captures.name("username").map_or("", |m| m.as_str());
                    let message = captures.name("message").map_or("", |m| m.as_str());
                    // Now you can use the username and message to filter out specific commands
                    // For example, if the message is a specific command, you could handle it differently
                    if message == "/help" {
                        // Handle the command
                        println!("{} command was called, by {}", message, username);
                    } else {
                        // Handle normal messages
                        println!("command {} doesn't exist", message)
                        //peer.lines.send(&msg).await?;
                    }
                } else {
                    peer.lines.send(&msg).await?;
                }
                */
            }
            result = peer.lines.next() => match result {
                // A message was received from the current user, we should
                // broadcast this message to the other users.
                Some(Ok(msg)) => {
                    // JOIN <room>
                    if let Some(new_room) = parse_join(&msg) {
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
                        let out = {
                            let mut state = state.lock().await;
                            state.next_clip_id += 1;
                            let id = state.next_clip_id;
                            format!("CLIP {room} {b64} {id}")
                        };

                        let mut state = state.lock().await;
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
