#!/usr/bin/env bash
# File: scripts/test-ci-locally.sh
# Local build & check script for gldf-search.
#
# Mirrors what CI would run: fmt → clippy on the leptos lib (both SSR
# and hydrate sides) → SSR check → WASM check → server bin check.
# Package names come from .env where available so a fork only needs
# to edit .env, not this script.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Source .env if present so $BIN_NAME and friends are available.
# Same `set -a` pattern as deploy_to_server.sh.
ENV_FILE="${ENV_FILE:-.env}"
if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
fi

# Crate names. Defaults match the gldf-search workspace; overridable
# from .env via FRONTEND_PKG / SERVER_PKG if a fork ever needs to.
FRONTEND_PKG="${FRONTEND_PKG:-gldf-search-leptos}"
SERVER_PKG="${SERVER_PKG:-${BIN_NAME:-gldf-search-server}}"

echo -e "${YELLOW}=== gldf-search local checks ===${NC}"
echo -e "    frontend: $FRONTEND_PKG"
echo -e "    server:   $SERVER_PKG\n"

# Step 1: cargo fmt — per-package, NOT `--all`.
#
# `cargo fmt --all` walks into sibling workspaces reachable via
# path-deps (e.g. our `../gldf-rs` and `../eulumdat-rs`) and tries to
# format THEIR sources too. That's both rude (we don't own those
# repos in this CI scope) and breaks: their fmt status is unrelated
# to our checks. Iterate explicitly over our own workspace members.
echo -e "${YELLOW}Step 1: cargo fmt check...${NC}"
fmt_failed=0
for pkg in gldf-search-schema gldf-search-gldf gldf-search-index \
           gldf-search-leptos gldf-search-server gldf-search-cli; do
    if ! cargo fmt -p "$pkg" -- --check; then
        echo -e "${RED}✗ fmt failed for $pkg${NC}"
        fmt_failed=1
    fi
done
if (( fmt_failed == 0 )); then
    echo -e "${GREEN}✓ fmt passed${NC}\n"
else
    echo -e "${YELLOW}Run 'cargo fmt -p <pkg>' to fix${NC}\n"
    exit 1
fi

# Step 2: clippy (SSR)
echo -e "${YELLOW}Step 2: clippy (SSR)...${NC}"
if cargo clippy --package "$FRONTEND_PKG" --features ssr -- -D warnings; then
    echo -e "${GREEN}✓ clippy SSR passed${NC}\n"
else
    echo -e "${RED}✗ clippy SSR failed${NC}\n"
    exit 1
fi

# Step 3: check SSR build
echo -e "${YELLOW}Step 3: cargo check (SSR)...${NC}"
if cargo check --package "$FRONTEND_PKG" --features ssr; then
    echo -e "${GREEN}✓ SSR build passed${NC}\n"
else
    echo -e "${RED}✗ SSR build failed${NC}\n"
    exit 1
fi

# Step 4: check WASM/hydrate build
echo -e "${YELLOW}Step 4: cargo check (WASM hydrate)...${NC}"
if cargo check --package "$FRONTEND_PKG" --features hydrate --target wasm32-unknown-unknown; then
    echo -e "${GREEN}✓ WASM hydrate build passed${NC}\n"
else
    echo -e "${RED}✗ WASM hydrate build failed${NC}\n"
    exit 1
fi

# Step 5: check server build (native host target).
echo -e "${YELLOW}Step 5: cargo check (server)...${NC}"
if cargo check --package "$SERVER_PKG"; then
    echo -e "${GREEN}✓ server build passed${NC}\n"
else
    echo -e "${RED}✗ server build failed${NC}\n"
    exit 1
fi

echo -e "\n${GREEN}=== All checks passed! ===${NC}"
