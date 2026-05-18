//! Search index abstractions for gldf-search.
//!
//! v0 ships a single implementation: [`InMemoryIndex`], a linear scan
//! over `Vec<LuminaireDoc>` driving the canonical [`Filter::matches_doc`]
//! reference predicate. It exists for three reasons:
//!
//! 1. **It's the reference.** The future fst+roaring [`MmapIndex`] must
//!    return the same hit set for every `(corpus, filter)` pair. The
//!    fuzz target for that property uses this implementation as the
//!    ground truth.
//! 2. **It unblocks the UI.** Leptos / CLI consumers can target the
//!    [`IndexSource`] trait today and stay source-compatible when the
//!    `.ldb`-backed index lands.
//! 3. **It's fast enough for the sample-10 corpus.** A linear scan
//!    over 10 docs is sub-microsecond. At 1000 docs it's still
//!    microseconds. The cliff is at ~100k docs — long before that,
//!    `MmapIndex` will be in place.
//!
//! No I/O, no fst, no roaring — pure linear scan with serde-friendly
//! result types. Compiles to `wasm32-unknown-unknown`.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod facets;
pub mod fold;
pub mod source;

pub use facets::{
    F32Bounds, FacetCounts, FacetField, FacetValueCount, RangeBounds, U16Bounds, U32Bounds,
};
pub use fold::{fold_docs, FoldStats};
pub use source::{
    ArticleSuggestion, FacetSuggestion, InMemoryIndex, IndexSource, Pagination, SearchHit,
    SearchResult, UNSPECIFIED_EMERGENCY,
};
