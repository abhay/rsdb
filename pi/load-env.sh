#!/usr/bin/env sh

if [ -z "${RSDB_SCRIPT_DIR:-}" ]; then
    echo "RSDB_SCRIPT_DIR is required before sourcing load-env.sh" >&2
    exit 1
fi

RSDB_REPO_ROOT="$(CDPATH= cd -- "$RSDB_SCRIPT_DIR/.." && pwd)"

if [ -f "$RSDB_REPO_ROOT/.env" ]; then
    set -a
    . "$RSDB_REPO_ROOT/.env"
    set +a
fi

if [ -f "$RSDB_SCRIPT_DIR/.env" ]; then
    set -a
    . "$RSDB_SCRIPT_DIR/.env"
    set +a
fi

RSDB_NODE_USER="${RSDB_NODE_USER:-rsdb}"
RSDB_NODE_HOSTNAME="${RSDB_NODE_HOSTNAME:-}"
if [ -n "$RSDB_NODE_HOSTNAME" ]; then
    RSDB_NODE_DEFAULT_HOST="$RSDB_NODE_HOSTNAME.local"
else
    RSDB_NODE_DEFAULT_HOST=""
fi
RSDB_NODE_HOST="${RSDB_NODE_HOST:-$RSDB_NODE_DEFAULT_HOST}"
RSDB_AGGREGATE_PORT="${RSDB_AGGREGATE_PORT:-8090}"
RSDB_NODE_SSH_KEY="${RSDB_NODE_SSH_KEY:-pi/secrets/rsdb_node_ed25519}"
