//! `IndexSource` trait + in-memory linear-scan implementation.

use std::collections::{BTreeMap, HashMap};

use compact_str::CompactString;
use gldf_search_schema::doc::{DocId, LuminaireDoc, MountingPlace, MountingType};
use gldf_search_schema::enums::{
    adjustability_str, application_str, control_gear_interface_str, emergency_lighting_type_str,
    ik_rating_str, ip_code_str, label_str, lamp_type_str, light_distribution_str, product_form_str,
    safety_class_str,
};
use gldf_search_schema::filter::Filter;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::facets::{
    F32Bounds, FacetCounts, FacetField, FacetValueCount, RangeBounds, U16Bounds, U32Bounds,
};

/// Page request — offset/limit. Callers cap `limit` to keep memory in
/// check; 50 is a reasonable default for result-list UIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pagination {
    /// Hits to skip from the front of the result list.
    pub offset: u32,
    /// Maximum hits to return.
    pub limit: u32,
}

impl Pagination {
    /// 0..50 — the default for "show me the first page".
    pub const DEFAULT: Self = Self {
        offset: 0,
        limit: 50,
    };
}

impl Default for Pagination {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A single search hit, identifying both the parent doc and the
/// specific variant inside it. Folded family docs have many variants;
/// emitting at variant grain keeps the displayed result count at
/// per-SKU resolution (the unit users care about for calculation)
/// while the in-memory doc store stays compact at family grain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Parent doc id (family-canonical id, or self-id for un-folded
    /// docs). Resolves through `InMemoryIndex::doc` like any
    /// `DocId`.
    pub doc: DocId,
    /// Variant index inside the parent doc's `variants` slice.
    /// `0` for un-folded single-variant docs.
    pub variant: u16,
}

/// One search execution's result. `hits` is paginated; `total` is the
/// full result-set size **before** pagination — what facet counts use
/// as a denominator, and what the UI displays as "X of N matches".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    /// One entry per matching variant. Order follows index insertion
    /// (linear scan); future BM25 ranker will sort.
    pub hits: Vec<SearchHit>,
    /// Total matching variants before pagination.
    pub total: u32,
}

/// One facet autosuggest result. The free-text search bar
/// classifies what the user typed and offers a typed action — click
/// the row to apply the matching facet filter — instead of running
/// substring text search.
///
/// `count` is the number of docs that satisfy this facet across the
/// whole corpus (no filter applied) — useful for ranking and giving
/// the user a sense of how many results clicking will yield.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
#[allow(missing_docs)] // value/count are documented at the enum doc; per-variant body is uniform.
pub enum FacetSuggestion {
    /// Manufacturer display name (e.g. `"AEC Illuminazione"`).
    Manufacturer { value: String, count: u32 },
    /// XSD IP code string (e.g. `"IP65"`).
    IpCode { value: String, count: u32 },
    /// XSD electrical safety class string (e.g. `"II"`).
    SafetyClass { value: String, count: u32 },
    /// XSD IK rating string (e.g. `"IK08"`).
    IkRating { value: String, count: u32 },
    /// XSD product-form string (e.g. `"Square"`).
    ProductForm { value: String, count: u32 },
    /// XSD adjustability mode (e.g. `"Tiltable"`).
    Adjustability { value: String, count: u32 },
    /// XSD regulatory label (e.g. `"CE"`).
    Label { value: String, count: u32 },
    /// Hierarchical application taxonomy entry
    /// (e.g. `"Interior: Office: Office Desks"`).
    Application { value: String, count: u32 },
    /// CIE-97 lamp type (e.g. `"Halogen"`, `"Metal Halide (250/400W)"`).
    LampType { value: String, count: u32 },
    /// Control-gear interface (e.g. `"DALI Broadcast"`, `"KNX"`,
    /// `"0-10V"`).
    ControlGearInterface { value: String, count: u32 },
    /// Dedicated emergency-lighting type (e.g. `"Exit Light"`,
    /// `"Evacuation Light"`).
    EmergencyLightingType { value: String, count: u32 },
}

impl FacetSuggestion {
    /// Doc count for this facet value across the unfiltered corpus.
    pub fn count(&self) -> u32 {
        match self {
            Self::Manufacturer { count, .. }
            | Self::IpCode { count, .. }
            | Self::SafetyClass { count, .. }
            | Self::IkRating { count, .. }
            | Self::ProductForm { count, .. }
            | Self::Adjustability { count, .. }
            | Self::Label { count, .. }
            | Self::Application { count, .. }
            | Self::LampType { count, .. }
            | Self::ControlGearInterface { count, .. }
            | Self::EmergencyLightingType { count, .. } => *count,
        }
    }

    /// The displayed value (e.g. `"AEC Illuminazione"`, `"IP65"`).
    pub fn value(&self) -> &str {
        match self {
            Self::Manufacturer { value, .. }
            | Self::IpCode { value, .. }
            | Self::SafetyClass { value, .. }
            | Self::IkRating { value, .. }
            | Self::ProductForm { value, .. }
            | Self::Adjustability { value, .. }
            | Self::Label { value, .. }
            | Self::Application { value, .. }
            | Self::LampType { value, .. }
            | Self::ControlGearInterface { value, .. }
            | Self::EmergencyLightingType { value, .. } => value.as_str(),
        }
    }
}

/// One article-number autosuggest result. Carries enough context for
/// the dropdown to be useful — bare GTINs all look the same, so we
/// ship the manufacturer + product name alongside.
///
/// `article` is the **original** string the manufacturer printed
/// (`"ART-1234"` not `"art1234"`). This matters because real-world
/// corpora ship cosmetic variants — manufacturers really do publish
/// both `ART-124` and `ART124` for distinct SKUs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArticleSuggestion {
    /// The article number as printed in the source GLDF.
    pub article: String,
    /// Manufacturer display name from the parent doc.
    pub manufacturer: String,
    /// Product display name from the parent doc.
    pub product: String,
}

/// The query surface every index implementation satisfies.
///
/// Implementations need not be `Send + Sync`; the Axum server wraps a
/// shared `MmapIndex` in `Arc<RwLock<_>>` and the browser uses a
/// thread-local `RemoteIndex`. The trait stays sync-only and
/// trait-object friendly.
pub trait IndexSource {
    /// Run a search. Result is paginated per [`Pagination`].
    fn search(&self, filter: &Filter, page: Pagination) -> SearchResult;

    /// Compute facet counts for the requested fields with
    /// "facet-relaxed" semantics: when counting values for facet
    /// field `F`, the filter's value for `F` is temporarily cleared,
    /// so the user sees what's reachable by widening that facet.
    fn facets(&self, filter: &Filter, fields: &[FacetField]) -> FacetCounts;

    /// Compute per-axis min/max bounds for the six numeric range
    /// fields (flux, power, efficacy, cct, beam, recessed-depth),
    /// with the same one-axis-relaxed semantics as [`facets`]: the
    /// bounds for axis A are computed with A's range cleared in the
    /// filter, so the UI's slider rails always span the *reachable*
    /// values given the other active filters.
    fn range_bounds(&self, filter: &Filter) -> RangeBounds;

    /// Look up a single doc by id. `None` if not in the index.
    fn doc(&self, id: &DocId) -> Option<&LuminaireDoc>;

    /// Look up docs by article number (GTIN or product_code).
    ///
    /// Match is exact after normalisation (lowercase + strip every
    /// non-alphanumeric character), so `"ART-1234"`, `"art 1234"`,
    /// and `"art1234"` all hit the same key. Returns an empty slice
    /// when nothing matches. One key can map to multiple docs in
    /// practice when manufacturers reuse codes across variants; the
    /// caller decides how to display them.
    fn lookup_article(&self, article: &str) -> Vec<&LuminaireDoc>;

    /// Prefix autosuggest over article numbers. Returns up to `limit`
    /// suggestions whose normalised key starts with the normalised
    /// `prefix`. Returns an empty vec for prefixes shorter than 3
    /// characters after normalisation (caller still pays a string
    /// walk; that's fine — keystrokes are cheap).
    ///
    /// The dropdown sees **original** (pre-normalisation) article
    /// strings, so cosmetic variants like `ART-1234` and `ART1234`
    /// each appear as their own suggestion. That's deliberate: real
    /// corpora ship both, for distinct SKUs.
    fn suggest_articles(&self, prefix: &str, limit: usize) -> Vec<ArticleSuggestion>;

