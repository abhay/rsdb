#!/usr/bin/env sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RSDB_REPO_ROOT="$(CDPATH= cd -- "$script_dir/.." && pwd)"

usage() {
    cat <<'USAGE'
Usage:
  RSDB_RECEIVER_LAT=37.753 RSDB_RECEIVER_LON=-122.447 ./pi/init-receiver-node.sh [remote-aggregate-url]

The default remote aggregate is https://rsdb.hackshare.com. The script creates a
local receiver signing seed, writes ignored local .env settings, and prints the
public key to add to deploy/fly/allowlist.txt in a PR.

Optional environment:
  RSDB_NODE_HOSTNAME          Suggested Pi hostname; defaults to rsdb-<public-key-prefix>.
  RSDB_RECEIVER_SEED_PATH     Local private seed path; defaults to pi/secrets/receiver.seed.
  RSDB_SUBMIT_URLS            Full submit URL list override.
  RSDB_USB_BIN                Existing rsdb-usb binary to use.
USAGE
}

shell_quote() {
    printf "'%s'" "$(printf "%s" "$1" | sed "s/'/'\\\\''/g")"
}

validate_hostname() {
    hostname="$1"

    case "$hostname" in
        *[!abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-]* | "")
            echo "invalid RSDB_NODE_HOSTNAME: $hostname" >&2
            exit 1
            ;;
    esac
}

validate_receiver_coordinates() {
    lat="$1"
    lon="$2"

    if [ -z "$lat" ] || [ -z "$lon" ]; then
        echo "set RSDB_RECEIVER_LAT and RSDB_RECEIVER_LON" >&2
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

set_env_key() {
    key="$1"
    value="$2"
    env_path="$RSDB_REPO_ROOT/.env"
    line="$key=$(shell_quote "$value")"
    tmp_config="$(mktemp)"

    if [ -f "$env_path" ]; then
        awk -v key="$key" -v line="$line" '
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
        ' "$env_path" >"$tmp_config"
    else
        printf '%s\n' "$line" >"$tmp_config"
    fi

    mv "$tmp_config" "$env_path"
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi
if [ "$#" -gt 1 ]; then
    usage >&2
    exit 1
fi

remote_urls="${1:-https://rsdb.hackshare.com}"
receiver_lat="${RSDB_RECEIVER_LAT:-}"
receiver_lon="${RSDB_RECEIVER_LON:-}"
validate_receiver_coordinates "$receiver_lat" "$receiver_lon"

if ! command -v openssl >/dev/null 2>&1; then
    echo "openssl is required to generate the receiver signing seed" >&2
    exit 1
fi

cd "$RSDB_REPO_ROOT"

seed_path="${RSDB_RECEIVER_SEED_PATH:-pi/secrets/receiver.seed}"
seed_dir="$(dirname -- "$seed_path")"
mkdir -p "$seed_dir"

if [ -f "$seed_path" ]; then
    echo "Preserving existing receiver seed: $seed_path" >&2
else
    umask 077
    openssl rand -hex 32 >"$seed_path"
fi
chmod 600 "$seed_path"

if [ -n "${RSDB_USB_BIN:-}" ]; then
    rsdb_usb_binary="$RSDB_USB_BIN"
else
    cargo build --release -p rsdb-usb --bin rsdb-usb
    rsdb_usb_binary="$RSDB_REPO_ROOT/target/release/rsdb-usb"
fi
if [ ! -x "$rsdb_usb_binary" ]; then
    echo "missing executable rsdb-usb binary: $rsdb_usb_binary" >&2
    exit 1
fi

tmp_config="$(mktemp)"
trap 'rm -f "$tmp_config"' EXIT INT TERM
{
    printf 'RSDB_SIGNING_KEY_PATH=%s\n' "$(shell_quote "$seed_path")"
} >"$tmp_config"

allowlist_entry_path="pi/secrets/allowlist-entry.txt"
env -i PATH="$PATH" HOME="$HOME" "$rsdb_usb_binary" --config "$tmp_config" allowlist-entry >"$allowlist_entry_path"

public_key="$(sed -n 's/^[[:space:]]*\([0-9a-fA-F][0-9a-fA-F]*\)[[:space:]]*$/\1/p' "$allowlist_entry_path" | head -n 1)"
if [ "${#public_key}" -ne 64 ]; then
    echo "failed to read public key from $allowlist_entry_path" >&2
    exit 1
fi

node_hostname="${RSDB_NODE_HOSTNAME:-rsdb-$(printf "%s" "$public_key" | cut -c 1-12)}"
validate_hostname "$node_hostname"

if [ -n "${RSDB_SUBMIT_URLS:-}" ]; then
    submit_urls="$RSDB_SUBMIT_URLS"
else
    submit_urls="http://127.0.0.1:8090"
    if [ -n "$remote_urls" ]; then
        submit_urls="$submit_urls,$remote_urls"
    fi
fi

set_env_key RSDB_NODE_HOSTNAME "$node_hostname"
set_env_key RSDB_RECEIVER_LAT "$receiver_lat"
set_env_key RSDB_RECEIVER_LON "$receiver_lon"
set_env_key RSDB_RECEIVER_SEED_PATH "$seed_path"
set_env_key RSDB_SUBMIT_URLS "$submit_urls"

cat <<EOF
Receiver identity initialized.

Private seed:
  $seed_path

Public allowlist key:
  $public_key

Allowlist entry file:
  $allowlist_entry_path

Local .env updated with:
  RSDB_NODE_HOSTNAME=$node_hostname
  RSDB_RECEIVER_LAT=$receiver_lat
  RSDB_RECEIVER_LON=$receiver_lon
  RSDB_RECEIVER_SEED_PATH=$seed_path
  RSDB_SUBMIT_URLS=$submit_urls

Open a PR that adds the public key to:
  deploy/fly/allowlist.txt

Then continue with:
  ./pi/create-node-ssh-key.sh
  ./pi/push-and-provision.sh

Do not commit .env, $seed_path, SSH private keys, or Wi-Fi credentials.
EOF
