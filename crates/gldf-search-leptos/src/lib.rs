//! gldf-search Leptos UI — runs both as Axum-driven SSR and as a
//! hydrated WASM bundle in the browser.
//!
//! The same component tree is used in both modes. Differences live
//! entirely behind the `ssr` / `hydrate` Cargo feature gates:
//!
//! - `ssr` pulls in `leptos_axum` and reads the [`InMemoryIndex`] from
//!   Leptos's request-context. The server fns execute Rust against the
//!   in-process index — no HTTP self-loopback.
//! - `hydrate` pulls in `wasm-bindgen` and provides the [`hydrate`]
//!   browser entry point. Server fns become `fetch` POSTs back to the
//!   server-side endpoints Leptos auto-generates.
//!
//! Neither feature pulls in raw `gldf-rs` — extraction is done at
//! server startup by `gldf-search-server`; the UI only consumes the
//! resulting `LuminaireDoc` types.

#![allow(non_snake_case)] // Leptos components are PascalCase by convention.

pub mod api;
pub mod app;
pub mod components;
pub mod glyphs;
#[cfg(target_arch = "wasm32")]
pub mod polar_client;
#[cfg(target_arch = "wasm32")]
pub mod url_sync;

pub use app::{shell, App};

// Re-export the server-fn types so the server binary can call
// `register_explicit()` on each. Without these explicit references,
// on some link configurations the inventory ctors get GC'd and the
// /api/leptos/* routes return "function not registered".
pub use api::{
    DocPayload, FetchDoc, FetchDocs, FetchFacets, FetchLdt, LookupArticle, SearchDocs,
    SuggestArticles, SuggestFacets,
};

#[cfg(feature = "ssr")]
pub use api::ssr::IndexHandle;

/// Browser hydrate entry — called by the wasm bundle to mount the SSR'd
/// markup as a reactive client app. The function is wired up to a
/// `start` JS export by `wasm-bindgen` so the SSR's `<script>` tag can
/// invoke it directly.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(App);
}
