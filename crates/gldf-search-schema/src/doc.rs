//! Document model — one [`LuminaireDoc`] per GLDF file, variants nested.
//!
//! Concept doc §4 spells out *why nested instead of flat*: a single GLDF
//! family typically ships ~4 variants. Per-variant flat rows would
//! inflate the index 3–5×, duplicate doc-scoped fields, and break the
//! gldf-rs viewer integration where the variant filter needs a stable
//! parent reference. The "any variant matches" semantics handle the
//! search-side use-case without duplicating documents.
//!
//! All XSD-restricted fields use index types from [`crate::enums`];
//! see that module for the rationale.

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::enums::{
    AdjustabilityId, ApplicationId, IkRatingId, IpCodeId, LabelId, LightDistributionId,
    ProductFormId, SafetyClassId,
};
use crate::filter::Locale;

/// Stable document id. BLAKE3 of file content; the index assumes
/// content-addressed dedup. See concept doc §11 q1 for the open
/// `<Header><UniqueIdentifier>` question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocId(pub [u8; 32]);

/// Local variant id within a `LuminaireDoc`. The pairing
/// `(DocId, VariantId)` uniquely identifies a variant globally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VariantId(pub u16);

/// Where the document came from. Provenance only — the index never
/// dereferences this at query time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceRef {
    /// Local filesystem path at index-build time.
    Path(CompactString),
    /// HTTP(S) URL the file was fetched from.
    Url(CompactString),
    /// Content hash only — no recoverable origin.
    ContentOnly,
}

/// File-level metadata captured at extraction time. Used for cache
/// invalidation and "indexed N hours ago" UI affordances; not a query
/// surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    /// Bytes on disk (compressed `.gldf` zip).
    pub size_bytes: u64,
    /// Unix epoch seconds. `None` for content-only / web sources.
    pub mtime_epoch_s: Option<u64>,
    /// Format version string from `<Header><FormatVersion>`. Free-form
    /// because the field allows minor tags ("1.0.0-rc.3") not modelled
    /// upstream as an enum.
    pub format_version: Option<CompactString>,
}

/// Photometric distribution hint at the variant level.
///
/// XSD `LightDistribution` is descriptive English ("Laterally
/// symmetrical narrow"), conflating distribution shape and beam class.
/// For ranking and faceting we keep the canonical id, but most filter
/// predicates only need a coarse symmetry class — exposed as this
/// enum so the filter doesn't have to know all 15 strings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhotometricSymmetryHint {
    /// Rotationally symmetric (most downlights, batten luminaires).
    Rotational,
    /// Symmetric about a single axis (asymmetric flood, wallwasher).
    Axial,
    /// Asymmetric in both axes (street lighting, signage).
    Asymmetric,
    /// Diffuse / no preferred direction.
    Diffuse,
    /// Distribution kind not classified.
    #[default]
    Unknown,
}

impl PhotometricSymmetryHint {
    /// Map a canonical [`gldf_rs::validation::xsd_enums::LIGHT_DISTRIBUTIONS`]
    /// id to its coarse symmetry class. Returns `Unknown` for unrecognised
    /// values so the field stays total.
    pub fn from_light_distribution_id(id: LightDistributionId) -> Self {
        // Indices match the canonical table order; encoded as numbers
        // not strings to keep this branch-free at the hot path.
        match id {
            0..=3 => Self::Rotational, // Laterally symmetrical narrow/medium/wide + each quadrant
            4..=5 => Self::Axial,      // Symmetric about 0-180 / 90-270 plane
            6..=8 => Self::Asymmetric, // Asymmetrical (+ floods)
            9..=10 => Self::Diffuse,   // Diffuse half/full spherical
            11..=13 => Self::Rotational, // Direct / Indirect / Direct indirect
            _ => Self::Unknown,        // 14 = Other, plus any future id
        }
    }
}

