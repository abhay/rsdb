# API

The aggregate owns the public API. The receiver diagnostics API stays local to
the receiver node.

## Aggregate HTTP Endpoints

```text
GET  /                 Browser UI
GET  /status.json      Aggregate health, receiver summaries, ingest counters
GET  /aircraft.json    Receiver-scoped aggregate aircraft snapshot
GET  /bootstrap.json   Current snapshot plus recent feed messages
GET  /receivers.json   Receiver summaries
GET  /schema.json      Machine-readable endpoint and field contract
GET  /ws               WebSocket stream of verified FeedMessage values
POST /submit           SignedSubmission ingest endpoint
```

`POST /submit` accepts 1 JSON `SignedSubmission`. The aggregate checks the
receiver public key allowlist, verifies the Ed25519 signature, checks the signed
`submission_id`, dedupes by that ID, and returns `202` for accepted or duplicate
submissions.

## Receiver Diagnostics Endpoints

When `rsdb-usb serve` is running locally:

```text
GET /status.json       Receiver/radio health
GET /aircraft.json     Current receiver aircraft snapshot
GET /bootstrap.json    Current snapshot plus recent feed messages
GET /schema.json       Machine-readable receiver API contract
GET /history.ndjson    Recent persisted feed messages, when persistence is enabled
GET /ws                Local receiver FeedMessage stream
```

The Pi setup binds this service to `127.0.0.1:8080` by default. The aggregate
serves the UI and public API.

## Feed Messages

Feed messages are versioned JSON with `schema_version: 1` and a protocol field.
Current message types:

```text
snapshot
aircraft
stale_aircraft
heartbeat
```

Every message may include receiver identity. Receiver handles are described in
[CONFIGURATION.md](CONFIGURATION.md).

## Bootstrap Then Stream

Browser and API clients should:

1. Fetch `/bootstrap.json`.
2. Render the current snapshot and recent feed messages.
3. Connect to `/ws`.
4. Apply live `FeedMessage` updates.

That gives clients enough recent history to draw trails before live updates
arrive.

## CLI Replay Formats

Feed replay uses newline-delimited `FeedMessage` JSON:

```sh
./target/release/rsdb-usb replay /tmp/rsdb-feed.ndjson
```

Frame replay uses newline-delimited decoded Mode S `FrameRecord` JSON:

```sh
./target/release/rsdb-usb record-frames 30 /tmp/rsdb-frames.ndjson
./target/release/rsdb-usb replay-frames /tmp/rsdb-frames.ndjson
```

Frame records include:

```text
schema_version
protocol
now_ms
sample_index
raw
downlink_format
bit_len
crc_valid
```
