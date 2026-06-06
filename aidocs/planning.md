# Rustynaut Planning Document

## Overview

Rustynaut is a cross-platform clipboard sharing application with room-based pub/sub,
broker-mediated file transfers, TLS enrollment, and terminal UIs for the broker and client.

This document tracks future work. It has been pruned to remove items that are already
implemented, obsolete, or better tracked as polish rather than core work.

## Current State

- TLS is enabled by default and enrollment exists, but strict mTLS is not fully enforced.
  The broker currently allows unauthenticated TLS clients and accepts `USER` without binding it
  to a verified client certificate.
- File transfer integrity is covered on the client side, and broker file-transfer state now lives
  in `broker/src/file_transfers.rs`.
- The client has command, room, username, filename, and transfer-id completion.
- Client and broker TUIs have visible command cursors and basic auto-scroll behavior.
- Lowercase and uppercase `y` now type normally when no TUI text selection is active.
- There is no automated integration test environment yet. Keep near-term tests mostly unit-like
  until a stable harness exists.

## Next Improvements

These are the best low-friction targets after the current branch.

### P0

- [x] **Fix reconnect early-drop stalls**
  - `ReconnectionManager::should_reconnect` rejects reconnects when a session drops before
    `min_connection_seconds`, but the TUI path may not schedule another retry afterward.
  - Expected result: a quick broker disconnect never leaves the client permanently idle.

- [ ] **Harden config merge and validation**
  - Current config merging infers "unset" by comparing concrete values to defaults, which can lose
    explicit default-valued settings and overwrite whole nested structs.
  - Use partial config overlays with `Option<T>` fields, then validate ranges, enum values, and
    cross-field constraints.

- [x] **Add typed protocol parsing and validation**
  - Move from mostly string-splitting helpers toward typed `ProtocolMessage` values with bounded
    and validated fields.
  - Validate usernames, room names, base64 payloads, file sizes, offsets, sha256 strings, and empty
    fields at the parsing boundary.

- [ ] **Make mTLS semantics true or explicit**
  - Add a broker mode that requires client certificates after enrollment.
  - Bind certificate identity to `USER` and reject mismatches.
  - Until this is implemented, docs should describe the current behavior as TLS with optional
    client certificates, not strict mTLS.

### P1

- [ ] **File-transfer authorization and lifecycle cleanup**
  - Restrict `FILE_CANCEL` and `/cancel` so only the sender, acceptors, or an authorized admin can
    cancel a transfer.
  - Remove stale offers and active transfers on timeout, sender disconnect, acceptor disconnect,
    cancellation, and completion.
  - Clean up cancelled or failed incoming transfers on the client, including temp files and
    completion context.

- [ ] **Broker-side file-transfer sanity limits**
  - Enforce cumulative transferred bytes against the advertised file size.
  - Validate chunk offset ordering and decoded chunk size.
  - Include sender identity in duplicate file-offer suppression so two users can offer files with
    the same name and size in the same room.

- [ ] **Avoid blocking file I/O in async client paths**
  - Incoming chunk writes and final checksum work currently happen with synchronous file APIs
    during network message handling.
  - Move large-file work to `tokio::fs`, `spawn_blocking`, or a dedicated transfer worker.

- [ ] **Enforce file-size limits before offering**
  - Reject oversized clipboard file offers client-side before advertising them to the room.

- [ ] **Rate limiting and backpressure**
  - Add bounded peer queues.
  - Rate-limit enrollment attempts, commands, clipboard messages, and file chunks per client.

- [ ] **TUI state correctness**
  - Wire real connection status into `App.connected`.
  - Keep `App.users_in_room` synchronized from join/leave information instead of only updating
    completion context.

- [ ] **TLS and enrollment hardening**
  - Add negative tests for enrollment response parsing.
  - Validate PEM shape, certificate/key matching, and CA trust expectations.
  - Write certificates and keys atomically, with private permissions before key material is written
    where possible. Review Windows ACL behavior.

