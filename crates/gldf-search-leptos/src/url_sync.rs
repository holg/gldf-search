//! Browser-only sync between `window.location` query string and the
//! shared `Filter` signal.
//!
//! On hydrate, [`read_filter_from_url`] parses `window.location.search`
//! and returns a [`Filter`]; the caller seeds the signal with it.
//!
//! Then [`install_url_writer`] installs an `Effect` that re-encodes the
//! filter on every change and pushes a new URL into the browser via
//! `history.replaceState` (so the back button doesn't get flooded with
//! per-keystroke history entries). The URL stays current with every
//! slider drag, chip toggle, and text edit.
//!
//! This module is gated `#[cfg(target_arch = "wasm32")]` because it
//! depends on `web-sys`. The SSR path leaves the URL alone — the
//! server doesn't need to round-trip its own writes.

#![cfg(target_arch = "wasm32")]

use gldf_search_schema::filter::Filter;
use leptos::prelude::*;

/// Parse `window.location.search` into a [`Filter`]. Returns
/// `Filter::default()` if there's no query string or every key was
/// unknown.
///
/// Lenient: unknown keys are ignored, malformed values fall back to
/// their default for that field but the rest of the filter applies.
pub fn read_filter_from_url() -> Filter {
    let Some(window) = web_sys::window() else {
        return Filter::default();
    };
    let location = window.location();
    let search = location.search().unwrap_or_default();
    parse_query_string(&search)
}

/// Install an [`Effect`] that mirrors the `filter` signal back into
/// the URL via `history.replaceState`. Re-fires on every filter
/// change. Call once from `SearchPage` after the signal is created.
///
/// Uses `replaceState` (not `pushState`) so per-keystroke filter
/// edits don't flood the browser history — only the latest URL stays
/// in the address bar. The page itself is not navigated, so no
/// reload.
pub fn install_url_writer(filter: RwSignal<Filter>) {
    Effect::new(move |_| {
        let f = filter.get();
        let qs = encode_filter(&f);
        write_query_string(&qs);
    });
}

/// Encode a `Filter` to a query-string suffix (e.g. `?text=led&app=10,12`).
/// Returns an empty string for the default filter, so the URL stays
/// clean when nothing is active.
fn encode_filter(filter: &Filter) -> String {
    let pairs = filter.to_query_pairs();
    if pairs.is_empty() {
        return String::new();
    }
    let mut out = String::from("?");
    let mut first = true;
    for (k, v) in pairs {
        if !first {
            out.push('&');
        }
        first = false;
        out.push_str(k);
        out.push('=');
        out.push_str(&urlencoding::encode(&v));
    }
    out
}

/// Parse a query string (including leading `?`) into a `Filter`.
/// Whitespace-only or empty inputs return `Filter::default()`.
fn parse_query_string(qs: &str) -> Filter {
    let trimmed = qs.trim_start_matches('?');
    if trimmed.is_empty() {
        return Filter::default();
    }
    let pairs: Vec<(String, String)> = trimmed
        .split('&')
        .filter_map(|pair| {
            let mut split = pair.splitn(2, '=');
            let k = split.next()?;
            let v = split.next().unwrap_or("");
            let k = urlencoding::decode(k).ok()?.into_owned();
            let v = urlencoding::decode(v).ok()?.into_owned();
            if k.is_empty() {
                return None;
            }
            Some((k, v))
        })
        .collect();
    Filter::from_query(pairs)
}

/// Push `qs` (e.g. `"?text=led"` or empty for clean URL) into the
/// address bar via `history.replaceState`. Silently no-ops if the
/// browser API is unavailable (we're in some weird non-browser env).
fn write_query_string(qs: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    let location = window.location();
    let pathname = location.pathname().unwrap_or_else(|_| "/".to_string());
    let new_url = if qs.is_empty() {
        pathname
    } else {
        format!("{pathname}{qs}")
    };
    // First arg `data` is null (we don't carry browser-history state).
    // Second arg `title` is ignored by most browsers; pass empty.
    let _ = history.replace_state_with_url(
        &wasm_bindgen::JsValue::NULL,
        "",
        Some(&new_url),
    );
}
