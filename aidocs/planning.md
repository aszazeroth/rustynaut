# Rustynaut Planning Document

## Overview

Rustynaut is a cross-platform clipboard sharing application with room-based pub/sub, file transfers, TLS/mTLS encryption, and auto-enrollment. This document tracks remaining improvements and future enhancements.

---

## Next Improvements (High Priority)

Quick wins and bug fixes to pick up next:

- [ ] **Command field cursor** - Show a blinking cursor in the input field for better UX
- [ ] **Fix 'Y' key in command field** - Currently 'Y' is locked for "Yank" (copy), but should allow typing 'Y'/'y' when no message is selected
- [ ] **Re-enable echo of own /SAY messages** - Show your own chat messages in the TUI after sending
- [ ] **TUI code refactor** - Extract duplicated TUI components (message types, text selection, input handling, completion, rendering) to shared module in `common/`
  - Broker TUI (`broker/src/tui.rs`) and client TUI (`client/src/tui.rs`) have significant duplication
  - Create `common/src/tui/` module with shared types and utilities
  - Keep broker TUI lightweight (no clipboard), client TUI includes full clipboard integration
- [ ] **Auto-scroll in TUI** - Ensure messages auto-scroll to bottom when new content arrives
  - Should auto-scroll when user is already at the bottom
  - Should NOT auto-scroll if user has scrolled up to read history
  - Maybe add a visual indicator when scrolled up (e.g., "New messages below" or arrow)

---

## Remaining Improvements

### File Transfers
- [ ] Auto-accept option (configurable per-user or per-room)
- [ ] Transfer timeout (configurable, default 60s)
- [ ] Resume support: `FILE_RESUME <transfer_id> <offset>` protocol message
- [ ] Rate limiting per client
- [ ] Max concurrent transfers per room

