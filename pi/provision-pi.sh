#!/usr/bin/env sh
set -eu

if [ "$(id -u)" -eq 0 ]; then
    echo "run this as the Pi user, not as root" >&2
    exit 1
fi

hostname="${RSDB_NODE_HOSTNAME:-$(hostname)}"

sudo hostnamectl set-hostname "$hostname"
sudo apt-get update
sudo apt-get install -y \
    avahi-daemon \
    build-essential \
    ca-certificates \
    curl \
    git \
    openssh-server \
    pkg-config \
    rsync

sudo systemctl enable --now ssh avahi-daemon

if [ -n "${RSDB_AUTHORIZED_KEY:-}" ]; then
    mkdir -p "$HOME/.ssh"
    chmod 700 "$HOME/.ssh"
    touch "$HOME/.ssh/authorized_keys"
    chmod 600 "$HOME/.ssh/authorized_keys"
    if ! grep -qxF "$RSDB_AUTHORIZED_KEY" "$HOME/.ssh/authorized_keys"; then
        printf '%s\n' "$RSDB_AUTHORIZED_KEY" >>"$HOME/.ssh/authorized_keys"
    fi
fi

sudo tee /etc/udev/rules.d/20-rsdb-rtlsdr.rules >/dev/null <<'RULES'
SUBSYSTEM=="usb", ATTR{idVendor}=="0bda", ATTR{idProduct}=="2832", MODE="0660", GROUP="plugdev", TAG+="uaccess"
SUBSYSTEM=="usb", ATTR{idVendor}=="0bda", ATTR{idProduct}=="2838", MODE="0660", GROUP="plugdev", TAG+="uaccess"
RULES

sudo udevadm control --reload-rules
sudo udevadm trigger || true
sudo usermod -aG plugdev "$USER"

if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal
fi

if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi

if [ -f Cargo.toml ]; then
    cargo test --workspace
    cargo build --release -p rsdb-usb --bin rsdb-usb
    cargo build --release -p rsdb-aggregate --bin rsdb-aggregate
fi

sudo install -d -m 0755 /etc/rsdb
sudo install -d -m 0755 -o "$USER" -g "$USER" /var/lib/rsdb
sudo install -d -m 0755 -o "$USER" -g "$USER" /var/lib/rsdb/aggregate
sudo install -d -m 0755 -o "$USER" -g "$USER" /var/lib/rsdb/submit

if [ ! -f /etc/rsdb/rsdb.env ]; then
    sudo tee /etc/rsdb/rsdb.env >/dev/null <<'CONFIG'
RSDB_DEVICE_INDEX=0
RSDB_PROTOCOL=adsb1090
# RSDB_CENTER_FREQUENCY_HZ=1090000000
# RSDB_SAMPLE_RATE_HZ=2000000
RSDB_COLLECTOR_PORT=8080
RSDB_GAIN=496
RSDB_BIAS_T=false
RSDB_STREAM_SECONDS=5
# RSDB_JSON_SECONDS=30
RSDB_STALE_AFTER_SECONDS=60
RSDB_HEARTBEAT_SECONDS=15
RSDB_RETRY_SECONDS=10
RSDB_PERSIST_DIR=/var/lib/rsdb
RSDB_PERSIST_FEED_MAX_MB=100
RSDB_AGGREGATE_SERVICE_ENABLED=auto
RSDB_AGGREGATE_HOST=0.0.0.0
RSDB_AGGREGATE_PORT=8090
RSDB_ALLOWLIST=/etc/rsdb/allowlist.txt
RSDB_AGGREGATE_DATA_DIR=/var/lib/rsdb/aggregate
RSDB_AGGREGATE_RETENTION_HOURS=72
RSDB_AGGREGATE_HOT_MAX_MB=250
# RSDB_SIGNING_KEY_PATH=/etc/rsdb/receiver.seed
RSDB_SUBMIT_URLS=http://127.0.0.1:8090
# RSDB_SUBMIT_URLS=http://127.0.0.1:8090,https://aggregate.example.com
RSDB_SUBMIT_RETRY_SECONDS=5
RSDB_SUBMIT_OUTBOX_DIR=/var/lib/rsdb/submit
RSDB_SUBMIT_OUTBOX_MAX_MB=25
# RSDB_RECEIVER_LAT=fill-me-in
# RSDB_RECEIVER_LON=fill-me-in
CONFIG
    sudo chmod 0644 /etc/rsdb/rsdb.env
else
    echo "Preserving existing /etc/rsdb/rsdb.env"
fi

ensure_config_key() {
    key="$1"
    line="$2"

    if ! sudo grep -Eq "^[[:space:]#]*${key}=" /etc/rsdb/rsdb.env; then
        printf '%s\n' "$line" | sudo tee -a /etc/rsdb/rsdb.env >/dev/null
    fi
}

