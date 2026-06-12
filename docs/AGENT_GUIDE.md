## Quick Context

RSDB is a Rust ADS-B / Mode S receiver stack. Raspberry Pi receiver nodes run
`rsdb-usb` next to an RTL-SDR and submit signed frame batches to
`rsdb-aggregate`. The aggregate verifies allowlisted Ed25519 public keys,
dedupes by `submission_id`, persists hot state in SQLite, and serves the public
browser UI plus JSON/WebSocket APIs.

## Common Tasks

### Join The Shared Aggregate

Create a local receiver seed, then open a PR adding only the public key to
`deploy/fly/allowlist.txt`:

```sh
RSDB_RECEIVER_LAT=37.753 RSDB_RECEIVER_LON=-122.447 ./pi/init-receiver-node.sh https://rsdb.hackshare.com
```

After the allowlist PR is deployed, provision a receiver-profile Pi from
nightly artifacts:

```sh
RSDB_NODE_PROFILE=receiver RSDB_RELEASE_CHANNEL=nightly ./pi/push-and-provision.sh
```

### Install A Release Directly On A Pi

```sh
./pi/install-release.sh receiver
RSDB_RELEASE_CHANNEL=nightly ./pi/install-release.sh receiver
./pi/install-release.sh receiver --enable-updater
```

Use `full` instead of `receiver` when the Pi should also run the local
aggregate UI/API.

### Deploy The Shared Fly Aggregate

Fly deployment is manual:

```sh
fly deploy
```

## Live Endpoints

- UI: https://rsdb.hackshare.com/
- Status: https://rsdb.hackshare.com/status.json
- Bootstrap: https://rsdb.hackshare.com/bootstrap.json
- Aircraft: https://rsdb.hackshare.com/aircraft.json
- Receivers: https://rsdb.hackshare.com/receivers.json
- Schema: https://rsdb.hackshare.com/schema.json
- Agent guide: https://rsdb.hackshare.com/agents.md
- Agent guide alias: https://rsdb.hackshare.com/llms.txt
- Crawler policy: https://rsdb.hackshare.com/robots.txt
- WebSocket: wss://rsdb.hackshare.com/ws

## Source Map

- `docs/AGENT_GUIDE.md`: Curated top-level guide, common tasks, live
  endpoints, and this source map.
- `pi/README.md`: Raspberry Pi flashing, provisioning, profiles, release
  installs, updater, and smoke checks.
- `docs/DEPLOYMENT.md`: Fly deployment, multi-receiver onboarding, and
  aggregate forwarding.
- `docs/CONFIGURATION.md`: Runtime environment keys and release-channel
  selection.
- `docs/API.md`: Public aggregate and local receiver diagnostics endpoints.
- `docs/ARCHITECTURE.md`: Component and data-flow overview.
- `AGENTS.md`: Repository agent rules.
