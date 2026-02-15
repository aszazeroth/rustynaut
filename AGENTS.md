# AGENTS.md - Rustynaut Developer Guide

## Communication Language

Please use **ENGLISH** for all communication as the codebase is written in ENGLISH.

## Project Overview

Rustynaut is a cross-platform clipboard sharing application with a broker (server) and CLI client, written in Rust using Tokio. Features TLS/mTLS encryption, room-based pub/sub, file transfers, and TUI interface.

- **broker/** - Tokio-based chat/clipboard server with TUI
- **client/** - CLI clipboard sync client with TUI
- **common/** - Shared library crate (protocol, TLS, config, utilities)
- Workspace root uses Cargo workspace with shared dependencies

## Build Commands

```bash
# Build all components (from workspace root)
cargo build --release

# Run broker
cd broker && cargo run --release -- 0.0.0.0:4242

# Run client
cd client && cargo run --release -- 192.168.1.100:4242 alice lobby

# Enroll first-time client
cd client && cargo run --release -- --enroll <TOKEN> <broker_addr> <username>
```

## Lint & Format

```bash
# Run clippy on entire workspace (REQUIRED before committing)
cargo clippy --workspace

# Review all warnings - DO NOT add allow directives to skip them
# allow(dead_code) is OK for future implementations only

# Format code
cargo fmt --all
```

## Test Commands

```bash
# Run all tests
cargo test --workspace

# Run tests with output
cargo test --workspace -- --nocapture

# Run specific test
cargo test test_parse_user_valid --workspace
```

## Code Style Guidelines

### Imports
- Group: std lib → external crates → internal modules
- Use `use crate::` for internal modules
- Prefer explicit imports over wildcard `*`
- In workspace: `use rustynaut_common::...` for shared types

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
- Keep stdout clean (TUI handles display)
- Override with `RUST_LOG="rustynaut=trace" cargo run`

### Security
- TLS enabled by default (--no-tls for dev only)
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

**Client → Broker:**
- `USER <name>`, `JOIN <room>`, `CLIP <room> <b64>`, `CMD /<cmd>`, `SAY <text>`, `ENROLL <token> <username>`
- File transfer: `FILE_OFFER`, `FILE_ACCEPT`, `FILE_CHUNK`, `FILE_END`, `FILE_CANCEL`

**Broker → Client:**
- `INFO <text>`, `ERR <text>`, `CLIP <room> <b64> <id>`, `SAY <user> <text>`, `ENROLLED <cert_b64> <key_b64> <ca_b64>`
- File transfer: `FILE_INCOMING`, `FILE_DONE`, `FILE_SENT`, `FILE_CANCELLED`

## Testing Notes

- Manual testing: run broker, connect multiple clients
- Automated tests: use `tokio::test` with mock transports
- Test TLS enrollment flow end-to-end
- Verify cross-platform clipboard sync
- Check echo suppression (broker-side hash dedup + client-side tracking)
- Test auto-reconnection: broker restart should trigger client reconnect

## Common Tasks

```bash
# Generate new enrollment token
cd broker && cargo run --release -- --regenerate-token 0.0.0.0:4242

# Enroll a new client
cd client && cargo run --release -- --enroll <TOKEN> <broker_addr> <username>

# Run with verbose logging
RUST_LOG="rustynaut=trace" cargo run --release

# Custom config file
cargo run --release -- --config /path/to/config.toml
```

## Workspace Dependencies

Key crates (defined at workspace level):
- `tokio` - Async runtime
- `tokio-rustls` / `rustls` - TLS encryption
- `rcgen` - Certificate generation
- `tokio-util::codec` - Line framing
- `tracing` - Logging
- `arboard` - Cross-platform clipboard
- `dirs` - Platform config directories
- `ratatui` / `tui-prompts` / `crossterm` - TUI framework
- `base64` - Binary encoding
- `sha2` / `ring` - Cryptography
- `config` - Configuration management

## Architecture Notes

### Workspace Structure
- `rustynaut-common`: Shared protocol, types, TLS utilities, config
- `rustynaut-broker`: Server with rooms, file transfers, TUI
- `rustynaut-client`: Client with clipboard sync, TUI, auto-reconnection

### Key Features
- **mTLS with auto-enrollment**: Token-based certificate generation
- **Room-based pub/sub**: Scoped clipboard/file sharing per room
- **File transfers**: Broker-mediated, chunked, SHA256 verified
- **Auto-reconnection**: Exponential backoff for enrolled clients
- **Configuration**: TOML-based with CLI/env overrides
- **TUI**: Ratatui-based with text selection and copy support

### Certificate Storage
```
~/.config/rustynaut/              # Linux
~/Library/Application Support/rustynaut/  # macOS
%APPDATA%\rustynaut\              # Windows
```
