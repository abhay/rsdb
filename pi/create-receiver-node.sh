#!/usr/bin/env sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RSDB_SCRIPT_DIR="$script_dir"
. "$script_dir/load-env.sh"

usage() {
    cat <<'USAGE'
Usage:
  ./pi/create-receiver-node.sh [remote-aggregate-url]

Optional environment:
  RSDB_NEW_RECEIVER_LAT       Required receiver latitude for the env snippet.
  RSDB_NEW_RECEIVER_LON       Required receiver longitude for the env snippet.
  RSDB_NEW_NODE_HOSTNAME      Suggested node hostname; defaults to rsdb-<public-key-prefix>.
  RSDB_RECEIVER_BUNDLE_DIR    Output directory for the generated bundle.
  RSDB_USB_BIN                Existing rsdb-usb binary to use.

The receiver ID is derived from the generated Ed25519 signing key.
USAGE
}

shell_quote() {
    printf "'%s'" "$(printf "%s" "$1" | sed "s/'/'\\\\''/g")"
}

validate_hostname() {
    hostname="$1"

    case "$hostname" in
        *[!abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-]* | "")
            echo "invalid RSDB_NEW_NODE_HOSTNAME: $hostname" >&2
            exit 1
            ;;
    esac
}

remote_urls="${1:-}"

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi
if [ "$#" -gt 1 ]; then
    usage >&2
    exit 1
fi

if [ -z "${RSDB_NEW_RECEIVER_LAT:-}" ] || [ -z "${RSDB_NEW_RECEIVER_LON:-}" ]; then
    echo "set RSDB_NEW_RECEIVER_LAT and RSDB_NEW_RECEIVER_LON" >&2
    exit 1
fi

if ! awk -v lat="$RSDB_NEW_RECEIVER_LAT" -v lon="$RSDB_NEW_RECEIVER_LON" '
    BEGIN {
        numeric = "^-?[0-9]+(\\.[0-9]+)?$"
        if (lat !~ numeric || lon !~ numeric) exit 1
        if (lat < -90 || lat > 90 || lon < -180 || lon > 180) exit 1
    }
'; then
    echo "invalid receiver coordinates: RSDB_NEW_RECEIVER_LAT=$RSDB_NEW_RECEIVER_LAT RSDB_NEW_RECEIVER_LON=$RSDB_NEW_RECEIVER_LON" >&2
    exit 1
fi

if ! command -v openssl >/dev/null 2>&1; then
    echo "openssl is required to generate the receiver signing seed" >&2
    exit 1
fi

if [ -n "${RSDB_NEW_NODE_HOSTNAME:-}" ]; then
    validate_hostname "$RSDB_NEW_NODE_HOSTNAME"
fi

cd "$RSDB_REPO_ROOT"

explicit_bundle_dir=false
if [ -n "${RSDB_RECEIVER_BUNDLE_DIR:-}" ]; then
    bundle_dir="$RSDB_RECEIVER_BUNDLE_DIR"
    explicit_bundle_dir=true
else
    bundle_dir="pi/secrets/receivers/.new-$(date -u +%Y%m%d%H%M%S)-$$"
fi
seed_path="$bundle_dir/receiver.seed"
env_path="$bundle_dir/receiver.env"
node_env_path="$bundle_dir/node.env"
allowlist_entry_path="$bundle_dir/allowlist-entry.txt"

umask 077
mkdir -p "$bundle_dir"

if [ -f "$seed_path" ]; then
    echo "Preserving existing signing seed: $seed_path" >&2
else
    openssl rand -hex 32 >"$seed_path"
fi

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

env -i PATH="$PATH" HOME="$HOME" "$rsdb_usb_binary" --config "$tmp_config" allowlist-entry >"$allowlist_entry_path"

public_key="$(sed -n 's/^[[:space:]]*\([0-9a-fA-F][0-9a-fA-F]*\)[[:space:]]*$/\1/p' "$allowlist_entry_path" | head -n 1)"
if [ "${#public_key}" -ne 64 ]; then
    echo "failed to read public key from $allowlist_entry_path" >&2
    exit 1
fi

if [ "$explicit_bundle_dir" = false ]; then
    public_key_prefix="$(printf "%s" "$public_key" | cut -c 1-16)"
    final_bundle_dir="pi/secrets/receivers/key-$public_key_prefix"
    if [ -e "$final_bundle_dir" ]; then
        echo "receiver bundle already exists: $final_bundle_dir" >&2
        echo "new bundle remains: $bundle_dir" >&2
        exit 1
    fi
    mv "$bundle_dir" "$final_bundle_dir"
    bundle_dir="$final_bundle_dir"
    seed_path="$bundle_dir/receiver.seed"
    env_path="$bundle_dir/receiver.env"
    node_env_path="$bundle_dir/node.env"
    allowlist_entry_path="$bundle_dir/allowlist-entry.txt"
fi

node_key_prefix="$(printf "%s" "$public_key" | cut -c 1-12)"
node_hostname="${RSDB_NEW_NODE_HOSTNAME:-rsdb-$node_key_prefix}"
submit_urls="http://127.0.0.1:8090"
if [ -n "$remote_urls" ]; then
    submit_urls="$submit_urls,$remote_urls"
fi

validate_hostname "$node_hostname"

{
    printf '# Laptop-side settings for flashing and deploys.\n'
    printf 'RSDB_NODE_HOSTNAME=%s\n' "$(shell_quote "$node_hostname")"
    printf 'RSDB_NODE_USER=%s\n' "$(shell_quote "rsdb")"
    printf '# Leave blank to use %s.local, or set a LAN/VPN/static IP.\n' "$node_hostname"
    printf 'RSDB_NODE_HOST=%s\n' "$(shell_quote "")"
    printf 'RSDB_NODE_SSH_KEY=%s\n' "$(shell_quote "$bundle_dir/ssh_ed25519")"
} >"$node_env_path"

{
    printf '# Receiver identity is derived from RSDB_SIGNING_KEY_PATH.\n'
    printf 'RSDB_PROTOCOL=%s\n' "$(shell_quote "adsb1090")"
    printf 'RSDB_RECEIVER_LAT=%s\n' "$(shell_quote "$RSDB_NEW_RECEIVER_LAT")"
    printf 'RSDB_RECEIVER_LON=%s\n' "$(shell_quote "$RSDB_NEW_RECEIVER_LON")"
    printf 'RSDB_SIGNING_KEY_PATH=%s\n' "$(shell_quote "/etc/rsdb/receiver.seed")"
    printf 'RSDB_SUBMIT_URLS=%s\n' "$(shell_quote "$submit_urls")"
} >"$env_path"

cat <<EOF
Receiver bundle created:
  Public key:       $public_key
  Private seed:     $seed_path
  Node env:         $node_env_path
  Pi env snippet:   $env_path
  Allowlist entry:  $allowlist_entry_path

Copy node.env to .env on the receiver owner's laptop, then run create-node-ssh-key.
Only share the allowlist entry with the aggregate operator.
The private seed and node SSH key should stay with the receiver owner.
EOF
