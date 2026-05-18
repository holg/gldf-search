#!/usr/bin/env bash
# Build the bincode index cache locally (multi-core), rsync it to the
# server, atomically swap it into place, and restart the systemd unit.
#
# Why: on the production host we keep the server pinned to a single
# core (per ops constraints). Extracting 270k+ GLDFs on a single core
# takes hours; on a fast laptop with rayon over all cores it takes
# minutes. The bincode cache file is the same format on both ends —
# build it where it's cheap, copy it where it's expensive.
#
# Requires the same `.env` (or `.env.<profile>`) as
# `deploy_to_server.sh`, plus:
#
#   LOCAL_GLDF_SEARCH_CORPUS  — local corpus root (the full 270k tree)
#                              Falls back to DEBUG_GLDF_SEARCH_CORPUS,
#                              then GLDF_SEARCH_CORPUS.
#   APP_NAME                  — systemd unit name (same as deploy)
#   SSH_HOST                  — SSH target (same as deploy)
#   GLDF_SEARCH_INDEX_CACHE   — absolute path of the cache file on the
#                              remote host (same key the server reads)
#
# Run from your local machine; the script never touches the server's
# corpus directory — only the cache file. The server is then started
# with the trust-cache code path (loads the bincode file as-is,
# no freshness check).
#
# Usage:
#   ./scripts/sync_index_to_server.sh                 # uses .env
#   ./scripts/sync_index_to_server.sh -e davids       # uses .env.davids
#   ./scripts/sync_index_to_server.sh --threads 8     # cap rayon pool
#   ./scripts/sync_index_to_server.sh --no-restart    # ship cache, leave server
#   ./scripts/sync_index_to_server.sh --cache <path>  # reuse an existing
#                                                       cache (skip local
#                                                       build entirely)

set -euo pipefail

# ── argv parsing ──────────────────────────────────────────────────────
ENV_PROFILE=""
THREADS=0
RESTART=1
EXISTING_CACHE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -e|--env)
            [[ -z "${2:-}" ]] && { echo "✗ $1 needs a profile name" >&2; exit 1; }
            ENV_PROFILE="$2"; shift 2 ;;
        --threads)
            [[ -z "${2:-}" ]] && { echo "✗ --threads needs N" >&2; exit 1; }
            THREADS="$2"; shift 2 ;;
        --no-restart)
            RESTART=0; shift ;;
        --cache)
            [[ -z "${2:-}" ]] && { echo "✗ --cache needs a path" >&2; exit 1; }
            EXISTING_CACHE="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,30p' "$0"; exit 0 ;;
        *)
            echo "✗ unknown arg: $1" >&2; exit 1 ;;
    esac
done

# ── load .env ─────────────────────────────────────────────────────────
ENV_FILE=".env"
[[ -n "$ENV_PROFILE" ]] && ENV_FILE=".env.$ENV_PROFILE"
if [[ ! -f "$ENV_FILE" ]]; then
    echo "✗ $ENV_FILE not found" >&2
    exit 1
fi
set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a

# ── resolve required values ───────────────────────────────────────────
: "${APP_NAME:?APP_NAME missing from $ENV_FILE}"
: "${SSH_HOST:?SSH_HOST missing from $ENV_FILE}"
: "${GLDF_SEARCH_INDEX_CACHE:?GLDF_SEARCH_INDEX_CACHE missing from $ENV_FILE}"

REMOTE_CACHE="$GLDF_SEARCH_INDEX_CACHE"

if [[ -n "$EXISTING_CACHE" ]]; then
    # Reuse-an-existing-cache mode: skip the local build entirely.
    # The caller has already produced a cache file (typically via
    # `./scripts/build_index_local.sh`) and just wants it shipped.
    if [[ ! -f "$EXISTING_CACHE" ]]; then
        echo "✗ --cache file not found: $EXISTING_CACHE" >&2
        exit 1
    fi
    LOCAL_CACHE="$EXISTING_CACHE"
    # No tempfile, no trap — the caller owns this file.
    echo "─── Reusing existing cache ───────────────────────────────────────"
    echo "cache   : $LOCAL_CACHE"
    ls -lh "$LOCAL_CACHE"
else
    LOCAL_CORPUS="${LOCAL_GLDF_SEARCH_CORPUS:-${DEBUG_GLDF_SEARCH_CORPUS:-${GLDF_SEARCH_CORPUS:-}}}"
    if [[ -z "$LOCAL_CORPUS" ]]; then
        echo "✗ no local corpus path — set LOCAL_GLDF_SEARCH_CORPUS or DEBUG_GLDF_SEARCH_CORPUS in $ENV_FILE" >&2
        exit 1
    fi
    if [[ ! -d "$LOCAL_CORPUS" ]]; then
        echo "✗ local corpus directory not found: $LOCAL_CORPUS" >&2
        exit 1
    fi

    LOCAL_CACHE="$(mktemp -t gldf-search-cache.XXXXXX.bin)"
    trap 'rm -f "$LOCAL_CACHE"' EXIT

    echo "─── Local index build ───────────────────────────────────────────"
    echo "corpus  : $LOCAL_CORPUS"
    echo "out     : $LOCAL_CACHE"
    echo "threads : ${THREADS:-all}"
    echo

    cargo build --release -p gldf-search-cli

    ./target/release/gldf-search corpus build-index \
        --root "$LOCAL_CORPUS" \
        --out  "$LOCAL_CACHE" \
        --threads "$THREADS"

    ls -lh "$LOCAL_CACHE"
fi
echo

echo "─── Upload to $SSH_HOST ─────────────────────────────────────────"
echo "remote  : $REMOTE_CACHE"
echo

# Stage the file as .new, then atomic rename — so a half-uploaded
# file never shadows a working cache. `--rsync-path=…sudo rsync` is
# left out: the deploy convention puts /var/www/<host> under the
# unprivileged deploy user, which already owns the cache.
rsync --progress -avz "$LOCAL_CACHE" "$SSH_HOST:${REMOTE_CACHE}.new"
ssh "$SSH_HOST" "mv ${REMOTE_CACHE}.new ${REMOTE_CACHE}"

if (( RESTART )); then
    echo
    echo "─── Restart $APP_NAME on $SSH_HOST ──────────────────────────────"
    ssh "$SSH_HOST" "sudo systemctl restart ${APP_NAME}"
    echo "restart issued; server reads the new cache on next boot"
else
    echo
    echo "skipped restart (--no-restart). Cache is in place at $REMOTE_CACHE."
fi
