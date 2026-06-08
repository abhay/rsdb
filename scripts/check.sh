#!/usr/bin/env sh
set -eu

scripts/check-fast.sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
