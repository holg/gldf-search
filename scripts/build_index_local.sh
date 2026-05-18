#!/usr/bin/env bash
# Build the bincode index cache locally (multi-core) without
# uploading anywhere. Useful for testing schema changes / new
# extractors before shipping the cache to production.
#
# This is the local-only half of `sync_index_to_server.sh`. By
# default the cache is written to `<repo>/.index/gldf-search.bin` —
# gitignored, NOT under `target/` (so `cargo clean` doesn't wipe an
# 8-minute rebuild), and out of `/tmp` (where macOS may sweep it).
#
# Reads from the same `.env` (or `.env.<profile>`) as
# `sync_index_to_server.sh`. Required / honoured keys:
#
#   LOCAL_GLDF_SEARCH_CORPUS  — local corpus root (the full tree).
#                              Falls back to GLDF_SEARCH_CORPUS.
#                              `DEBUG_GLDF_SEARCH_CORPUS` is NOT
#                              honoured — that key is a server-side
#                              dev override and may point at a
#                              sample dir; the local build always
#                              wants the full corpus.
#   LOCAL_GLDF_SEARCH_INDEX_CACHE — optional override for the output
#                              path. Use when you want to write
#                              somewhere other than `<repo>/.index/`.
#                              `GLDF_SEARCH_INDEX_CACHE` (the prod-
#                              side key) is NOT honoured here — it
#                              usually points at `/var/www/...` which
#                              isn't writable locally.
#
# Usage:
#   ./scripts/build_index_local.sh                 # uses .env
#   ./scripts/build_index_local.sh -e davids       # uses .env.davids
#   ./scripts/build_index_local.sh --threads 8     # cap rayon pool
#   ./scripts/build_index_local.sh --out /tmp/x.bin  # override output
#
# Restart the server afterwards with:
#   ./target/release-prod/gldf-search-server

set -euo pipefail

# ── argv parsing ──────────────────────────────────────────────────────
ENV_PROFILE=""
THREADS=0
OUT_OVERRIDE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -e|--env)
            [[ -z "${2:-}" ]] && { echo "✗ $1 needs a profile name" >&2; exit 1; }
            ENV_PROFILE="$2"; shift 2 ;;
        --threads)
            [[ -z "${2:-}" ]] && { echo "✗ --threads needs N" >&2; exit 1; }
            THREADS="$2"; shift 2 ;;
        --out)
            [[ -z "${2:-}" ]] && { echo "✗ --out needs a path" >&2; exit 1; }
            OUT_OVERRIDE="$2"; shift 2 ;;
        -h|--help)
            sed -n '2,30p' "$0"; exit 0 ;;
        *)
            echo "✗ unknown arg: $1" >&2; exit 1 ;;
    esac
done

# ── load .env ─────────────────────────────────────────────────────────
ENV_FILE=".env"
[[ -n "$ENV_PROFILE" ]] && ENV_FILE=".env.$ENV_PROFILE"
if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    . "$ENV_FILE"
    set +a
fi

# ── resolve paths ─────────────────────────────────────────────────────
#
# Resolution order for the corpus path:
#   1. `LOCAL_GLDF_SEARCH_CORPUS` — explicit local opt-in.
#   2. `GLDF_SEARCH_CORPUS`        — the production-side key, works
#                                    locally too when the path resolves.
#   3. error.
#
# `DEBUG_GLDF_SEARCH_CORPUS` is INTENTIONALLY NOT honoured here. That
# key is for the running server's dev-override path (often a small
# sample dir for fast iteration); the local index build always wants
# the full corpus.
LOCAL_CORPUS="${LOCAL_GLDF_SEARCH_CORPUS:-${GLDF_SEARCH_CORPUS:-}}"
if [[ -z "$LOCAL_CORPUS" ]]; then
    echo "✗ no local corpus path — set LOCAL_GLDF_SEARCH_CORPUS or GLDF_SEARCH_CORPUS in $ENV_FILE (or in your shell)" >&2
    exit 1
fi
if [[ ! -d "$LOCAL_CORPUS" ]]; then
    echo "✗ corpus directory not found: $LOCAL_CORPUS" >&2
    echo "  (resolved from \$LOCAL_GLDF_SEARCH_CORPUS or \$GLDF_SEARCH_CORPUS)" >&2
    exit 1
fi

if [[ -n "$OUT_OVERRIDE" ]]; then
    OUT_CACHE="$OUT_OVERRIDE"
elif [[ -n "${LOCAL_GLDF_SEARCH_INDEX_CACHE:-}" ]]; then
    # Operator-set local override wins. Used when the deploy cache
    # path (`GLDF_SEARCH_INDEX_CACHE`) points at a server-only path
    # like `/var/www/...` that doesn't exist locally.
    OUT_CACHE="$LOCAL_GLDF_SEARCH_INDEX_CACHE"
else
    # Default: a `.index/` directory at the workspace root. Dot-
    # prefixed so it doesn't clutter `ls`, gitignored, and — crucially
    # — NOT under `target/` so `cargo clean` doesn't nuke an
    # 8-minute rebuild. `GLDF_SEARCH_INDEX_CACHE` from `.env` is
    # intentionally ignored here: it usually points at the production
    # server path (`/var/www/...`) which we can't write to locally.
    REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
    OUT_CACHE="$REPO_ROOT/.index/gldf-search.bin"
fi
mkdir -p "$(dirname "$OUT_CACHE")"

echo "─── Local index build ───────────────────────────────────────────"
echo "corpus  : $LOCAL_CORPUS"
echo "out     : $OUT_CACHE"
echo "threads : ${THREADS:-all}"
echo

cargo build --release -p gldf-search-cli

./target/release/gldf-search corpus build-index \
    --root "$LOCAL_CORPUS" \
    --out  "$OUT_CACHE" \
    --threads "$THREADS"

ls -lh "$OUT_CACHE"
echo "✓ cache built at $OUT_CACHE"
echo
echo "next:"
echo "  GLDF_SEARCH_INDEX_CACHE='$OUT_CACHE' \\"
echo "  DEBUG_GLDF_SEARCH_CORPUS='$LOCAL_CORPUS' \\"
echo "  ./target/release-prod/gldf-search-server"