/// Where on the building the luminaire is mounted. Comes from which
/// **top-level `<Mountings>` child** is present in the source GLDF
/// (`Ceiling` / `Wall` / `WorkingPlane` / `Ground`). The XSD does not
/// expose a closed enumeration for this — it's encoded structurally,
/// so the extractor reads which element exists and assigns one value
/// per variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MountingPlace {
    /// On or in a ceiling.
    Ceiling,
    /// On a wall.
    Wall,
    /// On a working plane (table-top, desk, free-standing indoor).
    WorkingPlane,
    /// On the ground or on a pole (outdoor).
    Ground,
    /// Mounting place not declared in the source GLDF.
    Unknown,
}

/// How the luminaire is attached — recessed, surface-mounted, etc.
/// Comes from the **child element** under each `<Mountings>` place
/// (e.g. `Mountings/Ceiling/Recessed`). The XSD again uses element
/// names, not a closed enumeration; the extractor reads which child
/// element is present under the variant's mounting place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MountingType {
    /// Recessed into the mounting surface (carries `recessed_depth_mm`).
    Recessed,
    /// Surface-mounted (sits on the surface).
    SurfaceMounted,
    /// Suspended below a ceiling (pendant).
    Pendant,
    /// Free-standing — sits or stands without a fixed mounting class.
    FreeStanding,
    /// Mounted on top of a pole (outdoor street / path luminaire).
    PoleTop,
    /// Integrated into a pole shaft (outdoor street / path luminaire).
    PoleIntegrated,
    /// Mounting type not declared in the source GLDF.
    Unknown,
}

/// Per-variant photometric statistics. Numeric facets and ranking
/// inputs.
///
/// All numeric fields are `Option<…>` because the source data can
/// legitimately omit each one. `None` means "we don't know"; a real
/// zero (from a parsed LDT) is distinguishable. The
/// [`crate::filter::Filter::matches_variant`] reference predicate
/// follows the rule: any variant whose field is `None` is excluded
/// when the caller asked for a range on that field.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotometryStats {
    /// Rated luminous flux (lm). `None` when not declared.
    pub flux_lm: Option<f32>,
    /// Active electrical power (W). `None` when not declared.
    pub power_w: Option<f32>,
    /// Luminous efficacy (lm/W). Computed once at extraction so the
    /// query path doesn't divide. `None` when either `flux_lm` or
    /// `power_w` was absent (we don't fabricate a 0).
    pub efficacy_lm_w: Option<f32>,
    /// Correlated color temperature (K). `None` for non-white sources
    /// or when not declared.
    pub cct_k: Option<u16>,
    /// CRI Ra. `None` when not declared.
    pub cri_ra: Option<u8>,
    /// R9 special-color rendering. `None` when not declared.
    pub r9: Option<i8>,
    /// Half-peak beam angle (degrees). Symmetric assumption — for
    /// asymmetric distributions the extractor reports the smaller of
    /// C0/C90.
    pub beam_deg: Option<f32>,
    /// Field angle (1/10 peak, degrees).
    pub field_deg: Option<f32>,
    /// Upward light output ratio (0..=1).
    pub ulor: Option<f32>,
    /// Downward light output ratio (0..=1).
    pub dlor: Option<f32>,
    /// Coarse symmetry class derived from the canonical
    /// `LightDistribution`.
    pub symmetry: PhotometricSymmetryHint,
    /// Canonical `LightDistribution` id, if declared. `None` keeps the
    /// extractor honest: an absent `<LightDistribution>` is not the
    /// same as `"Other"`.
    pub light_distribution: Option<LightDistributionId>,
    // NOTE: full LDC intensity matrices used to live here (`\x05`
    // schema, 2026-05-14) — but at high angular resolution they
    // averaged ~33 KB per doc and ballooned the cache to 8.8 GB on
    // 270k docs. We rolled back to scalar-only stats and now ship
    // the raw LDT text to the **browser** on demand (server fn
    // `fetch_ldt_bytes`); the hydrate bundle uses the `eulumdat`
    // crate to parse + render polar / cartesian / butterfly SVGs
    // client-side. Production RAM is the binding constraint.
}

