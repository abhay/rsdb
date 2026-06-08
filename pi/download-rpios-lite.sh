#!/usr/bin/env sh
set -eu

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
RSDB_SCRIPT_DIR="$script_dir"
. "$script_dir/load-env.sh"

url="${1:-https://downloads.raspberrypi.org/raspios_lite_arm64_latest}"
output="${2:-pi/images/raspios-lite-arm64.img.xz}"

case "$output" in
    /*) ;;
    *) output="$RSDB_REPO_ROOT/$output" ;;
esac

mkdir -p "$(dirname "$output")"

echo "Downloading Raspberry Pi OS Lite:"
echo "  URL:    $url"
echo "  Output: $output"

curl -fL --retry 3 -o "$output" "$url"

echo
echo "Downloaded file SHA256:"
shasum -a 256 "$output"
