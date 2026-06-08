#!/usr/bin/env sh
set -eu

bun run build:web
bun run check:web
cargo fmt --check

for script in pi/*.sh scripts/*.sh; do
    [ -f "$script" ] || continue
    sh -n "$script"
done
