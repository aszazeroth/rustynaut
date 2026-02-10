![rustynaut logo](logo.png)

# Rustynaut

Clipboard-sync broker + CLI client (Tokio) with mTLS encryption and file transfers.

## What it is

Rustynaut is a small TCP, line-framed broker that relays clipboard updates and files between clients.

- The **broker** runs on a host (your server, a VM, etc.).
- The **clients** run on machines/VMs and publish/subscribe by **room**.
- **TLS encryption** is enabled by default with automatic certificate enrollment.
- **File transfers** up to 1GB via broker-mediated streaming (no P2P/NAT issues).

Rooms are the "pub/sub" unit: **one shared clipboard per room**.

## Workspace Layout

This repository is a Cargo workspace with three crates:

- `broker/` — server/broker (Tokio + TLS + TUI)
- `client/` — CLI/TUI client
- `common/` — shared protocol, parsing, TLS helpers, and utilities

See per-crate docs:

- `broker/README.md`
- `client/README.md`
- `common/README.md`

You can still run from each crate directory, or from the workspace root with `-p`.

## Quick Start

### 1. Start the Broker

```bash
cd broker
cargo run --release -- 0.0.0.0:4242
```

Or from workspace root:

```bash
cargo run -p broker --release -- 0.0.0.0:4242
```

On first run, the broker:
- Generates a CA certificate and server certificate
- Creates an enrollment token (UUID)
- Prints the token to stdout

```
TLS enabled
Enrollment token: a1b2c3d4-e5f6-7890-abcd-ef1234567890
Share this token with clients for first-time enrollment.
```

### 2. Enroll a Client

On each client machine, run with the enrollment token:

```bash
cd client
cargo run --release -- --enroll <TOKEN> <broker_address> [username] [room]
```

Or from workspace root:

```bash
cargo run -p client --release -- --enroll <TOKEN> <broker_address> [username] [room]
```

Example:
```bash
cargo run --release -- --enroll a1b2c3d4-e5f6-7890-abcd-ef1234567890 192.168.1.100:4242 alice lobby
```

The client:
- Connects to the broker
- Receives a signed client certificate
- Saves certs to \`~/.config/rustynaut/client/\` (Linux) or \`~/Library/Application Support/rustynaut/client/\` (macOS)
- Auto-connects with mTLS

### 3. Subsequent Connections

After enrollment, just connect normally:

```bash
cargo run --release -- 192.168.1.100:4242 alice lobby
```

Or from workspace root:

```bash
cargo run -p client --release -- 192.168.1.100:4242 alice lobby
```

## TLS & Certificates

### Certificate Storage

Certificates are stored in platform-appropriate locations:

| Platform | Broker | Client |
|----------|--------|--------|
| **Linux** | \`~/.config/rustynaut/\` | \`~/.config/rustynaut/client/\` |
| **macOS** | \`~/Library/Application Support/rustynaut/\` | \`~/Library/Application Support/rustynaut/client/\` |
| **Windows** | \`%APPDATA%\\rustynaut\\\` | \`%APPDATA%\\rustynaut\\client\\\` |

**Cross-platform note:** The broker's storage location doesn't matter to clients. Certificates are transferred over the wire during enrollment, so each platform stores them in its native location.

### Broker Files
```
~/.config/rustynaut/
├── ca/
│   ├── ca.crt           # CA certificate
│   └── ca.key           # CA private key (keep secret!)
├── broker/
│   ├── server.crt       # Server certificate
│   └── server.key       # Server private key
└── enrollment-token     # Shared secret for enrollment
```

### Client Files
```
~/.config/rustynaut/client/
├── client.crt           # Client certificate
├── client.key           # Client private key
└── ca.crt               # CA certificate (for verifying broker)
```

### Custom Certificate Directory

For Docker, systemd services, or other deployments:

```bash
# Broker
cargo run --release -- --cert-dir /etc/rustynaut 0.0.0.0:4242

# Client
cargo run --release -- --cert-dir /etc/rustynaut/client 192.168.1.100:4242
```

### Regenerate Enrollment Token

If the token is compromised:

```bash
cargo run --release -- --regenerate-token 0.0.0.0:4242
```

### Disable TLS (Development Only)

```bash
# Broker - TLS required
cargo run --release -- 0.0.0.0:4242

# Client - TLS required, must enroll first
cargo run --release -- --enroll <TOKEN> 127.0.0.1:4242 alice
```

## File Transfers

Rustynaut supports file transfers up to 1GB through the broker (no P2P/NAT issues).

### How it works

1. **Offer**: When you copy a file in Finder (macOS), Explorer (Windows), or a file path on Linux, the client detects it and sends a `FILE_OFFER` to the room.
2. **Accept**: Other clients see the offer and can accept it with `/accept <username> <filename>`.
3. **Transfer**: The broker coordinates the transfer, streaming 64KB chunks from sender to receiver(s).
4. **Complete**: Files are saved to the user's Downloads folder with automatic conflict resolution (adds `(1)`, `(2)`, etc.).

### File Transfer Commands

```bash
# List pending offers in your room
/offers

# Accept the most recent offer from a user
/accept alice

# Accept a specific file
/accept alice report.pdf

