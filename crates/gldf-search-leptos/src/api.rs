//! Server functions — the bridge between browser and index.
//!
//! Each `#[server]` fn appears in two forms:
//!
//! - Compiled with `feature = "ssr"`, the function body runs server-
//!   side and reads the [`InMemoryIndex`] out of Leptos's typed
//!   request-context. Direct Rust call, no HTTP.
//! - Compiled with `feature = "hydrate"`, the body is stubbed out by
//!   Leptos and replaced with a `fetch` to the auto-generated endpoint
//!   at `/api/leptos/...`. The browser doesn't see the index at all.
//!
//! The wire types (`Filter`, `SearchResult`, `FacetCounts`) are reused
//! from the schema and index crates — no DTO layer.

use gldf_search_index::{
    ArticleSuggestion, FacetCounts, FacetField, FacetSuggestion, Pagination, RangeBounds,
    SearchResult,
};
use gldf_search_schema::doc::LuminaireDoc;
use gldf_search_schema::filter::Filter;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

// SSR-only imports — the trait gives `Arc<InMemoryIndex>` its
// `search` / `facets` / `doc` methods; `SourceRef` and `ViewerConfig`
// are only inspected in the server-side body of `fetch_doc`.
#[cfg(feature = "ssr")]
use gldf_search_index::IndexSource;
#[cfg(feature = "ssr")]
use gldf_search_schema::{doc::SourceRef, viewer::ViewerConfig};

/// Wire payload for `fetch_doc`: the indexed doc plus a precomputed
/// "Open in viewer" URL. The URL is resolved server-side from the
/// `ViewerConfig` in context — that keeps the browser ignorant of
/// `ViewerConfig` (avoiding a SSR/CSR hydration mismatch where the
/// server has the config and the browser doesn't).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocPayload {
    /// The luminaire doc, unchanged from the index.
    pub doc: LuminaireDoc,
    /// The "Open in viewer" external link, ready to set as `<a href>`.
    /// `None` when the source carries no filename (e.g. `ContentOnly`)
    /// or the server has no viewer config.
    pub viewer_url: Option<String>,
}

/// Wire payload for one variant-grain search hit: the family doc
/// (which carries manufacturer / product / image source / etc.) plus
/// the index of the specific variant inside `doc.variants`.
///
/// The viewer URL is resolved per-variant: a folded family has one
/// source `.gldf` per variant, so the viewer link points at the
/// matching SKU's file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPayload {
    /// Family doc — manufacturer, product, image source, and the
    /// full `variants` array (caller dereferences `variant_index`).
    pub doc: LuminaireDoc,
    /// Index into `doc.variants`. Always < `doc.variants.len()`.
    pub variant_index: u16,
    /// "Open in viewer" link for THIS variant's source GLDF.
    /// Resolved server-side using `doc.source_paths[v.source_index]`.
    pub viewer_url: Option<String>,
}

/// Slim wire payload for `fetch_variants`. Carries only the fields
/// `VariantRow` actually renders, not the whole `LuminaireDoc`.
///
/// **Why this exists**: a folded family doc holds the full variants
/// array (hundreds of SKUs for ERCO Compar, Philips Luma). The old
/// `VariantPayload` shipped that whole array per result row even
/// though the row only displays one variant's fields. Per page of
/// 50 hits × ~10 variants/family avg ≈ 500 VariantDocs over the
/// wire when 50 would do. This payload sends exactly what the row
/// needs.
///
/// All enum-id fields are pre-resolved to canonical XSD strings so
/// the wasm bundle doesn't have to carry the table lookups. The
/// applications field is sent as `Vec<String>` of canonical XSD
/// values for the same reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantPayloadSlim {
    /// Lowercase 64-char hex doc id. Same field on every variant of
    /// a family; clients use it for `fetch_ldt_for_variant`.
    pub doc_id_hex: String,
    /// Variant index inside the family.
    pub variant_index: u16,

    // ── Header line ────────────────────────────────────────────────
    /// Manufacturer display string.
    pub manufacturer: String,
    /// Product display string.
    pub product: String,
    /// Variant display name.
    pub variant_name: String,
    /// SKU / product code or GTIN, whichever the source declared.
    pub sku: Option<String>,
    /// "Open in viewer" link for this variant's source GLDF.
    pub viewer_url: Option<String>,

    // ── Compact summary line ──────────────────────────────────────
    /// Photometry summary — flux, power, efficacy, cct, cri, beam.
    /// `None` when the variant declared no LDC or it failed to parse.
    pub phot: Option<PhotometrySlim>,
    /// Canonical XSD IP code string (e.g. `"IP65"`), if declared on
    /// the doc.
    pub ip_code: Option<String>,
    /// Canonical XSD CIE-97 lamp type string (e.g. `"LED"`), if
    /// declared on this variant.
    pub lamp_type: Option<String>,
    /// Canonical XSD control-gear interface strings (`"DALI"`,
    /// `"0-10V"`, ...) declared on this variant. Pre-deduped.
    pub control_gear_interfaces: Vec<String>,

    // ── Chips + badges ─────────────────────────────────────────────
    /// Canonical XSD application strings, doc-scoped. Front-end
    /// renders as chips; clicking pivots to that filter.
    pub applications: Vec<String>,
    /// Mounting place + type, formatted "Ceiling · Recessed".
    /// `None` when both are `Unknown`.
    pub mounting: Option<String>,
    /// Recessed depth in mm, if declared on the variant.
    pub recessed_depth_mm: Option<u32>,
    /// `true` if the source GLDF carries a 3D model.
    pub has_3d: bool,
    /// `true` if the variant declares any emergency-lighting block.
    pub has_emergency_lighting: bool,
    /// `true` if the variant has any photometric source (LDC/IES).
    pub has_photometry: bool,

    // ── Descriptions (expand on click) ─────────────────────────────
    /// English description if present, else the first description
    /// the doc carries. Truncated to ~200 chars; the full text is
    /// only shipped when the row expands (via a future
    /// `fetch_description` call) so we don't pay for it on every
    /// search result row.
    pub description_snippet: Option<String>,
    /// `true` when the original description is longer than the
    /// snippet — the row should show a "more" affordance.
    pub description_truncated: bool,
}

