# RSDB

RSDB is a Rust ADS-B / Mode S receiver stack for RTL-SDR dongles. It has 2
pieces: a small receiver process that reads USB I/Q samples, and an aggregate
service that verifies signed receiver submissions, persists recent state, and
serves the browser UI plus JSON/WebSocket APIs.

The live decoder today is ADS-B / Mode S on 1090 MHz. The codebase already
models protocol-specific radio jobs for future UAT 978, ACARS, VDL2, and AIS
work, but those live decoders still need to be built.

## What Runs Where

| Binary | Role | Typical host |
| --- | --- | --- |
| `rsdb-usb` | Opens the RTL-SDR, decodes the configured radio stream, signs frame batches and heartbeats, and serves local receiver diagnostics. | Raspberry Pi attached to the SDR |
| `rsdb-aggregate` | Verifies signed submissions, dedupes by submission ID, persists hot aggregate state, and serves the public UI/API/WebSocket feed. | Same Pi, Fly.io, or another server |

On a single Pi, both binaries run as managed services. The receiver submits to
the local aggregate at `http://127.0.0.1:8090`; it can also forward to remote
aggregates over HTTPS.

## Quick Start For Development

Install Rust and Bun, then run:

```sh
bun install
bun run check
```

Build the deployable binaries:

```sh
cargo build --release -p rsdb-usb --bin rsdb-usb
cargo build --release -p rsdb-aggregate --bin rsdb-aggregate
```

Common local commands:

```sh
./target/release/rsdb-usb list
./target/release/rsdb-usb open 0
./target/release/rsdb-usb decode 0 30
./target/release/rsdb-usb json 0 30
./target/release/rsdb-aggregate serve allowlist.txt 127.0.0.1 8090
```

## Raspberry Pi Receiver

The hardware path keeps the USB SDR off your laptop and puts it on a Raspberry
Pi receiver node. See [pi/README.md](pi/README.md) for the CLI-only flash,
first-boot, and provisioning flow.

To join the shared aggregate, see
[Multi-Receiver Onboarding](docs/DEPLOYMENT.md#multi-receiver-onboarding).

## Aggregate Deployment

The aggregate can run on the Pi for local viewing, or as a standalone service
on Fly.io or another container host. The Docker image builds only
`rsdb-aggregate`. This repo's shared aggregate is served at
`https://rsdb.hackshare.com`.

See [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) for Fly.io and multi-receiver
setup.

## Data Product

See [docs/API.md](docs/API.md) for the aggregate HTTP endpoints, WebSocket feed,
receiver diagnostics endpoints, and replay file formats.

The deployed aggregate also serves an agent-facing project guide at
`/agents.md`, generated from the committed repo docs and agent rules. The same
guide is available at `/llms.txt`, and `/robots.txt` allows those read-only
agent endpoints while disallowing `/submit`.

## Architecture

The short version:

```text
RTL-SDR USB -> rsdb-usb -> signed submissions -> rsdb-aggregate -> UI/API/WS
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the component and call
flow diagrams.

## Configuration

Runtime config comes from environment files. Scripts load `.env`; services load
`/etc/rsdb/rsdb.env`; both binaries also support `--config path` and
`RSDB_CONFIG`.

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) and [.env.example](.env.example).

## Repository Hygiene

Before pushing:

```sh
bun run check
git status --short
```

Ignored local paths include `.env`, `target/`, `node_modules/`, `web/dist/`,
`pi/images/`, `pi/backups/`, and `pi/secrets/`.

## License

Apache-2.0. See [LICENSE](LICENSE).
