// Leptos's typed view tree gets very deep with our 6 dual-handle
// sliders + the rest of the search page. The default recursion limit
// (128) is enough for `cargo build` debug, but `release-prod` (fat LTO,
// codegen-units=1) and the wasm cross-build trip the type-resolution
// depth limit. 512 is comfortably above what `cargo leptos build
// --release` needs today; bump again if a future Leptos version
// nests more.
#![recursion_limit = "512"]

//! `gldf-search-server` — Axum binary that loads a directory of
//! `.gldf` files at startup, builds an [`InMemoryIndex`], and exposes
//! the gldf-search query API.
//!
//! No Leptos here yet. The static file route also serves the source
//! `.gldf` files as downloads, so a future browser UI can let users
//! click "download original" without an extra mount.
//!
//! Index is held in `Arc<InMemoryIndex>` — clone-cheap, no locking
//! because the index is immutable after startup. Hot-reload from disk
//! would require an `Arc<RwLock<_>>`; that's a v2 concern.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use gldf_search_index::InMemoryIndex;
use gldf_search_leptos::api::ssr::CorpusRoot;
use gldf_search_leptos::{
    shell, App, FetchDoc, FetchDocs, FetchFacets, FetchLdt, IndexHandle, LookupArticle, SearchDocs,
    SuggestArticles, SuggestFacets,
};
use gldf_search_schema::ViewerConfig;
use leptos::prelude::*;
use leptos_axum::{generate_route_list, LeptosRoutes};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

mod api;
mod corpus;
mod state;

use state::AppState;

#[derive(Parser, Debug)]
#[command(
    name = "gldf-search-server",
    version,
    about = "gldf-search Axum server"
)]
struct Cli {
    /// Directory of `.gldf` files to load at startup.
    ///
    /// Resolution order (see [`resolve_corpus_root`]):
    /// 1. `--corpus-root` on the command line if given
    /// 2. `DEBUG_GLDF_SEARCH_CORPUS` env var if set and non-empty
    /// 3. `GLDF_SEARCH_CORPUS` env var
    /// 4. error out — no sensible default for "where are the GLDFs?"
    #[arg(long)]
    corpus_root: Option<PathBuf>,

