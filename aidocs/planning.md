# Planning: Broker-as-Clipboard Hub (Rooms)

## Current Intent (Agreed)
- Broker runs on a host; clients run in VMs.
- Clipboard sync is the MVP; chat is incidental.
- "One shared clipboard per room" (pub/sub by room).
- Transport: TCP only for now. Drop UDP. TLS + auth later (design should not block adding it).

## MVP Protocol (Text, line-framed)
We will use `LinesCodec` end-to-end (broker + client TCP) so that every message is a single UTF-8 line.

Suggested wire format (no JSON yet; human-readable + machine-parseable):
- Client → Broker:
  - `USER <name>` (sent once after connect)
  - `JOIN <room>` (defaults to `lobby` if omitted)
  - `CLIP <room> <b64>` (clipboard update)
  - `CMD <raw>` (slash command passthrough, e.g. `CMD /rooms`)
- Broker → Client:
  - `INFO <text>`
  - `ERR <text>`
  - `CLIP <room> <b64> <id>` (broadcast clipboard update with server-assigned id)

Notes:
- Base64 payload keeps clipboard binary-safe.
- `<id>` enables echo suppression (client should not re-send what it just applied).

## Architecture Targets
- Broker maintains:
  - Clients: `client_id`, `username`, `tx`, `room`.
  - Rooms: map `room -> set(client_id)` plus "last clipboard" metadata if needed.
- Clipboard broadcast scope: only within the sender's room.
- Slash commands are broker-side (IRC-like). Minimal set:
  - `/help`, `/rooms`, `/join <room>`, `/who`, `/me`, `/ping`.  - `/shout <msg>` – broadcast to ALL rooms (global announcement).
## Implementation Roadmap
### 1) Stabilize framing + protocol
- Broker: keep `LinesCodec`, but stop formatting ad-hoc `"{username}: {msg}"` as the primary protocol.
- Client: switch TCP handling from `BytesCodec` + regex heuristics to `LinesCodec` parsing.
- Remove UDP module and `--udp` CLI flag.

### 2) Introduce explicit message parsing/formatting
- Add minimal parsing helpers (in each crate, no shared workspace crate):
  - Parse first line(s) as `USER` and optional `JOIN`.
  - Treat user input starting with `/` as a command (`CMD /...`).
  - Treat clipboard watcher events as `CLIP <room> <b64>`.

### 3) Broker rooms + routing
- Current state: peers already store `username` + `room`, but broadcasts are still global.
- Add a room-scoped broadcast helper (e.g. `broadcast_room(sender, room, msg)`), and use it for `CLIP` (and optionally `SAY`).
- Decide semantics for `JOIN`:
  - Send `INFO left <old_room>` / `INFO joined <new_room>` to the client.
  - Broadcast `INFO <user> joined <room>` / `INFO <user> left <room>` to that room.

### 4) Client clipboard loop suppression
- Track `last_applied_clip_id` per room (or globally for MVP).
- When applying a remote clipboard update, do not re-trigger sending the same clipboard back.
  - Practical approach: store last applied decoded string (or hash) + last id.
- **Broker-side deduplication (implemented):**
  - Broker tracks recent clip content hashes per room (up to 20 entries)
  - Duplicate clips are detected and not re-broadcast
  - Prevents echo loops when multiple clients sync the same content

### 5) Observability + operability
- Broker uses `tracing` (already present). Add spans around connection lifecycle + room changes.
- Keep the ASCII banner printed first; push operational logs to `tracing`.

## Security/TLS/Auth Later (Design Hooks)
- Keep a stable "client id" concept server-side so auth tokens can bind to identities later.
- Keep protocol as line-based so it can run over TLS without changes.
- Plan for an `AUTH <token>` message in the future (no enforcement in MVP).
---

## Large Clipboard / File Transfer (Inline Binary Framing)

### Problem Statement
- Current `CLIP` protocol uses base64 over line-framed TCP (~8KB default limit)
- Real-world use cases: screenshots (PNG, 100KB–5MB), config files, logs
- Base64 adds 33% overhead; large payloads block the message stream

### Chosen Approach: Inline Binary Framing (Option B)

**Decision (2026-01-26):** Use inline binary framing over the existing connection rather than 
opening separate sideband ports. This reuses our existing single-port architecture and is 
firewall-friendly.

**Key insight:** FILE_CHUNK uses a line header followed by raw bytes - similar to HTTP chunked 
transfer encoding. The receiver reads the header line, then reads exactly `len` raw bytes.

The broker acts as a relay for file transfers (no P2P), avoiding NAT/firewall issues since 
clients already have a connection to the broker.

### Wire Protocol Extension

**Phase 1: Offer & Accept**
```
# Sender announces file (already implemented)
Client → Broker:  FILE_OFFER <room> <filename_b64> <size_bytes>
Broker → Room:    FILE_OFFER <room> <username> <filename_b64> <size_bytes>

# Receiver accepts (triggers transfer)
Client → Broker:  FILE_ACCEPT <room> <username> <filename_b64>
Broker → Sender:  FILE_START <transfer_id> <filename_b64> <acceptor_count>
Broker → Receiver: FILE_INCOMING <transfer_id> <filename_b64> <size_bytes>
```

**Phase 2: Binary Transfer (base64 encoded chunks over line protocol)**
```
# Sender streams chunks as base64 over normal line protocol
Sender → Broker:  FILE_CHUNK <transfer_id> <offset> <chunk_b64>
Broker → Acceptors: FILE_CHUNK <transfer_id> <offset> <chunk_b64>
```

**Phase 3: Completion**
```
Sender → Broker:    FILE_END <transfer_id> <sha256>
Broker → Acceptors: FILE_DONE <transfer_id> <sha256>
Broker → Sender:    FILE_SENT <transfer_id> <acceptor_count>
```

**Cancellation:**
```
Client → Broker:  FILE_CANCEL <transfer_id>
Broker → All:     FILE_CANCELLED <transfer_id> <reason>
```

### Chunk Size & Encoding
- **Chunk size:** 64KB raw → ~85KB base64 encoded
- **Encoding:** Standard base64, fits within 2MB MAX_LINE_LENGTH
- **Checksum:** SHA256 computed during file read, hex encoded

### Implementation Status

#### Step 1: Transfer State Tracking (Broker) ✅
- [x] Add `FileTransfer` struct with transfer_id, sender, acceptors, state, progress
- [x] Add `HashMap<TransferId, FileTransfer>` to `Shared` state
- [x] Add transfer ID generation (incrementing u64, like clip_id)
- [x] Add `PendingOffer` struct to track offers before acceptance