/// Photometry summary the row renders. Mirrors `render_phot_stats`
/// in the UI: only the fields shown inline.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PhotometrySlim {
    /// Luminous flux (lm).
    pub flux_lm: Option<f32>,
    /// Electrical power (W).
    pub power_w: Option<f32>,
    /// Efficacy (lm/W).
    pub efficacy_lm_w: Option<f32>,
    /// Correlated colour temperature (K).
    pub cct_k: Option<u16>,
    /// CRI Ra.
    pub cri_ra: Option<u8>,
    /// Half-peak beam angle (°).
    pub beam_deg: Option<f32>,
}

/// Wire payload for `fetch_product_image`: the first product picture
/// from a GLDF's `image/` directory, ready to drop into an
/// `<img src="data:{mime};base64,{data}">` tag.
///
/// Base64 trade-off: ~33% wire overhead vs a dedicated streaming
/// route, but no extra Axum routing + cache-key juggling and no
/// risk of cross-manufacturer leaks through a stable URL. Image
/// loads are user-triggered (row expand) so the overhead happens
/// at most once per hit-row, not per pageview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductImage {
    /// MIME type from the GLDF manifest — `image/png`, `image/jpeg`,
    /// `image/svg`, etc. Passed verbatim into the `data:` URL.
    pub mime: String,
    /// Image bytes encoded as standard base64. Browser builds the
    /// `data:` URL with this.
    pub data_base64: String,
}

// Note on argument shapes: both `search_docs` and `fetch_facets`
// take `Option<Filter>` rather than `Filter`. Leptos 0.8's
// form-urlencoded server-fn encoder occasionally drops a fully-
// default-valued struct argument — every field's `Default` makes
// the body collapse to no field at all, and the server decoder then
// reports `missing field "filter"`. Wrapping it in `Option` makes
// the encoder always emit *something* (`None` ⇒ explicit "null"),
// and we unwrap to `Filter::default()` server-side. Same workaround
// applied to `fields` in `fetch_facets` for consistency.
#[server(SearchDocs, prefix = "/api/leptos", endpoint = "search_docs")]
pub async fn search_docs(
    filter: Option<Filter>,
    pagination: Option<Pagination>,
) -> Result<SearchResult, ServerFnError> {
    let idx = ssr::expect_index()?;
    let mut filter = filter.unwrap_or_default();
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        ssr::lock_manufacturer(&mut filter, &scope);
    }
    let page = pagination.unwrap_or_default();
    // Offload the linear scan to the blocking pool. Each scan walks
    // ~270k docs and is multi-hundred-ms on a single core; running it
    // directly on the async runtime would head-of-line block every
    // other request landing on the same worker. With three resources
    // (search + facets + bounds) firing per filter change, the
    // runtime would serialise them; spawn_blocking lets the kernel
    // schedule them in parallel.
    let task = tokio::task::spawn_blocking(move || idx.search(&filter, page));
    let result = task
        .await
        .map_err(|e| ServerFnError::new(format!("search task: {e}")))?;
    Ok(result)
}