    /// Bind address. Defaults to `LEPTOS_SITE_ADDR` (matches the
    /// cargo-leptos / nginx upstream convention) or `127.0.0.1:3090`
    /// when unset.
    #[arg(long, env = "LEPTOS_SITE_ADDR", default_value = "127.0.0.1:3090")]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env first so subsequent `EnvFilter::from_default_env()`
    // and `clap`'s `env =` reads see the keys. Silent failure is
    // intentional — production deploys may not ship a `.env` and
    // rely on the real environment.
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,gldf_search_server=debug")),
        )
        .init();

    let cli = Cli::parse();

    let viewer_config = resolve_viewer_config();
    tracing::info!(
        public_base = %viewer_config.public_base,
        viewer_base = %viewer_config.viewer_base,
        query_param = %viewer_config.query_param,
        "viewer config resolved"
    );

    // Explicit server-fn registration. Belt-and-suspenders for the
    // case where `inventory`-based auto-registration is GC'd by the
    // linker (observed on macOS with the multi-crate split).
    leptos::server_fn::axum::register_explicit::<SearchDocs>();
    leptos::server_fn::axum::register_explicit::<FetchFacets>();
    leptos::server_fn::axum::register_explicit::<FetchDoc>();
    leptos::server_fn::axum::register_explicit::<FetchDocs>();
    leptos::server_fn::axum::register_explicit::<LookupArticle>();
    leptos::server_fn::axum::register_explicit::<SuggestArticles>();
    leptos::server_fn::axum::register_explicit::<SuggestFacets>();
    leptos::server_fn::axum::register_explicit::<FetchLdt>();
    for (path, method) in leptos::server_fn::axum::server_fn_paths() {
        tracing::info!(path = %path, method = ?method, "server fn registered");
    }

    let corpus_root = resolve_corpus_root(cli.corpus_root.as_deref())?;
    let cache_path = corpus::resolve_cache_path(&corpus_root);
    tracing::info!(
        corpus = %corpus_root.display(),
        cache = ?cache_path.as_deref().map(Path::display),
        "loading corpus"
    );
    let load_start = std::time::Instant::now();
    let docs = corpus::load_with_cache(&corpus_root, cache_path.as_deref())?;
    let n_docs = docs.len();
    // Group-and-fold dedup: collapse per-SKU GLDFs that share
    // `(manufacturer, product)` into one family doc with multi-
    // source paths + per-variant `source_index`. Old per-SKU DocIds
    // remain valid via `by_id` aliases — bookmarks survive.
    let (folded_index, fold_stats) = InMemoryIndex::from_docs_folded(docs);
    let index = Arc::new(folded_index);
    tracing::info!(
        elapsed_ms = load_start.elapsed().as_millis() as u64,
        input_docs = n_docs,
        output_docs = fold_stats.output_docs,
        variants = fold_stats.output_variants,
        aliases = fold_stats.aliases,
        reduction_pct = format!(
            "{:.1}",
            100.0 * (n_docs as f64 - fold_stats.output_docs as f64) / n_docs.max(1) as f64
        ),
        "index built (group-and-fold applied)"
    );

    let state = AppState {
        index: index.clone(),
        corpus_root: corpus_root.clone(),
    };

    // Leptos configuration. In `cargo leptos serve` the LEPTOS_*
    // env vars are exported; for plain `cargo run` we synthesise
    // sensible defaults so the server boots without the build tool.
    let leptos_options = leptos::config::get_configuration(None)
        .map(|c| c.leptos_options)
        .unwrap_or_else(|_| {
            LeptosOptions::builder()
                .output_name("gldf-search-leptos")
                .build()
        });
    let leptos_routes = generate_route_list(App);

    // Install the index handle + viewer config into every server-fn
    // invocation and every SSR page render. Both closures clone-once
    // per request; cloning the Arc and the small config struct is
    // cheap.
    let index_for_ctx = index.clone();
    let viewer_for_ctx = viewer_config.clone();
    let corpus_for_ctx = corpus_root.clone();
    let provide_index = move || {
        provide_context(IndexHandle(index_for_ctx.clone()));
        provide_context(viewer_for_ctx.clone());
        provide_context(CorpusRoot(corpus_for_ctx.clone()));
    };

    // Server-fn handler that runs each `#[server]` body inside an
    // Axum extractor. We need the context-aware variant so the
    // `IndexHandle` Leptos sees during a search-fn call matches the
    // one installed for SSR page renders.
    let index_for_server_fns = index.clone();
    let viewer_for_server_fns = viewer_config.clone();
    let corpus_for_server_fns = corpus_root.clone();
    let server_fn_handler = move |req: axum::extract::Request| {
        let index = index_for_server_fns.clone();
        let viewer = viewer_for_server_fns.clone();
        let corpus = corpus_for_server_fns.clone();
        async move {
            leptos_axum::handle_server_fns_with_context(
                move || {
                    provide_context(IndexHandle(index.clone()));
                    provide_context(viewer.clone());
                    provide_context(CorpusRoot(corpus.clone()));
                },
                req,
            )
            .await
        }
    };

    // Build the Leptos sub-router with `LeptosOptions` as its state.
    // We merge the no-state api/static routers in afterwards.
    let leptos_router: axum::Router<LeptosOptions> = axum::Router::new()
        // Server functions (#[server] macros): POST `/api/leptos/*`
        // catch-all driven by leptos_axum's generic handler. Must be
        // registered *before* the page routes so it wins the URL.
        .route(
            "/api/leptos/{*fn_name}",
            axum::routing::post(server_fn_handler.clone()).get(server_fn_handler),
        )
        .leptos_routes_with_context(&leptos_options, leptos_routes, provide_index, {
            let opts = leptos_options.clone();
            move || shell(opts.clone())
        })
        // Cargo-leptos drops the bundle here. Fallback lets the server
        // still boot when the bundle hasn't been built yet (plain
        // `cargo run` — the API works, the page just won't have JS).
        .fallback_service(ServeDir::new("target/site"));

    let app = axum::Router::new()
        // Our hand-rolled /api/* endpoints (curl-friendly, stable
        // wire format independent of Leptos's auto-generated routes).
        .merge(api::routes(state.clone()))
        // Static passthrough of the source .gldf directory. A future
        // UI's "download original" link points here.
        .nest_service("/gldfs", ServeDir::new(&corpus_root).precompressed_gzip())
        // Strip the LeptosOptions state so the sub-router fits into
        // this stateless one.
        .merge(leptos_router.with_state(leptos_options))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive());

    tracing::info!(addr = %cli.listen, "server listening");
    let listener = tokio::net::TcpListener::bind(cli.listen).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Read the viewer/public URL env vars and resolve a [`ViewerConfig`].
