# Rustynaut Client

CLI/TUI client for clipboard sync and file transfers.

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
client [--verbose|-v] [--no-tls] [--enroll <TOKEN>] [--cert-dir <PATH>] <addr> [username] [room]

  --verbose, -v         Enable verbose logging
  --no-tls              Disable TLS (insecure, for testing only)
  --enroll <TOKEN>      Enroll with broker using token
  --cert-dir <PATH>     Certificate directory
```

## TUI Controls

- Click a message to select it
- Press `y` to copy selected message to clipboard
- Press `Esc` to deselect

## Slash Commands

- `/help`, `/rooms`, `/who`, `/offers`, `/accept <user> [filename]`, `/cancel <transfer_id>`, `/quit`, `/exit`

## Development

```bash
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```
