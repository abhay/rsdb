#!/usr/bin/env sh
set -eu

usage() {
    cat <<'USAGE'
Usage:
  ./pi/add-allowlisted-receiver.sh <allowlist.txt> <public-key-file> [...]

The allowlist file is created when it does not exist. Files contain one or more
64-hex Ed25519 public keys separated by commas, whitespace, or newlines. Lines
can include # comments.
USAGE
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi

if [ "$#" -lt 2 ]; then
    usage >&2
    exit 1
fi

allowlist_path="$1"
shift

allowlist_dir="$(dirname -- "$allowlist_path")"
mkdir -p "$allowlist_dir"

tmp_base=""
tmp_output=""
tmp_unsorted=""
cleanup() {
    [ -z "$tmp_base" ] || rm -f "$tmp_base"
    [ -z "$tmp_output" ] || rm -f "$tmp_output"
    [ -z "$tmp_unsorted" ] || rm -f "$tmp_unsorted"
}
trap cleanup EXIT INT TERM

if [ -f "$allowlist_path" ]; then
    base_path="$allowlist_path"
else
    tmp_base="$(mktemp)"
    : >"$tmp_base"
    base_path="$tmp_base"
fi

tmp_output="$(mktemp "$allowlist_dir/.allowlist.XXXXXX")"
tmp_unsorted="$(mktemp)"

awk '
    {
        sub(/#.*/, "")
        gsub(/,/, " ")
        for (i = 1; i <= NF; i++) {
            key = tolower($i)
            if (key !~ /^[0-9a-f]{64}$/) {
                printf "%s: invalid public key: %s\n", FILENAME, $i > "/dev/stderr"
                exit 1
            }
            keys[key] = 1
        }
    }
    END {
        for (key in keys) print key
    }
' "$base_path" "$@" >"$tmp_unsorted"

sort "$tmp_unsorted" >"$tmp_output"

if [ ! -s "$tmp_output" ]; then
    echo "no public keys supplied" >&2
    exit 1
fi

mv "$tmp_output" "$allowlist_path"
tmp_output=""

cat <<EOF
Updated allowlist: $allowlist_path

Public keys:
EOF
sed 's/^/  - /' "$allowlist_path"

cat <<EOF

For Fly.io, commit $allowlist_path and redeploy. The Docker image copies this
file into /etc/rsdb/allowlist.txt.
EOF