#### Step 2: FILE_ACCEPT & FILE_START (Broker) ✅
- [x] Parse `FILE_ACCEPT <room> <username> <filename_b64>` command
- [x] Match accept to pending offer, create FileTransfer entry
- [x] Send `FILE_START` to sender with transfer_id
- [x] Send `FILE_INCOMING` to acceptor(s)
- [x] Handle `/accept <user> <filename>` command (broker-side)
- [x] Handle `/cancel <transfer_id>` command

#### Step 3: FILE_CHUNK Handling (Broker) ✅
- [x] Parse `FILE_CHUNK <transfer_id> <offset> <chunk_b64>`
- [x] Relay chunk to all acceptors
- [x] Track transfer state (Transferring)

#### Step 4: FILE_END & Completion (Broker) ✅
- [x] Parse `FILE_END <transfer_id> <sha256>`
- [x] Send `FILE_DONE` to acceptors with checksum
- [x] Send `FILE_SENT` to sender with acceptor count
- [x] Clean up transfer state

#### Step 5: Client Send Path ✅
- [x] On FILE_START received, spawn task to send chunks
- [x] Read file in 64KB chunks, base64 encode, send FILE_CHUNK for each
- [x] Compute SHA256 while reading (hex encoded)
- [x] Send FILE_END with checksum
- [x] FILE_TX channel for sending from async context

#### Step 6: Client Receive Path ✅
- [x] Parse FILE_INCOMING, FILE_CHUNK, FILE_DONE, FILE_SENT, FILE_CANCELLED
- [x] Display human-readable messages for all file transfer events
- [x] On FILE_INCOMING, prepare temp file for writing
- [x] Handle FILE_CHUNK: decode base64, write to temp file
- [x] On FILE_DONE, move temp file to downloads folder (with conflict resolution)
- [x] Files saved to user's Downloads directory

#### Step 7: User Commands ✅
- [x] `/accept <user> <filename>` - manually accept a file offer (via broker CMD)
- [x] `/cancel <transfer_id>` - cancel in-progress transfer (via broker CMD)
- [ ] Auto-accept option (configurable)

### Existing Progress (FILE_OFFER Notification)
- [x] Detect large clipboard (>64KB threshold)
- [x] Detect native file copies from Finder/Explorer (cross-platform)
- [x] Send `FILE_OFFER` for copied files to broker
- [x] Display received `FILE_OFFER` as user-friendly message with human-readable size
- [x] Broker parses and relays FILE_OFFER with username
- [x] Broker-side FILE_OFFER deduplication
- [x] Broker registers offers for later acceptance

#### Step 5: Robustness
- [ ] Transfer timeout (configurable, default 60s)
- [ ] Resume support (future): `FILE_RESUME <transfer_id> <offset>`
- [ ] Rate limiting per client
- [ ] Max concurrent transfers per room

### Size Thresholds (Configurable)
| Content Size | Strategy |
|--------------|----------|
| < 64 KB      | `CLIP` (base64, inline) |
| 64 KB – 50 MB| `FILE_OFFER` (sideband) |
| > 50 MB      | Reject with `ERR file too large` |

### State Structures (Broker)

```rust
struct FileTransfer {
    id: TransferId,
    sender: SocketAddr,
    room: String,
    filename: String,
    size: u64,
    sha256_expected: String,
    acceptors: HashSet<SocketAddr>,
    state: TransferState, // Offered | Accepted | Transferring | Done | Failed
    created_at: Instant,
}
```

### Design Decisions
- **Broker-mediated** (not P2P) — simpler, works through NAT
- **Streaming only** — no broker storage, just relay (no persistence)
- **SHA256 verification** — integrity check built-in
- **Threshold-based** — small clipboards stay fast, large ones use sideband
- **Auto for clipboard** — large clipboard auto-triggers file transfer
- **Explicit for files** — `/send <path>` for arbitrary files

---

## TLS Encryption with tokio-rustls

### Rationale
Running broker/client over the internet requires encrypted transport. TLS 1.3 via `tokio-rustls` provides:
- **Transparent streaming** — no message size limits, handles large file transfers seamlessly
- **Drop-in integration** — TLS streams implement `AsyncRead + AsyncWrite`, so `LinesCodec` works unchanged
- **Battle-tested** — same stack as HTTPS, well-audited

### Alternative Considered: Noise Protocol (snow crate)
- Simpler key management (no certificates, just X25519 key pairs)
- But: 65KB per-message limit requires manual chunking for large files
- Decision: Use TLS for simplicity with large transfers; revisit Noise for peer-to-peer scenarios

### Dependencies

**Broker (Cargo.toml):**
```toml
tokio-rustls = "0.26"
rustls = { version = "0.23", default-features = false, features = ["std", "tls12"] }
rustls-pemfile = "2"
rcgen = "0.13"  # Optional: generate self-signed certs at startup
```

**Client (Cargo.toml):**
```toml
tokio-rustls = "0.26"
rustls = { version = "0.23", default-features = false, features = ["std", "tls12"] }
rustls-pemfile = "2"
webpki-roots = "0.26"  # Optional: system CA roots
```

### CLI Changes

**Broker:**
```
broker [--verbose|-v] [--cert <path>] [--key <path>] [--no-tls] [addr]

  --cert <path>     Path to PEM certificate chain (required unless --no-tls)
  --key <path>      Path to PEM private key (required unless --no-tls)
  --no-tls          Run in plaintext mode (development only)
  --generate-cert   Auto-generate self-signed cert on startup
```

**Client:**
```
client [--verbose|-v] [--ca <path>] [--insecure] [--server-name <name>] <addr> [username] [room]

  --ca <path>       Path to CA certificate for verification
  --insecure        Skip certificate verification (self-signed dev certs)
  --server-name     SNI hostname (defaults to addr hostname)
```

### Integration Points

| Location | Current | With TLS |
|----------|---------|----------|
| Broker accept | `listener.accept()` → `Framed::new(stream, ...)` | `listener.accept()` → `acceptor.accept(stream)` → `Framed::new(tls_stream, ...)` |
| Broker Peer struct | `Framed<TcpStream, LinesCodec>` | `Framed<TlsStream, LinesCodec>` |
| Client connect | `TcpStream::connect()` → `stream.split()` | `TcpStream::connect()` → `connector.connect()` → `tokio::io::split(tls_stream)` |

### Implementation Roadmap

#### Step 1: Add Dependencies & CLI Parsing
- [ ] Add TLS crates to both Cargo.toml files
- [ ] Extend `parse_args()` in broker for `--cert`, `--key`, `--no-tls`, `--generate-cert`
- [ ] Extend client CLI for `--ca`, `--insecure`, `--server-name`

