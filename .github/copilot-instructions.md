# Rustynaut Copilot Instructions

## Project Layout
- `broker/` – Tokio-based chat server; entry point [broker/src/main.rs](../broker/src/main.rs).
- `client/` – CLI chat + clipboard sync client; entry point [client/src/main.rs](../client/src/main.rs).
- Each crate is isolated; run commands from its directory (`cargo run`). Shared types live inside their respective binaries, not in a workspace crate.

## Build & Run
- **Lint first**: `cd broker && cargo clippy` and `cd client && cargo clippy` before committing.
- Start broker: `cd broker && cargo run --release -- 127.0.0.1:4242` (addr param optional, defaults to `127.0.0.1:4242`).
- Start broker (verbose): `cd broker && cargo run --release -- --verbose 127.0.0.1:4242`.
- Start client: `cd client && cargo run --release -- [--verbose|-v] 127.0.0.1:4242 [username] [room]`.
- Logging uses `tracing`. Override verbosity with `RUST_LOG="chat=trace"` before running broker.
- Broker/client binaries print large ASCII banners; keep stdout clean so banners remain first output.

## Broker (server) Patterns
- `Shared` wraps `HashMap<SocketAddr, PeerInfo>` inside `Arc<Mutex<Shared>>` and currently also tracks a monotonically increasing clipboard id.
- `Peer::new` registers sessions and returns `Peer { lines, rx }`; cleanup happens after loop when removing from `Shared.peers`.
- Message flow: `process()` reads `USER <name>` (or legacy first line), then `tokio::select!` relays `rx.recv()` -> `peer.lines.send()` and incoming `LinesCodec` frames -> protocol handlers (`JOIN`/`CLIP`/`CMD`/`SAY`).
- New server features should slot into the select loop; prefer adding branches (e.g., command channels) rather than nested loops.

## Wire Protocol (current)
- Transport is line-framed TCP (`LinesCodec`).
- Client → Broker: `USER <name>`, `JOIN <room>`, `CLIP <room> <b64>`, `CMD /...`, `SAY <text>`.
- Broker → Client: `INFO <text>`, `ERR <text>`, `CLIP <room> <b64> <id>`, `SAY <user> <text>`.

## Client Patterns
- Clipboard watcher: global `SystemClipboard` + background task, sending `CLIP <room> <b64>` lines through `mpsc::channel` merged into the outgoing stream.
- User input: lines starting with `/` are sent as `CMD /...`; other lines are sent as `SAY <text>`.
- Incoming `CLIP <room> <b64> <id>` is applied to the local clipboard and not printed to stdout (to avoid base64 spam).
- `replace_clipboard_content()` silently ignores redundant updates; any new system command must guard against non-UTF8 payloads.
- Transport currently uses TCP only.

## Cross-Cutting Conventions
- All async work uses Tokio `full` feature set; prefer `tokio::select!` for fan-in, `tokio_stream` for stream adapters, and `tokio_util::codec` for line framing.
- No shared library crate: duplicate helpers only if semantics differ per binary. Otherwise, consider extracting modules within each crate.
- System/command messages always use human-readable prefix plus machine-parsable segment; update both broker + client when changing formats.
- Tests are currently manual (run broker + multiple clients). When adding automated tests, use `tokio::test` and mock transports; avoid blocking on stdin/stdout.

Please let me know if any section feels incomplete or if additional workflows should be documented.