ensure_active_config_key() {
    key="$1"
    line="$2"

    if ! sudo grep -Eq "^[[:space:]]*${key}=" /etc/rsdb/rsdb.env; then
        printf '%s\n' "$line" | sudo tee -a /etc/rsdb/rsdb.env >/dev/null
    fi
}

active_config_key() {
    sudo grep -Eq "^[[:space:]]*$1=[^#]*[^[:space:]#]" /etc/rsdb/rsdb.env
}

config_value() {
    key="$1"
    sudo awk -F= -v key="$key" '
        /^[[:space:]]*#/ { next }
        {
            lhs = $1
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", lhs)
            if (lhs == key) {
                sub(/^[^=]*=/, "", $0)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", $0)
                gsub(/^'\''|'\''$/, "", $0)
                gsub(/^"|"$/, "", $0)
                value = $0
            }
        }
        END { if (value != "") print value }
    ' /etc/rsdb/rsdb.env
}

set_config_key() {
    key="$1"
    value="$2"
    tmp_config="$(mktemp)"

    sudo awk -v key="$key" -v line="$key=$value" '
        BEGIN { written = 0 }
        {
            lhs = $0
            sub(/=.*/, "", lhs)
            gsub(/^[[:space:]#]+|[[:space:]]+$/, "", lhs)
            if (lhs == key) {
                if (!written) {
                    print line
                    written = 1
                }
                next
            }
            print
        }
        END { if (!written) print line }
    ' /etc/rsdb/rsdb.env >"$tmp_config"

    sudo install -m 0644 "$tmp_config" /etc/rsdb/rsdb.env
    rm -f "$tmp_config"
}

validate_receiver_coordinates() {
    lat="$1"
    lon="$2"

    if [ -z "$lat" ] || [ -z "$lon" ]; then
        echo "set RSDB_RECEIVER_LAT and RSDB_RECEIVER_LON before provisioning" >&2
        exit 1
    fi

    if ! awk -v lat="$lat" -v lon="$lon" '
        BEGIN {
            numeric = "^-?[0-9]+(\\.[0-9]+)?$"
            if (lat !~ numeric || lon !~ numeric) exit 1
            if (lat < -90 || lat > 90 || lon < -180 || lon > 180) exit 1
        }
    '; then
        echo "invalid receiver coordinates: RSDB_RECEIVER_LAT=$lat RSDB_RECEIVER_LON=$lon" >&2
        exit 1
    fi
}

truthy_config_value() {
    case "$(config_value "$1" | tr '[:upper:]' '[:lower:]')" in
        1 | true | yes | on) return 0 ;;
        *) return 1 ;;
    esac
}

falsey_config_value() {
    case "$(config_value "$1" | tr '[:upper:]' '[:lower:]')" in
        0 | false | no | off) return 0 ;;
        *) return 1 ;;
    esac
}

ensure_config_key RSDB_PERSIST_DIR "RSDB_PERSIST_DIR=/var/lib/rsdb"
ensure_config_key RSDB_PERSIST_FEED_MAX_MB "RSDB_PERSIST_FEED_MAX_MB=100"
ensure_config_key RSDB_PROTOCOL "RSDB_PROTOCOL=adsb1090"
ensure_config_key RSDB_COLLECTOR_PORT "RSDB_COLLECTOR_PORT=8080"
ensure_config_key RSDB_AGGREGATE_SERVICE_ENABLED "RSDB_AGGREGATE_SERVICE_ENABLED=auto"
ensure_config_key RSDB_AGGREGATE_HOST "RSDB_AGGREGATE_HOST=0.0.0.0"
ensure_config_key RSDB_AGGREGATE_PORT "RSDB_AGGREGATE_PORT=8090"
ensure_config_key RSDB_ALLOWLIST "RSDB_ALLOWLIST=/etc/rsdb/allowlist.txt"
ensure_config_key RSDB_AGGREGATE_DATA_DIR "RSDB_AGGREGATE_DATA_DIR=/var/lib/rsdb/aggregate"
ensure_config_key RSDB_AGGREGATE_RETENTION_HOURS "RSDB_AGGREGATE_RETENTION_HOURS=72"
ensure_config_key RSDB_AGGREGATE_HOT_MAX_MB "RSDB_AGGREGATE_HOT_MAX_MB=250"
ensure_config_key RSDB_SIGNING_KEY_PATH "# RSDB_SIGNING_KEY_PATH=/etc/rsdb/receiver.seed"
ensure_config_key RSDB_SUBMIT_URLS "RSDB_SUBMIT_URLS=http://127.0.0.1:8090"
ensure_config_key RSDB_SUBMIT_RETRY_SECONDS "RSDB_SUBMIT_RETRY_SECONDS=5"
ensure_config_key RSDB_SUBMIT_OUTBOX_DIR "RSDB_SUBMIT_OUTBOX_DIR=/var/lib/rsdb/submit"
ensure_config_key RSDB_SUBMIT_OUTBOX_MAX_MB "RSDB_SUBMIT_OUTBOX_MAX_MB=25"