#### Step 2: Broker TLS Acceptor
- [ ] Create `tls.rs` module with cert/key loading functions
- [ ] Build `TlsAcceptor` from loaded config
- [ ] Wrap accepted streams before passing to `process()`
- [ ] Update `Peer` struct to use `TlsStream` type

#### Step 3: Client TLS Connector
- [ ] Create TLS config with root store (or dangerous verifier for `--insecure`)
- [ ] Wrap `TcpStream` after connect, before split
- [ ] Change `stream.split()` to `tokio::io::split(tls_stream)`

#### Step 4: Self-Signed Certificate Generation
- [ ] Use `rcgen` to generate cert + key on broker startup with `--generate-cert`
- [ ] Print certificate fingerprint for client verification
- [ ] Optionally save generated cert to disk for reuse

#### Step 5: Testing & Documentation
- [ ] Test with self-signed certs locally
- [ ] Test `--insecure` client flag
- [ ] Document cert generation workflow in README
- [ ] Add example with Let's Encrypt for production

### Certificate Distribution Options
1. **Self-signed + `--insecure`** — development only
2. **Self-signed + `--ca`** — distribute broker's cert to clients
3. **Let's Encrypt** — production with proper domain
4. **mTLS (future)** — client certificates for mutual authentication

---

## mTLS with Auto-Enrollment

### Overview
Mutual TLS (mTLS) where both broker and clients authenticate via certificates. The broker acts as a Certificate Authority (CA), generating and signing client certificates on-demand through an enrollment protocol protected by a shared secret.

### Certificate Hierarchy

```
Rustynaut CA (generated by broker on first run)
├── Broker Server Certificate (signed by CA)
└── Client Certificates (signed by CA via ENROLL protocol)
```

### Certificate Storage Layout

```
~/.config/rustynaut/              # Platform-appropriate config dir
├── ca/
│   ├── ca.crt                    # CA cert (distributed to clients)
│   └── ca.key                    # CA private key (BROKER ONLY - KEEP SECRET)
├── broker/
│   ├── server.crt                # Server certificate
│   └── server.key                # Server private key
├── clients/                      # Pre-generated client certs (optional)
│   ├── alice.crt
│   └── alice.key
└── enrollment-token              # Shared secret for enrollment (auto-generated)
```

### Enrollment Protocol

**Problem:** How do new clients get certificates without manual file copying?

**Solution:** Token-protected enrollment over TLS. During enrollment, client connects with `--insecure` (skip server cert verification) but is restricted to ENROLL command only. Broker generates keypair + certificate and sends everything to client.

#### Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Accept any/no cert during enrollment** | Client doesn't have certs yet; use `--insecure` for first connect only |
| **Broker generates keypair** | Simpler than CSR flow; client receives complete cert + key bundle |
| **Allow re-enrollment** | Useful during development; overwrites existing cert for same username |
| **Auto-reconnect after enrollment** | Client saves certs, then reconnects transparently with mTLS |
| **Log certificate fingerprints** | Debug trust issues; log on broker startup and client connect |
| **Clock skew tolerance** | Backdate `not_before` by 1 hour to tolerate minor time differences |

#### Enrollment Flow

```
┌─────────────────────────────────────────────────────────────────────┐
│                     BROKER FIRST RUN                                │
├─────────────────────────────────────────────────────────────────────┤
│ 1. Generate CA certificate + key (not_before: now - 1 hour)        │
│ 2. Generate server certificate signed by CA                        │
│ 3. Generate enrollment token (UUID v4)                             │
│ 4. Save all to ~/.config/rustynaut/                                │
│ 5. Log CA + server cert fingerprints (SHA256)                      │
│ 6. Print:                                                          │
│    "Enrollment token: a1b2c3d4-e5f6-7890-abcd-ef1234567890"        │
│    "Share this token with clients for first-time enrollment"       │
└─────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────┐
│                     CLIENT ENROLLMENT                               │
├─────────────────────────────────────────────────────────────────────┤
│ 1. Connect with --insecure (skip server cert verification)         │
│ 2. Send: ENROLL <token> <username>                                 │
│ 3. Broker validates token, generates cert + key                    │
│ 4. Receive: ENROLLED <cert_b64> <key_b64> <ca_b64>                 │
│ 5. Save cert, key, CA to ~/.config/rustynaut/client/               │
│ 6. Log client cert fingerprint                                     │
│ 7. Disconnect, auto-reconnect with mTLS                            │
│ 8. Now: full mTLS connection, proceed with USER/JOIN               │
└─────────────────────────────────────────────────────────────────────┘
```

#### Wire Protocol Extension

```
# Client requests enrollment (connects with --insecure, no client cert)
Client → Broker:  ENROLL <token> <username>

# Broker generates keypair and certificate, sends everything
Broker → Client:  ENROLLED <cert_base64> <key_base64> <ca_cert_base64>

# Error cases
Broker → Client:  ERR enrollment failed: invalid token
Broker → Client:  ERR enrollment failed: rate limited
```

**Re-enrollment:** If username already has a cert, broker generates a new one (effectively revoking the old one). This is intentional for development; can be restricted with `--no-reenroll` flag later.

**Unenrolled client restrictions:** Before sending ENROLL, client can only send ENROLL command. Any other command returns `ERR not enrolled`.

### Enrollment Token Security

| Mechanism | Description |
|-----------|-------------|
| **Token format** | UUID v4 (128-bit random) |
| **Storage** | `~/.config/rustynaut/enrollment-token` (mode 0600) |
| **Regeneration** | `broker --regenerate-token` creates new token, invalidates old |
| **Expiration** | Optional: `--enrollment-window 24h` limits enrollment period |
| **One-time use** | Optional: `--single-use-token` invalidates after one enrollment |
| **Disable** | `--no-enrollment` disables enrollment, requires pre-generated certs |

### Additional Protection Options (Future)

| Feature | CLI Flag | Description | Status |
|---------|----------|-------------|--------|
| IP allowlist | `--enroll-from 192.168.1.0/24` | Restrict enrollment source IPs | Planned |
| Rate limiting | N/A (automatic) | Max 5 enrollment attempts per minute per IP | Planned |
| Username binding | `--bind-cn-to-user` | Certificate CN must match USER command | Planned |
| Audit log | `--audit-log <path>` | Log all enrollment attempts with IP, username, success/failure | Planned |
| Require mTLS | `--mtls` | Require client certificates, no enrollment fallback | Planned |
| Enrollment window | `--enrollment-window 24h` | Only allow enrollment for N hours after startup | Planned |
| Single-use token | `--single-use-token` | Token becomes invalid after one successful enrollment | Planned |
| Offline cert gen | `--generate-client <NAME>` | Generate client cert bundle without enrollment | Planned |
| Private key perms | N/A (automatic) | Set 0600 permissions on private key files (Linux) | Planned |

