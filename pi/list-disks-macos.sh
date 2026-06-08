#!/usr/bin/env sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
    echo "This helper is macOS-only." >&2
    exit 1
fi

diskutil list external physical