receiver_lat="${RSDB_RECEIVER_LAT:-$(config_value RSDB_RECEIVER_LAT)}"
receiver_lon="${RSDB_RECEIVER_LON:-$(config_value RSDB_RECEIVER_LON)}"
validate_receiver_coordinates "$receiver_lat" "$receiver_lon"
set_config_key RSDB_RECEIVER_LAT "$receiver_lat"
set_config_key RSDB_RECEIVER_LON "$receiver_lon"

if [ -n "${RSDB_SUBMIT_URLS:-}" ]; then
    set_config_key RSDB_SUBMIT_URLS "$RSDB_SUBMIT_URLS"
fi

if sudo test -f /etc/rsdb/receiver.seed; then
    sudo chown "$USER:$USER" /etc/rsdb/receiver.seed
    sudo chmod 0600 /etc/rsdb/receiver.seed
fi

if ! active_config_key RSDB_SIGNING_KEY_PATH; then
    if sudo test -f /etc/rsdb/receiver.seed; then
        ensure_active_config_key RSDB_SIGNING_KEY_PATH "RSDB_SIGNING_KEY_PATH=/etc/rsdb/receiver.seed"
    fi
fi

signing_configured=false
if active_config_key RSDB_SIGNING_KEY_PATH; then
    signing_configured=true
fi

if [ "$signing_configured" = true ]; then
    allowlist_path="$(config_value RSDB_ALLOWLIST)"
    if [ -z "$allowlist_path" ]; then
        allowlist_path=/etc/rsdb/allowlist.txt
        ensure_active_config_key RSDB_ALLOWLIST "RSDB_ALLOWLIST=$allowlist_path"
    fi
    if [ "$allowlist_path" = /etc/rsdb/allowlist.txt ] && ! sudo test -f "$allowlist_path"; then
        tmp_allowlist="$(mktemp)"
        "$HOME/rsdb/target/release/rsdb-usb" --config /etc/rsdb/rsdb.env allowlist-entry >"$tmp_allowlist"
        sudo install -m 0644 "$tmp_allowlist" "$allowlist_path"
        rm -f "$tmp_allowlist"
    elif [ "$allowlist_path" = /etc/rsdb/allowlist.txt ]; then
        echo "Preserving existing $allowlist_path"
    fi
fi

sudo tee /etc/systemd/system/rsdb.service >/dev/null <<SERVICE
[Unit]
Description=RSDB receiver collector
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User=$USER
WorkingDirectory=$HOME/rsdb
EnvironmentFile=-/etc/rsdb/rsdb.env
ExecStart=$HOME/rsdb/target/release/rsdb-usb serve
Restart=always
RestartSec=10
SupplementaryGroups=plugdev
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
SERVICE

sudo tee /etc/systemd/system/rsdb-aggregate.service >/dev/null <<SERVICE
[Unit]
Description=RSDB local aggregate ingest
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
User=$USER
WorkingDirectory=$HOME/rsdb
EnvironmentFile=-/etc/rsdb/rsdb.env
ExecStart=$HOME/rsdb/target/release/rsdb-aggregate serve
Restart=always
RestartSec=10
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
SERVICE

sudo systemctl daemon-reload
sudo systemctl enable rsdb.service
if ! sudo systemctl restart rsdb.service; then
    echo "rsdb.service did not start cleanly; check: journalctl -u rsdb.service" >&2
fi

aggregate_configured=false
allowlist_path="$(config_value RSDB_ALLOWLIST)"
if [ -n "$allowlist_path" ] && sudo test -f "$allowlist_path"; then
    aggregate_configured=true
fi

if falsey_config_value RSDB_AGGREGATE_SERVICE_ENABLED; then
    aggregate_configured=false
elif truthy_config_value RSDB_AGGREGATE_SERVICE_ENABLED; then
    aggregate_configured=true
fi

if [ "$aggregate_configured" = true ]; then
    sudo systemctl enable rsdb-aggregate.service
    if ! sudo systemctl restart rsdb-aggregate.service; then
        echo "rsdb-aggregate.service did not start cleanly; check: journalctl -u rsdb-aggregate.service" >&2
    fi
else
    sudo systemctl disable --now rsdb-aggregate.service >/dev/null 2>&1 || true
    echo "rsdb-aggregate.service is installed but disabled until an allowlist is configured"
fi

echo
echo "Pi receiver node provisioned."
echo "Reconnect after reboot with: ssh $USER@$hostname.local"
echo "WebSocket service: systemctl status rsdb.service"
echo "Aggregate service: systemctl status rsdb-aggregate.service"
echo "Logs: journalctl -u rsdb.service -f"
echo "A reboot is recommended so group membership and hostname are fully active."
