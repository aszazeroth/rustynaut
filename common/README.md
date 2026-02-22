# Rustynaut Common

Shared crate for protocol parsing, TLS helpers, configuration, and utilities used by both broker and client.

## Modules

### `config`
Configuration management with TOML support:
- `ClientConfig` - Client configuration structure
- `BrokerConfig` - Broker configuration structure
- `ReconnectConfig` - Auto-reconnection settings
- Config file loading with platform-appropriate paths

### `constants`
Protocol limits and sizes:
- `MAX_LINE_LENGTH` - 2MB for base64 payloads
- `MAX_FILE_SIZE` - 1GB file transfer limit
- `FILE_CHUNK_SIZE` - 64KB transfer chunks
- `MAX_RECENT_CLIPS_PER_ROOM` - 20 entries for deduplication

### `protocol`
Wire protocol message types and prefixes:
- Message type definitions
- Protocol constants
- Message builders

### `parsing`
Protocol parsing helpers:
- `parse_clip_fields()` - Parse CLIP messages
- `parse_file_offer_fields()` - Parse FILE_OFFER messages
- `parse_file_accept_fields()` - Parse FILE_ACCEPT messages
- `parse_say_fields()` - Parse SAY messages
- All parsing functions return structured data

### `tls`
Certificate IO, enrollment helpers, and path utilities:
- Certificate generation with `rcgen`
- PEM encoding/decoding
- Certificate fingerprinting
- Enrollment response parsing
- Platform-appropriate path resolution

### `utils`
Base64 + formatting helpers:
- `encode_base64()` / `decode_base64()` - Safe base64 operations
- `format_size()` - Human-readable file sizes (KB, MB, GB)
- `format_timestamp()` - Consistent timestamp formatting

## Usage

```rust
use rustynaut_common::constants::MAX_LINE_LENGTH;
use rustynaut_common::parsing::parse_clip_fields;
use rustynaut_common::utils::{decode_base64, format_size};
use rustynaut_common::config::{ClientConfig, ConfigLoader};
```

## Configuration Example

```rust
use rustynaut_common::config::{ClientConfig, ConfigLoader};

// Load configuration with defaults
let config = ClientConfig::default();

// Load from config file
let config = ClientConfig::load(None)?;

// Access reconnection settings
if config.connection.reconnect.enabled {
    println!("Max attempts: {}", config.connection.reconnect.max_attempts);
}
```

## Reconnection Configuration

The `ReconnectConfig` struct provides auto-reconnection settings:

```rust
ReconnectConfig {
    enabled: true,              // Enable auto-reconnection
    max_attempts: 3,            // Max retry attempts
    base_delay_seconds: 1,      // Initial backoff (1s)
    max_delay_seconds: 8,       // Max backoff cap (8s)
    min_connection_seconds: 5,  // Minimum connection duration
}
```

Backoff formula: `delay = min(base * 2^(attempt-1), max_delay)`

This results in backoff sequence: 1s → 2s → 4s → 8s (capped)

## Development

```bash
# Test this crate only
cargo test -p rustynaut-common

# Run clippy
cargo clippy -p rustynaut-common --all-targets --all-features

# Documentation
cargo doc -p rustynaut-common --open
```

## See Also

- [Main README](../README.md) - Project overview
- [Client README](../client/README.md) - Client-specific usage
- [Broker README](../broker/README.md) - Broker-specific usage
