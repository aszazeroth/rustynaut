//! An example of hooking up stdin/stdout to either a TCP or UDP stream.
//!
//! This example will connect to a socket address specified in the argument list
//! and then forward all data read on stdin to the server, printing out all data
//! received on stdout. An optional `--udp` argument can be passed to specify
//! that the connection should be made over UDP instead of TCP, translating each
//! line entered on stdin to a UDP packet to be sent to the remote address.
//!
//! Note that this is not currently optimized for performance, especially
//! around buffer management. Rather it's intended to show an example of
//! working with a client.
//!
//! This example can be quite useful when interacting with the other examples in
//! this repository! Many of them recommend running this as a simple "hook up
//! stdin/stdout to a server" to get up and running.

#![warn(rust_2018_idioms)]

//use futures::StreamExt;
use tokio::io;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;
use tokio_util::codec::{BytesCodec, FramedRead, FramedWrite};

use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::process::exit;

use base64::{engine::general_purpose, Engine as _};
use bytes::Bytes;
use crossclip::{Clipboard, SystemClipboard};

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref RE_SYSTEM_MESSAGE: Regex =
        Regex::new(r"(?<username>[\w\s\-_\.]+)(:\s)(?<system><\w+>)\s\|(?<clipboard>[\w+=]+)\|")
            .unwrap();
    static ref CLIPBOARD: SystemClipboard = get_current_clipboard();
}

const BANNER: &str = r#"
██████  ██      ██ ███████ ███    ██ ████████ 
██      ██      ██ ██      ████   ██    ██    
██      ██      ██ █████   ██ ██  ██    ██    
██      ██      ██ ██      ██  ██ ██    ██    
██████  ███████ ██ ███████ ██   ████    ██    
                                              
https://github.com/aszazeroth/rustynaut                                                
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("{BANNER}");
    // Determine if we're going to run in TCP or UDP mode
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let tcp = match args.iter().position(|a| a == "--udp") {
        Some(i) => {
            args.remove(i);
            false
        }
        None => true,
    };

    // Parse what address we're going to connect to
    let addr = args
        .first()
        .ok_or("this program requires at least one argument")?;
    let addr = addr.parse::<SocketAddr>()?;

    // ------------------- [begin] codeiumAI suggestions
    let (tx, rx) = mpsc::channel::<Bytes>(10);
    let rx = tokio_stream::wrappers::ReceiverStream::new(rx);

    let _ = CLIPBOARD.set_string_contents("".to_string()); //UGLY shit

    // Spawn a task to monitor clipboard changes
    tokio::spawn(async move {
        let mut previous_content = CLIPBOARD.get_string_contents().unwrap_or_default();
        let mut interval = time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let current_content = CLIPBOARD
                .get_string_contents()
                .expect("error getting the contents of the clipboard");
            if current_content != previous_content {
                let encoded = general_purpose::STANDARD.encode(&current_content);
                // Send the encoded content to the channel
                //println!("encoded :: |{}|", encoded);
                if tx
                    .send(Bytes::from(format!("<clipboard> |{}|\n", encoded)))
                    .await
                    .is_err()
                {
                    //the newline is IMPORTANT HERE!
                    eprintln!("Failed to send encoded content");
                    break;
                }
                previous_content = current_content;
            }
        }
    });
    // ------------------- [end] codeiumAI suggestions

    let stdin = FramedRead::new(io::stdin(), BytesCodec::new())
        .map(|i| i.map(|bytes| bytes.freeze()))
        .merge(rx.map(Result::<Bytes, io::Error>::Ok));

    let stdout = FramedWrite::new(io::stdout(), BytesCodec::new());

    if tcp {
        tcp::connect(&addr, stdin, stdout).await?;
    } else {
        udp::connect(&addr, stdin, stdout).await?;
    }

    Ok(())
}

fn get_current_clipboard() -> SystemClipboard {
    let Ok(clipboard) = SystemClipboard::new() else {
        eprintln!("could not connect to clipboard");
        exit(100); // We exit here, as if this doesn't work, there is no use continue the client
    };
    clipboard
}

fn replace_clipboard_content(content: &str) -> Result<(), Box<dyn std::error::Error>> {
    let decoded = general_purpose::STANDARD.decode(content)?;
    let decoded_string = String::from_utf8(decoded)?;
    //println!("decoded :: |{decoded_string}|");
    let current_content = CLIPBOARD
        .get_string_contents()
        .expect("error getting the contents of the clipboard");
    //println!("current_content :: |{current_content}| ");
    if current_content != decoded_string {
        CLIPBOARD.set_string_contents(decoded_string)?;
    };
    Ok(())
}