#[server(FetchFacets, prefix = "/api/leptos", endpoint = "fetch_facets")]
pub async fn fetch_facets(
    filter: Option<Filter>,
    fields: Option<Vec<FacetField>>,
) -> Result<FacetCounts, ServerFnError> {
    let idx = ssr::expect_index()?;
    let mut filter = filter.unwrap_or_default();
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        ssr::lock_manufacturer(&mut filter, &scope);
    }
    let fields = fields.unwrap_or_default();
    // Skip facet recompute on pure free-text queries. Each facet field
    // would do a full-corpus scan through the slow text predicate (~600
    // ms × 11 fields ≈ 6 s of CPU) and the result is stale by the time
    // the user types the next keystroke. Once they pick a facet/range,
    // the gate opens and facets recompute normally on the now-narrower
    // base set. See `Filter::is_text_only` for the precise condition.
    if filter.is_text_only() {
        return Ok(FacetCounts::default());
    }
    let task = tokio::task::spawn_blocking(move || idx.facets(&filter, &fields));
    let result = task
        .await
        .map_err(|e| ServerFnError::new(format!("facets task: {e}")))?;
    Ok(result)
}

/// Compute dynamic min/max bounds for each numeric range axis under
/// the current filter. Used by the UI to anchor dual-handle range
/// sliders so the user never drags into a zero-results dead zone.
///
/// Unlike [`fetch_facets`], this endpoint deliberately does **not**
/// honour the `Filter::is_text_only()` short-circuit. The user
/// explicitly accepted the CPU cost (~6 corpus walks per keystroke)
/// in exchange for live slider rails during pure-text typing. If
/// production CPU pressure forces a revisit, gate it the same way
/// `fetch_facets` does.
/// Returns the canonical manufacturer name the current request is
/// locked to (via the nginx `X-Manufacturer` header on a
/// `<mfr>.gldf-search.de` subdomain), or `None` when the request
/// came in on the apex domain.
///
/// The UI calls this once on hydrate to decide whether to:
/// - hide the Manufacturer facet (it's locked, no point letting the
///   user pretend to filter it),
/// - show a `<header>` line like "Showing Philips luminaires only".
///
/// Plain string (not the `ManufacturerScope` newtype) so the wire
/// type doesn't drag the SSR-only newtype into wasm — newtypes are
/// for the SSR-side context discipline, not over the wire.
#[server(FetchManufacturerScope, prefix = "/api/leptos", endpoint = "fetch_manufacturer_scope")]
pub async fn fetch_manufacturer_scope() -> Result<Option<String>, ServerFnError> {
    let idx = ssr::expect_index()?;
    Ok(ssr::current_manufacturer_scope(&idx).map(|s| s.0.to_string()))
}

#[server(FetchRangeBounds, prefix = "/api/leptos", endpoint = "fetch_range_bounds")]
pub async fn fetch_range_bounds(
    filter: Option<Filter>,
) -> Result<RangeBounds, ServerFnError> {
    let idx = ssr::expect_index()?;
    let mut filter = filter.unwrap_or_default();
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        ssr::lock_manufacturer(&mut filter, &scope);
    }
    let task = tokio::task::spawn_blocking(move || idx.range_bounds(&filter));
    let result = task
        .await
        .map_err(|e| ServerFnError::new(format!("bounds task: {e}")))?;
    Ok(result)
}

#[server(FetchDoc, prefix = "/api/leptos", endpoint = "fetch_doc")]
pub async fn fetch_doc(id_hex: String) -> Result<Option<DocPayload>, ServerFnError> {
    let idx = ssr::expect_index()?;
    let id = match ssr::parse_doc_id(&id_hex) {
        Some(id) => id,
        None => return Err(ServerFnError::new("invalid hex doc id")),
    };
    let Some(doc) = idx.doc(&id).cloned() else {
        return Ok(None);
    };

    // Honour the per-subdomain manufacturer lock. If a user types
    // a doc-id of another manufacturer's product on philips.gldf-search.de
    // we return None as if the doc didn't exist (avoids leaking
    // existence-confirmation outside the scope).
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        if !doc.manufacturer.eq_ignore_ascii_case(scope.0.as_str()) {
            return Ok(None);
        }
    }

    // Resolve the viewer URL server-side. `ViewerConfig` is installed
    // by `gldf-search-server::main` into the same context the index
    // uses; if it isn't there we silently emit `None` (the UI just
    // won't render the link, no hydration discrepancy).
    let viewer_url = use_context::<ViewerConfig>().and_then(|cfg| match &doc.source {
        SourceRef::Path(name) | SourceRef::Url(name) => Some(cfg.link_for(name.as_str())),
        SourceRef::ContentOnly => None,
    });

    Ok(Some(DocPayload { doc, viewer_url }))
}

