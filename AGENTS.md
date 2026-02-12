# AGENTS.md - Rustynaut Developer Guide

## Please use ENGLISH as the communication language as the code is written in ENGLISH

## Project Overview

Rustynaut is a cross-platform clipboard sharing application with a broker (server) and CLI client, written in Rust using Tokio. Features TLS/mTLS encryption, room-based pub/sub, and file transfers.

- **broker/** - Tokio-based chat/clipboard server
- **client/** - CLI clipboard sync client
- Each crate is isolated (no workspace); run commands from respective directories

## Build Commands

```bash
# Build both components
cd broker && cargo build --release
cd client && cargo build --release

# Run in development (TLS required)
cd broker && cargo run -- 0.0.0.0:4242
cd client && cargo run -- 127.0.0.1:4242 alice lobby

# Enroll first-time clients
cd client && cargo run -- --enroll <TOKEN> <broker_addr> <username>
```

## Lint & Format

```bash
# Run clippy in each crate directory (REQUIRED before committing)
cd broker && cargo clippy
cd client && cargo clippy

# Format code
cd broker && cargo fmt
cd client && cargo fmt
```

## Test Commands

```bash
# Run all tests
cargo test

# Run a single test
cargo test test_parse_user_valid

# Run tests with output
cargo test -- --nocapture

# Run with tokio (async tests use #[tokio::test])
cargo test
```

Note: Tests are primarily unit tests for protocol parsing. Integration testing requires running broker + multiple clients manually.

## Code Style Guidelines

### Imports
- Group: std lib → external crates → internal modules
- Use `use crate::` for internal modules
- Prefer explicit imports over wildcard `*`

### Formatting
- 4 spaces indentation
- Max line length: 100 characters
- Trailing commas in multi-line structs/enums
- No extra blank lines between function definitions

### Types & Naming
- `PascalCase` for types, traits, enums, structs
- `snake_case` for functions, variables, modules
- `SCREAMING_SNAKE_CASE` for constants
- Generic types: `S`, `T` (keep single letter for simple cases)

### Error Handling
- Use `?` operator for propagation
- Return `Result<T, Box<dyn Error>>` for main error types
- Use `tracing::error!` / `tracing::warn!` for logging errors
- Never use `unwrap()` or `expect()` in production code
- Handle clipboard errors gracefully (treat empty as empty string)

### Async Patterns
- Use `tokio::select!` for multiplexing
- Prefer `tokio::spawn` for concurrent tasks
- Use `mpsc` channels for cross-task communication
- Mock transports for tests (avoid blocking on stdin/stdout)

### Cross-Platform Requirements
- **MANDATORY**: All code must work on Linux, macOS, Windows
- **NEVER** use OS commands (`ifconfig`, `netstat`, shell commands)
- Use pure Rust crates instead:
  - `if-addrs` for network interfaces
  - `dirs` for config paths
  - `std::env::consts::OS` for OS detection

### Logging
- Use `tracing` crate (already configured)
- Levels: `trace!` (verbose), `debug!`, `info!`, `warn!`, `error!`
- Keep stdout clean (banners print first)
- Override with `RUST_LOG="chat=trace" cargo run`

### Security
- TLS enabled by default
- Use `rcgen` for certificate generation
- Certificate storage via `dirs` crate (platform-appropriate)
- Log certificate fingerprints for verification
- Clock skew tolerance: backdate `not_before` by 1 hour

### Protocol Constants
```rust
const MAX_LINE_LENGTH: usize = 2 * 1024 * 1024;  // 2MB for base64 payloads
const MAX_RECENT_CLIPS_PER_ROOM: usize = 20;
const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;   // 1GB
const FILE_CHUNK_SIZE: usize = 64 * 1024;        // 64KB chunks
```

## Wire Protocol

Client → Broker: `USER <name>`, `JOIN <room>`, `CLIP <room> <b64>`, `CMD /<cmd>`, `SAY <text>`, `ENROLL <token> <username>`

Broker → Client: `INFO <text>`, `ERR <text>`, `CLIP <room> <b64> <id>`, `SAY <user> <text>`, `ENROLLED <cert_b64> <key_b64> <ca_b64>`

## Testing Notes

- Manual testing: run broker, connect multiple clients
- Automated tests: use `tokio::test` with mock transports
- Test TLS enrollment flow end-to-end
- Verify cross-platform clipboard sync
- Check echo suppression (broker-side hash dedup + client-side tracking)

## Common Tasks

```bash
# Generate new enrollment token
cd broker && cargo run --release -- --regenerate-token 0.0.0.0:4242

# Enroll a new client
cd client && cargo run --release -- --enroll <TOKEN> <broker_addr> <username>

# Verbose logging
cargo run --release -- --verbose 0.0.0.0:4242
```

## Dependencies

Key crates used:
- `tokio` - Async runtime
- `tokio-rustls` / `rustls` - TLS encryption
- `rcgen` - Certificate generation
- `tokio-util::codec` - Line framing
- `tracing` - Logging
- `arboard` - Cross-platform clipboard
- `dirs` - Platform config directories
- `base64` - Binary encoding
- `sha2` / `ring` - Cryptography