mod tcp {
    use bytes::Bytes;
    use futures::{future, Sink, SinkExt, Stream, StreamExt};
    use std::{error::Error, io, net::SocketAddr};
    use tokio::net::TcpStream;
    use tokio_util::codec::{BytesCodec, FramedRead, FramedWrite};

    pub async fn connect(
        addr: &SocketAddr,
        mut stdin: impl Stream<Item = Result<Bytes, io::Error>> + Unpin,
        mut stdout: impl Sink<Bytes, Error = io::Error> + Unpin,
    ) -> Result<(), Box<dyn Error>> {
        let mut stream = TcpStream::connect(addr).await?;
        let (r, w) = stream.split();
        let mut sink = FramedWrite::new(w, BytesCodec::new());
        // filter map Result<BytesMut, Error> stream into just a Bytes stream to match stdout Sink
        // on the event of an Error, log the error and end the stream
        let mut stream = FramedRead::new(r, BytesCodec::new())
            .filter_map(|i| match i {
                //BytesMut into Bytes
                Ok(i) => {
                    // HERE IS WHERE WE CAN CHECK INCOMMING MESSAGES!!!
                    //println!("incomming message :: {:?}", std::str::from_utf8(&i.as_ref()));

                    if let Ok(message) = std::str::from_utf8(&i.as_ref()) {
                        if let Some(captures) = crate::RE_SYSTEM_MESSAGE.captures(message) {
                            //println!("message :: {}",&message);
                            //let username = captures.name("username").map_or("", |m| m.as_str());
                            let system = captures.name("system").map_or("", |m| m.as_str());
                            let clipboard = captures.name("clipboard").map_or("", |m| m.as_str());
                            // Filter stuff here
                            if system == "<clipboard>" {
                                //println!("replace |{clipboard}|");
                                match crate::replace_clipboard_content(clipboard) {
                                    Ok(_) => (),
                                    Err(err) => {
                                        eprintln!(
                                            "could not replace the clipboard content, {}",
                                            err
                                        )
                                    }
                                }
                            } else {
                                // Handle normal messages
                                println!("command {} doesn't exist", message)
                                //peer.lines.send(&msg).await?;
                            }
                        }
                    } else {
                        eprintln!("message was not UTF8")
                    }

                    future::ready(Some(i.freeze()))
                }
                Err(e) => {
                    eprintln!("failed to read from socket; error={}", e);
                    future::ready(None)
                }
            })
            .map(Ok);

        match future::join(sink.send_all(&mut stdin), stdout.send_all(&mut stream)).await {
            (Err(e), _) | (_, Err(e)) => Err(e.into()),
            _ => Ok(()),
        }
    }
}

mod udp {
    use bytes::Bytes;
    use futures::{Sink, SinkExt, Stream, StreamExt};
    use std::error::Error;
    use std::io;
    use std::net::SocketAddr;
    use tokio::net::UdpSocket;

    pub async fn connect(
        addr: &SocketAddr,
        stdin: impl Stream<Item = Result<Bytes, io::Error>> + Unpin,
        stdout: impl Sink<Bytes, Error = io::Error> + Unpin,
    ) -> Result<(), Box<dyn Error>> {
        // We'll bind our UDP socket to a local IP/port, but for now we
        // basically let the OS pick both of those.
        let bind_addr = if addr.ip().is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };

        let socket = UdpSocket::bind(&bind_addr).await?;
        socket.connect(addr).await?;

        tokio::try_join!(send(stdin, &socket), recv(stdout, &socket))?;

        Ok(())
    }

    async fn send(
        mut stdin: impl Stream<Item = Result<Bytes, io::Error>> + Unpin,
        writer: &UdpSocket,
    ) -> Result<(), io::Error> {
        while let Some(item) = stdin.next().await {
            let buf = item?;
            writer.send(&buf[..]).await?;
        }

        Ok(())
    }

    async fn recv(
        mut stdout: impl Sink<Bytes, Error = io::Error> + Unpin,
        reader: &UdpSocket,
    ) -> Result<(), io::Error> {
        loop {
            let mut buf = vec![0; 1024];
            let n = reader.recv(&mut buf[..]).await?;

            if n > 0 {
                stdout.send(Bytes::from(buf)).await?;
            }
        }
    }
}