/// Batched lookup: resolve every doc id in `ids_hex` to its
/// [`DocPayload`] in one round-trip. The UI uses this instead of
/// looping `fetch_doc` per row — 20 rows × 1 round-trip each was the
/// dominant network/serialize cost in the previous design.
///
/// Unknown / unparseable ids are silently dropped from the output;
/// the result preserves the input order for the ones that resolved.
#[server(FetchDocs, prefix = "/api/leptos", endpoint = "fetch_docs")]
pub async fn fetch_docs(ids_hex: Vec<String>) -> Result<Vec<DocPayload>, ServerFnError> {
    let idx = ssr::expect_index()?;
    let viewer_cfg = use_context::<ViewerConfig>();
    let scope = ssr::current_manufacturer_scope(&idx);
    let mut out = Vec::with_capacity(ids_hex.len());
    for hex in &ids_hex {
        let Some(id) = ssr::parse_doc_id(hex) else {
            continue;
        };
        let Some(doc) = idx.doc(&id).cloned() else {
            continue;
        };
        if let Some(scope) = &scope {
            if !doc.manufacturer.eq_ignore_ascii_case(scope.0.as_str()) {
                continue;
            }
        }
        let viewer_url = viewer_cfg.as_ref().and_then(|cfg| match &doc.source {
            SourceRef::Path(name) | SourceRef::Url(name) => Some(cfg.link_for(name.as_str())),
            SourceRef::ContentOnly => None,
        });
        out.push(DocPayload { doc, viewer_url });
    }
    Ok(out)
}

/// Wire payload for [`fetch_variants`]. Each pair is `(doc-id-hex,
/// variant-index)` — the variant-grain analog of a hex doc id list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantRef {
    /// Lowercase 64-char hex doc id.
    pub doc_id: String,
    /// 0-based index into the doc's `variants` slice.
    pub variant: u16,
}

/// Batched variant-grain lookup: resolve N `(doc_id, variant)` pairs
/// into `VariantPayload`s in one round trip. Mirrors `fetch_docs`
/// but at variant grain — the result list preserves input order.
///
/// Honours the per-subdomain manufacturer scope. Unknown / invalid /
/// out-of-bounds entries are silently dropped.
#[server(FetchVariants, prefix = "/api/leptos", endpoint = "fetch_variants")]
pub async fn fetch_variants(
    refs: Vec<VariantRef>,
) -> Result<Vec<VariantPayloadSlim>, ServerFnError> {
    use gldf_search_schema::enums::{
        application_str, control_gear_interface_str, ip_code_str, lamp_type_str,
    };
    let idx = ssr::expect_index()?;
    let viewer_cfg = use_context::<ViewerConfig>();
    let scope = ssr::current_manufacturer_scope(&idx);
    let mut out = Vec::with_capacity(refs.len());
    for r in &refs {
        let Some(id) = ssr::parse_doc_id(&r.doc_id) else {
            continue;
        };
        let Some(doc) = idx.doc(&id) else {
            continue;
        };
        if let Some(scope) = &scope {
            if !doc.manufacturer.eq_ignore_ascii_case(scope.0.as_str()) {
                continue;
            }
        }
        let v_idx = r.variant as usize;
        if v_idx >= doc.variants.len() {
            continue;
        }
        let v = &doc.variants[v_idx];

        // Viewer URL: variant's own source for folded families.
        let src = doc
            .source_paths
            .get(v.source_index as usize)
            .unwrap_or(&doc.source);
        let viewer_url = viewer_cfg.as_ref().and_then(|cfg| match src {
            SourceRef::Path(name) | SourceRef::Url(name) => Some(cfg.link_for(name.as_str())),
            SourceRef::ContentOnly => None,
        });

        // SKU line — same selection logic as the old HitRow.
        let sku = doc
            .product_code
            .as_ref()
            .map(|s| s.to_string())
            .or_else(|| doc.gtin.as_ref().map(|s| s.to_string()));

        // Photometry slim.
        let phot = v.photometry.as_ref().map(|p| PhotometrySlim {
            flux_lm: p.flux_lm,
            power_w: p.power_w,
            efficacy_lm_w: p.efficacy_lm_w,
            cct_k: p.cct_k,
            cri_ra: p.cri_ra,
            beam_deg: p.beam_deg,
        });

        // Pre-resolve enum-id fields to canonical XSD strings so
        // wasm doesn't have to ship the lookup tables.
        let ip_code = doc.ip_code.and_then(ip_code_str).map(String::from);
        let lamp_type = v.lamp_type.and_then(lamp_type_str).map(String::from);
        let control_gear_interfaces: Vec<String> = v
            .control_gear_interfaces
            .iter()
            .filter_map(|id| control_gear_interface_str(*id))
            .map(String::from)
            .collect();
        let applications: Vec<String> = doc
            .applications
            .iter()
            .filter_map(|id| application_str(*id))
            .map(String::from)
            .collect();

        // Mounting line.
        use gldf_search_schema::doc::{MountingPlace, MountingType};
        let mounting = match (v.mounting_place, v.mounting_type) {
            (MountingPlace::Unknown, MountingType::Unknown) => None,
            (place, kind) => Some(format!("{place:?} · {kind:?}")),
        };

        // English description preferred, else first available.
        // Truncate to 200 chars; the row expands to the full text.
        let (description_snippet, description_truncated) = pick_description_snippet(&doc);

        out.push(VariantPayloadSlim {
            doc_id_hex: r.doc_id.clone(),
            variant_index: r.variant,
            manufacturer: doc.manufacturer.to_string(),
            product: doc.product.to_string(),
            variant_name: v.name.to_string(),
            sku,
            viewer_url,
            phot,
            ip_code,
            lamp_type,
            control_gear_interfaces,
            applications,
            mounting,
            recessed_depth_mm: v.recessed_depth_mm,
            has_3d: v.has_3d,
            has_emergency_lighting: v.has_emergency_lighting,
            has_photometry: v.photometry.is_some(),
            description_snippet,
            description_truncated,
        });
    }
    Ok(out)
}

