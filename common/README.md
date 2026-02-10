# Rustynaut Common

Shared crate for protocol parsing, TLS helpers, and utilities used by both broker and client.

## Modules

- `constants` — protocol limits and sizes
- `protocol` — wire protocol prefixes
- `parsing` — protocol parsing helpers
- `tls` — certificate IO, enrollment helpers, and path utilities
- `utils` — base64 + formatting helpers

## Usage

```rust
use rustynaut_common::constants::MAX_LINE_LENGTH;
use rustynaut_common::parsing::parse_clip_fields;
use rustynaut_common::utils::{decode_base64, format_size};
```

## Development

```bash
cargo test -p rustynaut-common
cargo clippy -p rustynaut-common --all-targets --all-features
```
