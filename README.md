![rustynaut logo](logo.png)

# Rustynaut

Clipboard-sync broker + CLI client (Tokio) with mTLS encryption.

## What it is

Rustynaut is a small TCP, line-framed broker that relays clipboard updates between clients.

- The **broker** runs on a host (your server, a VM, etc.).
- The **clients** run on machines/VMs and publish/subscribe by **room**.
- **TLS encryption** is enabled by default with automatic certificate enrollment.

Rooms are the "pub/sub" unit: **one shared clipboard per room**.

## Quick Start

### 1. Start the Broker

```bash
cd broker
cargo run --release -- 0.0.0.0:4242
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
# Broker (WARNING: insecure!)
cargo run --release -- --no-tls 127.0.0.1:4242

# Client
cargo run --release -- --no-tls 127.0.0.1:4242 alice
```

## Build & Run

### Broker Options

```bash
cd broker
cargo run --release -- [OPTIONS] [addr]

Options:
  --verbose, -v         Enable verbose logging
  --no-tls              Disable TLS (insecure, for testing)
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
| \`/help\` | Show available commands |
| \`/rooms\` | List active rooms |
| \`/who\` | Show users in current room |
| \`/quit\` or \`/exit\` | Exit the client |

## Wire Protocol

Transport is line-framed TCP/TLS (\`LinesCodec\`).

**Client → Broker:**
- \`USER <name>\` — Set username
- \`JOIN <room>\` — Join a room
- \`CLIP <room> <b64>\` — Clipboard update (base64)
- \`CMD /...\` — Slash command
- \`SAY <text>\` — Chat message
- \`ENROLL <token> <username>\` — Request certificate enrollment

**Broker → Client:**
- \`INFO <text>\` — Informational message
- \`ERR <text>\` — Error message
- \`CLIP <room> <b64> <id>\` — Clipboard broadcast
- \`SAY <user> <text>\` — Chat message
- \`ENROLLED <cert_b64> <key_b64> <ca_b64>\` — Enrollment response

## Example Session

```bash
# Terminal 1: Start broker
cd broker && cargo run --release -- 0.0.0.0:4242

# Terminal 2: Enroll and connect first client
cd client && cargo run --release -- --enroll <TOKEN> 127.0.0.1:4242 alice lobby

# Terminal 3: Enroll and connect second client  
cd client && cargo run --release -- --enroll <TOKEN> 127.0.0.1:4242 bob lobby
```

Now changing the system clipboard on one client replicates to the other.

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
