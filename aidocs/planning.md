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
## Progress Tracking (Checklist)
- [x] Remove UDP from client and CLI parsing
- [x] Use `LinesCodec` on client TCP
- [x] Define and implement protocol messages (USER/JOIN/CLIP/CMD + INFO/ERR/CLIP)
- [ ] Implement broker rooms and room-scoped broadcasts
- [~] Implement broker slash commands (/help, /rooms, /who done; /join via JOIN line)
- [ ] Implement client echo suppression using `<id>`
- [ ] Manual test: start broker, connect 1 client, run `/rooms` and `/who`
- [ ] Manual test: 2 clients in same room sync; different rooms do not
