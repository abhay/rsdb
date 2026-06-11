# Raspberry Pi Receiver Setup

Use this CLI flow to build a Raspberry Pi receiver node. The RTL-SDR plugs into
the Pi instead of your laptop.

The provisioned Pi runs:

```text
rsdb.service             rsdb-usb serve
rsdb-aggregate.service   rsdb-aggregate serve
```

The receiver service handles the USB SDR and local diagnostics. The aggregate
service verifies signed submissions and serves the UI/API on port `8090`.

## Local Settings

Copy the example config and fill in `RSDB_RECEIVER_LAT` and
`RSDB_RECEIVER_LON`:

```sh
cp .env.example .env
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

- installs OS packages, Rust, SSH, and Avahi
- builds `rsdb-usb` and `rsdb-aggregate`
- writes `/etc/rsdb/rsdb.env` if missing
- installs systemd units
- enables and restarts `rsdb.service`
- enables `rsdb-aggregate.service` when an allowlist is available

Existing `/etc/rsdb/rsdb.env` and `/etc/rsdb/allowlist.txt` files are
preserved.

Reboot after the first provision so group membership and hostname changes take
effect:

```sh
ssh -i "$RSDB_NODE_SSH_KEY" -o ForwardAgent=no "$node_target" sudo reboot
```

## Test The SDR

Plug the RTL-SDR into the Pi.

```sh
ssh -i "$RSDB_NODE_SSH_KEY" -o ForwardAgent=no "$node_target"
cd ~/rsdb
./target/release/rsdb-usb list
./target/release/rsdb-usb open 0
./target/release/rsdb-usb decode 0 30
```

Stream newline-delimited feed JSON for a bounded test:

```sh
./target/release/rsdb-usb json 0 30
```

Record lower-level decoded frame records:

```sh
./target/release/rsdb-usb record-frames 30 /tmp/rsdb-frames.ndjson
./target/release/rsdb-usb replay-frames /tmp/rsdb-frames.ndjson
```

## Open The UI

The managed aggregate serves the browser UI:

```text
$node_service_url
```

If DNS is unavailable:

```text
http://<node-ip>:8090
```

Useful API checks from the laptop:

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

## Next References

- [Configuration](../docs/CONFIGURATION.md) covers `.env`, `/etc/rsdb/rsdb.env`,
  receiver coordinates, signing, and persistence.
- [Deployment](../docs/DEPLOYMENT.md) covers Fly.io, allowlists, and adding
  another receiver.
- [API](../docs/API.md) covers aggregate and diagnostics endpoints.