    /// Prefix autosuggest for the free-text search bar. Classifies
    /// what the user typed and returns typed facet hints — e.g.
    /// typing `"aec"` yields `Manufacturer: AEC Illuminazione`,
    /// `"IP65"` yields `IpCode: IP65`, `"office"` yields
    /// `Application: Interior: Office: …`. Clicking a row in the UI
    /// applies the corresponding filter.
    ///
    /// Ranking: case-insensitive prefix matches first (sorted by
    /// descending doc count), then case-insensitive substring
    /// matches if room remains. Empty for prefixes shorter than 2
    /// characters (free text needs less context than article numbers).
    fn suggest_facets(&self, prefix: &str, limit: usize) -> Vec<FacetSuggestion>;
}

/// One entry in the article-prefix index. Keeps the original string
/// the manufacturer printed (cosmetic separators preserved) alongside
/// the doc it belongs to.
#[derive(Debug, Clone)]
struct ArticleEntry {
    /// Article number as it appears in the source GLDF (pre-normalisation).
    original: CompactString,
    /// Index into `InMemoryIndex.docs`.
    doc_ix: u32,
}

/// Vec-backed reference implementation. Linear scan over every doc per
/// query. Use for tests, small static deployments, and as the truth
/// the fst+roaring index must agree with.
///
/// Three structural indexes are built at construction time:
///
/// - `by_id` — O(1) [`doc`](IndexSource::doc) lookup.
/// - `by_article` — O(1) [`lookup_article`](IndexSource::lookup_article)
///   exact-match lookup keyed on each doc's `gtin` and `product_code`
///   (lowercased + non-alphanumerics stripped, so cosmetic variants
///   like `ART-1234` / `art1234` collapse to the same key). Mapped to
///   a `SmallVec` because a single article number occasionally maps
///   to multiple docs in real corpora.
/// - `article_prefix` — ordered index for autosuggest. Same
///   normalisation as `by_article` on the key, but the value
///   preserves the **original** (pre-normalisation) article string so
///   the dropdown shows what the manufacturer actually printed.
///
/// Synthetic bucket name for variants that declared `<Emergency>`
/// but no `<DedicatedEmergencyLightingType>`. The facet emit code
/// uses this string verbatim; the UI reverse-resolves it back into
/// a `Filter::has_emergency_lighting = Some(true)` click so the user
/// can still narrow on the long tail.
pub const UNSPECIFIED_EMERGENCY: &str = "(unspecified)";

/// Vec-backed reference implementation (continued). `search` and
/// `facets` still do linear scans by design — the fst+roaring version
/// lives in `MmapIndex` (M2).
#[derive(Debug, Clone, Default)]
pub struct InMemoryIndex {
    docs: Vec<LuminaireDoc>,
    by_id: HashMap<DocId, usize>,
    by_article: HashMap<CompactString, SmallVec<[u32; 1]>>,
    article_prefix: BTreeMap<CompactString, SmallVec<[ArticleEntry; 1]>>,
    /// Precomputed facet suggestions for the SearchBar autosuggest.
    /// Tiny (a few hundred entries total — manufacturers + XSD enum
    /// values + applications), sorted by lowercased value so the
    /// prefix scan is a `partition_point` + walk.
    facet_suggest: Vec<FacetSuggestion>,
    /// Lowercased copy of `facet_suggest[i].value()` for the prefix
    /// search. Kept as a separate parallel vec rather than baked
    /// into FacetSuggestion so the wire type stays clean.
    facet_suggest_lc: Vec<String>,
}

impl InMemoryIndex {
    /// Build from a `Vec<LuminaireDoc>` AFTER running the
    /// group-and-fold dedup pass. Use this for the production
    /// pipeline; folding collapses per-SKU duplicate GLDFs into
    /// family rollups and registers every source DocId as an
    /// alias in `by_id`, so old URLs keep resolving.
    ///
    /// Returns `(index, fold_stats)` so callers (the CLI / server)
    /// can log the dedup outcome.
    pub fn from_docs_folded(
        docs: Vec<LuminaireDoc>,
    ) -> (Self, crate::fold::FoldStats) {
        let (folded, aliases, stats) = crate::fold::fold_docs(docs);
        let me = Self::from_docs_with_aliases(folded, aliases);
        (me, stats)
    }

    /// Build from already-folded docs plus an alias map
    /// `source_doc_id -> family_doc_id`. Every alias becomes a
    /// `by_id` entry pointing at the family's index in `docs`.
    fn from_docs_with_aliases(
        docs: Vec<LuminaireDoc>,
        aliases: HashMap<DocId, DocId>,
    ) -> Self {
        let mut by_id: HashMap<DocId, usize> = HashMap::with_capacity(docs.len() + aliases.len());
        let mut by_article: HashMap<CompactString, SmallVec<[u32; 1]>> = HashMap::new();
        let mut article_prefix: BTreeMap<CompactString, SmallVec<[ArticleEntry; 1]>> =
            BTreeMap::new();

        // Canonical id → position in the folded docs vec.
        let mut canonical_to_ix: HashMap<DocId, usize> = HashMap::with_capacity(docs.len());
        for (i, d) in docs.iter().enumerate() {
            canonical_to_ix.insert(d.id, i);
        }
        // Map every alias to the same position. Includes the
        // canonical-id-for-itself entries that `fold_docs` always
        // produces, so the doc's own id resolves too.
        for (src_id, family_id) in &aliases {
            if let Some(&ix) = canonical_to_ix.get(family_id) {
                by_id.insert(*src_id, ix);
            }
        }
        // Defensive: also insert canonical→ix for any doc that for
        // some reason didn't show up in the alias map (shouldn't
        // happen — `fold_docs` always emits the self-alias).
        for (i, d) in docs.iter().enumerate() {
            by_id.entry(d.id).or_insert(i);
        }

        for (i, d) in docs.iter().enumerate() {
            for raw in [d.gtin.as_deref(), d.product_code.as_deref()]
                .into_iter()
                .flatten()
            {
                let key = normalize_article(raw);
                if key.is_empty() {
                    continue;
                }
                let slot = by_article.entry(key.clone()).or_default();
                let ix = i as u32;
                if !slot.contains(&ix) {
                    slot.push(ix);
                }
                let entries = article_prefix.entry(key).or_default();
                let already = entries
                    .iter()
                    .any(|e| e.doc_ix == ix && e.original.as_str() == raw);
                if !already {
                    entries.push(ArticleEntry {
                        original: CompactString::from(raw),
                        doc_ix: ix,
                    });
                }
            }
        }
        let mut me = Self {
            docs,
            by_id,
            by_article,
            article_prefix,
            facet_suggest: Vec::new(),
            facet_suggest_lc: Vec::new(),
        };
        me.build_facet_suggest_table();
        me
    }

    /// Build from a `Vec<LuminaireDoc>`. The vec is moved in; doc
    /// order is preserved as the insertion order. All structural
    /// indexes (id, article exact-match, article prefix) are computed
    /// once here.
    pub fn from_docs(docs: Vec<LuminaireDoc>) -> Self {
        let mut by_id = HashMap::with_capacity(docs.len());
        let mut by_article: HashMap<CompactString, SmallVec<[u32; 1]>> = HashMap::new();
        let mut article_prefix: BTreeMap<CompactString, SmallVec<[ArticleEntry; 1]>> =
            BTreeMap::new();
        for (i, d) in docs.iter().enumerate() {
            by_id.insert(d.id, i);
            // Index both identifier fields. Manufacturers sometimes
            // store the same value in both, which is harmless — the
            // SmallVec dedupe step below collapses duplicates.
            for raw in [d.gtin.as_deref(), d.product_code.as_deref()]
                .into_iter()
                .flatten()
            {
                let key = normalize_article(raw);
                if key.is_empty() {
                    continue;
                }
                // Exact-match index (collapses cosmetic variants).
                let slot = by_article.entry(key.clone()).or_default();
                let ix = i as u32;
                if !slot.contains(&ix) {
                    slot.push(ix);
                }
                // Prefix index — keep the original string per entry
                // so `ART-1234` and `ART1234` show up as distinct
                // suggestions even though they collide on the
                // normalised key. Deduplicate by (original, doc_ix)
                // so storing both fields on one doc doesn't double-
                // count.
                let entries = article_prefix.entry(key).or_default();
                let already = entries
                    .iter()
                    .any(|e| e.doc_ix == ix && e.original.as_str() == raw);
                if !already {
                    entries.push(ArticleEntry {
                        original: CompactString::from(raw),
                        doc_ix: ix,
                    });
                }
            }
        }
        let mut me = Self {
            docs,
            by_id,
            by_article,
            article_prefix,
            facet_suggest: Vec::new(),
            facet_suggest_lc: Vec::new(),
        };
        me.build_facet_suggest_table();
        me
    }

