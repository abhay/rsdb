# Raspberry Pi Receiver Setup

Use this CLI flow to build a Raspberry Pi receiver node. The RTL-SDR plugs into
the Pi instead of your laptop.

Release installs use prebuilt ARM64 artifacts from GitHub Releases. They do not
compile Rust on the Pi.

## Profiles

Choose one Pi profile:

```text
receiver   rsdb.service only; submits to a remote aggregate
full       rsdb.service plus rsdb-aggregate.service for local UI/API
```

Use `receiver` for Pi 3-class nodes. Use `full` for Pi 4-class nodes or any Pi
that should host its own local aggregate.

## Local Settings

Copy the example config and fill in `RSDB_RECEIVER_LAT` and
`RSDB_RECEIVER_LON`:

```sh
cp .env.example .env
```

Set `RSDB_NODE_PROFILE` when the default `full` profile is not right:

```sh
RSDB_NODE_PROFILE='receiver'
```

Then create the receiver identity locally:

```sh
./pi/init-receiver-node.sh
```

The init script creates `pi/secrets/receiver.seed`, updates `.env`, and prints
the public key to add to `deploy/fly/allowlist.txt` in a PR.

Add Wi-Fi fields only when the Pi can't use Ethernet or another preconfigured
network.

## Create A Node SSH Key

```sh
./pi/create-node-ssh-key.sh
```

The private key lands under `pi/secrets/`, which git ignores.

## Flash Raspberry Pi OS Lite

Download the current Raspberry Pi OS Lite 64-bit image:

```sh
./pi/download-rpios-lite.sh
```

List removable disks:

```sh
./pi/list-disks-macos.sh
```

Flash the SD card and inject first-boot SSH, hostname, mDNS, and optional Wi-Fi
configuration:

```sh
./pi/flash-rpios-lite-macos.sh
./pi/flash-rpios-lite-macos.sh /dev/diskN
```

The flash script erases the selected disk and requires typed confirmation. If
there is exactly 1 external physical disk, it can auto-select it.

After boot, give the Pi a few minutes. First boot applies config and reboots
once.

## Connect

Load the script environment and derive local targets:

```sh
RSDB_SCRIPT_DIR="$PWD/pi"
. ./pi/load-env.sh
node_target="$RSDB_NODE_USER@$RSDB_NODE_HOST"
node_service_url="http://$RSDB_NODE_HOST:$RSDB_AGGREGATE_PORT"
```

SSH:

```sh
ssh -i "$RSDB_NODE_SSH_KEY" -o ForwardAgent=no "$node_target"
```

If mDNS doesn't resolve, check that your network allows client-to-client
discovery:

```sh
dns-sd -G v4 "$RSDB_NODE_HOSTNAME.local"
dns-sd -B _ssh._tcp local
```

Guest networks often block mDNS. In that case, set `RSDB_NODE_HOST` to the
Pi's LAN or VPN IP.

## Provision

From this repo on the laptop:

```sh
./pi/push-and-provision.sh
```

Provisioning:

- installs OS packages needed for a release-installed node
- installs RTL-SDR udev rules
- copies the local receiver seed to `/etc/rsdb/receiver.seed`
- downloads the selected ARM64 release artifact and `checksums.txt`
- verifies SHA-256 before installing
- installs to `/opt/rsdb/releases/<version>`
- atomically updates `/opt/rsdb/current`
- writes `/etc/rsdb/rsdb.env` if missing and preserves existing config
- installs systemd units that run `/opt/rsdb/current/bin/...`

Existing `/etc/rsdb/rsdb.env`, `/etc/rsdb/receiver.seed`, and
`/etc/rsdb/allowlist.txt` files are preserved.

Release selection defaults to the latest tagged release:

```sh
RSDB_RELEASE_CHANNEL=nightly ./pi/push-and-provision.sh
RSDB_VERSION=v0.1.0 ./pi/push-and-provision.sh
```

To run the installer directly on a Pi that already has this repo:

```sh
./pi/install-release.sh receiver
./pi/install-release.sh full
RSDB_RELEASE_CHANNEL=nightly ./pi/install-release.sh receiver
RSDB_VERSION=v0.1.0 ./pi/install-release.sh full
```

Reboot after the first provision so group membership is fully active:

```sh
ssh -i "$RSDB_NODE_SSH_KEY" -o ForwardAgent=no "$node_target" sudo reboot
```

## Updater

Auto-update is opt-in:

```sh
./pi/install-release.sh receiver --enable-updater
./pi/install-release.sh full --enable-updater
```

The updater installs `rsdb-update.service` and `rsdb-update.timer`. The timer
runs daily, reuses the selected profile and release channel from
`/etc/rsdb/rsdb.env`, and restarts services only when the installed release
version changes.

Previous release directories remain under `/opt/rsdb/releases/` for manual
rollback. To roll back, repoint `/opt/rsdb/current` to the previous release
directory and restart the affected services.

## Test The SDR

Plug the RTL-SDR into the Pi.

```sh
ssh -i "$RSDB_NODE_SSH_KEY" -o ForwardAgent=no "$node_target"
/opt/rsdb/current/bin/rsdb-usb list
/opt/rsdb/current/bin/rsdb-usb open 0
/opt/rsdb/current/bin/rsdb-usb decode 0 30
```

Stream newline-delimited feed JSON for a bounded test:

```sh
/opt/rsdb/current/bin/rsdb-usb json 0 30
```

Record lower-level decoded frame records:

```sh
/opt/rsdb/current/bin/rsdb-usb record-frames 30 /tmp/rsdb-frames.ndjson
/opt/rsdb/current/bin/rsdb-usb replay-frames /tmp/rsdb-frames.ndjson
```

## Open The UI

The `full` profile serves the browser UI from the local aggregate:

```text
$node_service_url
```

If DNS is unavailable:

```text
http://<node-ip>:8090
```

Useful API checks from the laptop for the `full` profile:

```sh
curl -fsS "$node_service_url/status.json"
curl -fsS "$node_service_url/receivers.json"
curl -fsS "$node_service_url/aircraft.json"
curl -fsS "$node_service_url/bootstrap.json"
curl -fsS "$node_service_url/schema.json"
```

Useful checks on the Pi:

```sh
systemctl status rsdb.service
journalctl -u rsdb.service -f
systemctl status rsdb-aggregate.service
journalctl -u rsdb-aggregate.service -f
curl -fsS http://127.0.0.1:8080/status.json
curl -fsS http://127.0.0.1:8090/status.json
```

For `receiver` profile nodes, `rsdb-aggregate.service` is disabled and only the
receiver diagnostics endpoint is local.

## Fly.io

Fly deploys are still manual:

```sh
fly deploy
```

Do not expect the GitHub release workflow to deploy Fly. Revisit that after Pi
release installs and the opt-in updater are proven.

## Next References

- [Configuration](../docs/CONFIGURATION.md) covers `.env`, `/etc/rsdb/rsdb.env`,
  receiver coordinates, signing, and persistence.
- [Deployment](../docs/DEPLOYMENT.md) covers Fly.io, allowlists, and adding
  another receiver.
- [API](../docs/API.md) covers aggregate and diagnostics endpoints.
