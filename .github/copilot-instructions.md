# Rustynaut Copilot Instructions

## Project Layout
- `broker/` – Tokio-based chat server; entry point [broker/src/main.rs](../broker/src/main.rs).
- `broker/src/tls.rs` – TLS certificate generation, CA management, and enrollment.
- `client/` – CLI chat + clipboard sync client; entry point [client/src/main.rs](../client/src/main.rs).
- `client/src/tls.rs` – TLS client configuration and enrollment handling.
- Each crate is isolated; run commands from its directory (`cargo run`). Shared types live inside their respective binaries, not in a workspace crate.

## Build & Run
- **Lint first**: `cd broker && cargo clippy` and `cd client && cargo clippy` before committing.
- Start broker: `cd broker && cargo run --release -- 0.0.0.0:4242` (TLS enabled by default, generates certs on first run).
- Start broker (verbose): `cd broker && cargo run --release -- --verbose 0.0.0.0:4242`.
- Start broker (no TLS, insecure): `cd broker && cargo run --release -- --no-tls 127.0.0.1:4242`.
- Start client (after enrollment): `cd client && cargo run --release -- 127.0.0.1:4242 [username] [room]`.
- Start client (enrollment): `cd client && cargo run --release -- --enroll <token> 127.0.0.1:4242 [username] [room]` (auto-connects after).
- Start client (no TLS, insecure): `cd client && cargo run --release -- --no-tls 127.0.0.1:4242 [username] [room]`.
- Logging uses `tracing`. Override verbosity with `RUST_LOG="chat=trace"` before running broker.
- Broker/client binaries print large ASCII banners; keep stdout clean so banners remain first output.

## TLS & Enrollment
- **TLS enabled by default**: Both broker and client use TLS unless `--no-tls` is specified.
- **mTLS**: Both broker and client authenticate via X.509 certificates (ECDSA P-256).
- **CA**: Broker acts as Certificate Authority, generating CA on first run.
- **Enrollment flow**:
  1. Broker starts, displays enrollment token (UUID v4).
  2. Client runs: `client --enroll <token> <addr> <username>` to obtain client certificate.
  3. Client auto-connects with mTLS after successful enrollment.
  4. Subsequent connections: `client <addr> <username>` (uses saved certificates).
- **Certificate storage** (platform-appropriate via `dirs` crate):
  - Linux: `~/.config/rustynaut/`
  - macOS: `~/Library/Application Support/rustynaut/`
  - Windows: `%APPDATA%\rustynaut\`
  - Broker subdirs: `ca/` (CA cert/key), `broker/` (server cert/key), `enrollment-token`
  - Client subdir: `client/` (client.crt, client.key, ca.crt)
- **Cross-platform**: Certificates are transferred over the wire during enrollment, so broker and client can run on different platforms.
- **Clock skew**: All certificates have `not_before` set to 1 hour in the past.
- **Fingerprints**: Both broker and client log certificate fingerprints (SHA-256) for manual verification.
- **Re-enrollment**: Supported; client can re-enroll to get new certificate.
- **Token regeneration**: `broker --regenerate-token` creates new token, invalidates old.

## Wire Protocol (current)
- Transport is line-framed TCP or TLS (`LinesCodec`).
- Client → Broker: `USER <name>`, `JOIN <room>`, `CLIP <room> <b64>`, `CMD /...`, `SAY <text>`, `ENROLL <token> <username>`.
- Broker → Client: `INFO <text>`, `ERR <text>`, `CLIP <room> <b64> <id>`, `SAY <user> <text>`, `ENROLLED <cert_b64> <key_b64> <ca_b64>`.

## Broker (server) Patterns
- `Shared` wraps `HashMap<SocketAddr, PeerInfo>` inside `Arc<Mutex<Shared>>` and currently also tracks a monotonically increasing clipboard id.
- `Peer::new` registers sessions and returns `Peer { lines, rx }`; cleanup happens after loop when removing from `Shared.peers`.
- `Peer<S>` is generic over `S: AsyncRead + AsyncWrite + Unpin` to support both TCP and TLS streams.
- Message flow: `process()` reads `USER <name>` (or `ENROLL` for TLS enrollment), then `tokio::select!` relays `rx.recv()` -> `peer.lines.send()` and incoming `LinesCodec` frames -> protocol handlers (`JOIN`/`CLIP`/`CMD`/`SAY`).
- New server features should slot into the select loop; prefer adding branches (e.g., command channels) rather than nested loops.

## Client Patterns
- Clipboard watcher: global `SystemClipboard` + background task, sending `CLIP <room> <b64>` lines through `mpsc::channel` merged into the outgoing stream.
- User input: lines starting with `/` are sent as `CMD /...`; other lines are sent as `SAY <text>`.
- Local commands: `/quit` and `/exit` exit the client gracefully without sending to broker.
- Incoming `CLIP <room> <b64> <id>` is applied to the local clipboard and not printed to stdout (to avoid base64 spam).
- `replace_clipboard_content()` silently ignores redundant updates; any new system command must guard against non-UTF8 payloads.
- Transport: TCP (`tcp` module) or TLS (`tls_transport` module).

## Cross-Cutting Conventions
- All async work uses Tokio `full` feature set; prefer `tokio::select!` for fan-in, `tokio_stream` for stream adapters, and `tokio_util::codec` for line framing.
- No shared library crate: duplicate helpers only if semantics differ per binary. Otherwise, consider extracting modules within each crate.
- System/command messages always use human-readable prefix plus machine-parsable segment; update both broker + client when changing formats.
- Tests are currently manual (run broker + multiple clients). When adding automated tests, use `tokio::test` and mock transports; avoid blocking on stdin/stdout.
- **AVOID OS commands and binaries** (e.g., `ifconfig`, `ip`, `netstat`, shell commands via `std::process::Command`). These break cross-platform compatibility. Use pure Rust crates instead (e.g., `if-addrs` for network interfaces, `dirs` for config paths).

Please let me know if any section feels incomplete or if additional workflows should be documented.
