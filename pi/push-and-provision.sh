#!/usr/bin/env sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RSDB_SCRIPT_DIR="$script_dir"
. "$script_dir/load-env.sh"

if [ -n "$RSDB_NODE_HOST" ]; then
    node_target="$RSDB_NODE_USER@$RSDB_NODE_HOST"
else
    node_target=""
fi

target="${1:-$node_target}"
key_path="$RSDB_NODE_SSH_KEY"
ssh_cmd="ssh -o ForwardAgent=no -o StrictHostKeyChecking=accept-new"

if [ -z "$target" ]; then
    echo "set RSDB_NODE_HOST or RSDB_NODE_HOSTNAME in .env, or pass a target as the first argument" >&2
    exit 1
fi

if [ -z "${RSDB_RECEIVER_LAT:-}" ] || [ -z "${RSDB_RECEIVER_LON:-}" ]; then
    echo "set RSDB_RECEIVER_LAT and RSDB_RECEIVER_LON in .env" >&2
    exit 1
fi

if ! awk -v lat="$RSDB_RECEIVER_LAT" -v lon="$RSDB_RECEIVER_LON" '
    BEGIN {
        numeric = "^-?[0-9]+(\\.[0-9]+)?$"
        if (lat !~ numeric || lon !~ numeric) exit 1
        if (lat < -90 || lat > 90 || lon < -180 || lon > 180) exit 1
    }
'; then
    echo "invalid receiver coordinates: RSDB_RECEIVER_LAT=$RSDB_RECEIVER_LAT RSDB_RECEIVER_LON=$RSDB_RECEIVER_LON" >&2
    exit 1
fi

shell_quote() {
    printf "'%s'" "$(printf "%s" "$1" | sed "s/'/'\\\\''/g")"
}

cd "$RSDB_REPO_ROOT"

if [ -f package.json ]; then
    bun install --frozen-lockfile
    bun run build:web
    bun run check:web
fi

if [ -f "$key_path" ]; then
    ssh_cmd="$ssh_cmd -i $key_path -o IdentitiesOnly=yes"
fi

rsync -az --delete \
    -e "$ssh_cmd" \
    --exclude .env \
    --exclude .git \
    --exclude node_modules \
    --exclude target \
    --exclude pi/.env \
    --exclude pi/backups \
    --exclude pi/images \
    --exclude pi/secrets \
    ./ "$target:~/rsdb/"

if [ -n "${RSDB_RECEIVER_SEED_PATH:-}" ]; then
    if [ ! -f "$RSDB_RECEIVER_SEED_PATH" ]; then
        echo "missing receiver seed: $RSDB_RECEIVER_SEED_PATH" >&2
        exit 1
    fi

    rsync -az \
        -e "$ssh_cmd" \
        "$RSDB_RECEIVER_SEED_PATH" \
        "$target:/tmp/rsdb-receiver.seed"
    $ssh_cmd "$target" 'sudo install -d -m 0755 /etc/rsdb && sudo install -m 0600 -o "$(id -un)" -g "$(id -gn)" /tmp/rsdb-receiver.seed /etc/rsdb/receiver.seed && rm -f /tmp/rsdb-receiver.seed'
fi

quoted_lat="$(shell_quote "$RSDB_RECEIVER_LAT")"
quoted_lon="$(shell_quote "$RSDB_RECEIVER_LON")"
remote_env="RSDB_RECEIVER_LAT=$quoted_lat RSDB_RECEIVER_LON=$quoted_lon"
if [ -n "${RSDB_SUBMIT_URLS:-}" ]; then
    remote_env="$remote_env RSDB_SUBMIT_URLS=$(shell_quote "$RSDB_SUBMIT_URLS")"
fi

$ssh_cmd "$target" "cd ~/rsdb && $remote_env ./pi/provision-pi.sh"