### P2

- [ ] **Complete environment variable support**
  - Cover all nested config keys, report parse errors, and test precedence across defaults, files,
    and environment.

- [ ] **Manual smoke-test checklist**
  - Add a lightweight checklist for running one broker and two clients locally.
  - Keep this separate from a full integration harness until the product shape stabilizes.

- [ ] **Documentation drift cleanup**
  - Fix README claims around mTLS enforcement.
  - Clarify broker TUI commands versus client slash commands.
  - Update common crate docs for actual exported helpers and TLS responsibilities.

- [ ] **TUI polish**
  - Refine auto-scroll and add a "new messages below" indicator.
  - Consider a blinking cursor if it improves readability.
  - Re-enable local echo for own chat messages if the broker intentionally does not echo to sender.
  - Prune stale file-offer and active-transfer completion entries after successful transfers.

- [ ] **Linux native file clipboard support**
  - Improve X11/Wayland file-manager clipboard detection instead of relying only on text path
    fallback.

## Later Improvements

### File Transfers

- [ ] Auto-accept option, configurable per user or room.
- [ ] Resume support: `FILE_RESUME <transfer_id> <offset>`.
- [ ] Max concurrent transfers per room and per client.
- [ ] File transfer progress bars.
- [ ] Recent file offers popup with selectable accept actions.

### TLS/mTLS

- [ ] Certificate revocation list.
- [ ] Automatic certificate renewal before expiration.
- [ ] Enrollment audit logging.
- [ ] Hardware key storage support with PKCS#11 or platform stores.

### TUI

- [ ] Shared TUI primitives where broker and client duplication is clearly worth extracting.
- [ ] Clipboard history popup for recent room entries.
- [ ] Copy format options.
- [ ] Command history persistence across restarts.

### Configuration and Operations

- [ ] Hot reload for selected settings such as logging level and UI preferences.
- [ ] Per-directory config files, similar to `.rustynaut.toml`.
- [ ] `--version` output for broker and client.
- [ ] Structured JSON logging if runtime logging does not already honor the config shape.
- [ ] Metrics and health endpoints.

### Documentation

- [ ] Architecture decision records.
- [ ] API documentation for the common crate.
- [ ] Deployment guides for Docker and systemd.
- [ ] Security hardening guide.

### Testing

- [ ] More common crate unit tests for config overlays, env precedence, parser failures, and TLS
  enrollment failures.
- [ ] Broker unit tests for file-transfer ownership, cleanup, duplicate suppression, and chunk
  validation.
- [ ] Client unit tests for reconnect state, TUI state updates, and transfer cleanup.
- [ ] Future integration harness for broker restart, network drop, two-client clipboard sync, and
  full file-transfer flows.

## Architecture

### Protocol Constants

```rust
const MAX_LINE_LENGTH: usize = 2 * 1024 * 1024;  // 2MB for base64 payloads
const MAX_RECENT_CLIPS_PER_ROOM: usize = 20;
const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;   // 1GB
const FILE_CHUNK_SIZE: usize = 64 * 1024;        // 64KB chunks
```

### Wire Protocol

**Client -> Broker:**

- `USER <name>` - Username registration
- `JOIN <room>` - Join room
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

**Broker -> Client:**

- `INFO <text>` - Information message
- `ERR <text>` - Error message
- `CLIP <room> <b64> <id>` - Clipboard broadcast
- `SAY <user> <text>` - Chat message
- `ENROLLED <cert_b64> <key_b64> <ca_b64>` - Enrollment response
- `FILE_OFFER <room> <username> <filename_b64> <size>` - File offer
- `FILE_START <transfer_id> <filename_b64> <count>` - Start transfer
- `FILE_INCOMING <transfer_id> <filename_b64> <size>` - Incoming file
- `FILE_CHUNK <transfer_id> <offset> <chunk_b64>` - File chunk
- `FILE_DONE <transfer_id> <sha256>` - Transfer complete
- `FILE_SENT <transfer_id> <count>` - Sent confirmation
- `FILE_CANCELLED <transfer_id> <reason>` - Cancelled