///
/// Rules:
/// - `DEBUG_*` variants take precedence if set & non-empty (so local
///   `cargo leptos serve` automatically points the viewer link at
///   localhost without juggling deploy profiles).
/// - Otherwise fall back to `PUBLIC_URL` / `VIEWER_URL`.
/// - If `PUBLIC_URL` is missing entirely, default to
///   `http://127.0.0.1:3090` (matches the bind default) and log a
///   warning — this keeps `cargo run` working in an empty environment.
/// - `VIEWER_QUERY_PARAM` defaults to `url` (matches the gldf-rs-wasm
///   viewer's query parser at `main.rs:2156`).
fn resolve_viewer_config() -> ViewerConfig {
    let pick = |dev: &str, prod: &str, default: &str| -> String {
        if let Ok(v) = std::env::var(dev) {
            if !v.trim().is_empty() {
                return v;
            }
        }
        if let Ok(v) = std::env::var(prod) {
            if !v.trim().is_empty() {
                return v;
            }
        }
        tracing::warn!(
            env_var = prod,
            "neither {dev} nor {prod} is set; using default {default}"
        );
        default.to_string()
    };

    let public_base = pick("DEBUG_PUBLIC_URL", "PUBLIC_URL", "http://127.0.0.1:3090");
    let viewer_base = pick(
        "DEBUG_VIEWER_URL",
        "VIEWER_URL",
        // No safe default for the viewer — log loudly. The link still
        // renders but points at an unreachable host.
        "https://example.invalid",
    );
    let query_param = std::env::var("VIEWER_QUERY_PARAM")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "url".to_string());

    ViewerConfig {
        public_base: trim_trailing_slash(&public_base),
        viewer_base: trim_trailing_slash(&viewer_base),
        query_param,
    }
}

fn trim_trailing_slash(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

/// Resolve the corpus root path the server will load at startup.
///
/// Rules:
/// 1. `--corpus-root <path>` on the command line wins outright.
/// 2. Otherwise `DEBUG_GLDF_SEARCH_CORPUS` if set and non-empty
///    (so `cargo leptos serve` picks up the local sample dir from
///    `.env` even when `GLDF_SEARCH_CORPUS` also points at a prod path).
/// 3. Otherwise `GLDF_SEARCH_CORPUS` — the per-deployment value.
/// 4. No default: return an error. A misconfigured server is louder
///    than a server silently loading zero docs from an unintended dir.
fn resolve_corpus_root(cli_value: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = cli_value {
        return Ok(p.to_path_buf());
    }
    let pick = |key: &str| -> Option<PathBuf> {
        std::env::var(key)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = pick("DEBUG_GLDF_SEARCH_CORPUS") {
        return Ok(p);
    }
    if let Some(p) = pick("GLDF_SEARCH_CORPUS") {
        return Ok(p);
    }
    anyhow::bail!(
        "no corpus root configured: set `GLDF_SEARCH_CORPUS` in .env, \
         pass `--corpus-root <DIR>`, or set `DEBUG_GLDF_SEARCH_CORPUS` \
         for local dev"
    )
}

/// Shut down cleanly on Ctrl-C / SIGTERM. Lets the listener drain
/// in-flight requests instead of dropping them on the floor.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
