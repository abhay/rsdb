#!/usr/bin/env sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RSDB_SCRIPT_DIR="$script_dir"
. "$script_dir/load-env.sh"

key_path="${1:-$RSDB_NODE_SSH_KEY}"
key_comment="${RSDB_NODE_HOSTNAME:-rsdb-node}"

case "$key_path" in
    /*) ;;
    *) key_path="$RSDB_REPO_ROOT/$key_path" ;;
esac

if [ -e "$key_path" ] || [ -e "$key_path.pub" ]; then
    echo "refusing to overwrite existing key: $key_path" >&2
    exit 1
fi

mkdir -p "$(dirname "$key_path")"
ssh-keygen -t ed25519 -a 64 -f "$key_path" -C "$key_comment" -N ""
chmod 600 "$key_path"

echo
echo "Private key: $key_path"
echo "Public key:  $key_path.pub"
echo
echo "Paste this public key into Raspberry Pi Imager's SSH public key field:"
cat "$key_path.pub"
