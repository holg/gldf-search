//! Facet counts — "how many hits would each value have, holding the
//! rest of the filter constant?".
//!
//! Concept §7 says facets should be live: every query recomputes the
//! count for every value of every requested facet, with **that one
//! facet temporarily relaxed** so the user can see what's available if
//! they widen their search. We implement exactly that semantics.

use serde::{Deserialize, Serialize};

/// One entry in a facet count list: a value (encoded per facet kind)
/// and how many docs would match if that value were the only choice
/// for this facet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetValueCount {
    /// String form of the value. For typed enums it's the canonical
    /// XSD string (`"IP54"`, `"CE"`, ...). For free-form facets like
    /// manufacturer, it's the display string verbatim.
    pub value: String,
    /// Number of docs that satisfy the filter with this value in the
    /// facet position.
    pub count: u32,
}

/// Aggregated facet counts. One vec per requested [`FacetField`],
/// sorted by descending count, ties broken by value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetCounts {
    /// Manufacturer display names → count.
    pub manufacturer: Vec<FacetValueCount>,
    /// `Application` canonical strings → count.
    pub applications: Vec<FacetValueCount>,
    /// `Label` canonical strings → count.
    pub labels: Vec<FacetValueCount>,
    /// `IpCode` canonical strings → count.
    pub ip_code: Vec<FacetValueCount>,
    /// `SafetyClass` canonical strings → count.
    pub safety_class: Vec<FacetValueCount>,
    /// `IkRating` canonical strings → count.
    pub ik_rating: Vec<FacetValueCount>,
    /// `ProductForm` canonical strings → count.
    pub product_form: Vec<FacetValueCount>,
    /// `Adjustability` canonical strings → count.
    pub adjustability: Vec<FacetValueCount>,
    /// `MountingPlace` debug string → count (`"Ceiling"`, `"Wall"`,
    /// `"WorkingPlane"`, `"Ground"`, `"Unknown"`).
    #[serde(alias = "geometry")]
    pub mounting_place: Vec<FacetValueCount>,
    /// `MountingType` debug string → count (`"Recessed"`,
    /// `"SurfaceMounted"`, `"Pendant"`, `"FreeStanding"`, `"PoleTop"`,
    /// `"PoleIntegrated"`, `"Unknown"`).
    pub mounting_type: Vec<FacetValueCount>,
    /// `LightDistribution` canonical strings → count.
    pub light_distribution: Vec<FacetValueCount>,
    /// Photometry-presence counts: `"present"` and `"missing"`.
    pub has_photometry: Vec<FacetValueCount>,
    /// 3D-model-file presence counts: `"present"` and `"missing"`.
    pub has_3d: Vec<FacetValueCount>,
    /// Emergency-lighting presence counts: `"present"` and `"missing"`.
    pub has_emergency_lighting: Vec<FacetValueCount>,
    /// Canonical CIE-97 lamp type strings → count.
    pub lamp_type: Vec<FacetValueCount>,
    /// Canonical control-gear interface strings → count.
    pub control_gear_interfaces: Vec<FacetValueCount>,
    /// Canonical emergency-lighting type strings → count.
    pub emergency_lighting_type: Vec<FacetValueCount>,
}

// ── Numeric range bounds ──────────────────────────────────────────────
//
// `RangeBounds` is the dynamic-rails companion to `FacetCounts`. For each
// numeric axis (flux, power, efficacy, cct, beam, recessed-depth) the
// server walks the corpus with that axis's range *relaxed* and folds
// min/max over the variants that survive the rest of the filter. The UI
// uses these to anchor dual-handle range sliders so the user never
// drags into a zero-results dead zone.
//
// `Option` per axis so the UI can render disabled rails when nothing in
// the current result set carries that field (e.g. zero variants with a
// declared CCT under the active filter).

/// `(min, max)` pair for an `f32`-typed numeric axis (flux, power,
/// efficacy, beam).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct F32Bounds {
    /// Smallest observed value on the axis.
    pub min: f32,
    /// Largest observed value on the axis.
    pub max: f32,
}

/// `(min, max)` pair for a `u16`-typed numeric axis (CCT).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct U16Bounds {
    /// Smallest observed value on the axis.
    pub min: u16,
    /// Largest observed value on the axis.
    pub max: u16,
}

/// `(min, max)` pair for a `u32`-typed numeric axis (recessed depth).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct U32Bounds {
    /// Smallest observed value on the axis.
    pub min: u32,
    /// Largest observed value on the axis.
    pub max: u32,
}

/// Per-axis dynamic min/max bounds across the current filter's result
/// set. Each axis is computed with its own filter slot *relaxed* (the
/// "what would I see if I widened just this axis?" semantics), mirroring
/// how `FacetCounts` is computed for categorical facets.
///
/// `None` for an axis means no variant in the relaxed result set
/// carries that field — the UI renders the slider disabled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RangeBounds {
    /// Luminous flux (lm) range.
    pub flux_lm: Option<F32Bounds>,
    /// Electrical power (W) range.
    pub power_w: Option<F32Bounds>,
    /// Luminous efficacy (lm/W) range.
    pub efficacy: Option<F32Bounds>,
    /// Correlated colour temperature (K) range.
    pub cct_k: Option<U16Bounds>,
    /// Half-peak beam angle (°) range.
    pub beam_deg: Option<F32Bounds>,
    /// Recessed-only installation depth (mm) range. Folds only over
    /// variants whose `mounting_type == Recessed` AND that declare a
    /// depth — same guard as `Filter::matches_variant`.
    pub recessed_depth_mm: Option<U32Bounds>,
}

/// Which facet fields the caller wants counts for. Empty list ⇒ no
/// facet work done (cheaper).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FacetField {
    /// Manufacturer display name.
    Manufacturer,
    /// `LuminaireDoc.applications`.
    Applications,
    /// `LuminaireDoc.labels`.
    Labels,
    /// `LuminaireDoc.ip_code`.
    IpCode,
    /// `LuminaireDoc.safety_class`.
    SafetyClass,
    /// `LuminaireDoc.ik_rating`.
    IkRating,
    /// `LuminaireDoc.product_form`.
    ProductForm,
    /// `LuminaireDoc.adjustability`.
    Adjustability,
    /// `VariantDoc.mounting_place` aggregated to the doc.
    #[serde(alias = "Geometry")]
    MountingPlace,
    /// `VariantDoc.mounting_type` aggregated to the doc.
    MountingType,
    /// `PhotometryStats.light_distribution` aggregated to the doc.
    LightDistribution,
    /// Photometry presence at the variant level.
    HasPhotometry,
    /// 3D-model-file presence at the variant level.
    Has3d,
    /// Emergency-lighting presence at the variant level.
    HasEmergencyLighting,
    /// CIE-97 lamp type, per variant.
    LampType,
    /// Control-gear interfaces, per variant.
    ControlGearInterface,
    /// Dedicated emergency-lighting type, per variant.
    EmergencyLightingType,
}
