# Planning: Broker-as-Clipboard Hub (Rooms)

## Current Intent (Agreed)
- Broker runs on a host; clients run in VMs.
- Clipboard sync is the MVP; chat is incidental.
- “One shared clipboard per room” (pub/sub by room).
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
  - Rooms: map `room -> set(client_id)` plus “last clipboard” metadata if needed.
- Clipboard broadcast scope: only within the sender’s room.
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
- Keep a stable “client id” concept server-side so auth tokens can bind to identities later.
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
| **Clock skew tolerance** | Backdate `not_before` by 1 hour to handle minor time differences |

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
