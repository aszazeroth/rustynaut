![rustynaut logo](logo.png)

# Rustynaut

Clipboard-sync broker + CLI client (Tokio).

## What it is

Rustynaut is a small TCP, line-framed broker that relays clipboard updates between clients.

- The **broker** runs on a host.
- The **clients** run on machines/VMs and publish/subscribe by **room**.

Rooms are the “pub/sub” unit: **one shared clipboard per room**.

## Build & Run

Start the broker:

```bash
cd broker
cargo run --release -- 127.0.0.1:4242
```

Enable verbose broker logs:

```bash
cd broker
cargo run --release -- --verbose 127.0.0.1:4242
```

Listen on all interfaces (useful for VMs/LAN clients):

```bash
cd broker
cargo run --release -- 0.0.0.0:4242
```

You can also override logging with `RUST_LOG`, e.g.:

```bash
cd broker
RUST_LOG="chat=trace" cargo run --release -- 127.0.0.1:4242
```

Start a client:

```bash
cd client
cargo run --release -- [--verbose|-v] 127.0.0.1:4242 [username] [room]
```

Examples:

```bash
# in terminal 1
cd broker && cargo run --release -- 127.0.0.1:4242

# in terminal 2
cd client && cargo run --release -- 127.0.0.1:4242 alice lobby

# in terminal 3
cd client && cargo run --release -- 127.0.0.1:4242 bob lobby
```

Now changing the system clipboard on one client should replicate to the other client in the same room.

## Slash Commands (from the client)

Type commands into the client stdin:

- `/help`
- `/rooms`
- `/who`

## Debugging with netcat

To sanity-check the broker without running the client UI:

```bash
printf "USER alice\nJOIN lobby\nCMD /rooms\nCMD /who\n" | nc -w 1 127.0.0.1 4242
```

## Wire Protocol (current)

Transport is line-framed TCP (`LinesCodec`).

Client → Broker:

- `USER <name>`
- `JOIN <room>`
- `CLIP <room> <b64>`
- `CMD /...`
- `SAY <text>`

Broker → Client:

- `INFO <text>`
- `ERR <text>`
- `CLIP <room> <b64> <id>`
- `SAY <user> <text>`

*HEAVILY* inspired by ol' coding adventures and the Tokio chat server/client.

## Credits

The logo was generated using the AI generator in Adobe Illustrator, then modified to better fit the original idea.