    /// One-shot build of the SearchBar autosuggest table. Walks all
    /// docs ~8 times via `count_one_facet`, but the result fits in a
    /// few KB and is queried per keystroke. Called once from
    /// `from_docs`.
    fn build_facet_suggest_table(&mut self) {
        use crate::facets::FacetField;
        let default_filter = Filter::default();

        let mut all: Vec<FacetSuggestion> = Vec::new();
        let kinds: &[FacetField] = &[
            FacetField::Manufacturer,
            FacetField::IpCode,
            FacetField::SafetyClass,
            FacetField::IkRating,
            FacetField::ProductForm,
            FacetField::Adjustability,
            FacetField::Labels,
            FacetField::Applications,
            FacetField::LampType,
            FacetField::ControlGearInterface,
            FacetField::EmergencyLightingType,
        ];
        for &kind in kinds {
            let counts = count_one_facet(self, &default_filter, kind);
            for c in counts {
                let s = match kind {
                    FacetField::Manufacturer => FacetSuggestion::Manufacturer {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::IpCode => FacetSuggestion::IpCode {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::SafetyClass => FacetSuggestion::SafetyClass {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::IkRating => FacetSuggestion::IkRating {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::ProductForm => FacetSuggestion::ProductForm {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::Adjustability => FacetSuggestion::Adjustability {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::Labels => FacetSuggestion::Label {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::Applications => FacetSuggestion::Application {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::LampType => FacetSuggestion::LampType {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::ControlGearInterface => FacetSuggestion::ControlGearInterface {
                        value: c.value,
                        count: c.count,
                    },
                    FacetField::EmergencyLightingType => FacetSuggestion::EmergencyLightingType {
                        value: c.value,
                        count: c.count,
                    },
                    // The other variants of FacetField are excluded
                    // by the `kinds` slice above.
                    _ => continue,
                };
                all.push(s);
            }
        }
        // Sort by lowercased value so the prefix scan can use
        // `partition_point`. Stable order across values with the
        // same casing is fine — we re-rank inside `suggest_facets`.
        all.sort_by(|a, b| {
            a.value()
                .to_lowercase()
                .cmp(&b.value().to_lowercase())
        });
        let lc: Vec<String> = all.iter().map(|s| s.value().to_lowercase()).collect();
        self.facet_suggest = all;
        self.facet_suggest_lc = lc;
    }

    /// Number of docs in the index. The future fst+roaring index will
    /// expose this on a richer Stats struct.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// `true` if no docs are indexed.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Borrow the underlying doc slice. Useful for snapshot tests.
    pub fn docs(&self) -> &[LuminaireDoc] {
        &self.docs
    }

    /// Resolve a slug (e.g. `"aec-illuminazione"` from
    /// `aec-illuminazione.gldf-search.de`) to the canonical
    /// manufacturer string the corpus uses (e.g. `"AEC Illuminazione"`).
    ///
    /// DNS labels can't contain underscores or spaces, so the
    /// subdomain spelling won't always match the corpus string
    /// verbatim. We try, in order:
    /// 1. **Exact-folded match** of the *whole* manufacturer string
    ///    against the slug — both lowercased, both with `-` and `_`
    ///    treated as whitespace. Catches `aec-illuminazione` →
    ///    `"AEC Illuminazione"`, `lena-lighting-s.a.` →
    ///    `"Lena Lighting S.A."`, and the boring single-word cases
    ///    (`philips` → `"Philips"`).
    /// 2. **First-word folded match**: slug equals only the first
    ///    word of the manufacturer (same folding rules). Lets
    ///    `aec` alone work even on the verbose corpus name.
    ///    Audited 2026-05-17: zero first-word collisions across
    ///    the 54 manufacturers in the current 270k corpus.
    /// 3. **Alias table** (hard-coded): for names whose literal
    ///    form can't be reconstructed by either rule — `a.l.s.`
    ///    (dots) and `nobilé` (diacritic). Extend as needed.
    ///
    /// Returns `None` only if none of the three resolve. Callers
    /// treat that as "no scope" — show the whole corpus.
    ///
    /// O(n) over the doc slice. Expected once per request, against
    /// ~270k docs — sub-millisecond. If this becomes hot, build an
    /// index in `from_docs` and look up there.
    pub fn resolve_manufacturer(&self, slug: &str) -> Option<CompactString> {
        let needle = slug.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return None;
        }
        let needle_folded = fold_slug(&needle);

        // Alias table — slugs that can't be reconstructed from the
        // corpus name via case-fold + hyphen-to-space. Kept tiny on
        // purpose: the corpus is the source of truth.
        let alias_target: Option<&str> = match needle.as_str() {
            "als" | "a-l-s" => Some("A.L.S."),
            "nobile" => Some("nobilé"),
            _ => None,
        };

        for doc in &self.docs {
            let m = doc.manufacturer.as_str();
            // 1. Whole-string folded match.
            if fold_slug(m) == needle_folded {
                return Some(doc.manufacturer.clone());
            }
            // 2. First-word folded match.
            if let Some(first) = m.split_whitespace().next() {
                if fold_slug(first) == needle_folded {
                    return Some(doc.manufacturer.clone());
                }
            }
            // 3. Alias — only used when 1+2 above failed.
            if let Some(canonical) = alias_target {
                if m == canonical {
                    return Some(doc.manufacturer.clone());
                }
            }
        }
        None
    }
}

/// Slug-fold a manufacturer string so a subdomain spelling and the
/// corpus spelling can be compared as equals.
///
/// Rules:
/// - ASCII-lowercase.
/// - `-` and `_` become a single ` ` (subdomain hyphens map to corpus
///   spaces; underscores in legacy URLs also work).
/// - Consecutive whitespace collapses to one space.
/// - Trailing whitespace stripped.
///
/// Non-ASCII characters (e.g. `é`) pass through unchanged — those
/// cases route through the alias table.
fn fold_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = true;
    for ch in s.chars() {
        let normalised = match ch {
            'A'..='Z' => ch.to_ascii_lowercase(),
            '-' | '_' => ' ',
            other => other,
        };
        if normalised == ' ' {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(normalised);
            last_was_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

impl IndexSource for InMemoryIndex {
    fn search(&self, filter: &Filter, page: Pagination) -> SearchResult {
        // Variant-grain search: each matching variant is one hit.
        // For un-folded docs with one variant this matches the old
        // doc-grain semantics 1:1. For folded family docs with N
        // variants, up to N hits per family — preserves the variant-
        // count UX users expect (a SKU is the calculation unit).
        let mut matching: Vec<SearchHit> = Vec::new();
        for doc in &self.docs {
            // Cheap doc-scoped reject first — same as matches_doc but
            // skipping the variant-AND step at the end.
            if !filter.matches_doc_scope(doc) {
                continue;
            }
            if filter.has_variant_predicate() {
                // Each surviving variant becomes its own hit.
                for (vi, v) in doc.variants.iter().enumerate() {
                    if filter.matches_variant(v) {
                        matching.push(SearchHit {
                            doc: doc.id,
                            variant: vi as u16,
                        });
                    }
                }
            } else {
                // No variant predicate — every variant is a hit.
                // Folded family contributes N hits, un-folded
                // single-variant docs contribute 1.
                for (vi, _) in doc.variants.iter().enumerate() {
                    matching.push(SearchHit {
                        doc: doc.id,
                        variant: vi as u16,
                    });
                }
            }
        }
        let total = matching.len() as u32;
        let start = (page.offset as usize).min(matching.len());
        let end = (start + page.limit as usize).min(matching.len());
        let hits: Vec<SearchHit> = matching.drain(start..end).collect();
        SearchResult { hits, total }
    }

    fn facets(&self, filter: &Filter, fields: &[FacetField]) -> FacetCounts {
        let mut out = FacetCounts::default();
        for &field in fields {
            let counts = count_one_facet(self, filter, field);
            assign_counts(&mut out, field, counts);
        }
        out
    }

    fn range_bounds(&self, filter: &Filter) -> RangeBounds {
        // One pass per axis. Each pass uses a filter clone with that
        // axis relaxed, mirroring `facets`. Cost: 6 corpus walks per
        // request — same shape as the 11-field facet recompute.
        RangeBounds {
            flux_lm: fold_f32_axis(self, filter, NumericAxis::Flux),
            power_w: fold_f32_axis(self, filter, NumericAxis::Power),
            efficacy: fold_f32_axis(self, filter, NumericAxis::Efficacy),
            cct_k: fold_u16_axis(self, filter, NumericAxis::Cct),
            beam_deg: fold_f32_axis(self, filter, NumericAxis::Beam),
            recessed_depth_mm: fold_u32_axis(self, filter, NumericAxis::RecessedDepth),
        }
    }

    fn doc(&self, id: &DocId) -> Option<&LuminaireDoc> {
        self.by_id.get(id).map(|&i| &self.docs[i])
    }

    fn lookup_article(&self, article: &str) -> Vec<&LuminaireDoc> {
        let key = normalize_article(article);
        if key.is_empty() {
            return Vec::new();
        }
        match self.by_article.get(&key) {
            Some(ixs) => ixs.iter().map(|&i| &self.docs[i as usize]).collect(),
            None => Vec::new(),
        }
    }

    fn suggest_facets(&self, prefix: &str, limit: usize) -> Vec<FacetSuggestion> {
        let p = prefix.trim().to_lowercase();
        if p.len() < 2 || limit == 0 {
            return Vec::new();
        }

        // Prefix matches: `partition_point` finds the first lc-value
        // ≥ p; we then walk forward collecting while starts_with.
        let start = self
            .facet_suggest_lc
            .partition_point(|v| v.as_str() < p.as_str());
        let mut prefix_hits: Vec<&FacetSuggestion> = Vec::new();
        for i in start..self.facet_suggest_lc.len() {
            if !self.facet_suggest_lc[i].starts_with(p.as_str()) {
                break;
            }
            prefix_hits.push(&self.facet_suggest[i]);
        }

        // Rank prefix hits by descending count, then alphabetic.
        prefix_hits.sort_by(|a, b| {
            b.count()
                .cmp(&a.count())
                .then_with(|| a.value().cmp(b.value()))
        });

        let mut out: Vec<FacetSuggestion> = prefix_hits
            .iter()
            .take(limit)
            .map(|s| (*s).clone())
            .collect();

        // If still room, fall back to substring matches (mid-word
        // hits like `office` matching `Interior: Office: Office Desks`
        // would already be a prefix hit; this catches e.g. typing
        // `desks` to land on the same entry).
        if out.len() < limit {
            let need = limit - out.len();
            let mut substr_hits: Vec<&FacetSuggestion> = Vec::new();
            for (i, lc) in self.facet_suggest_lc.iter().enumerate() {
                if lc.starts_with(p.as_str()) {
                    continue; // already in prefix_hits
                }
                if lc.contains(p.as_str()) {
                    substr_hits.push(&self.facet_suggest[i]);
                }
            }
            substr_hits.sort_by(|a, b| {
                b.count()
                    .cmp(&a.count())
                    .then_with(|| a.value().cmp(b.value()))
            });
            out.extend(substr_hits.into_iter().take(need).cloned());
        }

        out
    }

    fn suggest_articles(&self, prefix: &str, limit: usize) -> Vec<ArticleSuggestion> {
        // Require at least 3 normalised characters. Shorter prefixes
        // would match too many keys to be useful (and would defeat
        // the point of typing-as-search).
        let norm = normalize_article(prefix);
        if norm.len() < 3 || limit == 0 {
            return Vec::new();
        }

        let mut out: Vec<ArticleSuggestion> = Vec::with_capacity(limit.min(64));
        // Range starting at `norm`, stopping as soon as the key no
        // longer starts with `norm`. `BTreeMap::range` over a
        // ascending-ordered key set guarantees we visit every
        // matching key in order.
        for (key, entries) in self.article_prefix.range(norm.clone()..) {
            if !key.starts_with(norm.as_str()) {
                break;
            }
            for entry in entries {
                let doc = &self.docs[entry.doc_ix as usize];
                out.push(ArticleSuggestion {
                    article: entry.original.to_string(),
                    manufacturer: doc.manufacturer.to_string(),
                    product: doc.product.to_string(),
                });
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }
}

/// Canonical form for article-number lookup keys. Lowercases ASCII
/// and drops every non-alphanumeric byte (so `"ART-1234"`,
/// `"art 1234"`, `"art.1234"` all collapse to the same key). Empty
/// strings round-trip to empty — the caller treats that as
/// "no input" and skips the lookup.
pub(crate) fn normalize_article(raw: &str) -> CompactString {
    let mut out = CompactString::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            // ASCII fast path — same trick the text predicate uses.
            out.push(c.to_ascii_lowercase());
        } else if c.is_alphanumeric() {
            // Unicode-letter fallback (rare in corpus identifiers but
            // cheap to support). Lowercase may return multiple chars
            // for some scripts; honour all of them.
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        }
    }
    out
}

/// Compute one facet's counts with that facet relaxed in the filter.
/// Returns a vec sorted by descending count, then ascending value.
fn count_one_facet(
    idx: &InMemoryIndex,
    filter: &Filter,
    field: FacetField,
) -> Vec<FacetValueCount> {
    let relaxed = relax_filter(filter, field);
    // Counts keyed by the canonical string form for the facet.
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for doc in &idx.docs {
        if !relaxed.matches_doc(doc) {
            continue;
        }
        emit_doc_values(doc, field, &mut counts);
    }
    let mut out: Vec<FacetValueCount> = counts
        .into_iter()
        .map(|(value, count)| FacetValueCount { value, count })
        .collect();
    // Sort: descending count, then ascending value.
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    out
}

/// Which numeric axis a `range_bounds` pass is computing. Local to
/// this module — `RangeBounds` itself is keyed by field name, not
/// by enum, so the wire surface doesn't need this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumericAxis {
    Flux,
    Power,
    Efficacy,
    Cct,
    Beam,
    RecessedDepth,
}

/// Clone `filter` and clear the numeric range slot for the axis
/// we're about to compute bounds for. Mirrors `relax_filter` for
/// categorical facets.
///
/// **Recessed-depth note**: we only clear `recessed_depth_mm`,
/// **not** `mounting_type`. The `recessed_depth_mm` filter is
/// "only matches recessed variants with a declared depth in this
/// range"; relaxing the depth keeps the mounting constraint so the
/// rails reflect "depths reachable for the user's current mounting
/// choice".
fn relax_numeric(filter: &Filter, axis: NumericAxis) -> Filter {
    let mut f = filter.clone();
    match axis {
        NumericAxis::Flux => f.flux_lm = None,
        NumericAxis::Power => f.power_w = None,
        NumericAxis::Efficacy => f.efficacy = None,
        NumericAxis::Cct => f.cct_k = None,
        NumericAxis::Beam => f.beam_deg = None,
        NumericAxis::RecessedDepth => f.recessed_depth_mm = None,
    }
    f
}

/// Fold min/max of an `f32` axis (flux / power / efficacy / beam).
/// Walks `idx.docs` once with the axis relaxed, applies
/// `matches_doc` for the cheap doc-scoped rejects, then per
/// surviving variant applies `matches_variant` (so other variant-
/// scoped filters still bite) and reads the relevant
/// `photometry.<field>`. Returns `None` if no variant had a value
/// for the axis.
fn fold_f32_axis(idx: &InMemoryIndex, filter: &Filter, axis: NumericAxis) -> Option<F32Bounds> {
    let relaxed = relax_numeric(filter, axis);
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut any = false;
    for doc in &idx.docs {
        if !relaxed.matches_doc(doc) {
            continue;
        }
        for v in &doc.variants {
            if !relaxed.matches_variant(v) {
                continue;
            }
            let Some(p) = v.photometry.as_ref() else {
                continue;
            };
            let value = match axis {
                NumericAxis::Flux => p.flux_lm,
                NumericAxis::Power => p.power_w,
                NumericAxis::Efficacy => p.efficacy_lm_w,
                NumericAxis::Beam => p.beam_deg,
                _ => unreachable!("fold_f32_axis called with non-f32 axis"),
            };
            if let Some(v) = value {
                if v.is_finite() {
                    if v < min {
                        min = v;
                    }
                    if v > max {
                        max = v;
                    }
                    any = true;
                }
            }
        }
    }
    if any {
        Some(F32Bounds { min, max })
    } else {
        None
    }
}

/// Fold min/max of the `u16` CCT axis.
fn fold_u16_axis(idx: &InMemoryIndex, filter: &Filter, axis: NumericAxis) -> Option<U16Bounds> {
    debug_assert!(matches!(axis, NumericAxis::Cct));
    let relaxed = relax_numeric(filter, axis);
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    let mut any = false;
    for doc in &idx.docs {
        if !relaxed.matches_doc(doc) {
            continue;
        }
        for v in &doc.variants {
            if !relaxed.matches_variant(v) {
                continue;
            }
            let Some(p) = v.photometry.as_ref() else {
                continue;
            };
            if let Some(cct) = p.cct_k {
                if cct < min {
                    min = cct;
                }
                if cct > max {
                    max = cct;
                }
                any = true;
            }
        }
    }
    if any {
        Some(U16Bounds { min, max })
    } else {
        None
    }
}

/// Fold min/max of the `u32` recessed-depth axis. Only folds over
/// variants whose `mounting_type == Recessed` AND that declare a
/// depth (same guard as `Filter::matches_variant`).
fn fold_u32_axis(idx: &InMemoryIndex, filter: &Filter, axis: NumericAxis) -> Option<U32Bounds> {
    debug_assert!(matches!(axis, NumericAxis::RecessedDepth));
    let relaxed = relax_numeric(filter, axis);
    let mut min = u32::MAX;
    let mut max = u32::MIN;
    let mut any = false;
    for doc in &idx.docs {
        if !relaxed.matches_doc(doc) {
            continue;
        }
        for v in &doc.variants {
            if !relaxed.matches_variant(v) {
                continue;
            }
            if v.mounting_type != MountingType::Recessed {
                continue;
            }
            if let Some(depth) = v.recessed_depth_mm {
                if depth < min {
                    min = depth;
                }
                if depth > max {
                    max = depth;
                }
                any = true;
            }
        }
    }
    if any {
        Some(U32Bounds { min, max })
    } else {
        None
    }
}

/// Clone `filter` and clear the facet field we're about to count.
fn relax_filter(filter: &Filter, field: FacetField) -> Filter {
    let mut f = filter.clone();
    match field {
        FacetField::Manufacturer => f.manufacturer = None,
        FacetField::Applications => f.applications.clear(),
        FacetField::Labels => f.labels.clear(),
        FacetField::IpCode => f.ip_code.clear(),
        FacetField::SafetyClass => f.safety_class.clear(),
        FacetField::IkRating => f.ik_rating.clear(),
        FacetField::ProductForm => f.product_form.clear(),
        FacetField::Adjustability => f.adjustability.clear(),
        FacetField::MountingPlace => f.mounting_place.clear(),
        FacetField::MountingType => f.mounting_type.clear(),
        FacetField::LightDistribution => f.light_distribution.clear(),
        FacetField::HasPhotometry => f.has_photometry = None,
        FacetField::Has3d => f.has_3d = None,
        FacetField::HasEmergencyLighting => f.has_emergency_lighting = None,
        FacetField::LampType => f.lamp_type.clear(),
        FacetField::ControlGearInterface => f.control_gear_interfaces.clear(),
        FacetField::EmergencyLightingType => f.emergency_lighting_type.clear(),
    }
    f
}

/// Emit one or more `(value, +1)` increments for a single doc into
/// the running counts map.
fn emit_doc_values(doc: &LuminaireDoc, field: FacetField, counts: &mut BTreeMap<String, u32>) {
    match field {
        FacetField::Manufacturer => {
            if !doc.manufacturer.is_empty() {
                *counts.entry(doc.manufacturer.to_string()).or_default() += 1;
            }
        }
        FacetField::Applications => {
            for &id in &doc.applications {
                if let Some(s) = application_str(id) {
                    *counts.entry(s.to_string()).or_default() += 1;
                }
            }
        }
        FacetField::Labels => {
            for &id in &doc.labels {
                if let Some(s) = label_str(id) {
                    *counts.entry(s.to_string()).or_default() += 1;
                }
            }
        }
        FacetField::IpCode => {
            if let Some(id) = doc.ip_code {
                if let Some(s) = ip_code_str(id) {
                    *counts.entry(s.to_string()).or_default() += 1;
                }
            }
        }
        FacetField::SafetyClass => {
            if let Some(id) = doc.safety_class {
                if let Some(s) = safety_class_str(id) {
                    *counts.entry(s.to_string()).or_default() += 1;
                }
            }
        }
        FacetField::IkRating => {
            if let Some(id) = doc.ik_rating {
                if let Some(s) = ik_rating_str(id) {
                    *counts.entry(s.to_string()).or_default() += 1;
                }
            }
        }
        FacetField::ProductForm => {
            if let Some(id) = doc.product_form {
                if let Some(s) = product_form_str(id) {
                    *counts.entry(s.to_string()).or_default() += 1;
                }
            }
        }
        FacetField::Adjustability => {
            for &id in &doc.adjustability {
                if let Some(s) = adjustability_str(id) {
                    *counts.entry(s.to_string()).or_default() += 1;
                }
            }
        }
        FacetField::MountingPlace => {
            // Doc-level mounting-place aggregation: increment once
            // per distinct variant `mounting_place` the doc declares.
            // Avoids double-counting a doc whose two variants are
            // both `Ceiling`.
            let mut seen: BTreeMap<MountingPlace, ()> = BTreeMap::new();
            for v in &doc.variants {
                if seen.insert(v.mounting_place, ()).is_none() {
                    let key = format!("{:?}", v.mounting_place);
                    *counts.entry(key).or_default() += 1;
                }
            }
        }
        FacetField::MountingType => {
            // Same one-per-distinct logic for `mounting_type`.
            let mut seen: BTreeMap<MountingType, ()> = BTreeMap::new();
            for v in &doc.variants {
                if seen.insert(v.mounting_type, ()).is_none() {
                    let key = format!("{:?}", v.mounting_type);
                    *counts.entry(key).or_default() += 1;
                }
            }
        }
        FacetField::LightDistribution => {
            let mut seen: BTreeMap<u8, ()> = BTreeMap::new();
            for v in &doc.variants {
                let Some(p) = v.photometry.as_ref() else {
                    continue;
                };
                let Some(ld) = p.light_distribution else {
                    continue;
                };
                if seen.insert(ld, ()).is_none() {
                    if let Some(s) = light_distribution_str(ld) {
                        *counts.entry(s.to_string()).or_default() += 1;
                    }
                }
            }
        }
        FacetField::HasPhotometry => {
            let any_present = doc.variants.iter().any(|v| v.photometry.is_some());
            let any_missing = doc.variants.iter().any(|v| v.photometry.is_none());
            if any_present {
                *counts.entry("present".to_string()).or_default() += 1;
            }
            if any_missing {
                *counts.entry("missing".to_string()).or_default() += 1;
            }
        }
        FacetField::Has3d => {
            let any_present = doc.variants.iter().any(|v| v.has_3d);
            let any_missing = doc.variants.iter().any(|v| !v.has_3d);
            if any_present {
                *counts.entry("present".to_string()).or_default() += 1;
            }
            if any_missing {
                *counts.entry("missing".to_string()).or_default() += 1;
            }
        }
        FacetField::HasEmergencyLighting => {
            let any_present = doc.variants.iter().any(|v| v.has_emergency_lighting);
            let any_missing = doc.variants.iter().any(|v| !v.has_emergency_lighting);
            if any_present {
                *counts.entry("present".to_string()).or_default() += 1;
            }
            if any_missing {
                *counts.entry("missing".to_string()).or_default() += 1;
            }
        }
        FacetField::LampType => {
            // Count once per distinct lamp_type the doc declares.
            let mut seen: BTreeMap<u8, ()> = BTreeMap::new();
            for v in &doc.variants {
                let Some(id) = v.lamp_type else {
                    continue;
                };
                if seen.insert(id, ()).is_none() {
                    if let Some(s) = lamp_type_str(id) {
                        *counts.entry(s.to_string()).or_default() += 1;
                    }
                }
            }
        }
        FacetField::ControlGearInterface => {
            let mut seen: BTreeMap<u8, ()> = BTreeMap::new();
            for v in &doc.variants {
                for &id in &v.control_gear_interfaces {
                    if seen.insert(id, ()).is_none() {
                        if let Some(s) = control_gear_interface_str(id) {
                            *counts.entry(s.to_string()).or_default() += 1;
                        }
                    }
                }
            }
        }
        FacetField::EmergencyLightingType => {
            // Two streams feed this facet:
            //   1. Variants that declared a canonical subtype — bucket
            //      by the typed value (`Exit Light`, etc.).
            //   2. Variants flagged as emergency but with no subtype
            //      — bucket as the synthetic `(unspecified)` value so
            //      they're still discoverable from this facet.
            // Each doc contributes at most once per bucket via the
            // `seen` set keyed by bucket name (`type:<id>` /
            // `unspecified`), avoiding double-counts when multiple
            // variants share the same subtype.
            let mut seen_typed: BTreeMap<u8, ()> = BTreeMap::new();
            let mut seen_unspec = false;
            for v in &doc.variants {
                match (v.has_emergency_lighting, v.emergency_lighting_type) {
                    (_, Some(id)) => {
                        if seen_typed.insert(id, ()).is_none() {
                            if let Some(s) = emergency_lighting_type_str(id) {
                                *counts.entry(s.to_string()).or_default() += 1;
                            }
                        }
                    }
                    (true, None) => {
                        if !seen_unspec {
                            seen_unspec = true;
                            *counts.entry(UNSPECIFIED_EMERGENCY.to_string()).or_default() +=
                                1;
                        }
                    }
                    (false, None) => {}
                }
            }
        }
    }
}

/// Push the per-field counts into the right field of [`FacetCounts`].
fn assign_counts(out: &mut FacetCounts, field: FacetField, counts: Vec<FacetValueCount>) {
    match field {
        FacetField::Manufacturer => out.manufacturer = counts,
        FacetField::Applications => out.applications = counts,
        FacetField::Labels => out.labels = counts,
        FacetField::IpCode => out.ip_code = counts,
        FacetField::SafetyClass => out.safety_class = counts,
        FacetField::IkRating => out.ik_rating = counts,
        FacetField::ProductForm => out.product_form = counts,
        FacetField::Adjustability => out.adjustability = counts,
        FacetField::MountingPlace => out.mounting_place = counts,
        FacetField::MountingType => out.mounting_type = counts,
        FacetField::LightDistribution => out.light_distribution = counts,
        FacetField::HasPhotometry => out.has_photometry = counts,
        FacetField::Has3d => out.has_3d = counts,
        FacetField::HasEmergencyLighting => out.has_emergency_lighting = counts,
        FacetField::LampType => out.lamp_type = counts,
        FacetField::ControlGearInterface => out.control_gear_interfaces = counts,
        FacetField::EmergencyLightingType => out.emergency_lighting_type = counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;
    use gldf_search_schema::doc::{
        FileMeta, LuminaireDoc, PhotometricSymmetryHint, PhotometryStats, SourceRef, VariantDoc,
        VariantId,
    };
    use smallvec::smallvec;

    fn doc_id(seed: u8) -> DocId {
        DocId([seed; 32])
    }

    fn mk_variant(flux: f32, power: f32, cct: u16) -> VariantDoc {
        VariantDoc {
            id: VariantId(0),
            name: CompactString::from("v"),
            photometry: Some(PhotometryStats {
                flux_lm: Some(flux),
                power_w: Some(power),
                efficacy_lm_w: if power > 0.0 {
                    Some(flux / power)
                } else {
                    None
                },
                cct_k: Some(cct),
                cri_ra: Some(80),
                r9: None,
                beam_deg: Some(30.0),
                field_deg: None,
                ulor: None,
                dlor: None,
                symmetry: PhotometricSymmetryHint::Rotational,
                light_distribution: Some(11),
                ..Default::default()
            }),
            mounting_place: MountingPlace::Ceiling,
            mounting_type: MountingType::SurfaceMounted,
            recessed_depth_mm: None,
            has_3d: false,
            lamp_type: None,
            control_gear_interfaces: smallvec![],
            has_emergency_lighting: false,
            emergency_lighting_type: None,
            emitter_count: 1,
            source_index: 0,
        }
    }

    fn mk_doc(id: u8, manufacturer: &str, product: &str, ip_code: Option<u8>) -> LuminaireDoc {
        LuminaireDoc {
            id: doc_id(id),
            source: SourceRef::ContentOnly,
            source_paths: smallvec![SourceRef::ContentOnly],
            manufacturer: CompactString::from(manufacturer),
            product: CompactString::from(product),
            gtin: None,
            product_code: None,
            descriptions: smallvec![],
            keywords: smallvec![],
            applications: smallvec![],
            labels: smallvec![],
            adjustability: smallvec![],
            ip_code,
            safety_class: None,
            ik_rating: None,
            product_form: None,
            variants: smallvec![mk_variant(1500.0, 15.0, 4000)],
            file_meta: FileMeta {
                size_bytes: 0,
                mtime_epoch_s: None,
                format_version: None,
            },
        }
    }

    #[test]
    fn empty_filter_returns_everything() {
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc(1, "Acme", "P1", Some(0)),
            mk_doc(2, "Beta", "P2", Some(1)),
        ]);
        let r = idx.search(&Filter::default(), Pagination::DEFAULT);
        assert_eq!(r.total, 2);
        assert_eq!(r.hits.len(), 2);
    }

    #[test]
    fn manufacturer_filter_narrows() {
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc(1, "Acme", "P1", Some(0)),
            mk_doc(2, "Beta", "P2", Some(1)),
            mk_doc(3, "Acme", "P3", Some(0)),
        ]);
        let f = Filter {
            manufacturer: Some(smallvec![CompactString::from("Acme")]),
            ..Default::default()
        };
        let r = idx.search(&f, Pagination::DEFAULT);
        assert_eq!(r.total, 2);
    }

    #[test]
    fn pagination_returns_window_but_keeps_total() {
        let docs: Vec<LuminaireDoc> = (1..=5_u8)
            .map(|i| mk_doc(i, "Acme", "P", Some(0)))
            .collect();
        let idx = InMemoryIndex::from_docs(docs);
        let r = idx.search(
            &Filter::default(),
            Pagination {
                offset: 2,
                limit: 2,
            },
        );
        assert_eq!(r.total, 5);
        assert_eq!(r.hits.len(), 2);
    }

    #[test]
    fn doc_lookup_works() {
        let idx = InMemoryIndex::from_docs(vec![mk_doc(42, "Acme", "P", Some(0))]);
        assert!(idx.doc(&doc_id(42)).is_some());
        assert!(idx.doc(&doc_id(99)).is_none());
    }

    #[test]
    fn facets_count_per_value() {
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc(1, "Acme", "P1", Some(0)),
            mk_doc(2, "Acme", "P2", Some(0)),
            mk_doc(3, "Beta", "P3", Some(1)),
        ]);
        let counts = idx.facets(&Filter::default(), &[FacetField::Manufacturer]);
        assert_eq!(counts.manufacturer.len(), 2);
        assert_eq!(counts.manufacturer[0].value, "Acme");
        assert_eq!(counts.manufacturer[0].count, 2);
        assert_eq!(counts.manufacturer[1].value, "Beta");
        assert_eq!(counts.manufacturer[1].count, 1);
    }

    #[test]
    fn facet_relaxes_only_the_field_being_counted() {
        // Two manufacturers, both have a doc with ip_code=0. A filter
        // narrowing to ip_code=0 + manufacturer=Acme should:
        // - For Manufacturer facet: relax manufacturer → still constrain
        //   on ip_code=0 → both Acme and Beta have ip=0, so both appear.
        // - For IpCode facet: relax ip_code → still constrain on
        //   manufacturer=Acme → only Acme-ip0 doc counts → IP code at
        //   index 0 appears once.
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc(1, "Acme", "P1", Some(0)),
            mk_doc(2, "Acme", "P2", Some(1)),
            mk_doc(3, "Beta", "P3", Some(0)),
        ]);
        let f = Filter {
            manufacturer: Some(smallvec![CompactString::from("Acme")]),
            ip_code: smallvec![0],
            ..Default::default()
        };
        let counts = idx.facets(&f, &[FacetField::Manufacturer, FacetField::IpCode]);
        // Manufacturer counts: with manufacturer relaxed, both Acme-ip0
        // and Beta-ip0 satisfy the rest (ip_code = 0).
        assert_eq!(counts.manufacturer.len(), 2);
        // IpCode counts: with ip_code relaxed, only Acme docs survive
        // (manufacturer=Acme), and they have ip codes 0 and 1.
        assert_eq!(counts.ip_code.len(), 2);
    }

    #[test]
    fn has_photometry_facet_counts_present_and_missing() {
        let doc_with = mk_doc(1, "Acme", "with", Some(0));
        let mut doc_without = mk_doc(2, "Acme", "without", Some(0));
        for v in doc_without.variants.iter_mut() {
            v.photometry = None;
        }
        // Confirm doc_with's variant still has photometry (mk_variant
        // builds it with Some).
        let _ = doc_with.variants[0].photometry.as_ref().unwrap();
        let idx = InMemoryIndex::from_docs(vec![doc_with, doc_without]);
        let counts = idx.facets(&Filter::default(), &[FacetField::HasPhotometry]);
        let values: Vec<(&str, u32)> = counts
            .has_photometry
            .iter()
            .map(|c| (c.value.as_str(), c.count))
            .collect();
        assert!(values.contains(&("present", 1)), "got: {values:?}");
        assert!(values.contains(&("missing", 1)), "got: {values:?}");
    }

    fn mk_doc_with_codes(
        id: u8,
        manufacturer: &str,
        product: &str,
        gtin: Option<&str>,
        product_code: Option<&str>,
    ) -> LuminaireDoc {
        let mut d = mk_doc(id, manufacturer, product, Some(0));
        d.gtin = gtin.map(CompactString::from);
        d.product_code = product_code.map(CompactString::from);
        d
    }

    #[test]
    fn lookup_article_exact_match() {
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc_with_codes(1, "Acme", "P1", Some("4012345678901"), None),
            mk_doc_with_codes(2, "Beta", "P2", None, Some("BX-200")),
        ]);
        // GTIN match.
        assert_eq!(idx.lookup_article("4012345678901").len(), 1);
        assert_eq!(idx.lookup_article("4012345678901")[0].id, doc_id(1));
        // product_code match.
        assert_eq!(idx.lookup_article("BX-200").len(), 1);
        assert_eq!(idx.lookup_article("BX-200")[0].id, doc_id(2));
    }

