//! gldf-search schema — document model, query filter, and reference
//! predicates for the gldf-search index.
//!
//! The two predicate functions [`Filter::matches_doc`] and
//! [`Filter::matches_variant`] are the **canonical query semantics**.
//! Any index implementation in `gldf-search-index` must produce the
//! same result set as a linear scan applying these predicates — this is
//! the primary fuzz target.
//!
//! Closed XSD enums are not redefined here. Schema fields key into the
//! `&'static [&'static str]` canonical tables that `gldf-rs` ships in
//! [`gldf_rs::validation::xsd_enums`]. See [`enums`] for the index-typed
//! wrapper functions.
//!
//! No I/O, no parser, no allocator-hungry deps. Compiles to
//! `wasm32-unknown-unknown` clean.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod doc;
pub mod enums;
pub mod filter;
pub mod filter_url;
pub mod viewer;

pub use viewer::ViewerConfig;

pub use doc::{
    DocId, FileMeta, LuminaireDoc, MountingPlace, MountingType, PhotometricSymmetryHint,
    PhotometryStats, SourceRef, VariantDoc, VariantId,
};
pub use enums::{
    AdjustabilityId, ApplicationId, IkRatingId, IpCodeId, LabelId, LightDistributionId,
    ProductFormId, SafetyClassId,
};
pub use filter::{Filter, Locale};
