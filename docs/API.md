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

`POST /submit` accepts 1 JSON `SignedSubmission`. Its `payload` may be a
`FeedMessage` or a `FrameRecordBatch`. The aggregate checks the receiver public
key allowlist, verifies the Ed25519 signature, validates the payload, checks the
signed `submission_id`, dedupes by that ID, and returns `202` for accepted or
duplicate submissions.

Signed frame-record batches stay JSON on the wire. They are larger than a
binary stream, but they keep submission logs inspectable, curl-friendly, and easy
to replay. Binary receiver formats should be adapters that produce signed JSON
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
frame_sequence
stream_start_ms
rx_elapsed_ns
rx_timestamp_uncertainty_ns_estimate
receiver
receiver_site
center_frequency_hz
sample_rate_hz
gain_mode
gain_tenth_db
bias_t
device_index
tuner_name
stream_id
chunk_sequence
chunk_sample_index
dropped_samples_before
clipped_sample_ratio
dc_i_offset
dc_q_offset
signal.signal_power
signal.noise_power
signal.signal_dbfs_estimate
signal.snr_db_estimate
signal.chunk_noise_power
signal.beast_signal_level
signal.preamble_high_avg
signal.preamble_low_avg
signal.preamble_delta
signal.bit_margin_min
signal.bit_margin_mean
icao
adsb_type_code
raw
downlink_format
bit_len
crc_valid
```

Signal fields are relative estimates from the RTL-SDR sample stream, not
calibrated RF power measurements.

Frame records are validated before replay. The validator rejects unsupported
schema or protocol values, malformed raw frames, frame metadata that disagrees
with `raw`, impossible radio/timing values, invalid signal fields, and sequence
regressions within a stream.

## Signed Frame Batches

Raw-frame submissions wrap multiple decoded records under one receiver:

```json
{
  "schema_version": 1,
  "submission_id": "...",
  "receiver_id": "ed25519-...",
  "algorithm": "ed25519",
  "submitted_at_ms": 1780891560000,
  "payload": {
    "schema_version": 1,
    "protocol": "adsb1090",
    "receiver": {
      "id": "ed25519-..."
    },
    "records": [
      {
        "schema_version": 2,
        "protocol": "adsb1090",
        "now_ms": 1780891560000,
        "sample_index": 0,
        "center_frequency_hz": 1090000000,
        "sample_rate_hz": 2000000,
        "raw": "8DA062EF9910B19A38040ACE2B14",
        "downlink_format": 17,
        "bit_len": 112,
        "crc_valid": true
      }
    ]
  },
  "signature": "..."
}
```

The aggregate validates the batch schema, receiver ID, protocol, record
metadata, and sequence fields before appending the accepted submission to disk.
Decoded aircraft updates from the batch are broadcast as normal `FeedMessage`
values on `/ws`.
