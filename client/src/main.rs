//! Clipboard sync client for the Rustynaut broker.
//!
//! Transport is TCP only and line-framed (`LinesCodec`). Clipboard updates are
//! sent as base64 to keep payloads binary-safe.

#![warn(rust_2018_idioms)]

use tokio::io;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};
use tokio_stream::StreamExt;
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec, LinesCodecError};

use std::env;
use std::error::Error;
use std::net::SocketAddr;
use std::process::exit;

use base64::{engine::general_purpose, Engine as _};
use crossclip::{Clipboard, SystemClipboard};

use lazy_static::lazy_static;

lazy_static! {
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

    // Args: client [--verbose|-v] <addr> [username] [room]
    // Flags can appear anywhere.
    let mut verbose = false;
    let mut positional = Vec::new();
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            _ => positional.push(arg),
        }
    }

    let addr = positional
        .first()
        .ok_or("usage: client [--verbose|-v] <addr> [username] [room]")?
        .parse::<SocketAddr>()?;

    let username = positional.get(1).cloned().unwrap_or_else(default_username);
    let room = positional
        .get(2)
        .cloned()
        .unwrap_or_else(|| "lobby".to_string());

    let (tx, rx) = mpsc::channel::<String>(10);
    let rx = tokio_stream::wrappers::ReceiverStream::new(rx);

    // Spawn a task to monitor clipboard changes
    let room_for_clipboard = room.clone();
    let verbose_for_clipboard = verbose;
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
        let mut interval = time::interval(Duration::from_secs(2));
        let mut warned = false;
        loop {
            interval.tick().await;
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
        Ok::<String, LinesCodecError>(format!("USER {username}")),
        Ok::<String, LinesCodecError>(format!("JOIN {room}")),
    ]);

    let stdin = FramedRead::new(io::stdin(), LinesCodec::new()).map(|i| {
        i.map(|line| {
            if line.starts_with('/') {
                format!("CMD {line}")
            } else {
                format!("SAY {line}")
            }
        })
    });

    let stdin = init
        .merge(stdin)
        .merge(rx.map(Result::<String, LinesCodecError>::Ok));

    let stdout = FramedWrite::new(io::stdout(), LinesCodec::new());

    tcp::connect(&addr, stdin, stdout, verbose).await?;

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
    let current_content = CLIPBOARD.get_string_contents()?;
    if current_content != decoded_string {
        CLIPBOARD.set_string_contents(decoded_string)?;
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

mod tcp {
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
        let mut sink = FramedWrite::new(w, LinesCodec::new());
        let mut stream = FramedRead::new(r, LinesCodec::new()).filter_map(|i| match i {
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