/// Pick an English description if present, else the first one. Trim
/// to ~200 chars on a UTF-8 char boundary. Returns
/// `(snippet, truncated)` where `truncated` is true when the source
/// text exceeded the cap.
#[cfg(feature = "ssr")]
fn pick_description_snippet(
    doc: &gldf_search_schema::doc::LuminaireDoc,
) -> (Option<String>, bool) {
    use gldf_search_schema::Locale;
    const MAX_CHARS: usize = 200;
    let en = Locale::from_str("en");
    let chosen = en
        .and_then(|target| {
            doc.descriptions
                .iter()
                .find(|(loc, _)| Some(*loc) == en.map(|_| target))
        })
        .or_else(|| doc.descriptions.first())
        .map(|(_, s)| s.as_str());
    let Some(full) = chosen else {
        return (None, false);
    };
    let char_count = full.chars().count();
    if char_count <= MAX_CHARS {
        return (Some(full.to_string()), false);
    }
    let mut idx = 0;
    for (i, _) in full.char_indices().take(MAX_CHARS) {
        idx = i;
    }
    // `idx` is the start of the MAX_CHARSth char; we want the end of it.
    if let Some((next_idx, _)) = full.char_indices().nth(MAX_CHARS) {
        idx = next_idx;
    } else {
        idx = full.len();
    }
    let mut snippet = full[..idx].to_string();
    snippet.push('…');
    (Some(snippet), true)
}

/// Article-number lookup — single-input "jump to product code / GTIN"
/// path. Returns every doc whose `gtin` or `product_code` matches the
/// input after normalisation (lowercase + strip non-alphanumerics),
/// rendered as `DocPayload`s so the UI can render them directly with
/// `<HitRow>`.
///
/// Empty / whitespace-only / pure-punctuation input returns an empty
/// vec without touching the index.
#[server(LookupArticle, prefix = "/api/leptos", endpoint = "lookup_article")]
pub async fn lookup_article(article: String) -> Result<Vec<DocPayload>, ServerFnError> {
    let idx = ssr::expect_index()?;
    let viewer_cfg = use_context::<ViewerConfig>();
    let scope = ssr::current_manufacturer_scope(&idx);
    let hits = idx.lookup_article(article.as_str());
    let mut out = Vec::with_capacity(hits.len());
    for doc in hits {
        if let Some(scope) = &scope {
            if !doc.manufacturer.eq_ignore_ascii_case(scope.0.as_str()) {
                continue;
            }
        }
        let viewer_url = viewer_cfg.as_ref().and_then(|cfg| match &doc.source {
            SourceRef::Path(name) | SourceRef::Url(name) => Some(cfg.link_for(name.as_str())),
            SourceRef::ContentOnly => None,
        });
        out.push(DocPayload {
            doc: doc.clone(),
            viewer_url,
        });
    }
    Ok(out)
}

/// Prefix autosuggest for the main free-text search bar. Returns up
/// to `limit` typed facet hints — `Manufacturer: AEC Illuminazione`,
/// `IpCode: IP65`, `Application: Interior: Office: Office Desks`,
/// etc. The UI renders each as a clickable row that applies the
/// matching facet filter on click. Two-character minimum.
#[server(SuggestFacets, prefix = "/api/leptos", endpoint = "suggest_facets")]
pub async fn suggest_facets(
    prefix: String,
    limit: Option<u32>,
) -> Result<Vec<FacetSuggestion>, ServerFnError> {
    let idx = ssr::expect_index()?;
    let limit = limit.unwrap_or(10).min(30) as usize;
    let raw = idx.suggest_facets(&prefix, limit);
    // Under a manufacturer scope, drop every `Manufacturer:` suggestion
    // that isn't the locked one — the dropdown would otherwise propose
    // jumps to other manufacturers' results, which the subdomain bars.
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        let filtered: Vec<FacetSuggestion> = raw
            .into_iter()
            .filter(|s| match s {
                FacetSuggestion::Manufacturer { value, .. } => {
                    value.eq_ignore_ascii_case(scope.0.as_str())
                }
                _ => true,
            })
            .collect();
        return Ok(filtered);
    }
    Ok(raw)
}

