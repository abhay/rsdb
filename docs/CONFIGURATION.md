# Configuration

RSDB uses shell-style environment files.

| Context | Config source |
| --- | --- |
| Local scripts | `.env` in the repo root |
| Pi services | `/etc/rsdb/rsdb.env` |
| Explicit binary override | `--config path` |
| Shared binary override | `RSDB_CONFIG=path` |

Copy [.env.example](../.env.example) to `.env` for local receiver work.

## Required Local Receiver Fields

```sh
RSDB_NODE_HOSTNAME='rsdb-your-node'
RSDB_RECEIVER_LAT='37.753'
RSDB_RECEIVER_LON='-122.447'
```

`RSDB_NODE_HOSTNAME` is used for first boot and mDNS. Scripts default to
`<RSDB_NODE_HOSTNAME>.local`. Set `RSDB_NODE_HOST` only when scripts should use
a LAN, VPN, static IP, or DNS name directly.

Receiver coordinates are required. They drive range and bearing calculations.
Use a nearby generic location if you don't want to expose exact receiver
position.

## Laptop And First-Boot Fields

```sh
RSDB_NODE_USER='rsdb'
RSDB_NODE_HOST=''
RSDB_NODE_SSH_KEY='pi/secrets/rsdb_node_ed25519'
RSDB_TIMEZONE='America/Los_Angeles'
RSDB_WIFI_COUNTRY='US'
RSDB_WIFI_SSID='your-network'
RSDB_WIFI_PASSWORD='your-password'
```

Wi-Fi fields are only needed when the Pi cannot use Ethernet or another
preconfigured network. The flash script writes Wi-Fi credentials into the boot
partition first-run script; that script removes itself after first boot.

## Receiver Radio

```sh
RSDB_DEVICE_INDEX=0
RSDB_PROTOCOL=adsb1090
RSDB_CENTER_FREQUENCY_HZ=1090000000
RSDB_SAMPLE_RATE_HZ=2000000
RSDB_GAIN=496
RSDB_BIAS_T=false
```

`RSDB_PROTOCOL=adsb1090` is the only implemented live decoder today. Recognized
future protocol keys are `uat978`, `acars`, `vdl2`, and `ais`. Center frequency
and sample rate default from the protocol and only need to be set for overrides.

1 RTL-SDR dongle runs 1 radio job at a time. Simultaneous protocols need
multiple dongles or a different SDR.

## Receiver Service

```sh
RSDB_COLLECTOR_PORT=8080
RSDB_STALE_AFTER_SECONDS=60
RSDB_HEARTBEAT_SECONDS=15
RSDB_RETRY_SECONDS=10
RSDB_PERSIST_DIR=/var/lib/rsdb
RSDB_PERSIST_FEED_MAX_MB=100
```

`rsdb-usb serve` keeps the diagnostics listener running even when the USB
receiver is missing. It reports the error, emits heartbeats, and retries.

## Signing And Submission

```sh
RSDB_SIGNING_KEY_PATH=/etc/rsdb/receiver.seed
RSDB_SUBMIT_URLS=http://127.0.0.1:8090
RSDB_SUBMIT_RETRY_SECONDS=5
RSDB_SUBMIT_OUTBOX_DIR=/var/lib/rsdb/submit
RSDB_SUBMIT_OUTBOX_MAX_MB=25
```

Receiver identity is derived from the Ed25519 seed. Add remote/shared
aggregates to `RSDB_SUBMIT_URLS` as a comma or whitespace separated list:

```sh
RSDB_SUBMIT_URLS='http://127.0.0.1:8090,https://rsdb.hackshare.com'
```

The outbox is durable. If an aggregate is unavailable, signed submissions remain
queued and replay later. Submission IDs make replay idempotent at the aggregate.

## Aggregate Service

```sh
RSDB_AGGREGATE_HOST=0.0.0.0
RSDB_AGGREGATE_PORT=8090
RSDB_ALLOWLIST=/etc/rsdb/allowlist.txt
RSDB_AGGREGATE_DATA_DIR=/var/lib/rsdb/aggregate
RSDB_AGGREGATE_RETENTION_HOURS=72
RSDB_AGGREGATE_HOT_MAX_MB=250
```

`RSDB_ALLOWLIST` can be a path or an inline list of public Ed25519 keys. Public
allowlists can live in repo config or environment variables.

`PORT` is treated as `RSDB_AGGREGATE_PORT` when `RSDB_AGGREGATE_PORT` is not
set, which is useful on container platforms.

## Generated Receiver Handles

The UI derives a 3-word handle from the key-derived receiver ID. A short
hex suffix exists for collision handling, but normal UI surfaces should hide it
unless 2 receivers in the same dataset collide.
