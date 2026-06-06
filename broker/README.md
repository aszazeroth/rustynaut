# Rustynaut Broker

Tokio-based broker that relays clipboard updates and file transfers between clients.

## Features

- **Room-based pub/sub** - Multiple isolated clipboard/file transfer spaces
- **TLS encryption** with automatic certificate generation
- **mTLS authentication** via enrollment tokens
- **Broker-mediated file transfers** - No P2P/NAT issues
- **File streaming** - No persistence on broker

## Build

```bash
# from broker/
cargo build --release

# from workspace root
cargo build -p broker --release
```

## Run

```bash
# from broker/
cargo run --release -- 0.0.0.0:4242

# from workspace root
cargo run -p broker --release -- 0.0.0.0:4242
```

## CLI Options

```
broker [--verbose|-v] [--cert-dir <PATH>] [--regenerate-token] [addr]

  --verbose, -v         Enable verbose logging
  --cert-dir <PATH>     Certificate directory (default: ~/.config/rustynaut)
  --regenerate-token    Generate new enrollment token

Default address: 127.0.0.1:4242
```

## Certificate Management

On first run, the broker automatically generates:
- CA certificate and private key
- Server certificate signed by CA
- Enrollment token for client enrollment

### Certificate Storage

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

### Regenerate Enrollment Token

If the token is compromised or lost:

```bash
cargo run --release -- --regenerate-token 0.0.0.0:4242
```

### Custom Certificate Directory

```bash
cargo run --release -- --cert-dir /etc/rustynaut 0.0.0.0:4242
```

## Client Enrollment

The broker acts as a Certificate Authority (CA) and issues client certificates:

1. Client connects with enrollment token
2. Broker validates token and generates client certificate
3. Client receives certificate bundle (cert + key + CA)
4. Subsequent connections use mTLS

## File Transfer Support

The broker coordinates file transfers between clients:

- **FILE_OFFER** - Sender announces file to room
- **FILE_ACCEPT** - Recipient accepts offer
- **FILE_CHUNK** - 64KB chunks relayed through broker
- **FILE_END** - Transfer completion with SHA-256 checksum

### Transfer Limits

- Maximum file size: 1GB
- Chunk size: 64KB
- Max concurrent transfers: Limited by memory

## Room Management

- Rooms are created dynamically when first client joins
- Empty rooms are automatically cleaned up
- Clipboard sync is scoped to room
- File offers are room-scoped

## Slash Commands

The broker supports these commands:

- `/help` - Show available commands
- `/rooms` - List active rooms
- `/who` - Show users in current room
- `/offers` - List pending file offers
- `/clips` - List recent clipboard entries
- `/accept <user> [filename]` - Accept a file offer
- `/cancel <transfer_id>` - Cancel a file transfer

## Development

```bash
# Run clippy
cargo clippy --workspace --all-targets --all-features

# Run tests
cargo test --workspace

# Run broker tests specifically
cargo test -p broker
```

## Architecture

The broker uses Tokio for async I/O:

- **TCP listener** accepts connections
- **TLS acceptor** wraps streams for encryption
- **LinesCodec** frames messages
- **Shared state** tracks clients, rooms, and transfers
- **Broadcast channels** relay messages to room members

## See Also

- [Main README](../README.md) - Overview and quick start
- [Client README](../client/README.md) - Client documentation
- [Common README](../common/README.md) - Shared components
- [Planning Document](../aidocs/planning.md) - Architecture and roadmap
