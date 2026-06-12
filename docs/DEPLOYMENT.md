# Deployment

## Raspberry Pi Receiver

Use [pi/README.md](../pi/README.md) for the CLI-only Pi setup.

Release-installed Pi nodes use prebuilt ARM64 GitHub Release artifacts and do
not compile Rust on-device. Choose a profile:

```text
receiver   rsdb.service only; submits to a remote aggregate
full       rsdb.service plus rsdb-aggregate.service for local UI/API
```

Install commands:

```sh
./pi/install-release.sh receiver
./pi/install-release.sh full
RSDB_RELEASE_CHANNEL=nightly ./pi/install-release.sh receiver
RSDB_VERSION=v0.1.0 ./pi/install-release.sh full
./pi/install-release.sh receiver --enable-updater
```

From a laptop that has already followed the receiver onboarding flow, provision
a Pi from the nightly channel with:

```sh
RSDB_NODE_PROFILE=receiver RSDB_RELEASE_CHANNEL=nightly ./pi/push-and-provision.sh
```

The receiver service is hardware-adjacent and normally binds diagnostics on
`127.0.0.1:8080`. The aggregate service verifies signed submissions and serves
the UI/API on `0.0.0.0:8090` when the `full` profile is installed.

Release installs put binaries under `/opt/rsdb/releases/<version>` and update
`/opt/rsdb/current` atomically. Systemd units execute
`/opt/rsdb/current/bin/rsdb-usb` and
`/opt/rsdb/current/bin/rsdb-aggregate`.

Useful checks on the Pi:

```sh
systemctl status rsdb.service
journalctl -u rsdb.service -f
systemctl status rsdb-aggregate.service
journalctl -u rsdb-aggregate.service -f
curl -fsS http://127.0.0.1:8090/status.json
```

## Fly.io Aggregate

The repo includes an aggregate-only `Dockerfile` and
`deploy/fly/fly.toml.example`.

Create `fly.toml` from the example, update the app name and region, then create
a volume for hot aggregate state:

```sh
fly volumes create rsdb_data --size 1 --region sjc
```

Public receiver keys live in `deploy/fly/allowlist.txt`. Additions should come
through PRs so the public allowlist is reviewed with the code.

Deploy:

```sh
fly deploy
```

Fly deploys are still manual. The GitHub release workflow only publishes
Raspberry Pi release artifacts and the `nightly` prerelease channel.

The Fly service listens on internal port `8080`, serves HTTPS publicly, and
stores hot aggregate state under `/data/aggregate`. The Docker image copies
`deploy/fly/allowlist.txt` into `/etc/rsdb/allowlist.txt`.

The shared RSDB aggregate is deployed as `rsdb-aggregate` and is served at:

```text
https://rsdb.hackshare.com
```

The custom hostname is a DNS-only CNAME:

```text
rsdb.hackshare.com -> m1qe5xn.rsdb-aggregate.fly.dev
```

Check the certificate and endpoint after DNS changes:

```sh
fly certs check rsdb.hackshare.com --app rsdb-aggregate
curl -fsS https://rsdb.hackshare.com/status.json
```

## Multi-Receiver Onboarding

Each receiver owner creates their own private seed locally and opens a PR with
only the public key:

```sh
RSDB_RECEIVER_LAT=37.753311 \
RSDB_RECEIVER_LON=-122.447029 \
./pi/init-receiver-node.sh https://rsdb.hackshare.com
```

The script writes ignored local files under `pi/secrets/`, updates the local
`.env`, and prints a 64-hex public key. The private seed never needs to leave
the receiver owner's machine.

Files:

```text
pi/secrets/receiver.seed         private Ed25519 signing seed
pi/secrets/allowlist-entry.txt   public Ed25519 key for the aggregate operator
```

Open a PR adding the public key to:

```text
deploy/fly/allowlist.txt
```

Maintainers can validate and sort additions with:

```sh
./pi/add-allowlisted-receiver.sh deploy/fly/allowlist.txt path/to/public-key-file
```

For Fly.io, merge the PR and redeploy.

For a Pi-hosted aggregate:

```sh
scp deploy/fly/allowlist.txt "$node_target:/tmp/allowlist.txt"
ssh "$node_target" 'sudo install -m 0644 /tmp/allowlist.txt /etc/rsdb/allowlist.txt && sudo systemctl restart rsdb-aggregate.service'
```

## Local Aggregate Plus Remote Forwarding

Keep the Pi local aggregate in the submit target list:

```sh
RSDB_SUBMIT_URLS=http://127.0.0.1:8090
RSDB_SUBMIT_MAX_LAG_SECONDS=60
```

Add remote aggregates to the same list:

```sh
RSDB_SUBMIT_URLS=http://127.0.0.1:8090,https://rsdb.hackshare.com
```

The receiver tracks pending destinations per submission. A local aggregate can
drain while a remote aggregate stays queued for retry. Rows that age past
`RSDB_SUBMIT_MAX_LAG_SECONDS` are dropped before replay so the shared aggregate
does not ingest old data as live traffic.