### Dependencies (additional)

```toml
# Both broker and client
rcgen = { version = "0.14", features = ["crypto"] }  # Certificate generation
dirs = "6.0"                                          # Platform config directories
time = "0.3"                                          # Certificate validity
uuid = { version = "1", features = ["v4"] }           # Enrollment token generation
```

### CLI Changes

**Broker (current implementation):**
```
broker [--verbose|-v] [--no-tls] [--cert-dir <PATH>] [--regenerate-token] [addr]

Options:
  --verbose, -v         Enable verbose logging
  --no-tls              Disable TLS (insecure, for testing only)
  --cert-dir <PATH>     Certificate directory (default: ~/.config/rustynaut)
  --regenerate-token    Generate new enrollment token

Default address: 127.0.0.1:4242
TLS is enabled by default with auto-generated certificates.
```

**Client (current implementation):**
```
client [--verbose|-v] [--no-tls] [--enroll <TOKEN>] [--cert-dir <PATH>] <addr> [username] [room]

Options:
  --verbose, -v         Enable verbose logging
  --no-tls              Disable TLS (insecure, for testing only)
  --enroll <TOKEN>      Enroll with broker using token (auto-connects after)
  --cert-dir <PATH>     Certificate directory (default: ~/.config/rustynaut/client)

Default username: $USER or "anon"
Default room: "lobby"
TLS is enabled by default; requires enrollment first.
```

**Future CLI additions:**
```
Broker (planned):
  --mtls                    Require client certificates (no enrollment fallback)
  --enrollment-token <TOKEN> Use specific enrollment token (instead of auto-generate)
  --no-enrollment           Disable enrollment, require pre-generated certs
  --enrollment-window <DUR> Enrollment allowed for duration after startup (e.g., 24h)
  --single-use-token        Invalidate token after one successful enrollment
  --enroll-from <CIDR>      Restrict enrollment to specific IP ranges
  --generate-client <NAME>  Generate client cert bundle (offline enrollment)
  --audit-log <PATH>        Log enrollment attempts

Client (planned):
  --mtls                    Require mTLS (fail if no client cert)
  --ca-cert <PATH>          CA certificate to trust (override auto-discovered)
  --client-cert <PATH>      Client certificate PEM file (override auto-discovered)
  --client-key <PATH>       Client private key PEM file (override auto-discovered)
  --cert-dir <PATH>         Certificate storage directory
```

### Implementation Roadmap

#### Step 1: Certificate Generation Module
- [ ] Create `broker/src/tls.rs` with rcgen-based cert generation
- [ ] Implement `generate_ca()`, `generate_server_cert()`, `generate_client_cert()`
- [ ] Implement PEM save/load helpers with proper file permissions
- [ ] Add enrollment token generation (UUID v4)

#### Step 2: Broker CA Setup
- [ ] On first run with `--tls`, generate CA + server cert + token
- [ ] Load existing certs on subsequent runs
- [ ] Print enrollment token to stdout for user to share

#### Step 3: Broker TLS with Optional Client Auth
- [ ] Configure `TlsAcceptor` with `allow_unauthenticated()` for enrollment
- [ ] Track client authentication state in `PeerInfo`
- [ ] Restrict non-enrolled clients to ENROLL command only

#### Step 4: Enrollment Handler
- [ ] Parse `ENROLL <token> <username>` command
- [ ] Verify token against stored enrollment token
- [ ] Generate client keypair + certificate (not_before: now - 1 hour for clock skew)
- [ ] Send `ENROLLED <cert_b64> <key_b64> <ca_b64>` response
- [ ] Log enrollment with client IP, username, cert fingerprint
- [ ] Allow re-enrollment: overwrite existing cert for same username

#### Step 5: Client Enrollment Flow
- [ ] Check for existing client cert on startup
- [ ] If missing and `--enroll <TOKEN>` provided:
  - [ ] Connect with `--insecure` (dangerous verifier, enrollment only)
  - [ ] Send `ENROLL <token> <username>` 
  - [ ] Parse ENROLLED response, save cert + key + CA to disk
  - [ ] Log received cert fingerprint
  - [ ] Disconnect and auto-reconnect with full mTLS
- [ ] If cert exists, connect normally with mTLS

#### Step 6: Full mTLS Mode
- [ ] Add `--mtls` flag to require client certs (no enrollment fallback)
- [ ] Extract CN from client certificate for authenticated username
- [ ] Optionally bind certificate CN to USER command (reject mismatch)

### Security Considerations

| Risk | Mitigation |
|------|------------|
| Token leaked | Regenerate with `--regenerate-token`; tokens are 128-bit random |
| Rogue enrollment | Rate limit + audit log + optional IP allowlist |
| CA key compromise | Store on broker only; consider HSM for production |
| Stolen client cert | Re-enroll to overwrite; implement CRL in future |
| Man-in-middle during enroll | Enrollment uses `--insecure`; token protects against unauthorized enrollment |
| Clock skew | All certs use `not_before: now - 1 hour` to tolerate minor time differences |
| VM cloning (duplicate certs) | Re-enrollment generates new cert; old one effectively revoked |

### Fingerprint Logging

Log SHA256 fingerprints at key moments for debugging:
- **Broker startup:** Log CA cert and server cert fingerprints
- **Client enrollment:** Log received CA, client cert fingerprints  
- **Client connect:** Log server cert fingerprint (for verification)
- **Broker accept:** Log client cert fingerprint (if mTLS)

### Future Enhancements

1. **Certificate revocation list (CRL):** Broker publishes list of revoked certs
2. **Automatic renewal:** Client requests new cert before expiration
3. **Hardware key storage:** Support for PKCS#11/HSM
4. **OIDC integration:** Exchange OAuth token for client certificate

---
## Progress Tracking (Checklist)
- [x] Remove UDP from client and CLI parsing
- [x] Use `LinesCodec` on client TCP
- [x] Define and implement protocol messages (USER/JOIN/CLIP/CMD + INFO/ERR/CLIP)
- [x] Implement broker rooms and room-scoped broadcasts
- [x] Implement broker slash commands (/help, /rooms, /who, /status, /quit)
- [x] Implement broker-side echo suppression using content hash deduplication
- [x] Client-side echo suppression (recent applied clips tracking)
- [x] Manual test: full clipboard sync between 3 clients (macOS, Windows, Linux)

