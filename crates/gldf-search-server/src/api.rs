//! HTTP handlers.
//!
//! The wire types are the schema's own `Filter` / `Pagination` /
//! `FacetField` / `SearchResult` / `FacetCounts` / `LuminaireDoc`. No
//! DTO layer: a Rust client can call `serde_json::to_string(&filter)`
//! and POST the result. The browser (once Leptos lands) will do
//! exactly the same through server functions.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use gldf_search_index::{
    ArticleSuggestion, FacetCounts, FacetField, FacetSuggestion, IndexSource, Pagination,
    SearchResult,
};
use gldf_search_schema::doc::{DocId, LuminaireDoc};
use gldf_search_schema::filter::Filter;
use serde::Deserialize;

use crate::state::AppState;

/// Build the `/api/*` routes.
pub fn routes(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/search", post(search))
        .route("/api/facets", post(facets))
        .route("/api/doc/{id}", get(doc_by_id))
        .route("/api/article/{article}", get(doc_by_article))
        .route("/api/suggest/article", get(suggest_article))
        .route("/api/suggest/facets", get(suggest_facets_route))
        .with_state(Arc::new(state))
}

/// Simple liveness probe — no index access. Useful for orchestration.
async fn health() -> &'static str {
    "ok"
}

/// Index-side stats. Currently just doc count; grows as the index
/// gains more interesting metadata (build timestamp, schema hash, ...).
async fn stats(State(state): State<Arc<AppState>>) -> Json<StatsResponse> {
    Json(StatsResponse {
        docs: state.index.len() as u32,
    })
}

#[derive(serde::Serialize)]
struct StatsResponse {
    docs: u32,
}

/// POST `/api/search` body. `pagination` is optional and defaults to
/// the index's standard `Pagination::DEFAULT` (0..50).
#[derive(Deserialize, Default)]
struct SearchBody {
    #[serde(default)]
    filter: Filter,
    #[serde(default)]
    pagination: Option<Pagination>,
}

async fn search(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SearchBody>,
) -> Json<SearchResult> {
    let page = body.pagination.unwrap_or_default();
    let started = std::time::Instant::now();
    let result = state.index.search(&body.filter, page);
    tracing::info!(
        elapsed_us = started.elapsed().as_micros() as u64,
        total = result.total,
        page_size = result.hits.len(),
        "search"
    );
    Json(result)
}

/// POST `/api/facets` body. Caller specifies which facet fields to
/// count; empty list returns an empty `FacetCounts` (so a client can
/// hit the endpoint to confirm wiring without paying compute).
#[derive(Deserialize, Default)]
struct FacetsBody {
    #[serde(default)]
    filter: Filter,
    #[serde(default)]
    fields: Vec<FacetField>,
}

async fn facets(
    State(state): State<Arc<AppState>>,
    Json(body): Json<FacetsBody>,
) -> Json<FacetCounts> {
    let started = std::time::Instant::now();
    let n_fields = body.fields.len();
    let counts = state.index.facets(&body.filter, &body.fields);
    tracing::info!(
        elapsed_us = started.elapsed().as_micros() as u64,
        fields = n_fields,
        "facets"
    );
    Json(counts)
}

/// GET `/api/doc/:id` — id is a hex-encoded 32-byte BLAKE3 digest.
async fn doc_by_id(
    State(state): State<Arc<AppState>>,
    Path(id_hex): Path<String>,
) -> Result<Json<LuminaireDoc>, StatusCode> {
    let id = parse_doc_id(&id_hex).ok_or(StatusCode::BAD_REQUEST)?;
    match state.index.doc(&id) {
        Some(d) => Ok(Json(d.clone())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// GET `/api/article/:article` — exact-match lookup against indexed
/// `gtin` + `product_code` (normalised: lowercase, non-alphanumerics
/// stripped). Returns every doc that matches.
///
/// 404 when nothing matches. The hits Vec is returned at any size
/// > 0 — clients decide whether to render a list or jump straight to
/// the single hit.
async fn doc_by_article(
    State(state): State<Arc<AppState>>,
    Path(article): Path<String>,
) -> Result<Json<Vec<LuminaireDoc>>, StatusCode> {
    let hits = state.index.lookup_article(&article);
    if hits.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(hits.into_iter().cloned().collect()))
}

#[derive(Deserialize)]
struct SuggestArticleQuery {
    /// Prefix to autosuggest on. Min 3 chars after normalisation.
    q: String,
    /// Max suggestions returned. Defaults to 20, capped at 50.
    #[serde(default)]
    limit: Option<u32>,
}

/// GET `/api/suggest/article?q=art&limit=20` — prefix autosuggest
/// over article numbers. Returns an array of `{article, manufacturer,
/// product}` triples. Empty array (200) when the prefix is too short
/// or has no matches.
async fn suggest_article(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SuggestArticleQuery>,
) -> Json<Vec<ArticleSuggestion>> {
    let limit = q.limit.unwrap_or(20).min(50) as usize;
    Json(state.index.suggest_articles(&q.q, limit))
}

/// GET `/api/suggest/facets?q=aec&limit=10` — prefix autosuggest
/// across typed facet kinds. Returns an array of `FacetSuggestion`
/// variants (manufacturer / ip code / product form / …). Empty
/// array (200) for short or unmatched prefixes.
async fn suggest_facets_route(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SuggestArticleQuery>,
) -> Json<Vec<FacetSuggestion>> {
    let limit = q.limit.unwrap_or(10).min(30) as usize;
    Json(state.index.suggest_facets(&q.q, limit))
}

/// Parse a `DocId` from a 64-char hex string. Returns `None` on bad
/// length or non-hex digits.
fn parse_doc_id(s: &str) -> Option<DocId> {
    if s.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        bytes[i] = (hi << 4) | lo;
    }
    Some(DocId(bytes))
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// `IntoResponse` for `StatusCode` is provided by Axum; this trait
// import is required to keep that conversion in scope when we return
// a bare `StatusCode` from `doc_by_id`.
#[allow(dead_code)]
fn _trait_anchor() -> impl IntoResponse {
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lowercase_hex() {
        let id = parse_doc_id(&"a".repeat(64)).expect("parse");
        assert_eq!(id.0, [0xaa; 32]);
    }

    #[test]
    fn rejects_short_input() {
        assert!(parse_doc_id("abc").is_none());
    }

    #[test]
    fn rejects_non_hex_chars() {
        assert!(parse_doc_id(&"z".repeat(64)).is_none());
    }
}
