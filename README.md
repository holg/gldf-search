# gldf-search

[![License: AGPL v3+](https://img.shields.io/badge/License-AGPL--3.0--or--later-blue.svg)](LICENSE)

**Lichtdatenbank** — a Rust search index and Leptos web UI over the
public GLDF / EULUMDAT corpus of luminaire data.

> **Licensed under the GNU Affero General Public License v3 or later
> (AGPL-3.0-or-later).** See [LICENSE](LICENSE) for the full text. The
> AGPL is a strong copyleft licence: if you run a modified version of
> this software over a network and let others interact with it, you
> must make the corresponding source code available to those users.
> Dual-licensing for commercial use is on the roadmap but not yet
> offered — contact the author if you need different terms.

Lighting designers today need DIALux, Relux, or per-vendor portals to
discover photometric files. `gldf-search` is an open, browser-capable
alternative: it indexes hundreds of thousands of `.gldf` archives,
mines facet metadata out of them, and serves a filter-driven search
UI with per-variant polar curves and product images rendered on demand.

The same `Filter` type runs the canonical reference predicates in the
schema crate, the linear-scan in-memory index in the server, and (in
future iterations) the on-disk `.ldb` mmap index. That predicate
identity is the load-bearing design rule.

## Architecture

Six-crate Cargo workspace:

| Crate | Role |
|---|---|
| `gldf-search-schema` | `LuminaireDoc` / `VariantDoc` / `Filter` + canonical XSD enum tables, URL codec for shareable filter links. WASM-clean. |
| `gldf-search-gldf` | Corpus extractor over [`gldf-rs`], LDT parser via [`eulumdat-rs`], name-mining for sparse-metadata corpora, bincode cache with magic-versioned schema, sanity caps for parser outliers. |
| `gldf-search-index` | `InMemoryIndex` (linear-scan reference), facets with relax-this-axis semantics, dynamic numeric range bounds, **group-and-fold** dedup for `(manufacturer, product)` families with DocId aliases that preserve old URLs, variant-grain `SearchHit`. |
| `gldf-search-leptos` | Leptos SSR + WASM hydrate UI: facet panel, dual-handle range sliders, URL ↔ filter sync, lazy per-variant polar + image, slim wire payload (`VariantPayloadSlim`). |
| `gldf-search-server` | Axum binary. `X-Manufacturer` header → scoped subdomain (`philips.gldf-search.de`). `spawn_blocking` around the index hot paths. |
| `gldf-search-cli` | `build-index`, `inspect`, `search`. Examples for sizing reports, Zstd repack benchmark, manufacturer listings, single-mfr corpus probes. |

External dependencies (sibling crates, expected as path-deps at
`../gldf-rs` and `../eulumdat-rs`):

- [`gldf-rs`] — primary GLDF parser (the upstream we depend on).
- [`eulumdat-rs`] — EULUMDAT/LDT parser (used by `gldf-rs` and
  re-used directly client-side via the WASM bundle to render polars).

## Quick start

```bash
# 1. Build the corpus index (writes <repo>/.index/gldf-search.bin).
#    Reads LOCAL_GLDF_SEARCH_CORPUS from .env (the full corpus tree).
./scripts/build_index_local.sh

# 2. Run the dev server (SSR + WASM hydrate, port from .env).
cargo leptos serve
```

The server reads the path of the index cache from `GLDF_SEARCH_INDEX_CACHE`
and the corpus root from `GLDF_SEARCH_CORPUS` / `LOCAL_GLDF_SEARCH_CORPUS`.
Both come from `.env` (untracked) — see `.env` in your fork for the
full key list.

## Local CI

`scripts/test-ci-locally.sh` runs the same five checks the GitHub
Actions workflow runs. Run it before every push:

```bash
./scripts/test-ci-locally.sh
```

Steps:

1. **fmt** — `cargo fmt --all -- --check`
2. **clippy (SSR)** — `cargo clippy --package gldf-search-leptos --features ssr -- -D warnings`
3. **check (SSR)** — `cargo check --package gldf-search-leptos --features ssr`
4. **check (WASM hydrate)** — `cargo check --package gldf-search-leptos --features hydrate --target wasm32-unknown-unknown`
5. **check (server)** — `cargo check --package gldf-search-server`

GitHub Actions mirrors this in `.github/workflows/ci.yml` on every
push and pull-request.

## Deploy

Production runs on a single Linux host behind nginx. The deploy
script bundles binary + hashed Leptos assets + (optionally) the
extracted-doc cache and rsyncs them in one shot:

```bash
# Cross-build the Linux binary on macOS.
./scripts/cross_build_on_mac.sh x86_64-unknown-linux-gnu

# Ship binary + assets only (cache stays untouched on prod).
./scripts/deploy_to_server.sh deploy

# Ship binary + assets + cache (use after a CACHE_MAGIC bump).
./scripts/deploy_to_server.sh --with-index deploy
```

All deploy targets and secrets live in `.env` (gitignored).

## Wire format / cache magic

The index cache (`.index/gldf-search.bin` locally, `index.bin` on
prod) is bincode-encoded `Vec<LuminaireDoc>` prefixed with an 8-byte
magic / version (`GSCACHE\xNN`). Old caches are rejected on the
magic byte and the operator must rebuild. See
`crates/gldf-search-gldf/src/cache.rs` for the full version history
and what each bump changed.

## Examples

Several diagnostic binaries live under
`crates/gldf-search-cli/examples/`:

```bash
# RAM cost per field + dedup-potential report on the live cache.
GLDF_SEARCH_INDEX_CACHE=$PWD/.index/gldf-search.bin \
  cargo run --release --example report_sizes -p gldf-search-cli -- --folded

# Distinct manufacturer strings + suggested subdomain slugs.
GLDF_SEARCH_INDEX_CACHE=$PWD/.index/gldf-search.bin \
  cargo run --release --example list_manufacturers -p gldf-search-cli

# Zstd-vs-deflate sizing benchmark on a sample of the corpus.
cargo run --release --example zstd_repack_bench -p gldf-search-cli -- \
  --mode top-largest --sample 100 --level 19

# Single-manufacturer extraction probe (sanity check before adding to corpus).
cargo run --release --example probe_deko -p gldf-search-cli -- \
  /path/to/corpus/<mfr-dir>
```

## License

This software is released under the **GNU Affero General Public
License version 3 or later (AGPL-3.0-or-later)**. The full text is in
[`LICENSE`](LICENSE).

Key implication: if you host a modified version of `gldf-search` (or
any AGPL-derived service) and let other people interact with it, you
have to provide them the source of your version. This is intentional
— it keeps a forked-and-extended Lichtdatenbank in the open even when
the fork only runs as a service. Plain library use inside another
AGPL or AGPL-compatible application is fine.

Dual-licensing for commercial use is on the roadmap but not yet
offered.

[`gldf-rs`]: https://github.com/holg/gldf-rs
[`eulumdat-rs`]: https://github.com/holg/eulumdat-rs