### FILE_OFFER Protocol (Notification Phase - Complete)
- [x] Broker parses FILE_OFFER from client: `FILE_OFFER <room> <filename_b64> <size>`
- [x] Broker relays to room with username: `FILE_OFFER <room> <username> <filename_b64> <size>`
- [x] Broker-side FILE_OFFER deduplication (prevents echo loops)
- [x] Client sends FILE_OFFER for native file copies (Finder/Explorer)
- [x] Client sends FILE_OFFER for text-detected file paths (Linux file:// URLs)
- [x] Client displays received FILE_OFFER with human-readable size
- [ ] **Next Phase: Binary Transfer**
  - [ ] FILE_ACCEPT command and handling
  - [ ] Sideband TCP connection for file data
  - [ ] Progress indication and completion notification

### Clipboard Architecture
- [x] Cross-platform clipboard access using `arboard` crate
- [x] Native file detection for Finder (macOS) via NSPasteboard/NSURL
- [x] Native file detection for Explorer (Windows) via clipboard-win
- [x] Text-based file:// URL detection for Linux file managers
- [x] Platform-specific clipboard file detection module (`clipboard_files.rs`)
- [x] Sync text clipboard content when files detected (prevents path leaking as CLIP)

#### Platform Dependencies
| Platform | Crate | Purpose |
|----------|-------|---------|
| All | `arboard` | Cross-platform text/image clipboard |
| macOS | `objc2-app-kit`, `objc2-foundation` | NSPasteboard file URL detection |
| Windows | `clipboard-win` | Explorer file list detection |
| Linux | (none - text fallback) | Parses file:// URIs from text clipboard |

### TLS Implementation Status
- [x] Add TLS dependencies to broker (tokio-rustls with ring, rcgen, etc.)
- [x] Add TLS dependencies to client
- [x] Create broker tls.rs module (CA/cert generation, TLS acceptor)
- [x] Create client tls.rs module (cert storage, enrollment handling)
- [x] Implement CLI flags: broker --no-tls, --cert-dir, --regenerate-token
- [x] Implement CLI flags: client --no-tls, --enroll, --cert-dir
- [x] Implement ENROLL command parsing and handling on broker
- [x] Implement enrollment flow on client
- [x] Generic Peer<S> struct for TCP and TLS stream abstraction
- [x] Clock skew tolerance (certificates backdated 1 hour)
- [x] Fingerprint logging for CA, server, and client certs
- [x] TLS enabled by default (--no-tls to disable)
- [x] Auto-connect after enrollment
- [x] Client /quit and /exit commands for graceful disconnect
- [x] Cross-platform certificate storage (dirs crate)
- [x] Update README with TLS documentation
- [x] Update copilot-instructions.md with TLS documentation
- [x] Manual test: full enrollment and mTLS connection flow
- [ ] Add --mtls flag to require client certificates (no enrollment fallback)
- [ ] Optional: Certificate CN binding to USER command
- [ ] Set proper file permissions (0600) on private keys
- [ ] Rate limiting for enrollment attempts
- [ ] Enrollment audit logging

### Broker Operability
- [x] Graceful shutdown via /quit, /shutdown, /exit commands
- [x] Ctrl+C signal handling with client notification
- [x] /status command showing connected clients and rooms
- [x] Tracing integration with configurable verbosity (--verbose)
- [x] Broker TUI for unified experience (match client UX style) ✅

### CI and Release
- [ ] Fix GitHub Actions to run `cargo clippy`, `cargo test`, and `cargo build` for both `broker/` and `client/`
- [ ] Stamp a version in the build process (visible in `--version` output and/or build metadata)

### Future Enhancements
- [ ] **TUI Client with Tab Completion** - Replace raw terminal with ratatui + tui-prompts. Provides better UI (no scrolling issues), built-in tab completion for commands/usernames/filenames, and visual polish. See detailed planning below.
- [ ] **File transfer resume support** - Add `FILE_RESUME <transfer_id> <offset>` protocol message to resume interrupted transfers. Track partial transfers on disk with metadata file. Useful for large files (1GB max) over unreliable connections.
- [ ] Sunset `--no-tls` flag (require TLS for all connections)
- [ ] Auto-accept option for file transfers (configurable per-user or per-room)

---

## TUI Client with Tab Completion

### Overview
Replace the raw terminal interface with a proper TUI (Terminal User Interface) using **ratatui** + **tui-prompts**. This consolidates two efforts:
1. Eliminate scrolling issues and improve terminal experience
2. Add tab completion for commands, usernames, and filenames

**Why consolidate?** Ratatui doesn't provide input handling itself, but `tui-prompts` (a ratatui extension) has **built-in autocomplete support** along with readline-style keybindings. Using a TUI library gives us both goals in one implementation.

### Key Insight
> Ratatui is just a widget/rendering library. For input with completion, we need `tui-prompts` which explicitly supports "Autocomplete" and "Autocomplete multi-select" as features.

### Library Stack

| Crate | Purpose | Version |
|-------|---------|---------|
| `ratatui` | TUI rendering framework | `0.29` |
| `tui-prompts` | Input prompts with autocomplete | `0.6` |
| `crossterm` | Cross-platform terminal events | `0.28` |

**Alternative considered:** `rustyline` + raw terminal - rejected because it doesn't solve the scrolling/UI issues.

### New Architecture

**Current (raw terminal):**
```
┌─────────────────────────────┐
│ ██████  ██   ██ ...         │  ← ASCII banner prints once
│                             │
│ INFO alice joined           │  ← Messages scroll up
│ INFO [lobby] bob offers...  │
│ > /accept                   │  ← Input at bottom
└─────────────────────────────┘
```

**New (TUI layout):**
```
┌─────────────────────────────┐
│ ██████  ██   ██ ...         │  ← Banner (top, fixed)
├─────────────────────────────┤
│ alice: Hey everyone!        │  ← Chat/log area (scrollable)
│ INFO [lobby] bob offers...  │
│ ...                         │
├─────────────────────────────┤
│ Users: alice, bob, charlie  │  ← Status bar (optional)
├─────────────────────────────┤
│ rustynaut> /accept bo       │  ← Input prompt with completion
│        bob                  │     dropdown
└─────────────────────────────┘
```

### UI Layout Components

```rust
struct App {
    // Protocol state
    connection: ConnectionState,
    current_room: String,
    username: String,
    
    // UI state
    messages: Vec<ChatMessage>,       // Chat history (scrollable)
    users_in_room: Vec<String>,       // For sidebar/status
    pending_offers: Vec<FileOffer>,   // Active file offers
    
    // Input with completion
    input_state: TextState<'static>,  // From tui-prompts
    completer: RustynautCompleter,    // Custom completion logic
    
    // Layout refs
    scroll_offset: usize,
    show_sidebar: bool,
}
```

### Layout Areas

```rust
// Proposed layout
┌───────────────────────────────────┐
│ Banner (2 lines, fixed)           │ area[0]
├───────────────────────────────────┤
│                                   │
│  Chat / Log Area                  │ area[1] (scrollable)
│  (Messages + file transfer status)│
│                                   │
├───────────────────────────────────┤
│ Status Bar (optional): Room | N users │ area[2]
├───────────────────────────────────┤
│ Input: rustynaut> _               │ area[3] (with completion dropdown)
└───────────────────────────────────┘
```

### Tab Completion Integration

**tui-prompts features we use:**
- Text input with cursor positioning
- Readline/emacs keybindings (`C-a`, `C-e`, `C-k`, etc.)
- **Autocomplete** - our custom completion provider
- History support
- Bracketed paste

**Completion Context (same data as before):**
```rust
struct RustynautCompleter {
    known_rooms: Vec<String>,
    users_in_room: Vec<String>,
    pending_filenames: HashMap<String, Vec<String>>,
}

impl tui_prompts::completion::Completer for RustynautCompleter {
    fn complete(&self, line: &str, pos: usize) -> Vec<Completion> {
        // Parse: "/accept bo" → complete "bo" against usernames
        // Parse: "/accept bob re" → complete "re" against bob's files
        // Parse: "/join " → complete room names
    }
}
```

**Completion targets (same as before):**

| Command | Completion Target | Data Source |
|---------|------------------|-------------|
| `/join <room>` | Room names | Known rooms from `/rooms` |
| `/accept <user>` | Usernames | Users in current room |
| `/accept <user> <file>` | Filenames | Pending offers from that user |
| `/cancel <id>` | Transfer IDs | Active transfers |

### Protocol Integration

**No broker changes required** - client tracks state from existing messages:

- **Users**: Track from `INFO <user> joined/left` messages
- **Rooms**: Track from successful `JOIN` responses
- **File offers**: Track from `FILE_OFFER` messages
- **Transfers**: Track from `FILE_START`/`FILE_INCOMING` responses

### Implementation Phases

**Phase 1: Basic TUI Setup**
- [ ] Add dependencies: `ratatui`, `tui-prompts`, `crossterm`
- [ ] Create `App` struct with layout state
- [ ] Implement basic render loop (banner + chat area + input)
- [ ] Replace `FramedRead(stdin)` with TUI event loop
- [ ] Connect protocol messages to UI (display in chat area)

**Phase 2: Message Display**
- [ ] Format protocol messages nicely (CLIP, SAY, FILE_OFFER, etc.)
- [ ] Scrollable message history (PgUp/PgDn)
- [ ] Color-coded message types (green=info, red=error, etc.)
- [ ] Timestamp display (optional)

**Phase 3: Input with History**
- [ ] Integrate `TextPrompt` from tui-prompts
- [ ] Command history (Up/Down arrows)
- [ ] Show current room in prompt: `[lobby] rustynaut> `

**Phase 4: Tab Completion**
- [ ] Implement `Completer` trait
- [ ] Command completion (`/help`, `/join`, etc.)
- [ ] Username completion for `/accept`
- [ ] Room name completion for `/join`
- [ ] Filename completion for `/accept <user>`

**Phase 5: Polish**
- [ ] Sidebar showing users in room (toggle with key)
- [ ] File transfer progress bars
- [ ] Configuration file for TUI preferences

### Dependencies

```toml
[dependencies]
ratatui = "0.29"
tui-prompts = "0.6"
crossterm = "0.28"
# existing deps remain...
```

### Code Structure Changes

**New file: `client/src/tui.rs`**
```rust
use ratatui::{
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, List, Paragraph},
    Frame,
};
use tui_prompts::{Prompt, TextPrompt, TextState};

pub struct App {
    messages: Vec<Message>,
    input: TextState<'static>,
    completer: RustynautCompleter,
}

impl App {
    pub fn draw(&mut self, frame: &mut Frame) {
        // Layout: banner | chat | status | input
    }
    
    pub fn handle_event(&mut self, event: Event) -> Option<String> {
        // Returns Some(line) when user presses Enter
    }
}
```

**Modified: `client/src/main.rs`**
```rust
// Old: stdin stream merged into protocol loop
// New: TUI event loop drives everything
#[tokio::main]
async fn main() -> Result<()> {
    let mut terminal = ratatui::init()?;
    let mut app = App::new(args);
    
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        
        // Handle terminal events (non-blocking with timeout)
        if event::poll(Duration::from_millis(100))? {
            if let Some(line) = app.handle_event(event::read()?) {
                // Send to broker
                tx.send(line).await?;
            }
        }
        
        // Handle protocol messages (from async channel)
        while let Ok(msg) = rx.try_recv() {
            app.handle_protocol_message(msg);
        }
    }
}
```

### Benefits of TUI Approach

| Aspect | Raw Terminal | TUI |
|--------|-------------|-----|
| **Scrolling** | Terminal scrollback (messy) | Controlled viewport |
| **Layout** | Single stream | Multiple areas (banner, chat, input) |
| **Completion** | Requires readline lib | Built into tui-prompts |
| **Visual polish** | ASCII only | Colors, borders, progress bars |
| **File transfers** | Text messages | Progress bars + status |
| **Cross-platform** | Varies | Crossterm abstracts differences |

### Edge Cases & Mitigations

| Issue | Mitigation |
|-------|------------|
| Async + TUI mixing | TUI runs in main thread, protocol in spawned task |
| Terminal resize | Handle `Resize` event, redraw layout |
| Raw mode cleanup | Use `ratatui::restore()` in panic handler |
| Windows compatibility | Crossterm handles Windows console |
| Completion blocking | Completion is synchronous (fast), UI stays responsive |
| TLS logs in TUI | Route TLS handshake/SNI logs into TUI message stream when verbose; avoid stderr writes |

### Testing Strategy

- [ ] Manual: Basic TUI renders correctly
- [ ] Manual: All slash commands work
- [ ] Manual: Tab completion for commands
- [ ] Manual: Tab completion for usernames
- [ ] Manual: History (Up/Down) works
- [ ] Cross-platform: Linux, macOS, Windows
- [ ] Resize terminal, layout adjusts

### TUI Text Selection & Copy

**Current Implementation:**
- Click on any message in the TUI to select it (highlighted with accent color background)
- Press 'y' key to copy the currently selected message to clipboard
- Simple and intuitive for copying entire messages

**Future Enhancements:**
- [x] **Exit copy mode**: Add a way to exit/escape from message selection mode (e.g., Esc key or clicking elsewhere) ✅ Press ESC to deselect currently selected message
- [ ] **Click and drag text selection**: Enable mouse click-and-drag to select arbitrary text within messages
  - Should work anywhere in the message area
  - Allow partial text selection (not just whole messages)
  - Copy selected text to clipboard on release or with copy key
- [ ] **Command line text selection**: Allow click-and-drag selection in the input/command line area
  - Useful for editing long commands
  - Select and copy portions of typed text
- [ ] **Visual feedback**: Show selection highlight during drag operation
  - Invert colors or use different background for selected text range
  - Update selection in real-time as user drags

**Implementation Notes:**
- Requires tracking mouse down position and drag coordinates
- Need to map screen coordinates to text positions within messages
- Consider using terminal's native selection where possible (e.g.,按住 Shift for terminal selection)
- Balance between TUI-managed selection and terminal-native selection

---

## Workspace Refactoring: Broker, Client, and Common Crates

### Overview
Currently, the codebase has significant duplication between the `broker/` and `client/` directories. Both crates implement similar protocol parsing, message types, and utility functions. This refactoring will create a proper Cargo workspace with three crates:

1. **`rustynaut-common`** - Shared protocol definitions, parsing logic, and utilities
2. **`rustynaut-broker`** - The server/broker component
3. **`rustynaut-client`** - The client component

### Benefits

| Benefit | Description |
|---------|-------------|
| **DRY Principle** | Eliminate code duplication between broker and client |
| **Type Safety** | Shared types ensure protocol consistency |
| **Maintainability** | Changes to protocol only need to be made in one place |
| **Testing** | Common crate can be tested independently |
| **Documentation** | Clear separation of concerns |
| **Future Extensibility** | Easier to add new components (e.g., GUI client, web interface) |

### Current Duplication Analysis

#### Protocol Messages (High Priority)
Both broker and client implement parsing for:
- `CLIP <room> <b64> [id]` - Clipboard updates
- `FILE_OFFER <room> <filename_b64> <size>` - File offers
- `FILE_ACCEPT <room> <user> <filename_b64>` - File acceptance
- `FILE_START <transfer_id> <filename_b64> <count>` - Transfer start
- `FILE_INCOMING <transfer_id> <filename_b64> <size>` - Incoming file notification
- `FILE_CHUNK <transfer_id> <offset> <chunk_b64>` - File chunk
- `FILE_END <transfer_id> <sha256>` - Transfer end
- `FILE_DONE <transfer_id> <sha256>` - Transfer completion
- `FILE_SENT <transfer_id> <count>` - Sent confirmation
- `FILE_CANCELLED <transfer_id> <reason>` - Transfer cancellation
- `SAY <user> <text>` - Chat messages
- `CMD <command>` - Slash commands
- `USER <name>` - Username registration
- `JOIN <room>` - Room joining
- `INFO <text>` / `ERR <text>` - Status messages

#### Shared Types and Structures
- Message enums (Info, Error, Chat, Clip, FileOffer, etc.)
- Protocol constants (MAX_LINE_LENGTH, MAX_FILE_SIZE, etc.)
- Transfer ID types
- Room name validation
- Base64 encoding/decoding helpers
- Timestamp formatting

#### TLS/Certificate Code (Partial)
- Certificate generation (rcgen usage)
- PEM encoding/decoding
- File path handling for certs
- Some certificate validation logic

### Proposed Workspace Structure

```
rustynaut/
├── Cargo.toml                    # Workspace root
├── Cargo.lock                    # Shared lockfile
├── README.md
├── AGENTS.md
├── aidocs/
│   └── planning.md
├── common/                       # NEW: Shared library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── protocol.rs           # Protocol messages & parsing
│       ├── types.rs              # Shared types (Message enum, etc.)
│       ├── constants.rs          # Protocol constants
│       ├── parsing.rs            # Message parsing utilities
│       ├── tls/                  # Shared TLS utilities
│       │   ├── mod.rs
│       │   ├── certs.rs          # Certificate generation
│       │   └── paths.rs          # Certificate path utilities
│       └── utils.rs              # General utilities (base64, etc.)
├── broker/                       # EXISTING: Refactored to use common
│   ├── Cargo.toml                # Add dependency on common
│   └── src/
│       ├── main.rs               # Slimmed down
│       ├── broker.rs             # Broker-specific logic
│       ├── rooms.rs              # Room management
│       └── peers.rs              # Peer/connection management
└── client/                       # EXISTING: Refactored to use common
    ├── Cargo.toml                # Add dependency on common
    └── src/
        ├── main.rs               # Slimmed down
        ├── tui.rs                # TUI implementation
        ├── clipboard.rs          # Clipboard handling
        └── file_transfer.rs      # File transfer UI
```

### Implementation Roadmap

#### Phase 1: Setup Workspace Structure
**Goal:** Create the workspace foundation without breaking existing code

- [x] **Create workspace root `Cargo.toml`**
  ```toml
  [workspace]
  members = ["common", "broker", "client"]
  resolver = "2"
  
  [workspace.package]
  version = "0.1.1"
  edition = "2021"
  authors = ["..."]
  license = "MIT OR Apache-2.0"
  repository = "https://github.com/aszazeroth/rustynaut"
  ```

- [x] **Create `common/` directory structure**
  - Create `common/Cargo.toml`
  - Create `common/src/lib.rs` with module declarations
  - Set up basic crate structure

- [x] **Move shared dependencies to workspace level**
  - Identify common dependencies (tokio, tracing, base64, etc.)
  - Define in workspace `Cargo.toml` `[workspace.dependencies]`
  - Reference in individual crates with `dep = { workspace = true }`

- [x] **Verify existing crates still build**
  - Ensure broker and client compile independently
  - Run tests on both
  - This is a checkpoint - everything should work as before

#### Phase 2: Extract Protocol Layer
**Goal:** Move protocol messages and parsing to common crate

- [x] **Create `common/src/protocol.rs`**
  - Define all protocol message types as enums
  - Implement `Display` for serialization
  - Implement `FromStr` or parsing functions for deserialization
  - Add comprehensive tests

- [x] **Create `common/src/types.rs`**
  - Move shared `Message` enum
  - Move transfer ID types
  - Move room-related types
  - Ensure serde compatibility if needed

- [x] **Create `common/src/constants.rs`**
  - Move all protocol constants
  - MAX_LINE_LENGTH
  - MAX_FILE_SIZE
  - FILE_CHUNK_SIZE
  - MAX_RECENT_CLIPS_PER_ROOM
  - etc.

- [x] **Create `common/src/parsing.rs`**
  - Move all parse_* functions
  - parse_clip_fields
  - parse_file_offer_fields
  - parse_file_accept_fields
  - parse_file_start_fields
  - parse_file_incoming_fields
  - parse_file_chunk_fields
  - parse_file_end_fields
  - parse_file_done_fields
  - parse_file_sent_fields
  - parse_file_cancelled_fields
  - parse_say_fields
  - parse_command_response
  - Add unit tests for each parser

- [x] **Update broker to use common::protocol**
  - Replace local protocol functions with common exports
  - Update imports
  - Remove duplicate code
  - Verify broker still builds and works

- [x] **Update client to use common::protocol**
  - Replace local protocol functions with common exports
  - Update imports
  - Remove duplicate code
  - Verify client still builds and works

#### Phase 3: Extract TLS Utilities
**Goal:** Move shared TLS code to common crate

- [x] **Analyze TLS code duplication**
  - Review broker/src/tls.rs
  - Review client/src/tls.rs
  - Identify truly shared vs. crate-specific code

- [x] **Create `common/src/tls/mod.rs`**
  - Define shared TLS types and traits
  - Export common functionality

- [x] **Create `common/src/tls/certs.rs`**
  - Move certificate generation logic (rcgen)
  - Certificate loading/saving
  - PEM encoding/decoding helpers
  - Keep broker-specific and client-specific logic minimal

- [x] **Create `common/src/tls/paths.rs`**
  - Certificate directory utilities
  - Path resolution
  - Platform-specific path handling

- [x] **Refactor broker TLS**
  - Use common::tls for shared functionality
  - Keep broker-specific code (acceptor config, etc.)

- [x] **Refactor client TLS**
  - Use common::tls for shared functionality
  - Keep client-specific code (connector config, enrollment, etc.)

#### Phase 4: Extract Utility Functions
**Goal:** Move general utilities to common crate

- [x] **Create `common/src/utils.rs`**
  - Base64 encoding/decoding wrappers
  - Formatting utilities (format_size, etc.)
  - Timestamp formatting
  - SHA256 helpers
  - Any other shared utilities

- [x] **Move base64 helpers**
  - encode_clipboard_content
  - decode_clipboard_content
  - encode_filename
  - decode_filename

- [x] **Move formatting helpers**
  - format_size (human-readable file sizes)
  - format_timestamp

- [x] **Update broker and client**
  - Replace local utility calls with common::utils
  - Remove duplicate implementations

#### Phase 5: Cleanup and Optimization
**Goal:** Remove all duplicate code, optimize imports

- [x] **Audit for remaining duplication**
  - Search for similar function implementations
  - Check for duplicate types
  - Review error handling patterns

- [x] **Standardize error types**
  - Consider creating common error types
  - Use `thiserror` or similar for consistent error handling

- [x] **Optimize imports**
  - Use `pub use` in common/lib.rs for clean re-exports
  - Update broker and client to use clean import paths

- [x] **Add documentation**
  - Document all public APIs in common crate
  - Add examples in doc comments
  - Update top-level README with workspace structure
  - Add per-crate README files:
    - broker/README.md (server usage, flags, examples)
    - client/README.md (client usage, TUI controls, enrollment)
    - common/README.md (shared API overview)

- [x] **Add tests to common crate**
  - Protocol parsing tests
  - Type conversion tests
  - Utility function tests
  - Aim for high coverage on common code

#### Phase 6: CI/CD Updates
**Goal:** Ensure CI works with new workspace structure

- [x] **Update GitHub Actions workflow**
  - Change from building individual crates to `cargo build --workspace`
  - Update test commands to `cargo test --workspace`
  - Update clippy to `cargo clippy --workspace`

- [x] **Add workspace-level checks**
  - Ensure all crates compile
  - Run tests across all crates
  - Check for unused dependencies

- [x] **Version management**
  - Decide on versioning strategy (single version vs. independent)
  - Update workspace Cargo.toml with version

### Common Crate Public API (Draft)

```rust
// common/src/lib.rs
pub mod protocol;
pub mod types;
pub mod constants;
pub mod parsing;
pub mod tls;
pub mod utils;

// Re-export commonly used items
pub use types::Message;
pub use protocol::ProtocolMessage;
pub use constants::*;
```

```rust
// Example usage in broker/client
use rustynaut_common::{
    Message, 
    protocol::ProtocolMessage,
    parsing::{parse_clip_fields, parse_file_offer_fields},
    constants::{MAX_LINE_LENGTH, FILE_CHUNK_SIZE},
    utils::format_size,
};
```

### Dependencies to Share

**Move to workspace level:**
```toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
tracing = "0.1"
tracing-subscriber = "0.3"
base64 = "0.22"
sha2 = "0.10"
ring = "0.17"
serde = { version = "1", features = ["derive"] }
thiserror = "1"

# TLS (shared)
tokio-rustls = "0.26"
rustls = { version = "0.23", default-features = false, features = ["std", "tls12"] }
rustls-pemfile = "2"
rcgen = { version = "0.14", features = ["crypto"] }

# Broker-specific
futures = "0.3"

# Client-specific
arboard = "3"
ratatui = "0.29"
crossterm = "0.28"
```

### Testing Strategy

**Common crate testing:**
- Unit tests for all parsing functions
- Property-based tests for protocol round-trips
- Test fixtures for sample messages

**Integration testing:**
- Test broker and client with common crate
- Ensure protocol compatibility
- End-to-end tests for file transfers

**Migration testing:**
- Before/after behavior comparison
- Ensure no regressions in existing functionality

### Migration Checklist

- [x] Phase 1: Workspace setup complete ✅
- [x] Phase 2: Protocol extraction complete ✅
- [x] Phase 3: TLS extraction complete ✅
- [x] Phase 4: Utilities extraction complete ✅
- [x] Phase 5: Cleanup complete ✅
- [x] Phase 6: CI/CD updated ✅
- [x] All existing tests pass ✅
- [x] Documentation updated ✅
- [x] README reflects new structure ✅
- [x] No code duplication remains between broker and client ✅
- [ ] Common crate has >80% test coverage (future goal)

### Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Breaking changes | Do refactoring incrementally, test at each phase |
| Merge conflicts | Complete refactoring in dedicated branch, minimize other changes |
| Increased complexity | Clear documentation, examples in common crate |
| Dependency bloat | Careful dependency management, feature flags |
| Performance regression | Benchmark before/after, common crate should be thin |

### Future Considerations

After workspace refactoring is complete, consider:

- **Publishing common crate to crates.io** - If others want to build Rustynaut clients
- **Additional client implementations** - GUI client, web client, mobile client
- **Plugin system** - Use common crate as foundation for broker plugins
- **Protocol versioning** - Common crate can handle multiple protocol versions

---

## Feature Enhancement Backlog

Ready-to-pick tasks organized by category. Select one and move it to "In Progress".

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
- [ ] Click-and-drag text selection in messages
- [ ] Click-and-drag text selection in input/command line
- [ ] Visual selection highlight during drag operations
- [ ] File transfer progress bars
- [ ] Sidebar showing users in room (toggle with key)
- [ ] Configuration file for TUI preferences
- [ ] Command history persistence across restarts

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