/// Prefix autosuggest for the `Art.#` input. Returns up to `limit`
/// suggestions (default 20 server-side, capped to ≤ 50 to bound the
/// payload). Empty when the normalised prefix is shorter than 3
/// characters — the browser-side debouncer also guards this, but
/// re-asserting it server-side keeps the API honest.
#[server(SuggestArticles, prefix = "/api/leptos", endpoint = "suggest_articles")]
pub async fn suggest_articles(
    prefix: String,
    limit: Option<u32>,
) -> Result<Vec<ArticleSuggestion>, ServerFnError> {
    let idx = ssr::expect_index()?;
    let limit = limit.unwrap_or(20).min(50) as usize;
    let raw = idx.suggest_articles(&prefix, limit);
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        return Ok(raw
            .into_iter()
            .filter(|s| s.manufacturer.eq_ignore_ascii_case(scope.0.as_str()))
            .collect());
    }
    Ok(raw)
}

/// Fetch the raw LDT/IES text for one doc's first photometric file.
/// The hydrate bundle uses `eulumdat-rs` (in wasm) to parse + render
/// the polar / cartesian / butterfly diagrams client-side, so the
/// index doesn't have to carry intensity matrices.
///
/// Returns `Ok(None)` when the doc isn't indexed, when it has no
/// declared photometric file, or when the source isn't a path-on-
/// disk (i.e. content-only docs).
#[server(FetchLdt, prefix = "/api/leptos", endpoint = "fetch_ldt")]
pub async fn fetch_ldt(id_hex: String) -> Result<Option<String>, ServerFnError> {
    fetch_ldt_for_variant(id_hex, None).await
}

/// Variant-targeted twin of [`fetch_ldt`]. `variant_index` is the
/// 0-based position of the variant in the GLDF's `<Variants>`
/// block — the same ordering `VariantDoc.id.0` uses. Pass `None`
/// for "doc-level / first photometric file" behaviour.
#[server(FetchLdtForVariant, prefix = "/api/leptos", endpoint = "fetch_ldt_for_variant")]
pub async fn fetch_ldt_for_variant(
    id_hex: String,
    variant_index: Option<u16>,
) -> Result<Option<String>, ServerFnError> {
    let idx = ssr::expect_index()?;
    let corpus_root = ssr::expect_corpus_root()?;
    let Some(id) = ssr::parse_doc_id(&id_hex) else {
        return Err(ServerFnError::new("invalid hex doc id"));
    };
    let Some(doc) = idx.doc(&id) else {
        return Ok(None);
    };
    // Manufacturer scope check — same posture as fetch_doc.
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        if !doc.manufacturer.eq_ignore_ascii_case(scope.0.as_str()) {
            return Ok(None);
        }
    }
    // Resolve which .gldf to open:
    // - With a variant_index AND the doc is folded (multiple
    //   source_paths), use `source_paths[variant.source_index]`.
    //   Each folded variant came from its own source file.
    // - Without a variant_index OR doc has a single source, fall
    //   back to the doc-level `source` (the doc-level / first
    //   variant default).
    let source = resolve_variant_source(&doc, variant_index);
    use gldf_search_schema::doc::SourceRef;
    let rel = match source {
        SourceRef::Path(p) => p.to_string(),
        SourceRef::Url(_) | SourceRef::ContentOnly => return Ok(None),
    };
    let abs = corpus_root.join(&rel);
    // For folded variants we already picked the right .gldf, so the
    // inner read takes the FIRST LDT of THAT file. For un-folded
    // multi-LDT families the positional heuristic still applies via
    // the caller's variant_index.
    let v_idx = if doc.source_paths.len() > 1 {
        // Folded — each variant is its own .gldf; first LDT of that
        // file is what we want.
        None
    } else {
        variant_index.map(|n| n as usize)
    };
    let result = tokio::task::spawn_blocking(move || {
        gldf_search_gldf::read_ldt_for_variant(&abs, v_idx)
    })
    .await
    .map_err(|e| ServerFnError::new(format!("join error: {e}")))?
    .map_err(|e| ServerFnError::new(format!("read_ldt_for_variant: {e}")))?;
    Ok(result)
}

