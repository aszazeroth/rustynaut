# Rustynaut Broker

Tokio-based broker that relays clipboard updates and file transfers between clients.

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
broker [--verbose|-v] [--no-tls] [--cert-dir <PATH>] [--regenerate-token] [addr]

  --verbose, -v         Enable verbose logging
  --no-tls              Disable TLS (insecure, for testing only)
  --cert-dir <PATH>     Certificate directory (default: ~/.config/rustynaut)
  --regenerate-token    Generate new enrollment token
```

## Notes

- TLS is enabled by default; the broker auto-generates CA + server certificates.
- Enrollment token is printed on startup for first-time client enrollment.
- File transfers are broker-mediated (streaming relay, no persistence).

## Commands (Broker-side)

- `/help`, `/rooms`, `/who`, `/offers`, `/accept <user> [filename]`, `/cancel <transfer_id>`

## Development

```bash
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
```
