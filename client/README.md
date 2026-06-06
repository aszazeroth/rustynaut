# Rustynaut Client

CLI/TUI client for clipboard sync and file transfers with automatic reconnection support.

## Features

- **Clipboard synchronization** across multiple machines
- **File transfers** up to 1GB through broker-mediated streaming
- **Auto-reconnection** with exponential backoff on broker restart or network issues
- **Rich TUI** with text selection, copy functionality, and user sidebar
- **Configuration file** support for persistent settings
- **mTLS encryption** with automatic certificate enrollment

## Build

```bash
# from client/
cargo build --release

# from workspace root
cargo build -p client --release
```

## Enroll (first time)

```bash
# from client/
cargo run --release -- --enroll <TOKEN> <addr> [username] [room]

# from workspace root
cargo run -p client --release -- --enroll <TOKEN> <addr> [username] [room]
```

## Run

```bash
# from client/
cargo run --release -- <addr> [username] [room]

# from workspace root
cargo run -p client --release -- <addr> [username] [room]
```

## CLI Options

```
client [--verbose|-v] [--enroll <TOKEN>] [--cert-dir <PATH>] [--config <PATH>] <addr> [username] [room]

  --verbose, -v         Enable verbose logging
  --enroll <TOKEN>      Enroll with broker using token
  --cert-dir <PATH>     Certificate directory
  --config <PATH>       Configuration file path
  --dump-config         Print default configuration and exit

Default username: $USER or "anon"
Default room: "lobby"
```

## Configuration

The client supports configuration via TOML files. Configuration is loaded from platform-appropriate locations:

| Platform | Config Path |
|----------|-------------|
| **Linux** | `~/.config/rustynaut/client/config.toml` |
| **macOS** | `~/Library/Application Support/rustynaut/client/config.toml` |
| **Windows** | `%APPDATA%\rustynaut\client\config.toml` |

### Example Configuration

```toml
[ui]
scrollback_limit = 1000
show_timestamps = true

[connection]
# Auto-reconnection settings
reconnect.enabled = true
reconnect.max_attempts = 3
reconnect.base_delay_seconds = 1
reconnect.max_delay_seconds = 8
reconnect.min_connection_seconds = 5
```

### Configuration Priority

Configuration is loaded in the following priority (highest to lowest):
1. CLI flags
2. Environment variables
3. Config file
4. Built-in defaults

## Auto-Reconnection

The client automatically reconnects when:
- The broker restarts
- Network connectivity is temporarily lost
- Connection is dropped unexpectedly

### How it works

1. **Detection**: Connection loss is detected immediately
2. **Backoff**: Waits with exponential backoff (1s → 2s → 4s, max 8s)
3. **Retry**: Attempts to reconnect up to 3 times
4. **State restoration**: Automatically re-sends USER and JOIN commands
5. **Manual retry**: After max attempts, press Enter to retry immediately

### Reconnection Status Messages

- `Disconnected from broker` - Connection lost
- `Reconnecting in Xs... (attempt Y/3)` - Waiting before retry
- `Reconnected to broker` - Successfully reconnected
- `Reconnection failed. Press Enter to retry, Ctrl+C to exit` - Max attempts reached

### Requirements

Auto-reconnection only works for enrolled clients (those with certificates). Unenrolled clients will exit on disconnect.

## TUI Controls

### Navigation
- **↑/↓** - Navigate command history
- **PgUp/PgDn** - Scroll messages
- **F1** - Toggle sidebar showing users in room

### Text Selection
- **Click** - Select a message
- **Drag** - Select text in messages or input area
- **y** - Copy selected message to clipboard
- **Esc** - Deselect

### Input
- **Tab** - Trigger tab completion (when available)
- **Enter** - Send message
- **Ctrl+C** - Exit (or cancel during selection)

## Slash Commands

- `/help` - Show available commands
- `/rooms` - List active rooms
- `/who` - Show users in current room
- `/offers` - List pending file offers
- `/clips` - List recent clipboard entries
- `/accept <user> [filename]` - Accept a file offer
- `/cancel <transfer_id>` - Cancel a file transfer
- `/quit`, `/exit` - Exit the client

## File Transfers

Files are transferred through the broker (no P2P/NAT issues):

1. Copy a file in Finder/Explorer or paste a file path
2. Other clients see: `alice offers file: report.pdf (245 KB)`
3. Recipient runs: `/accept alice report.pdf`
4. File downloads to Downloads folder with conflict resolution

### Size Limits

| Size | Method |
|------|--------|
| < 64 KB | Inline clipboard (CLIP) |
| 64 KB - 1 GB | File transfer (FILE_OFFER) |
| > 1 GB | Rejected |

## Certificate Management

Certificates are stored in platform-appropriate locations:

```
~/.config/rustynaut/client/
├── client.crt           # Client certificate
├── client.key           # Client private key
├── ca.crt               # CA certificate
└── config.toml          # Configuration (optional)
```

### Custom Certificate Directory

```bash
cargo run --release -- --cert-dir /etc/rustynaut/client 192.168.1.100:4242
```

## Development

```bash
# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace

# Run reconnection tests specifically
cargo test -p client reconnect
```

## Troubleshooting

### Client won't connect
- Verify enrollment: Check for certs in `~/.config/rustynaut/client/`
- Check broker address and port
- Try verbose mode: `--verbose`

### Auto-reconnection not working
- Ensure client is enrolled (has certificates)
- Check config: `reconnect.enabled = true`
- Look for status messages in TUI

### File transfers failing
- Check file size (< 1GB)
- Verify recipient accepted the offer
- Check Downloads folder permissions

## See Also

- [Main README](../README.md) - Overview and quick start
- [Broker README](../broker/README.md) - Broker documentation
- [Planning Document](../aidocs/planning.md) - Architecture and roadmap