### Certificate Hierarchy

```text
Rustynaut CA (generated by broker on first run)
├── Broker Server Certificate (signed by CA)
└── Client Certificates (signed by CA via ENROLL protocol)
```

**Storage:**

```text
~/.config/rustynaut/              # Platform-appropriate config dir
├── ca/
│   ├── ca.crt                    # CA cert, distributed to clients
│   └── ca.key                    # CA private key, broker only
├── broker/
│   ├── server.crt                # Server certificate
│   └── server.key                # Server private key
└── client/
    ├── client.crt                # Client certificate
    ├── client.key                # Client private key
    └── ca.crt                    # CA certificate
```

### Reconnection

Auto-reconnection is intended for enrolled clients:

- Exponential delays: 1s -> 2s -> 4s, capped at 8s.
- State restoration re-sends `USER` and `JOIN` after reconnect.
- Manual retry should be available when automatic attempts are exhausted or suppressed.

### File Transfer Flow

```text
Sender -> Broker:    FILE_OFFER <room> <filename_b64> <size>
Broker -> Room:      FILE_OFFER <room> <username> <filename_b64> <size>
Receiver -> Broker:  FILE_ACCEPT <room> <username> <filename_b64>
Broker -> Sender:    FILE_START <transfer_id> <filename_b64> <acceptor_count>
Broker -> Receiver:  FILE_INCOMING <transfer_id> <filename_b64> <size>
Sender -> Broker:    FILE_CHUNK <transfer_id> <offset> <chunk_b64>
Broker -> Acceptors: FILE_CHUNK <transfer_id> <offset> <chunk_b64>
Sender -> Broker:    FILE_END <transfer_id> <sha256>
Broker -> Acceptors: FILE_DONE <transfer_id> <sha256>
Broker -> Sender:    FILE_SENT <transfer_id> <acceptor_count>
```

## Workspace Structure

```text
rustynaut/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── AGENTS.md
├── aidocs/
│   └── planning.md
├── common/
│   ├── Cargo.toml
│   └── src/
│       ├── constants.rs
│       ├── error.rs
│       ├── lib.rs
│       ├── parsing.rs
│       ├── protocol.rs
│       ├── types.rs
│       ├── utils.rs
│       ├── config/
│       │   ├── error.rs
│       │   ├── load.rs
│       │   ├── mod.rs
│       │   ├── paths.rs
│       │   └── types.rs
│       └── tls/
│           ├── certs.rs
│           ├── enrollment.rs
│           ├── mod.rs
│           └── paths.rs
├── broker/
│   ├── Cargo.toml
│   └── src/
│       ├── file_transfers.rs
│       ├── main.rs
│       ├── tls.rs
│       └── tui.rs
└── client/
    ├── Cargo.toml
    └── src/
        ├── clipboard_files.rs
        ├── completion.rs
        ├── main.rs
        ├── reconnect.rs
        ├── tls.rs
        └── tui.rs
```

## Key Design Decisions

1. **Workspace with common crate** - Shared protocol, config, TLS helpers, constants, and utilities.
2. **Broker-mediated file transfers** - Simpler than P2P and friendlier to NAT/firewall setups.
3. **Base64 over a line protocol** - Binary-safe framing that is straightforward to debug.
4. **TLS enrollment** - Token-based onboarding can distribute client credentials without manual
   certificate copying.
5. **Cross-platform clipboard via arboard** - Keeps the core clipboard path portable.
6. **Ratatui-based TUIs** - Rich terminal UI with selectable text, completion, and command input.
7. **Reconnect with state restoration** - Clients should recover from broker restarts and transient
   network drops.