    #[test]
    fn lookup_article_normalises_separators_and_case() {
        let idx = InMemoryIndex::from_docs(vec![mk_doc_with_codes(
            1,
            "Acme",
            "P",
            None,
            Some("ART-1234"),
        )]);
        // Same key under three cosmetic variants.
        assert_eq!(idx.lookup_article("art1234").len(), 1);
        assert_eq!(idx.lookup_article("ART 1234").len(), 1);
        assert_eq!(idx.lookup_article("art.1234").len(), 1);
    }

    #[test]
    fn lookup_article_misses_return_empty() {
        let idx =
            InMemoryIndex::from_docs(vec![mk_doc_with_codes(1, "Acme", "P", None, Some("X1"))]);
        assert!(idx.lookup_article("nope").is_empty());
        // Empty / pure-punctuation queries normalise to "" and miss
        // cleanly (no panic, no spurious hits).
        assert!(idx.lookup_article("").is_empty());
        assert!(idx.lookup_article(" - . / ").is_empty());
    }

    #[test]
    fn lookup_article_returns_all_matches_when_shared() {
        // Two docs sharing the same product_code — rare but legal.
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc_with_codes(1, "Acme", "P1", None, Some("SHARED")),
            mk_doc_with_codes(2, "Beta", "P2", None, Some("shared")),
        ]);
        let hits = idx.lookup_article("SHARED");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn suggest_articles_min_3_chars() {
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc_with_codes(1, "Acme", "P1", None, Some("ART123")),
        ]);
        // Below the threshold: empty.
        assert!(idx.suggest_articles("ar", 10).is_empty());
        assert!(idx.suggest_articles("a", 10).is_empty());
        // 3 chars: hit.
        let s = idx.suggest_articles("art", 10);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].article, "ART123");
        assert_eq!(s[0].manufacturer, "Acme");
    }

    #[test]
    fn suggest_articles_preserves_cosmetic_variants() {
        // Manufacturer ships both ART-123 and ART123 as distinct SKUs.
        // Their normalised key collides (art123) but the dropdown
        // shows the originals so the operator can pick the right one.
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc_with_codes(1, "Acme", "P1", None, Some("ART-123")),
            mk_doc_with_codes(2, "Acme", "P2", None, Some("ART123")),
        ]);
        let s = idx.suggest_articles("art", 10);
        assert_eq!(s.len(), 2, "got: {s:?}");
        let originals: Vec<&str> = s.iter().map(|x| x.article.as_str()).collect();
        assert!(originals.contains(&"ART-123"));
        assert!(originals.contains(&"ART123"));
    }

    #[test]
    fn suggest_articles_caps_at_limit() {
        let docs: Vec<LuminaireDoc> = (1..=10_u8)
            .map(|i| {
                mk_doc_with_codes(
                    i,
                    "Acme",
                    "P",
                    None,
                    Some(&format!("ART{:03}", i)),
                )
            })
            .collect();
        let idx = InMemoryIndex::from_docs(docs);
        let s = idx.suggest_articles("art", 4);
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn suggest_facets_manufacturer_prefix() {
        // Three manufacturers, one match.
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc(1, "AEC Illuminazione", "P1", Some(0)),
            mk_doc(2, "ERCO", "P2", Some(0)),
            mk_doc(3, "AEC Illuminazione", "P3", Some(0)),
        ]);
        let s = idx.suggest_facets("aec", 5);
        assert!(!s.is_empty(), "got: {s:?}");
        let first = &s[0];
        match first {
            FacetSuggestion::Manufacturer { value, count } => {
                assert_eq!(value, "AEC Illuminazione");
                assert_eq!(*count, 2);
            }
            _ => panic!("expected Manufacturer suggestion, got {first:?}"),
        }
    }

    #[test]
    fn suggest_facets_ip_code_prefix() {
        // Two docs at IP54 (ix=20 in XSD table?) vs one at IP65.
        // We don't need the precise indexes; just verify the canonical
        // string is recovered.
        let idx = InMemoryIndex::from_docs(vec![
            // Use the doc helper's `ip_code: Some(idx)` directly. The
            // canonical strings come from XSD, but for the prefix
            // scan we just need *some* values populated. Pick any
            // small ip_code idx (0).
            mk_doc(1, "Acme", "P", Some(0)),
            mk_doc(2, "Acme", "P", Some(0)),
        ]);
        // Whatever XSD canonical string is at ip_code 0, suggesting
        // its 2-char prefix should hit IpCode kind.
        let xsd_str = gldf_search_schema::enums::ip_code_str(0)
            .expect("ip_code(0) exists in xsd table");
        let prefix: String = xsd_str.chars().take(3).collect::<String>().to_lowercase();
        let s = idx.suggest_facets(&prefix, 10);
        let any_ip = s
            .iter()
            .any(|x| matches!(x, FacetSuggestion::IpCode { .. }));
        assert!(any_ip, "no IpCode suggestion in {s:?}");
    }

    #[test]
    fn suggest_facets_min_2_chars() {
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "Acme", "P", Some(0))]);
        assert!(idx.suggest_facets("a", 10).is_empty());
        assert!(idx.suggest_facets("", 10).is_empty());
    }

    #[test]
    fn suggest_articles_prefix_walks_ordered_keys() {
        // Two distinct prefix neighbourhoods. "art" should hit only
        // the ART* entries, not BAY*.
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc_with_codes(1, "Acme", "P1", None, Some("ART100")),
            mk_doc_with_codes(2, "Acme", "P2", None, Some("ART200")),
            mk_doc_with_codes(3, "Bay", "P3", None, Some("BAY999")),
        ]);
        let s = idx.suggest_articles("art", 10);
        assert_eq!(s.len(), 2);
        let s2 = idx.suggest_articles("bay", 10);
        assert_eq!(s2.len(), 1);
    }

    #[test]
    fn facets_use_relaxed_count_not_full_corpus() {
        // 3 docs: ip 0/0/1. With filter constraining manufacturer to
        // Acme (only one Acme), the ip-code facet should show counts
        // of Acme docs only — not the whole corpus.
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc(1, "Acme", "P", Some(0)),
            mk_doc(2, "Beta", "P", Some(0)),
            mk_doc(3, "Beta", "P", Some(1)),
        ]);
        let f = Filter {
            manufacturer: Some(smallvec![CompactString::from("Acme")]),
            ..Default::default()
        };
        let counts = idx.facets(&f, &[FacetField::IpCode]);
        // Only one Acme doc, its ip=0.
        assert_eq!(counts.ip_code.len(), 1);
        assert_eq!(counts.ip_code[0].count, 1);
    }

    // ── range_bounds ──────────────────────────────────────────────────

    fn doc_with_variant(id: u8, manufacturer: &str, v: VariantDoc) -> LuminaireDoc {
        LuminaireDoc {
            id: doc_id(id),
            source: SourceRef::ContentOnly,
            source_paths: smallvec![SourceRef::ContentOnly],
            manufacturer: CompactString::from(manufacturer),
            product: CompactString::from("P"),
            gtin: None,
            product_code: None,
            descriptions: smallvec![],
            keywords: smallvec![],
            applications: smallvec![],
            labels: smallvec![],
            adjustability: smallvec![],
            ip_code: None,
            safety_class: None,
            ik_rating: None,
            product_form: None,
            variants: smallvec![v],
            file_meta: FileMeta {
                size_bytes: 0,
                mtime_epoch_s: None,
                format_version: None,
            },
        }
    }

    #[test]
    fn range_bounds_empty_filter_spans_corpus() {
        // Two docs with flux 1000 and 5000 → bounds 1000..=5000.
        let idx = InMemoryIndex::from_docs(vec![
            doc_with_variant(1, "Acme", mk_variant(1000.0, 10.0, 3000)),
            doc_with_variant(2, "Beta", mk_variant(5000.0, 50.0, 5000)),
        ]);
        let b = idx.range_bounds(&Filter::default());
        let flux = b.flux_lm.expect("flux bounds should be present");
        assert!((flux.min - 1000.0).abs() < 0.01);
        assert!((flux.max - 5000.0).abs() < 0.01);
        let cct = b.cct_k.expect("cct bounds should be present");
        assert_eq!(cct.min, 3000);
        assert_eq!(cct.max, 5000);
    }

    #[test]
    fn range_bounds_flux_axis_is_relaxed_under_its_own_filter() {
        // If the user sets flux 1500..=2000 and the corpus has
        // variants 1000 / 3000 / 5000, the flux bounds returned should
        // still be 1000..=5000 (axis relaxed), NOT 3000..=3000.
        let idx = InMemoryIndex::from_docs(vec![
            doc_with_variant(1, "Acme", mk_variant(1000.0, 10.0, 3000)),
            doc_with_variant(2, "Acme", mk_variant(3000.0, 30.0, 4000)),
            doc_with_variant(3, "Acme", mk_variant(5000.0, 50.0, 5000)),
        ]);
        let f = Filter {
            flux_lm: Some(1500.0..=2000.0),
            ..Default::default()
        };
        let b = idx.range_bounds(&f);
        let flux = b.flux_lm.expect("flux bounds should be present");
        assert!((flux.min - 1000.0).abs() < 0.01);
        assert!((flux.max - 5000.0).abs() < 0.01);
    }

    #[test]
    fn range_bounds_narrows_when_other_filter_active() {
        // Two manufacturers; Acme has flux 1000, Beta has flux 5000.
        // Filtering manufacturer=Acme should narrow flux bounds to 1000.
        let idx = InMemoryIndex::from_docs(vec![
            doc_with_variant(1, "Acme", mk_variant(1000.0, 10.0, 3000)),
            doc_with_variant(2, "Beta", mk_variant(5000.0, 50.0, 5000)),
        ]);
        let f = Filter {
            manufacturer: Some(smallvec![CompactString::from("Acme")]),
            ..Default::default()
        };
        let b = idx.range_bounds(&f);
        let flux = b.flux_lm.expect("flux bounds should be present");
        assert!((flux.min - 1000.0).abs() < 0.01);
        assert!((flux.max - 1000.0).abs() < 0.01);
    }

    #[test]
    fn range_bounds_empty_axis_returns_none() {
        // Variant without photometry → all photometry-derived axes
        // should return None.
        let v_no_phot = VariantDoc {
            photometry: None,
            ..mk_variant(0.0, 0.0, 0)
        };
        let idx = InMemoryIndex::from_docs(vec![doc_with_variant(1, "Acme", v_no_phot)]);
        let b = idx.range_bounds(&Filter::default());
        assert!(b.flux_lm.is_none());
        assert!(b.cct_k.is_none());
    }

    #[test]
    fn range_bounds_recessed_depth_skips_non_recessed_variants() {
        // Surface variant with a stray depth=15 is excluded; the only
        // contributing variant is the recessed one with depth=40.
        let v_surface = VariantDoc {
            mounting_type: MountingType::SurfaceMounted,
            recessed_depth_mm: Some(15),
            ..mk_variant(1000.0, 10.0, 3000)
        };
        let v_recessed = VariantDoc {
            mounting_type: MountingType::Recessed,
            recessed_depth_mm: Some(40),
            ..mk_variant(2000.0, 20.0, 4000)
        };
        let idx = InMemoryIndex::from_docs(vec![
            doc_with_variant(1, "Acme", v_surface),
            doc_with_variant(2, "Acme", v_recessed),
        ]);
        let b = idx.range_bounds(&Filter::default());
        let depth = b.recessed_depth_mm.expect("depth bounds should be present");
        assert_eq!(depth.min, 40);
        assert_eq!(depth.max, 40);
    }

    // ── resolve_manufacturer ──────────────────────────────────────────

    #[test]
    fn resolve_manufacturer_exact_single_word() {
        let idx = InMemoryIndex::from_docs(vec![
            mk_doc(1, "Philips", "P", None),
            mk_doc(2, "ERCO", "P", None),
        ]);
        assert_eq!(idx.resolve_manufacturer("philips").as_deref(), Some("Philips"));
        assert_eq!(idx.resolve_manufacturer("PHILIPS").as_deref(), Some("Philips"));
        assert_eq!(idx.resolve_manufacturer("erco").as_deref(), Some("ERCO"));
    }

    #[test]
    fn resolve_manufacturer_hyphen_to_space() {
        // `aec-illuminazione.gldf-search.de` → header `aec-illuminazione`
        // → must resolve to `AEC Illuminazione` (corpus spelling).
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "AEC Illuminazione", "P", None)]);
        assert_eq!(
            idx.resolve_manufacturer("aec-illuminazione").as_deref(),
            Some("AEC Illuminazione"),
        );
    }

    #[test]
    fn resolve_manufacturer_first_word_only_also_works() {
        // Short slug `aec` alone still maps to AEC Illuminazione via
        // the first-word fallback (audited zero-collision).
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "AEC Illuminazione", "P", None)]);
        assert_eq!(
            idx.resolve_manufacturer("aec").as_deref(),
            Some("AEC Illuminazione"),
        );
    }

    #[test]
    fn resolve_manufacturer_multi_word_with_dots() {
        // `lena-lighting-s.a.` would be a weird subdomain but the fold
        // still resolves it. More importantly, the short form `lena`
        // works as a first-word match.
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "Lena Lighting S.A.", "P", None)]);
        assert_eq!(
            idx.resolve_manufacturer("lena").as_deref(),
            Some("Lena Lighting S.A."),
        );
    }

    #[test]
    fn resolve_manufacturer_alias_table_handles_dots() {
        // `a.l.s.` has dots that aren't valid in a DNS label, so the
        // alias maps `als` (and `a-l-s`) to it.
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "A.L.S.", "P", None)]);
        assert_eq!(idx.resolve_manufacturer("als").as_deref(), Some("A.L.S."));
        assert_eq!(idx.resolve_manufacturer("a-l-s").as_deref(), Some("A.L.S."));
    }

    #[test]
    fn resolve_manufacturer_alias_table_handles_diacritic() {
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "nobilé", "P", None)]);
        assert_eq!(idx.resolve_manufacturer("nobile").as_deref(), Some("nobilé"));
    }

    #[test]
    fn resolve_manufacturer_returns_none_for_unknown_slug() {
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "Philips", "P", None)]);
        assert!(idx.resolve_manufacturer("does-not-exist").is_none());
    }

    #[test]
    fn resolve_manufacturer_returns_none_for_empty_slug() {
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "Philips", "P", None)]);
        assert!(idx.resolve_manufacturer("").is_none());
        assert!(idx.resolve_manufacturer("  ").is_none());
    }

    #[test]
    fn resolve_manufacturer_iguzzini_mixed_case() {
        // Corpus has `iGuzzini` (mixed case). Subdomain `iguzzini`
        // must still resolve.
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "iGuzzini", "P", None)]);
        assert_eq!(idx.resolve_manufacturer("iguzzini").as_deref(), Some("iGuzzini"));
        assert_eq!(idx.resolve_manufacturer("IGUZZINI").as_deref(), Some("iGuzzini"));
    }

    #[test]
    fn resolve_manufacturer_3f_filippi() {
        // Leading-digit manufacturers — `3f.gldf-search.de` or
        // `3f-filippi.gldf-search.de` both work.
        let idx = InMemoryIndex::from_docs(vec![mk_doc(1, "3F Filippi", "P", None)]);
        assert_eq!(idx.resolve_manufacturer("3f").as_deref(), Some("3F Filippi"));
        assert_eq!(
            idx.resolve_manufacturer("3f-filippi").as_deref(),
            Some("3F Filippi"),
        );
    }
}
