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

### 5) Observability + operability
- Broker uses `tracing` (already present). Add spans around connection lifecycle + room changes.
- Keep the ASCII banner printed first; push operational logs to `tracing`.

## Security/TLS/Auth Later (Design Hooks)
- Keep a stable “client id” concept server-side so auth tokens can bind to identities later.
- Keep protocol as line-based so it can run over TLS without changes.
- Plan for an `AUTH <token>` message in the future (no enforcement in MVP).
---

## Large Clipboard / File Transfer (Sideband Channel)

### Problem Statement
- Current `CLIP` protocol uses base64 over line-framed TCP (~8KB default limit)
- Real-world use cases: screenshots (PNG, 100KB–5MB), config files, logs
- Base64 adds 33% overhead; large payloads block the message stream

### Chosen Approach: Broker-Mediated Sideband Transfer

The broker acts as a relay for file transfers (no P2P), avoiding NAT/firewall issues since clients already have a connection to the broker.

### Wire Protocol Extension

**Phase 1: Offer & Accept**
```
Client → Broker:  FILE_OFFER <room> <filename> <size_bytes> <sha256>
Broker → Room:    FILE_AVAIL <from_user> <transfer_id> <filename> <size_bytes> <sha256>
Client → Broker:  FILE_ACCEPT <transfer_id>
Broker → Sender:  FILE_START <transfer_id> <port>
```

**Phase 2: Binary Transfer (separate TCP connection)**
- Sender connects to `broker:<port>` and streams raw bytes
- Broker relays to all acceptors in real-time (or buffers if acceptor is slow)
- Broker closes connection when `size_bytes` received

**Phase 3: Completion**
```
Broker → Acceptor: FILE_DONE <transfer_id> <sha256_verified: true|false>
Broker → Sender:   FILE_SENT <transfer_id> <acceptor_count>
```

### Implementation Roadmap

#### Step 1: Protocol Messages (No Transfer Yet)
- [ ] Add `FILE_OFFER` / `FILE_AVAIL` / `FILE_ACCEPT` parsing to broker
- [ ] Add `FILE_CANCEL <transfer_id>` for cleanup
- [ ] Track pending transfers in broker state: `HashMap<TransferId, FileTransfer>`
- [ ] Broadcast `FILE_AVAIL` to room (excluding sender)

#### Step 2: Sideband Listener on Broker
- [ ] On `FILE_ACCEPT`, broker opens ephemeral TCP port (or reuses a pool)
- [ ] Send `FILE_START <transfer_id> <port>` to sender
- [ ] Sender connects and streams; broker validates size + computes SHA256

#### Step 3: Relay to Acceptors
- [ ] Broker pushes bytes to each acceptor's sideband connection
- [ ] Handle backpressure (slow acceptor shouldn't block others)
- [ ] On completion, send `FILE_DONE` with checksum verification result

#### Step 4: Client Integration
- [ ] Detect large clipboard (>64KB threshold) → trigger `FILE_OFFER` instead of `CLIP`
- [ ] Auto-accept files from same room (configurable)
- [ ] Save received files to temp dir, optionally copy to clipboard as file reference
- [ ] Show progress bar in verbose mode

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

### Additional Protection Options

1. **IP allowlist:** `--enroll-from 192.168.1.0/24` restricts enrollment source IPs
2. **Rate limiting:** Max 5 enrollment attempts per minute per IP
3. **Username binding:** Certificate CN must match USER command username
4. **Audit log:** Log all enrollment attempts with IP, username, success/failure

### Dependencies (additional)

```toml
# Both broker and client
rcgen = { version = "0.14", features = ["crypto"] }  # Certificate generation
dirs = "6.0"                                          # Platform config directories
time = "0.3"                                          # Certificate validity
uuid = { version = "1", features = ["v4"] }           # Enrollment token generation
```

### CLI Changes

**Broker (additional flags):**
```
broker [existing flags...] [mTLS flags...]

mTLS Options:
  --tls                     Enable TLS (auto-generates CA/certs on first run)
  --mtls                    Require client certificates (implies --tls)
  --cert-dir <PATH>         Certificate directory (default: ~/.config/rustynaut)
  --enrollment-token <TOKEN> Use specific enrollment token (default: auto-generate)
  --regenerate-token        Generate new enrollment token
  --no-enrollment           Disable enrollment, require pre-generated certs
  --enrollment-window <DUR> Enrollment allowed for duration after startup (e.g., 24h)
  --generate-client <NAME>  Generate client cert bundle (offline enrollment)
```

**Client (additional flags):**
```
client [existing flags...] [mTLS flags...]

mTLS Options:
  --tls                     Enable TLS connection
  --mtls                    Use client certificate (auto-enroll if missing)
  --ca-cert <PATH>          CA certificate to trust
  --client-cert <PATH>      Client certificate PEM file
  --client-key <PATH>       Client private key PEM file
  --enroll <TOKEN>          Enrollment token for first-time setup
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
- [ ] Implement broker rooms and room-scoped broadcasts
- [~] Implement broker slash commands (/help, /rooms, /who done; /join via JOIN line)
- [ ] Implement client echo suppression using `<id>`
- [ ] Manual test: start broker, connect 1 client, run `/rooms` and `/who`
- [ ] Manual test: 2 clients in same room sync; different rooms do not