### TLS/mTLS Enhancements
- [ ] Add `--mtls` flag to require client certificates (no enrollment fallback)
- [ ] Certificate CN binding to USER command (reject mismatch)
- [ ] Certificate revocation list (CRL)
- [ ] Automatic certificate renewal before expiration
- [ ] Rate limiting for enrollment attempts
- [ ] Enrollment audit logging
- [ ] Hardware key storage support (PKCS#11/HSM)

### TUI Improvements
- [ ] Tab completion for commands (`/help`, `/join`, etc.)
- [ ] Tab completion for usernames (in `/accept`)
- [ ] Tab completion for room names (in `/join`)
- [ ] Tab completion for filenames (in `/accept <user>`)
- [ ] **Recent File Offers Popup** - Show last N file offers per active client in room
  - Use same modal popup as tab completions
  - Configurable: 3-5 offers per client (default: 3)
  - Only show offers from clients currently active in the room
  - Broker stores offers in Shared state (per-client ring buffer)
  - Keybinding to open popup (e.g., `Ctrl+O` or `/offers` command)
  - Click or navigate to select and accept an offer directly
  - Prevents accidental over-shadowing of offers by newer ones
- [ ] **Clipboard History** - Access previous clipboard entries
  - Same popup concept as file offers
  - Configurable: 3-5 recent clips (default: 3)
  - Broker stores last N clips per room (or per client)
  - Keybinding to cycle through history (e.g., `Ctrl+Shift+V` or `/clips` command)
  - Prevents losing clipboard content when new copy over-shadows
- [ ] Click-and-drag text selection
- [ ] Multi-message selection
- [ ] Copy format options
- [ ] File transfer progress bars
- [ ] Command history persistence across restarts

### Configuration
- [ ] Hot-reload support for some settings (logging level, UI preferences)
- [ ] Environment variable support for nested keys
- [ ] Per-directory config files (git-style `.rustynaut.toml`)

### Operational
- [ ] Sunset `--no-tls` flag (require TLS for all connections)
- [ ] Version stamping in build process (`--version` output)
- [ ] Structured logging (JSON format option)
- [ ] Metrics endpoint (Prometheus-compatible)
- [ ] Health check endpoint

### Documentation
- [ ] Architecture decision records (ADRs)
- [ ] API documentation for common crate
- [ ] Deployment guides (Docker, systemd)
- [ ] Security hardening guide

### Testing
- [ ] Common crate >80% test coverage
- [ ] Integration: Network drop testing (iptables -j DROP)

---

## Architecture

### Protocol Constants

```rust
const MAX_LINE_LENGTH: usize = 2 * 1024 * 1024;  // 2MB for base64 payloads
const MAX_RECENT_CLIPS_PER_ROOM: usize = 20;
const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;   // 1GB
const FILE_CHUNK_SIZE: usize = 64 * 1024;        // 64KB chunks
```

### Wire Protocol

**Client → Broker:**
- `USER <name>` - Username registration
- `JOIN <room>` - Join room (default: "lobby")
- `CLIP <room> <b64>` - Clipboard update
- `CMD /<cmd>` - Slash command
- `SAY <text>` - Chat message
- `ENROLL <token> <username>` - Certificate enrollment
- `FILE_OFFER <room> <filename_b64> <size>` - File offer
- `FILE_ACCEPT <room> <username> <filename_b64>` - Accept file
- `FILE_START <transfer_id> <filename_b64> <count>` - Start transfer
- `FILE_CHUNK <transfer_id> <offset> <chunk_b64>` - File chunk
- `FILE_END <transfer_id> <sha256>` - Transfer end
- `FILE_CANCEL <transfer_id>` - Cancel transfer

**Broker → Client:**
- `INFO <text>` - Information message
- `ERR <text>` - Error message
- `CLIP <room> <b64> <id>` - Clipboard broadcast
- `SAY <user> <text>` - Chat message
- `ENROLLED <cert_b64> <key_b64> <ca_b64>` - Enrollment response
- `FILE_OFFER <room> <username> <filename_b64> <size>` - File offer
- `FILE_INCOMING <transfer_id> <filename_b64> <size>` - Incoming file
- `FILE_DONE <transfer_id> <sha256>` - Transfer complete
- `FILE_SENT <transfer_id> <count>` - Sent confirmation
- `FILE_CANCELLED <transfer_id> <reason>` - Cancelled

### Certificate Hierarchy

```
Rustynaut CA (generated by broker on first run)
├── Broker Server Certificate (signed by CA)
└── Client Certificates (signed by CA via ENROLL protocol)
```

**Storage:**
```
~/.config/rustynaut/              # Platform-appropriate config dir
├── ca/
│   ├── ca.crt                    # CA cert (distributed to clients)
│   └── ca.key                    # CA private key (BROKER ONLY)
├── broker/
│   ├── server.crt                # Server certificate
│   └── server.key                # Server private key
└── client/
    ├── client.crt                # Client certificate
    ├── client.key                # Client private key
    └── ca.crt                    # CA certificate
```

### Reconnection

Auto-reconnection with exponential backoff (enrolled clients only):
- 3 attempts with delays: 1s → 2s → 4s (max 8s)
- State restoration: re-sends USER and JOIN after reconnect
- Manual retry available after max attempts

### File Transfer Flow

```
Sender → Broker:  FILE_OFFER <room> <filename_b64> <size>
Broker → Room:    FILE_OFFER <room> <username> <filename_b64> <size>
Receiver → Broker: FILE_ACCEPT <room> <username> <filename_b64>
Broker → Sender:  FILE_START <transfer_id> <filename_b64> <acceptor_count>
Broker → Receiver: FILE_INCOMING <transfer_id> <filename_b64> <size>
Sender → Broker:  FILE_CHUNK <transfer_id> <offset> <chunk_b64>
Broker → Acceptors: FILE_CHUNK <transfer_id> <offset> <chunk_b64>
Sender → Broker:  FILE_END <transfer_id> <sha256>
Broker → Acceptors: FILE_DONE <transfer_id> <sha256>
Broker → Sender:  FILE_SENT <transfer_id> <acceptor_count>
```

---

## Workspace Structure

```
rustynaut/
├── Cargo.toml                    # Workspace root
├── Cargo.lock                    # Shared lockfile
├── README.md
├── AGENTS.md
├── aidocs/
│   └── planning.md               # This file
├── common/                       # Shared library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── protocol.rs           # Protocol messages & parsing
│       ├── types.rs              # Shared types
│       ├── constants.rs          # Protocol constants
│       ├── parsing.rs            # Message parsing utilities
│       ├── tls/                  # Shared TLS utilities
│       │   ├── mod.rs
│       │   ├── certs.rs          # Certificate generation
│       │   └── paths.rs          # Certificate paths
│       ├── config/               # Configuration system
│       │   ├── mod.rs
│       │   ├── types.rs
│       │   ├── loader.rs
│       │   └── validator.rs
│       └── utils.rs              # General utilities
├── broker/                       # Server/broker component
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs
│       ├── broker.rs             # Broker-specific logic
│       ├── rooms.rs              # Room management
│       ├── peers.rs              # Peer/connection management
│       ├── file_transfers.rs     # File transfer state
│       └── tui.rs                # Broker TUI
└── client/                       # Client component
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── tui.rs                # TUI implementation
        ├── clipboard.rs          # Clipboard handling
        ├── file_transfer.rs      # File transfer UI
        ├── reconnect.rs          # Auto-reconnection logic
        └── config.rs             # Client config
```

---

## Key Design Decisions

1. **No workspace before, now workspace with common crate** - Eliminated code duplication between broker and client
2. **Broker-mediated file transfers** - Not P2P, works through NAT/firewall
3. **Base64 encoding over line protocol** - Binary-safe, simple framing
4. **TLS 1.3 with mTLS enrollment** - Battle-tested, auto-enrollment with token
5. **Cross-platform clipboard via arboard** - Handles text, images, file detection
6. **Ratatui + tui-prompts for TUI** - Readline keybindings, autocomplete support
7. **Exponential backoff reconnection** - Resilient to broker restarts/network issues