# Cancel a transfer (use the transfer_id shown in messages)
/cancel 42
```

### Size Limits

| Content Size | Strategy |
|--------------|----------|
| < 64 KB | Sent as `CLIP` (inline base64) |
| 64 KB – 1 GB | `FILE_OFFER` + broker-mediated transfer |
| > 1 GB | Rejected with error |

### Security

- Files are transferred through the broker over the existing TLS connection
- SHA-256 checksums verify integrity
- Transfers are room-scoped (only room members see offers)
- No files are stored on the broker (streaming relay only)

## Build & Run

### Broker Options

```bash
cd broker
cargo run --release -- [OPTIONS] [addr]

Options:
  --verbose, -v         Enable verbose logging
  --cert-dir <PATH>     Certificate directory (default: ~/.config/rustynaut)
  --regenerate-token    Generate new enrollment token

Default address: 127.0.0.1:4242
```

### Client Options

```bash
cd client
cargo run --release -- [OPTIONS] <addr> [username] [room]

Options:
  --verbose, -v         Enable verbose logging
  --enroll <TOKEN>      Enroll with broker using token
  --cert-dir <PATH>     Certificate directory

Default username: $USER or "anon"
Default room: "lobby"
```

### Client Options

```bash
cd client
cargo run --release -- [OPTIONS] <addr> [username] [room]

Options:
  --verbose, -v         Enable verbose logging
  --no-tls              Disable TLS (insecure, for testing)
  --enroll <TOKEN>      Enroll with broker using token
  --cert-dir <PATH>     Certificate directory

Default username: \$USER or "anon"
Default room: "lobby"
```

## Slash Commands

Type commands into the client stdin:

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/rooms` | List active rooms |
| `/who` | Show users in current room |
| `/offers` | List pending file offers in current room |
| `/accept <user> [filename]` | Accept a file offer from a user |
| `/cancel <transfer_id>` | Cancel an in-progress file transfer |
| `/quit` or `/exit` | Exit the client |

## Wire Protocol

Transport is line-framed TCP/TLS (`LinesCodec`).

**Client → Broker:**
- `USER <name>` — Set username
- `JOIN <room>` — Join a room
- `CLIP <room> <b64>` — Clipboard update (base64)
- `CMD /...` — Slash command
- `SAY <text>` — Chat message
- `ENROLL <token> <username>` — Request certificate enrollment
- `FILE_OFFER <room> <filename_b64> <size>` — Offer a file to the room
- `FILE_ACCEPT <room> <sender_username> <filename_b64>` — Accept a file offer
- `FILE_CANCEL <transfer_id>` — Cancel a file transfer
- `FILE_CHUNK <transfer_id> <offset> <chunk_b64>` — File data chunk (64KB)
- `FILE_END <transfer_id> <sha256>` — End of file transfer

**Broker → Client:**
- `INFO <text>` — Informational message
- `ERR <text>` — Error message
- `CLIP <room> <b64> <id>` — Clipboard broadcast
- `SAY <user> <text>` — Chat message
- `ENROLLED <cert_b64> <key_b64> <ca_b64>` — Enrollment response
- `FILE_OFFER <room> <username> <filename_b64> <size>` — File offer notification
- `FILE_START <transfer_id> <filename_b64> <acceptor_count>` — Start sending file
- `FILE_INCOMING <transfer_id> <filename_b64> <size>` — Incoming file notification
- `FILE_CHUNK <transfer_id> <offset> <chunk_b64>` — Relayed file chunk
- `FILE_DONE <transfer_id> <sha256>` — File transfer complete
- `FILE_SENT <transfer_id> <receiver_count>` — Confirmation to sender
- `FILE_CANCELLED <transfer_id> <reason>` — Transfer cancelled

## Example Session

### Clipboard Sync

```bash
# Terminal 1: Start broker
cd broker && cargo run --release -- 0.0.0.0:4242

# Terminal 2: Enroll and connect first client
cd client && cargo run --release -- --enroll <TOKEN> 127.0.0.1:4242 alice lobby

# Terminal 3: Enroll and connect second client  
cd client && cargo run --release -- --enroll <TOKEN> 127.0.0.1:4242 bob lobby
```

Now changing the system clipboard on one client replicates to the other.

### File Transfer

```bash
# In Terminal 2 (alice), copy a file to clipboard (macOS example)
# Or simply copy a file in Finder/Explorer

# Terminal 3 (bob) sees:
# INFO [lobby] alice offers file: report.pdf (245 KB) - use /accept alice report.pdf to receive

# In Terminal 3, accept the file:
/accept alice report.pdf

# File is downloaded to ~/Downloads/report.pdf
```

## Debugging

Enable verbose logging:
```bash
cd broker && cargo run --release -- --verbose 0.0.0.0:4242
cd client && cargo run --release -- --verbose 127.0.0.1:4242 alice
```

Override with \`RUST_LOG\`:
```bash
RUST_LOG="chat=trace" cargo run --release -- 0.0.0.0:4242
```

Test broker without client (plaintext mode only):
```bash
printf "USER alice\nJOIN lobby\nCMD /rooms\nCMD /who\n" | nc -w 1 127.0.0.1 4242
```

## Credits

*HEAVILY* inspired by ol' coding adventures and the Tokio chat server/client.

The logo was generated using the AI generator in Adobe Illustrator, then modified to better fit the original idea.