/// Resolve a variant's source `.gldf` path.
///
/// For folded family docs (>1 source path), the variant carries
/// `source_index` pointing at the contributing GLDF. For un-folded
/// or "no specific variant" requests, the doc-level `source` is
/// the right answer.
#[cfg(feature = "ssr")]
fn resolve_variant_source(
    doc: &gldf_search_schema::doc::LuminaireDoc,
    variant_index: Option<u16>,
) -> gldf_search_schema::doc::SourceRef {
    if let Some(idx) = variant_index {
        if doc.source_paths.len() > 1 {
            if let Some(v) = doc.variants.get(idx as usize) {
                if let Some(src) = doc.source_paths.get(v.source_index as usize) {
                    return src.clone();
                }
            }
        }
    }
    doc.source.clone()
}

/// Fetch the first product picture out of a doc's `.gldf`. Lazy:
/// the UI calls this when the user expands a hit row, so the
/// front-page load doesn't pay for hundreds of image fetches.
///
/// Returns:
/// - `Ok(Some(ProductImage))` — image bytes ready for a `data:` URL.
/// - `Ok(None)` — doc unknown, no image declared, or source is not
///   a path on disk (content-only / URL-only).
/// - `Err(...)` — only for genuine corruption (zip unreadable etc.).
///
/// Honours the per-subdomain manufacturer scope: requests for a
/// doc whose manufacturer differs from the locked scope return
/// `None` (no existence leak across the boundary).
#[server(FetchProductImage, prefix = "/api/leptos", endpoint = "fetch_product_image")]
pub async fn fetch_product_image(
    id_hex: String,
) -> Result<Option<ProductImage>, ServerFnError> {
    fetch_product_image_for_variant(id_hex, None).await
}

/// Variant-targeted twin of [`fetch_product_image`]. Same positional
/// heuristic as `fetch_ldt_for_variant`: when image count == variant
/// count, the variant-index'd image wins; else fall back to the
/// doc-level first picture.
#[server(FetchProductImageForVariant, prefix = "/api/leptos", endpoint = "fetch_product_image_for_variant")]
pub async fn fetch_product_image_for_variant(
    id_hex: String,
    variant_index: Option<u16>,
) -> Result<Option<ProductImage>, ServerFnError> {
    use base64::Engine;
    use gldf_search_schema::doc::SourceRef;

    let idx = ssr::expect_index()?;
    let corpus_root = ssr::expect_corpus_root()?;
    let Some(id) = ssr::parse_doc_id(&id_hex) else {
        return Err(ServerFnError::new("invalid hex doc id"));
    };
    let Some(doc) = idx.doc(&id) else {
        return Ok(None);
    };
    if let Some(scope) = ssr::current_manufacturer_scope(&idx) {
        if !doc.manufacturer.eq_ignore_ascii_case(scope.0.as_str()) {
            return Ok(None);
        }
    }
    let source = resolve_variant_source(&doc, variant_index);
    let rel = match source {
        SourceRef::Path(p) => p.to_string(),
        SourceRef::Url(_) | SourceRef::ContentOnly => return Ok(None),
    };
    let abs = corpus_root.join(&rel);
    let v_idx = if doc.source_paths.len() > 1 {
        None
    } else {
        variant_index.map(|n| n as usize)
    };
    let result = tokio::task::spawn_blocking(move || {
        gldf_search_gldf::read_image_for_variant(&abs, v_idx)
    })
    .await
    .map_err(|e| ServerFnError::new(format!("join error: {e}")))?
    .map_err(|e| ServerFnError::new(format!("read_image_for_variant: {e}")))?;
    Ok(result.map(|(mime, bytes)| ProductImage {
        mime,
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    }))
}

/// SSR-only utilities — the [`IndexHandle`] type the server inserts
/// into Leptos's request context, plus a couple of plumbing helpers.
#[cfg(feature = "ssr")]
pub mod ssr {
    use std::path::PathBuf;
    use std::sync::Arc;

    use gldf_search_index::{InMemoryIndex, IndexSource};
    use gldf_search_schema::doc::DocId;
    use leptos::prelude::*;

    /// Newtype around the shared index. Leptos's request-context is
    /// keyed by type, so a plain `Arc<InMemoryIndex>` would alias if
    /// some other crate ever installs the same type. Named newtype
    /// avoids that.
    #[derive(Clone)]
    pub struct IndexHandle(pub Arc<InMemoryIndex>);

    impl IndexHandle {
        /// Borrow the underlying trait object for one query.
        pub fn as_source(&self) -> &dyn IndexSource {
            self.0.as_ref()
        }
    }

    /// Newtype for the corpus root path on disk. Installed into
    /// Leptos's request-context alongside [`IndexHandle`]; the
    /// `fetch_ldt_bytes` server fn uses it to resolve a doc's
    /// `SourceRef::Path` to an absolute filesystem path for the
    /// on-demand LDT read.
    #[derive(Clone)]
    pub struct CorpusRoot(pub PathBuf);

