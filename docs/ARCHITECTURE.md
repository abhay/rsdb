# Architecture

RSDB splits hardware collection from public serving.

`rsdb-usb` is the hardware-adjacent receiver process. It opens the RTL-SDR,
runs 1 configured radio job, maintains local receiver diagnostics, signs raw
frame batches and heartbeat messages when a receiver seed is configured, and
retries submissions through a durable local outbox.

`rsdb-aggregate` is the public data product. It verifies signed submissions,
dedupes by submission ID, owns aggregate state through a single writer, persists
hot state, and serves the browser UI plus HTTP/WebSocket APIs.

## Component Diagram

```mermaid
flowchart LR
  USB[RTL-SDR USB] --> Receiver[rsdb-usb<br/>receiver service]
  Receiver --> Decode[Protocol decoder<br/>ADS-B / Mode S today]
  Decode --> LocalAPI[Local diagnostics<br/>status, bootstrap, history, ws]
  Decode --> Submit[Submission worker<br/>sign, queue, retry]
  Submit --> Outbox[Durable outbox<br/>submission-outbox.ndjson]
  Outbox --> LocalAgg[Local aggregate<br/>rsdb-aggregate]
  Outbox -. optional HTTPS .-> RemoteAgg[Remote aggregate<br/>Fly.io or other host]
  LocalAgg --> Writer[Single aggregate writer<br/>verify, dedupe, mutate state]
  RemoteAgg --> RemoteWriter[Single aggregate writer]
  Writer --> HotStore[Hot persistence<br/>snapshot.json<br/>submissions.ndjson]
  RemoteWriter --> RemoteHotStore[Hot persistence]
  Writer --> PublicAPI[UI/API/WebSocket]
  RemoteWriter --> RemoteAPI[UI/API/WebSocket]
  PublicAPI --> Browser[Browser or API client]
  RemoteAPI -. optional .-> Browser
```

```text
RTL-SDR USB
    |
    v
rsdb-usb
    role: receiver diagnostics and radio decode
    bind: usually 127.0.0.1:8080
    radio: 1 configured protocol per RTL-SDR dongle
    |
    +-- local diagnostics API
    |
    +-- signed FrameRecordBatch aircraft data
    +-- signed heartbeat FeedMessage health data
           |
           v
       submission worker
           sign -> append durable outbox -> retry POST
           |
           +-- http://127.0.0.1:8090/submit
           +-- https://remote-aggregate.example.com/submit

rsdb-aggregate
    role: public UI/API/WebSocket
    bind: usually 0.0.0.0:8090 on Pi, 0.0.0.0:$PORT on Fly.io
    |
    +-- verify allowlist and signature
    +-- dedupe submission_id
    +-- maintain receiver-scoped aircraft state
    +-- append accepted submissions to hot journal
    +-- periodically checkpoint and compact
    |
    +-- public HTTP and WebSocket API
```

## Runtime Call Flow

```mermaid
sequenceDiagram
  participant USB as RTL-SDR USB
  participant R as Receiver service
  participant Q as Submit outbox
  participant A as Aggregate service
  participant P as Hot persistence
  participant U as Browser UI

  USB->>R: IQ sample stream
  R->>R: demodulate configured protocol
  R->>R: update local receiver state
  R->>Q: signed FrameRecordBatch aircraft data
  R->>Q: signed heartbeat FeedMessage health data
  Q->>A: POST /submit
  A->>A: verify allowlist and signature
  A->>A: enqueue to single aggregate writer
  A->>A: dedupe submission_id
  A->>P: append accepted SignedSubmission
  A->>A: update AggregateStore
  A->>P: periodic sync, checkpoint, and compaction
  A-->>Q: 202 accepted or duplicate

  U->>A: GET /
  A-->>U: HTML/CSS/JS
  U->>A: GET /bootstrap.json
  A-->>U: current snapshot and recent feed messages
  U->>A: WebSocket /ws
  A-->>U: verified aggregate FeedMessage stream
```

```text
1. USB sends I/Q samples to rsdb-usb.
2. The receiver decodes the configured protocol into protocol-tagged feed updates and frame records.
3. The receiver updates its local diagnostic state.
4. The submission worker signs frame batches for aircraft data and heartbeat messages for receiver health.
5. The worker appends submissions to the durable outbox.
6. The worker POSTs queued submissions to each configured aggregate /submit.
7. The aggregate verifies the allowlist and signature.
8. The HTTP worker queues the verified submission to the single writer.
9. The writer dedupes submission_id and appends accepted submissions to disk.
10. The writer decodes frame batches into receiver-scoped state and broadcasts live FeedMessage updates.
11. Browser/API clients bootstrap from HTTP JSON, then use WebSocket for live updates.
```

## Protocol Boundary

The live protocol registry currently routes ADS-B / Mode S 1090ES to the
implemented decoder. The project recognizes `uat978`, `acars`, `vdl2`, and
`ais` as radio job keys so defaults can be configured and tested, but live
decoding for those protocols still needs to be built.

Frame replay and raw I/Q capture records are protocol-scoped. Future decoder
fixtures have to name their protocol instead of silently falling through as
ADS-B.

## Persistence Model

Receiver persistence is local diagnostic history:

```text
feed.ndjson
feed.previous.ndjson
latest-aircraft.json
```

Aggregate persistence is hot serving state:

```text
snapshot.json
submissions.ndjson
```

The aggregate keeps current serving state in memory. Accepted submissions are
queued through 1 writer, appended to the hot journal, and periodically
checkpointed/compacted by age and size. It keeps hot serving state only; evicted
hot submissions are intentionally removed from local storage. Cold export can
be added later.

## Trust Model

Receiver trust is key-based: private seeds stay on receiver nodes, public keys
go into aggregate allowlists, and `submission_id` provides idempotent replay.
See [DEPLOYMENT.md](DEPLOYMENT.md) for onboarding and [API.md](API.md) for the
submission contract.