/// One variant of a GLDF product family.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
/// One variant of a GLDF product family. Field order is the
/// **original** wire-format order — see the [`LuminaireDoc`] comment
/// for why bincode 1.x pins us to declaration order.
pub struct VariantDoc {
    /// Stable id within the parent document.
    pub id: VariantId,
    /// Human-readable variant name (e.g. flux package label,
    /// "3000 lm 4000 K"). Locale source: parent doc's first locale,
    /// or `<Variant><Name>` if the GLDF carries it.
    pub name: CompactString,
    /// Photometric stats from the LDC/IES file when present.
    ///
    /// `None` means the GLDF either did not reference a photometric
    /// file or the extractor failed to parse it. Variants with no
    /// photometry are still indexed (text + facets), but variant-scoped
    /// numeric filters (`flux_lm`, `power_w`, `cct_k`, etc.) exclude
    /// them — see [`crate::filter::Filter::matches_variant`].
    pub photometry: Option<PhotometryStats>,
    /// Where on the building the luminaire is mounted
    /// (see [`MountingPlace`]). Derived from which `<Mountings>` child
    /// element is present in the source GLDF.
    #[serde(alias = "geometry")]
    pub mounting_place: MountingPlace,
    /// How the luminaire is attached (see [`MountingType`]). Derived
    /// from the child element under the mounting place (e.g.
    /// `<Mountings><Ceiling><Recessed>` → `Recessed`).
    #[serde(default = "default_mounting_type")]
    pub mounting_type: MountingType,
    /// Installation depth for `Recessed` mountings (millimetres).
    /// `None` when the variant isn't recessed or the source didn't
    /// declare a depth. The XSD makes `@recessedDepth` required on
    /// every `<Recessed>` element, so in practice this is `Some(_)`
    /// for every recessed variant.
    #[serde(default)]
    pub recessed_depth_mm: Option<u32>,
    /// `true` if the variant references a 3D model file inside the
    /// GLDF (e.g. `.l3d`, `.gltf`). Per the XSD, presence of
    /// `<ModelGeometryReference>` guarantees at least one
    /// `<GeometryFileReference>` exists — so the extractor just
    /// checks presence of the reference. `SimpleGeometry` primitives
    /// (Cuboid / Cylinder / Sphere) do NOT count: they describe a
    /// shape but ship no file an architect could render.
    #[serde(default)]
    pub has_3d: bool,
    /// Canonical CIE-97 lamp type, when declared on any light source
    /// inside the variant (`FixedLightSource.LightSourceMaintenance.
    /// Cie97LampType`). `None` for non-canonical / missing.
    #[serde(default)]
    pub lamp_type: Option<crate::enums::LampTypeId>,
    /// Control-gear interfaces declared on the variant's equipment
    /// (`Equipment.ControlGear.Interfaces`). One id per distinct
    /// canonical interface across all gear in the variant; non-
    /// canonical values are dropped.
    #[serde(default)]
    pub control_gear_interfaces: SmallVec<[crate::enums::ControlGearInterfaceId; 4]>,
    /// `true` when the variant declares an `<Emergency>` block — the
    /// luminaire is intended as emergency lighting. Independent of
    /// `emergency_lighting_type`: a variant can be emergency without
    /// declaring the dedicated subtype.
    #[serde(default)]
    pub has_emergency_lighting: bool,
    /// Canonical "dedicated emergency lighting type" (`Exit Light` /
    /// `Guide Light` / etc.) when declared. `None` when the variant
    /// is not an emergency luminaire or didn't declare a subtype.
    #[serde(default)]
    pub emergency_lighting_type: Option<crate::enums::EmergencyLightingTypeId>,
    /// Number of light emitters in this variant (LED count, lamp count).
    pub emitter_count: u16,
    /// Index into the parent `LuminaireDoc.source_paths` slice
    /// telling which source GLDF this variant came from. Always 0
    /// for un-folded docs (single-source). Server fns that need to
    /// open the right .gldf for a variant (LDT fetch, image fetch)
    /// resolve via `doc.source_paths[v.source_index]`.
    #[serde(default)]
    pub source_index: u16,
}

fn default_mounting_type() -> MountingType {
    MountingType::Unknown
}