    /// Per-request manufacturer scope, derived from the
    /// `X-Manufacturer` header nginx adds on per-manufacturer
    /// subdomains (e.g. `philips.gldf-search.de` →
    /// `X-Manufacturer: philips`).
    ///
    /// When present, every server fn pre-applies it to the incoming
    /// `Filter` before hitting the index, so the user cannot see
    /// docs from other manufacturers regardless of what the client
    /// posted. The carried value is the canonical (corpus-spelled)
    /// manufacturer name, e.g. `"Philips"` — not the lowercase slug.
    #[derive(Clone, Debug)]
    pub struct ManufacturerScope(pub compact_str::CompactString);

    /// Header name nginx uses to forward the manufacturer slug from
    /// the subdomain. Kept as a constant so the server fns and the
    /// page-render path stay in lock-step.
    pub const X_MANUFACTURER_HEADER: &str = "x-manufacturer";

    /// Read the `X-Manufacturer` header from the current request's
    /// `Parts` (made available by leptos_axum), resolve it against
    /// the index's actual manufacturer list, and return the canonical
    /// scope. Returns `None` when:
    /// - the header is absent (request came in on the apex domain)
    /// - the header is present but doesn't match any manufacturer in
    ///   the corpus (subdomain typo / outdated cert wildcard)
    ///
    /// Called once per server-fn body. The `Parts` lookup is
    /// `use_context::<axum::http::request::Parts>()` — Leptos's axum
    /// adapter installs this automatically.
    pub(crate) fn current_manufacturer_scope(
        idx: &InMemoryIndex,
    ) -> Option<ManufacturerScope> {
        // Prefer an explicit `ManufacturerScope` if the server-side
        // page-render closure pre-installed one (cheaper than the
        // header lookup, and lets non-axum tests inject a scope).
        if let Some(scope) = use_context::<ManufacturerScope>() {
            return Some(scope);
        }
        let parts = use_context::<axum::http::request::Parts>()?;
        let slug = parts
            .headers
            .get(X_MANUFACTURER_HEADER)
            .and_then(|v| v.to_str().ok())?;
        let canonical = idx.resolve_manufacturer(slug)?;
        Some(ManufacturerScope(canonical))
    }

    /// Apply the manufacturer scope (if any) to a filter in-place.
    /// Overwrites whatever the client requested — the subdomain is
    /// authoritative. Idempotent and cheap.
    pub(crate) fn lock_manufacturer(filter: &mut gldf_search_schema::filter::Filter, scope: &ManufacturerScope) {
        use smallvec::smallvec;
        filter.manufacturer = Some(smallvec![scope.0.clone()]);
    }

    /// Read the [`IndexHandle`] out of Leptos's request-scoped
    /// context. Returns a `ServerFnError` if missing — that means the
    /// server failed to install it during `LeptosRoutes` wiring.
    pub(crate) fn expect_index() -> Result<Arc<InMemoryIndex>, ServerFnError> {
        let handle = use_context::<IndexHandle>().ok_or_else(|| {
            ServerFnError::new(
                "IndexHandle missing from Leptos context — \
                 server wired LeptosRoutes without provide_context",
            )
        })?;
        Ok(handle.0)
    }

    /// Read the [`CorpusRoot`] out of Leptos's request-scoped context.
    pub(crate) fn expect_corpus_root() -> Result<PathBuf, ServerFnError> {
        let cr = use_context::<CorpusRoot>().ok_or_else(|| {
            ServerFnError::new("CorpusRoot missing from Leptos context")
        })?;
        Ok(cr.0)
    }

    /// Hex → DocId. Same as `gldf-search-server::api`'s parser. Kept
    /// here to avoid pulling the server crate into the UI dep graph.
    pub(crate) fn parse_doc_id(s: &str) -> Option<DocId> {
        if s.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = nibble(chunk[0])?;
            let lo = nibble(chunk[1])?;
            bytes[i] = (hi << 4) | lo;
        }
        Some(DocId(bytes))
    }

    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
}

// Hydrate-side stub so the cfg-gated `ssr::expect_index` calls in the
// server-fn bodies still compile when only `hydrate` is enabled. Those
// branches are never executed in the wasm bundle (Leptos rewrites the
// body to a fetch), but they need to type-check.
#[cfg(not(feature = "ssr"))]
#[allow(dead_code)]
pub mod ssr {
    use gldf_search_index::InMemoryIndex;
    use gldf_search_schema::doc::DocId;
    use leptos::prelude::*;
    use std::sync::Arc;

    /// Stub — never instantiated in the hydrate build. Exists so the
    /// type name resolves for `#[server]` bodies.
    pub(crate) fn expect_index() -> Result<Arc<InMemoryIndex>, ServerFnError> {
        Err(ServerFnError::new("index access on hydrate side"))
    }

    pub(crate) fn parse_doc_id(_s: &str) -> Option<DocId> {
        None
    }
}