/// One luminaire document. Maps 1:1 to a GLDF file; variants nested.
///
/// Field order is the **original** wire-format order — bincode 1.x
/// is not self-describing and serialises by declaration order, so
/// reordering would invalidate existing caches. RAM layout is left
/// to `repr(Rust)` (the compiler auto-reorders for alignment, which
/// measurement showed is sufficient: manual descend-by-align reorder
/// gave zero bytes back). RAM win comes entirely from SmallVec
/// inline-size tuning (see the per-field comments below).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LuminaireDoc {
    /// Stable doc id (BLAKE3 of file bytes).
    pub id: DocId,
    /// Primary provenance: the path/URL of the first GLDF that
    /// contributed to this doc. For an un-folded doc this is the
    /// only source. For a folded family doc this is the FIRST
    /// source — kept here because the viewer link / default
    /// `fetch_ldt` (variant_index = None) path uses it; both have
    /// well-defined "doc-level" semantics that map to "first source"
    /// after folding.
    pub source: SourceRef,
    /// All source paths after folding. Identical content to
    /// `[source]` for un-folded docs (length 1). For a folded family
    /// doc with N source GLDFs, length = N; each variant points into
    /// this list via `VariantDoc.source_index`. Inline size = 1: the
    /// majority of docs in the corpus are un-folded.
    #[serde(default)]
    pub source_paths: SmallVec<[SourceRef; 1]>,

    /// Manufacturer name (free-form). The extractor lowercases for the
    /// term dictionary but stores the original case here.
    pub manufacturer: CompactString,
    /// Product family / model name.
    pub product: CompactString,
    /// Global Trade Item Number, if declared.
    pub gtin: Option<CompactString>,
    /// Manufacturer's product code, if declared.
    pub product_code: Option<CompactString>,

    /// Per-locale fulltext source. The order is significant: the
    /// extractor places the primary `<Header><CreationLocale>` first.
    // Inline size = 1: 1–2 locales is the corpus norm. Multi-locale
    // docs heap-allocate; the saved 110 bytes/doc inline pays for
    // itself across 270k+ docs (~30 MB resident). Wire format is
    // unchanged — SmallVec serialises as Vec regardless of inline N.
    pub descriptions: SmallVec<[(Locale, String); 1]>,
    /// Free-form keywords from `<Marketing>` blocks.
    // Inline size = 2: most docs land 0–3 mined keywords; >2 spills
    // to heap. Saves ~150 bytes/doc inline (~40 MB across 270k).
    pub keywords: SmallVec<[CompactString; 2]>,

    /// Application taxonomy ids (XSD `Marketing/Applications/Application`).
    /// Stored as a sorted, deduplicated list for tight bitmap-friendly
    /// representation.
    pub applications: SmallVec<[ApplicationId; 4]>,
    /// Regulatory / quality marks (XSD `Marketing/Labels/Label`).
    pub labels: SmallVec<[LabelId; 4]>,
    /// Adjustability modes (XSD `Mechanical/Adjustabilities/Adjustability`).
    pub adjustability: SmallVec<[AdjustabilityId; 2]>,

    /// Ingress protection code (XSD `Electrical/IngressProtectionIPCode`).
    /// Comparison uses the table order, which is **not** numerically
    /// monotone (legacy two-digit forms interleave). Use
    /// [`crate::enums::ip_code_str`] before any prose-style comparison.
    pub ip_code: Option<IpCodeId>,
    /// Electrical safety class (XSD `Electrical/ElectricalSafetyClass`).
    pub safety_class: Option<SafetyClassId>,
    /// Mechanical impact protection (XSD `Mechanical/IKRating`).
    pub ik_rating: Option<IkRatingId>,
    /// Geometric form (XSD `Mechanical/ProductForm`).
    pub product_form: Option<ProductFormId>,

    /// Variant data; "any variant matches" applies for variant-scoped
    /// filter predicates.
    // Inline size = 1: the corpus is dominated by one-variant docs
    // (a single .gldf = one luminaire SKU; multi-variant families
    // are the minority). VariantDoc is 136 bytes; inline 4 = 560 B
    // inline, vs 136 B with inline 1 + 24 B SmallVec header. Saves
    // 420 B/doc on single-variant docs → ~110 MB across 270k. Multi-
    // variant docs heap-allocate (1 alloc per doc, acceptable).
    pub variants: SmallVec<[VariantDoc; 1]>,

    /// File-level metadata.
    pub file_meta: FileMeta,
}
