//! Query filter type and reference predicate implementation.
//!
//! [`Filter::matches_doc`] and [`Filter::matches_variant`] are the
//! canonical query semantics. The future fst+roaring index in
//! `gldf-search-index` MUST produce the same result set as a linear
//! scan applying these predicates — this is the primary fuzz target.
//!
//! Combination semantics:
//! - within a multi-value field (e.g. `applications: {Office, Retail}`):
//!   OR — match if any of the requested values is present
//! - across fields: AND — every populated filter field must match
//! - numeric ranges: inclusive on both ends; an unset range or `None`
//!   bound means "unbounded on that side"
//! - text: case-insensitive substring at the reference level. The real
//!   index implements BM25 — the predicate stays simple here so the
//!   fuzzer can compare against an obvious truth.

use core::ops::RangeInclusive;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::doc::{LuminaireDoc, MountingPlace, MountingType, VariantDoc};
use crate::enums::{
    AdjustabilityId, ApplicationId, ControlGearInterfaceId, EmergencyLightingTypeId, IkRatingId,
    IpCodeId, LabelId, LampTypeId, LightDistributionId, ProductFormId, SafetyClassId,
};

/// ISO-639-1 language code (`"de"`, `"en"`, `"fr"`, `"zh"`, ...).
/// Two-byte storage; the schema does not validate against the ISO list
/// so unknown codes round-trip unchanged.
///
/// `Ord` is implemented so callers can key a `BTreeMap` on `Locale` for
/// deterministic iteration order — useful when descriptions need to
/// serialise the same way across runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Locale(pub [u8; 2]);

impl Locale {
    /// Build a [`Locale`] from a two-character ISO-639-1 string.
    /// Returns `None` for inputs of any other length.
    ///
    /// Inherent method (not [`std::str::FromStr`]) so the return type
    /// can be `Option<Self>` instead of `Result<Self, _>` — there's no
    /// useful error type beyond "wrong length".
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        let bytes = s.as_bytes();
        if bytes.len() == 2 {
            Some(Self([bytes[0], bytes[1]]))
        } else {
            None
        }
    }

    /// View the locale as a two-byte string slice. Always valid UTF-8
    /// because the constructor only accepts ASCII letters in practice
    /// (this is not enforced — junk in, junk out).
    pub fn as_str(&self) -> &str {
        // SAFETY: Locale stores two bytes that originated as a `&str`.
        // If a caller hand-built a non-UTF-8 byte pair, behaviour is
        // implementation-defined but safe — we treat it as opaque.
        core::str::from_utf8(&self.0).unwrap_or("")
    }
}

/// Search filter — the entire query surface.
///
/// `#[serde(default)]` makes every field optional on deserialisation
/// so HTTP clients can POST partial JSON bodies (e.g. `{"text":"led"}`).
/// Missing fields take their `Default` values, matching `Filter::default()`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    // ── Fulltext ──────────────────────────────────────────────────────
    /// Free-text query (case-insensitive substring at the reference
    /// level). `None` skips text matching entirely.
    pub text: Option<CompactString>,
    /// Restrict text matching to a specific locale's descriptions.
    /// `None` searches all locales.
    pub locale: Option<Locale>,

    // ── Document-scoped facets ────────────────────────────────────────
    /// Match if the doc's manufacturer is in this list (case-sensitive,
    /// exact). Empty / `None` skips manufacturer matching.
    pub manufacturer: Option<SmallVec<[CompactString; 4]>>,
    /// Match if the doc declares any of these application taxonomy ids.
    pub applications: SmallVec<[ApplicationId; 4]>,
    /// Match if the doc declares any of these regulatory labels.
    pub labels: SmallVec<[LabelId; 4]>,
    /// Match if the doc declares any of these adjustability modes.
    pub adjustability: SmallVec<[AdjustabilityId; 2]>,
    /// Match if the doc's IP code is in this list.
    pub ip_code: SmallVec<[IpCodeId; 2]>,
    /// Match if the doc's safety class is in this list.
    pub safety_class: SmallVec<[SafetyClassId; 2]>,
    /// Match if the doc's IK rating is in this list.
    pub ik_rating: SmallVec<[IkRatingId; 2]>,
    /// Match if the doc's product form is in this list.
    pub product_form: SmallVec<[ProductFormId; 2]>,

    // ── Variant-scoped numeric ranges ─────────────────────────────────
    /// Inclusive luminous flux range (lm).
    pub flux_lm: Option<RangeInclusive<f32>>,
    /// Inclusive electrical power range (W).
    pub power_w: Option<RangeInclusive<f32>>,
    /// Inclusive efficacy range (lm/W).
    pub efficacy: Option<RangeInclusive<f32>>,
    /// Inclusive correlated colour temperature range (K).
    pub cct_k: Option<RangeInclusive<u16>>,
    /// Minimum CRI Ra (variants below this are filtered out).
    pub cri_min: Option<u8>,
    /// Inclusive half-peak beam angle range (degrees).
    pub beam_deg: Option<RangeInclusive<f32>>,
    /// Inclusive recessed-depth range (mm). Only matches variants
    /// whose `mounting_type == Recessed` and that declare a depth.
    /// Architects with shallow ceiling cavities use this to prune
    /// non-viable luminaires.
    pub recessed_depth_mm: Option<RangeInclusive<u32>>,

    // ── Variant-scoped facets ─────────────────────────────────────────
    /// Match if any variant's mounting place is in this list.
    #[serde(alias = "geometry")]
    pub mounting_place: SmallVec<[MountingPlace; 4]>,
    /// Match if any variant's mounting type is in this list.
    pub mounting_type: SmallVec<[MountingType; 4]>,
    /// Match if any variant's `LightDistribution` id is in this list.
    pub light_distribution: SmallVec<[LightDistributionId; 4]>,

    // ── Photometry-presence tristate ──────────────────────────────────
    /// Tristate filter for "does this doc have photometric data?":
    /// - `None`: don't care (default)
    /// - `Some(true)`: at least one variant has `Some(photometry)`
    /// - `Some(false)`: at least one variant has `None` photometry
    ///   (use this to triage corpus files missing their LDC)
    ///
    /// Note the asymmetry: `Some(false)` is "any variant *lacks*
    /// photometry", not "every variant lacks it". The any-variant-
    /// matches semantics are consistent with the rest of the filter.
    pub has_photometry: Option<bool>,

    /// Tristate filter for "does this doc ship a 3D model file?".
    /// Same semantics as `has_photometry` — any-variant matches.
    pub has_3d: Option<bool>,
    /// Tristate filter for "is this doc an emergency luminaire?".
    /// True ⇒ any variant has `has_emergency_lighting`. Same
    /// any-variant semantics as the other tristates.
    pub has_emergency_lighting: Option<bool>,

    /// Match if any variant's canonical CIE-97 lamp type is in this
    /// list.
    pub lamp_type: SmallVec<[LampTypeId; 2]>,
    /// Match if any variant declares at least one of these control-
    /// gear interfaces.
    pub control_gear_interfaces: SmallVec<[ControlGearInterfaceId; 4]>,
    /// Match if any variant's dedicated emergency lighting type is
    /// in this list.
    pub emergency_lighting_type: SmallVec<[EmergencyLightingTypeId; 2]>,
}

impl Filter {
    /// Doc-scoped predicate only — applies fields that don't
    /// reference a specific variant. The full [`matches_doc`] AND-s
    /// this with "at least one variant matches"; variant-grain
    /// search wants to apply doc-scope alone and then enumerate
    /// matching variants separately.
    ///
    /// Mirrors steps 1-5 of [`matches_doc`] without step 6.
    pub fn matches_doc_scope(&self, doc: &LuminaireDoc) -> bool {
        if !self.ip_code.is_empty() && !contains_opt(&self.ip_code, doc.ip_code) {
            return false;
        }
        if !self.safety_class.is_empty() && !contains_opt(&self.safety_class, doc.safety_class) {
            return false;
        }
        if !self.ik_rating.is_empty() && !contains_opt(&self.ik_rating, doc.ik_rating) {
            return false;
        }
        if !self.product_form.is_empty() && !contains_opt(&self.product_form, doc.product_form) {
            return false;
        }
        if !self.applications.is_empty() && !any_in(&self.applications, doc.applications.as_slice())
        {
            return false;
        }
        if !self.labels.is_empty() && !any_in(&self.labels, doc.labels.as_slice()) {
            return false;
        }
        if !self.adjustability.is_empty()
            && !any_in(&self.adjustability, doc.adjustability.as_slice())
        {
            return false;
        }
        if let Some(allowed) = &self.manufacturer {
            if !allowed.is_empty() && !allowed.contains(&doc.manufacturer) {
                return false;
            }
        }
        if let Some(want) = self.has_3d {
            let satisfied = doc.variants.iter().any(|v| v.has_3d == want);
            if !satisfied {
                return false;
            }
        }
        if let Some(want) = self.has_emergency_lighting {
            let satisfied = doc
                .variants
                .iter()
                .any(|v| v.has_emergency_lighting == want);
            if !satisfied {
                return false;
            }
        }
        if let Some(want) = self.has_photometry {
            let satisfied = doc.variants.iter().any(|v| {
                if want {
                    v.photometry.is_some()
                } else {
                    v.photometry.is_none()
                }
            });
            if !satisfied {
                return false;
            }
        }
        if let Some(needle) = &self.text {
            if !text_matches(needle.as_str(), self.locale, doc) {
                return false;
            }
        }
        true
    }

    /// True if the document satisfies all populated filter fields.
    /// Variant-scoped fields are checked with "any variant matches"
    /// semantics. Doc-grain truth value — variant-grain callers use
    /// [`matches_doc_scope`] + [`matches_variant`] separately.
    ///
    /// Predicate order is **cheap-first** so most non-matching docs
    /// are rejected before we touch the text-substring scan or walk
    /// the variants:
    ///
    /// 1. Single-value `Option<u8>` facets — one int compare per slot
    /// 2. Multi-value `SmallVec<u8>` facets — short linear scan
    /// 3. Manufacturer — string equality over a small `Option<list>`
    /// 4. Photometry tristate — walks variants but only once per doc
    /// 5. Text — case-lowering + substring over potentially long
    ///    description strings; the most expensive doc-scoped check
    /// 6. Variant predicates — last; only fires when there's no
    ///    cheaper way to reject the doc
    pub fn matches_doc(&self, doc: &LuminaireDoc) -> bool {
        // 1. Single-value u8 facets — cheapest possible reject.
        if !self.ip_code.is_empty() && !contains_opt(&self.ip_code, doc.ip_code) {
            return false;
        }
        if !self.safety_class.is_empty() && !contains_opt(&self.safety_class, doc.safety_class) {
            return false;
        }
        if !self.ik_rating.is_empty() && !contains_opt(&self.ik_rating, doc.ik_rating) {
            return false;
        }
        if !self.product_form.is_empty() && !contains_opt(&self.product_form, doc.product_form) {
            return false;
        }

        // 2. Multi-value u8 facets — short SmallVec scan.
        if !self.applications.is_empty() && !any_in(&self.applications, doc.applications.as_slice())
        {
            return false;
        }
        if !self.labels.is_empty() && !any_in(&self.labels, doc.labels.as_slice()) {
            return false;
        }
        if !self.adjustability.is_empty()
            && !any_in(&self.adjustability, doc.adjustability.as_slice())
        {
            return false;
        }

        // 3. Manufacturer — string equality over a small list.
        if let Some(allowed) = &self.manufacturer {
            if !allowed.is_empty() && !allowed.contains(&doc.manufacturer) {
                return false;
            }
        }

        // 4a. Has-3D tristate. Same any-variant-matches semantics
        // as the photometry tristate just below.
        if let Some(want) = self.has_3d {
            let satisfied = doc.variants.iter().any(|v| v.has_3d == want);
            if !satisfied {
                return false;
            }
        }
        if let Some(want) = self.has_emergency_lighting {
            let satisfied = doc
                .variants
                .iter()
                .any(|v| v.has_emergency_lighting == want);
            if !satisfied {
                return false;
            }
        }

        // 4. Photometry-presence tristate. Walks variants (cheap; at
        // most ~4 entries per doc in practice).
        if let Some(want) = self.has_photometry {
            let satisfied = doc.variants.iter().any(|v| {
                if want {
                    v.photometry.is_some()
                } else {
                    v.photometry.is_none()
                }
            });
            if !satisfied {
                return false;
            }
        }

        // 5. Text last — case-lowering + substring scan over
        // descriptions/keywords/manufacturer is the most expensive
        // doc-scoped predicate. Most non-matching docs have already
        // been rejected above.
        if let Some(needle) = &self.text {
            if !text_matches(needle.as_str(), self.locale, doc) {
                return false;
            }
        }

        // 6. Variant-scoped predicates: at least one variant must match.
        if self.has_variant_predicate() {
            return doc.variants.iter().any(|v| self.matches_variant(v));
        }

        true
    }

    /// True if the variant satisfies all populated **variant-scoped**
    /// filter fields. The caller is responsible for doc-scoped
    /// predicates — they don't reference a variant.
    ///
    /// Variants whose `photometry` is `None` are excluded from any
    /// variant-scoped numeric or light-distribution predicate (they
    /// can't satisfy `flux_lm` if there's no flux measurement to
    /// compare against). They CAN still pass on `geometry`, which is
    /// independent of LDC presence.
    pub fn matches_variant(&self, v: &VariantDoc) -> bool {
        // Resolve photometry once. Every numeric/light_distribution
        // predicate excludes a None-photometry variant.
        let phot = v.photometry.as_ref();
        let need_phot = self.flux_lm.is_some()
            || self.power_w.is_some()
            || self.efficacy.is_some()
            || self.cct_k.is_some()
            || self.cri_min.is_some()
            || self.beam_deg.is_some()
            || !self.light_distribution.is_empty();
        if need_phot && phot.is_none() {
            return false;
        }

        if let (Some(r), Some(p)) = (&self.flux_lm, phot) {
            match p.flux_lm {
                Some(v) if range_f32_contains(r, v) => {}
                _ => return false,
            }
        }
        if let (Some(r), Some(p)) = (&self.power_w, phot) {
            match p.power_w {
                Some(v) if range_f32_contains(r, v) => {}
                _ => return false,
            }
        }
        if let (Some(r), Some(p)) = (&self.efficacy, phot) {
            match p.efficacy_lm_w {
                Some(v) if range_f32_contains(r, v) => {}
                _ => return false,
            }
        }
        if let (Some(r), Some(p)) = (&self.cct_k, phot) {
            match p.cct_k {
                Some(cct) if r.contains(&cct) => {}
                // Photometry present but variant doesn't declare CCT —
                // exclude when caller asked for a range. Tunable-white
                // handling is a v2 concern (concept §11 q3).
                _ => return false,
            }
        }
        if let (Some(min), Some(p)) = (self.cri_min, phot) {
            match p.cri_ra {
                Some(ra) if ra >= min => {}
                _ => return false,
            }
        }
        if let (Some(r), Some(p)) = (&self.beam_deg, phot) {
            match p.beam_deg {
                Some(b) if range_f32_contains(r, b) => {}
                _ => return false,
            }
        }

        if !self.mounting_place.is_empty() && !self.mounting_place.contains(&v.mounting_place) {
            return false;
        }
        if !self.mounting_type.is_empty() && !self.mounting_type.contains(&v.mounting_type) {
            return false;
        }
        if let Some(r) = &self.recessed_depth_mm {
            // Only recessed variants can pass a depth-range filter.
            // Non-recessed or depth-unset variants are excluded.
            match v.recessed_depth_mm {
                Some(d) if v.mounting_type == MountingType::Recessed && r.contains(&d) => {}
                _ => return false,
            }
        }

        if !self.light_distribution.is_empty() {
            match phot.and_then(|p| p.light_distribution) {
                Some(ld) if self.light_distribution.contains(&ld) => {}
                _ => return false,
            }
        }

        if !self.lamp_type.is_empty() {
            match v.lamp_type {
                Some(id) if self.lamp_type.contains(&id) => {}
                _ => return false,
            }
        }
        if !self.emergency_lighting_type.is_empty() {
            match v.emergency_lighting_type {
                Some(id) if self.emergency_lighting_type.contains(&id) => {}
                _ => return false,
            }
        }
        if !self.control_gear_interfaces.is_empty()
            && !any_in(
                &self.control_gear_interfaces,
                v.control_gear_interfaces.as_slice(),
            )
        {
            return false;
        }

        true
    }

    /// True when the only populated field is `text` (and optionally
    /// `locale`). Used by the server to skip facet recomputation on
    /// pure free-text queries — facet counts would otherwise dominate
    /// CPU (11 full-corpus scans through the text predicate) while
    /// adding no useful narrowing signal for the user (they already
    /// see results land within ~600 ms; facets for a 270k-doc text
    /// query take ~6 s on a single core and stay stale by the time
    /// the next keystroke arrives).
    ///
    /// When any facet OR range filter is *also* active, the gate
    /// opens — facets are useful to refine a constrained set, and
    /// the corpus walked is much smaller because the base predicate
    /// rejects most docs cheaply before reaching the text scan.
    pub fn is_text_only(&self) -> bool {
        self.text.is_some()
            && self.manufacturer.as_ref().map_or(true, |m| m.is_empty())
            && self.applications.is_empty()
            && self.labels.is_empty()
            && self.adjustability.is_empty()
            && self.ip_code.is_empty()
            && self.safety_class.is_empty()
            && self.ik_rating.is_empty()
            && self.product_form.is_empty()
            && self.flux_lm.is_none()
            && self.power_w.is_none()
            && self.efficacy.is_none()
            && self.cct_k.is_none()
            && self.cri_min.is_none()
            && self.beam_deg.is_none()
            && self.recessed_depth_mm.is_none()
            && self.mounting_place.is_empty()
            && self.mounting_type.is_empty()
            && self.light_distribution.is_empty()
            && self.has_photometry.is_none()
            && self.has_3d.is_none()
            && self.has_emergency_lighting.is_none()
            && self.lamp_type.is_empty()
            && self.control_gear_interfaces.is_empty()
            && self.emergency_lighting_type.is_empty()
    }

    /// Whether any **variant-scoped** field is populated. Drives the
    /// "must walk variants" branch in [`matches_doc`] and the
    /// variant-grain `search` path that emits one hit per matching
    /// variant.
    pub fn has_variant_predicate(&self) -> bool {
        self.flux_lm.is_some()
            || self.power_w.is_some()
            || self.efficacy.is_some()
            || self.cct_k.is_some()
            || self.cri_min.is_some()
            || self.beam_deg.is_some()
            || self.recessed_depth_mm.is_some()
            || !self.mounting_place.is_empty()
            || !self.mounting_type.is_empty()
            || !self.light_distribution.is_empty()
            || !self.lamp_type.is_empty()
            || !self.emergency_lighting_type.is_empty()
            || !self.control_gear_interfaces.is_empty()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

fn any_in<T: PartialEq>(query: &[T], stored: &[T]) -> bool {
    query.iter().any(|q| stored.contains(q))
}

fn contains_opt<T: PartialEq>(query: &[T], stored: Option<T>) -> bool {
    match stored {
        Some(s) => query.contains(&s),
        None => false,
    }
}

fn range_f32_contains(r: &RangeInclusive<f32>, v: f32) -> bool {
    // RangeInclusive::contains panics on NaN under some adapters; this
    // open-coded form is total.
    v >= *r.start() && v <= *r.end()
}

fn text_matches(needle: &str, locale: Option<Locale>, doc: &LuminaireDoc) -> bool {
    // Lower-case the needle once; avoid per-hay allocations by using
    // a borrow-only ASCII-fold substring search. ASCII fold is what
    // 99%+ of corpus content needs — manufacturer slugs and
    // mined-keyword pool are ASCII; descriptions are mixed but case-
    // distinctions outside ASCII (Greek, Cyrillic) are rare in the
    // lighting-domain text we index, and the failure mode is "this
    // word only matches in its exact case" which is acceptable for a
    // best-effort substring search.
    let needle_lower = needle.to_lowercase();
    let pred = |hay: &str| ascii_icase_contains(hay, &needle_lower);

    if let Some(want) = locale {
        let hit_desc = doc
            .descriptions
            .iter()
            .any(|(loc, text)| *loc == want && pred(text));
        if hit_desc {
            return true;
        }
    } else if doc.descriptions.iter().any(|(_, text)| pred(text)) {
        return true;
    }

    if pred(doc.manufacturer.as_str()) || pred(doc.product.as_str()) {
        return true;
    }
    if doc.keywords.iter().any(|k| pred(k.as_str())) {
        return true;
    }
    false
}

/// ASCII-case-insensitive substring search. `needle` is assumed to
/// already be lowercase. Walks `hay`'s bytes, lowercasing ASCII on
/// the fly; non-ASCII bytes compare as-is (so a non-ASCII letter
/// won't case-fold to anything, matching only its literal form).
///
/// Trades a small loss of correctness for non-ASCII text against a
/// large win in allocation/CPU: previously every hay was
/// `String::to_lowercase()`'d which allocated and walked the full
/// string. Now we walk hay once with no allocation.
fn ascii_icase_contains(hay: &str, needle: &str) -> bool {
    let nb = needle.as_bytes();
    let hb = hay.as_bytes();
    if nb.is_empty() {
        return true;
    }
    if hb.len() < nb.len() {
        return false;
    }
    // Use memchr::memmem to vectorise the candidate-position search.
    // The candidate filter is: any byte b where lowercase(b) ==
    // needle[0]. Since the needle is already lowercase, we look for
    // either needle[0] OR its uppercase form in the haystack. Each
    // candidate is verified with an ASCII-fold byte compare against
    // the rest of the needle.
    //
    // memchr::memchr2 finds the next position of either of two bytes
    // in a single SIMD pass. For ASCII letters that's needle[0] and
    // needle[0]-32 (uppercase form). For non-letter needles (digits,
    // punctuation), the two-byte search degenerates to a single-byte
    // search at the same speed.
    let n0 = nb[0];
    let (probe_lo, probe_hi) = if n0.is_ascii_lowercase() {
        (n0, n0 - 32) // 'a'..='z' → also probe 'A'..='Z'
    } else {
        (n0, n0)
    };
    let mut start = 0;
    while start + nb.len() <= hb.len() {
        // SIMD-skip to next candidate first-byte.
        let rest = &hb[start..hb.len() - nb.len() + 1];
        let Some(off) = memchr::memchr2(probe_lo, probe_hi, rest) else {
            return false;
        };
        let i = start + off;
        // Verify the rest of the needle with the ASCII-fold compare.
        let mut ok = true;
        for j in 1..nb.len() {
            let h = hb[i + j];
            let h_fold = if h.is_ascii_uppercase() { h + 32 } else { h };
            if h_fold != nb[j] {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
        start = i + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{
        DocId, FileMeta, LuminaireDoc, MountingPlace, MountingType, PhotometricSymmetryHint,
        PhotometryStats, SourceRef, VariantDoc, VariantId,
    };

    fn sample_variant(id: u16, flux: f32, power: f32, cct: u16) -> VariantDoc {
        VariantDoc {
            id: VariantId(id),
            name: CompactString::from("v"),
            photometry: Some(PhotometryStats {
                flux_lm: Some(flux),
                power_w: Some(power),
                efficacy_lm_w: Some(flux / power),
                cct_k: Some(cct),
                cri_ra: Some(80),
                r9: None,
                beam_deg: Some(60.0),
                field_deg: Some(120.0),
                ulor: Some(0.0),
                dlor: Some(1.0),
                symmetry: PhotometricSymmetryHint::Rotational,
                light_distribution: Some(0),
                ..Default::default()
            }),
            mounting_place: MountingPlace::Ceiling,
            mounting_type: MountingType::SurfaceMounted,
            recessed_depth_mm: None,
            has_3d: false,
            lamp_type: None,
            control_gear_interfaces: smallvec::smallvec![],
            has_emergency_lighting: false,
            emergency_lighting_type: None,
            emitter_count: 1,
            source_index: 0,
        }
    }

    /// A variant whose photometry was missing (no LDC in the GLDF, or
    /// LDC parsing failed). Mounting is still meaningful — that's the
    /// case the schema cares about.
    fn variant_without_photometry(id: u16) -> VariantDoc {
        VariantDoc {
            id: VariantId(id),
            name: CompactString::from("v-noldc"),
            photometry: None,
            mounting_place: MountingPlace::Wall,
            mounting_type: MountingType::SurfaceMounted,
            recessed_depth_mm: None,
            has_3d: false,
            lamp_type: None,
            control_gear_interfaces: smallvec::smallvec![],
            has_emergency_lighting: false,
            emergency_lighting_type: None,
            emitter_count: 1,
            source_index: 0,
        }
    }

    fn sample_doc() -> LuminaireDoc {
        LuminaireDoc {
            id: DocId([0; 32]),
            source: SourceRef::ContentOnly,
            source_paths: smallvec::smallvec![SourceRef::ContentOnly],
            manufacturer: CompactString::from("LEDVANCE"),
            product: CompactString::from("BayArena"),
            gtin: None,
            product_code: None,
            descriptions: smallvec::smallvec![
                (
                    Locale([b'd', b'e']),
                    String::from("Hallenstrahler für Sportstätten")
                ),
                (
                    Locale([b'e', b'n']),
                    String::from("High-bay luminaire for sports halls")
                ),
            ],
            keywords: smallvec::smallvec![CompactString::from("sports")],
            applications: smallvec::smallvec![10, 20], // arbitrary canonical ids
            labels: smallvec::smallvec![0, 1],         // CE, GS
            adjustability: smallvec::smallvec![0],     // Fixed
            ip_code: Some(25),                         // some IP-something
            safety_class: Some(1),                     // I
            ik_rating: Some(8),                        // IK08
            product_form: Some(2),                     // Square
            variants: smallvec::smallvec![
                sample_variant(0, 10_000.0, 100.0, 4000),
                sample_variant(1, 20_000.0, 180.0, 5000),
            ],
            file_meta: FileMeta {
                size_bytes: 1234,
                mtime_epoch_s: None,
                format_version: None,
            },
        }
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = Filter::default();
        assert!(f.matches_doc(&sample_doc()));
    }

    #[test]
    fn or_within_facet() {
        let f = Filter {
            applications: smallvec::smallvec![10, 200], // 10 is present, 999 is not
            ..Default::default()
        };
        assert!(f.matches_doc(&sample_doc()));

        let f_miss = Filter {
            applications: smallvec::smallvec![199, 200],
            ..Default::default()
        };
        assert!(!f_miss.matches_doc(&sample_doc()));
    }

    #[test]
    fn and_across_facets() {
        // Both must hit. labels=0 (CE) yes, applications=999 no → reject.
        let f = Filter {
            labels: smallvec::smallvec![0],
            applications: smallvec::smallvec![200],
            ..Default::default()
        };
        assert!(!f.matches_doc(&sample_doc()));
    }

    #[test]
    fn flux_range_inclusive() {
        // Variant 0 has flux 10_000.
        let exact = Filter {
            flux_lm: Some(10_000.0..=10_000.0),
            ..Default::default()
        };
        assert!(exact.matches_doc(&sample_doc()));

        let above = Filter {
            flux_lm: Some(15_000.0..=20_000.0),
            ..Default::default()
        };
        // Variant 1 has flux 20_000 → matches.
        assert!(above.matches_doc(&sample_doc()));

        let none = Filter {
            flux_lm: Some(50_000.0..=60_000.0),
            ..Default::default()
        };
        assert!(!none.matches_doc(&sample_doc()));
    }

    #[test]
    fn cct_range_excludes_undeclared() {
        let mut doc = sample_doc();
        if let Some(p) = doc.variants[0].photometry.as_mut() {
            p.cct_k = None;
        }
        if let Some(p) = doc.variants[1].photometry.as_mut() {
            p.cct_k = None;
        }
        let f = Filter {
            cct_k: Some(3000..=4500),
            ..Default::default()
        };
        // No variant declares CCT → no match.
        assert!(!f.matches_doc(&doc));
    }

    #[test]
    fn cri_min() {
        let pass = Filter {
            cri_min: Some(80),
            ..Default::default()
        };
        assert!(pass.matches_doc(&sample_doc()));

        let fail = Filter {
            cri_min: Some(95),
            ..Default::default()
        };
        assert!(!fail.matches_doc(&sample_doc()));
    }

    #[test]
    fn manufacturer_exact_match() {
        let hit = Filter {
            manufacturer: Some(smallvec::smallvec![CompactString::from("LEDVANCE")]),
            ..Default::default()
        };
        assert!(hit.matches_doc(&sample_doc()));

        let miss = Filter {
            manufacturer: Some(smallvec::smallvec![CompactString::from("Other")]),
            ..Default::default()
        };
        assert!(!miss.matches_doc(&sample_doc()));
    }

    #[test]
    fn text_finds_in_locale() {
        let de = Filter {
            text: Some(CompactString::from("Hallen")),
            locale: Some(Locale([b'd', b'e'])),
            ..Default::default()
        };
        assert!(de.matches_doc(&sample_doc()));

        let de_miss = Filter {
            text: Some(CompactString::from("not present")),
            locale: Some(Locale([b'd', b'e'])),
            ..Default::default()
        };
        assert!(!de_miss.matches_doc(&sample_doc()));
    }

    #[test]
    fn text_in_one_locale_does_not_match_another() {
        // "Hallen" is in the de description; restricting to en must miss
        // it (and also miss in manufacturer/product/keywords).
        let f = Filter {
            text: Some(CompactString::from("Hallen")),
            locale: Some(Locale([b'e', b'n'])),
            ..Default::default()
        };
        assert!(!f.matches_doc(&sample_doc()));
    }

    #[test]
    fn text_unrestricted_searches_all_locales_and_metadata() {
        let f = Filter {
            text: Some(CompactString::from("Bay")),
            ..Default::default()
        };
        // Matches "BayArena" product name.
        assert!(f.matches_doc(&sample_doc()));
    }

    #[test]
    fn text_case_insensitive() {
        let f = Filter {
            text: Some(CompactString::from("LEDVANCE")),
            ..Default::default()
        };
        assert!(f.matches_doc(&sample_doc()));

        let lower = Filter {
            text: Some(CompactString::from("ledvance")),
            ..Default::default()
        };
        assert!(lower.matches_doc(&sample_doc()));
    }

    #[test]
    fn variant_predicate_with_doc_predicate_must_both_hit() {
        // Doc-scoped: applications must include 10 (yes).
        // Variant-scoped: flux must be in 50_000..=60_000 (no variant
        // satisfies). Combined: reject.
        let f = Filter {
            applications: smallvec::smallvec![10],
            flux_lm: Some(50_000.0..=60_000.0),
            ..Default::default()
        };
        assert!(!f.matches_doc(&sample_doc()));
    }

    #[test]
    fn light_distribution_undeclared_excluded_when_required() {
        let mut doc = sample_doc();
        for v in doc.variants.iter_mut() {
            if let Some(p) = v.photometry.as_mut() {
                p.light_distribution = None;
            }
        }
        let f = Filter {
            light_distribution: smallvec::smallvec![0],
            ..Default::default()
        };
        assert!(!f.matches_doc(&doc));
    }

    // ── New tests for Option<PhotometryStats> + has_photometry ──────

    #[test]
    fn variant_without_photometry_still_matches_mounting_only_filter() {
        // A doc whose only variant has no LDC. A mounting-only filter
        // must still find it — mounting comes from <Mountings>, not
        // the LDC.
        let doc = LuminaireDoc {
            variants: smallvec::smallvec![variant_without_photometry(0)],
            ..sample_doc()
        };
        let f = Filter {
            mounting_place: smallvec::smallvec![MountingPlace::Wall],
            ..Default::default()
        };
        assert!(f.matches_doc(&doc));
    }

    #[test]
    fn recessed_depth_range_filters_only_recessed_variants() {
        // Surface-mounted variant with a stray depth value gets
        // excluded by a depth filter (filter is for Recessed only).
        let v_surface = VariantDoc {
            mounting_type: MountingType::SurfaceMounted,
            recessed_depth_mm: Some(15),
            ..sample_variant(0, 1000.0, 10.0, 4000)
        };
        let v_recessed_shallow = VariantDoc {
            mounting_type: MountingType::Recessed,
            recessed_depth_mm: Some(15),
            ..sample_variant(1, 1000.0, 10.0, 4000)
        };
        let v_recessed_deep = VariantDoc {
            mounting_type: MountingType::Recessed,
            recessed_depth_mm: Some(60),
            ..sample_variant(2, 1000.0, 10.0, 4000)
        };

        let f = Filter {
            recessed_depth_mm: Some(0..=20),
            ..Default::default()
        };

        let doc = LuminaireDoc {
            variants: smallvec::smallvec![v_surface.clone()],
            ..sample_doc()
        };
        assert!(!f.matches_doc(&doc), "surface-mounted must not pass");

        let doc = LuminaireDoc {
            variants: smallvec::smallvec![v_recessed_shallow.clone()],
            ..sample_doc()
        };
        assert!(f.matches_doc(&doc), "shallow recessed must pass");

        let doc = LuminaireDoc {
            variants: smallvec::smallvec![v_recessed_deep.clone()],
            ..sample_doc()
        };
        assert!(!f.matches_doc(&doc), "deep recessed must not pass");
    }

    #[test]
    fn variant_without_photometry_excluded_from_numeric_range() {
        let doc = LuminaireDoc {
            variants: smallvec::smallvec![variant_without_photometry(0)],
            ..sample_doc()
        };
        let f = Filter {
            flux_lm: Some(1000.0..=100_000.0),
            ..Default::default()
        };
        assert!(!f.matches_doc(&doc));
    }

    #[test]
    fn has_photometry_true_requires_at_least_one_variant_with_ldc() {
        // Mixed: variant 0 has photometry, variant 1 does not.
        let doc = LuminaireDoc {
            variants: smallvec::smallvec![
                sample_variant(0, 10_000.0, 100.0, 4000),
                variant_without_photometry(1),
            ],
            ..sample_doc()
        };
        let f = Filter {
            has_photometry: Some(true),
            ..Default::default()
        };
        assert!(f.matches_doc(&doc));
    }

    #[test]
    fn has_photometry_false_finds_docs_missing_ldc() {
        // Mixed: doc passes when we're triaging for missing-LDC files.
        let mixed_doc = LuminaireDoc {
            variants: smallvec::smallvec![
                sample_variant(0, 10_000.0, 100.0, 4000),
                variant_without_photometry(1),
            ],
            ..sample_doc()
        };
        let only_ldc_doc = LuminaireDoc {
            variants: smallvec::smallvec![sample_variant(0, 10_000.0, 100.0, 4000)],
            ..sample_doc()
        };
        let triage = Filter {
            has_photometry: Some(false),
            ..Default::default()
        };
        assert!(triage.matches_doc(&mixed_doc));
        assert!(!triage.matches_doc(&only_ldc_doc));
    }

    #[test]
    fn has_3d_tristate() {
        let with_3d = VariantDoc {
            has_3d: true,
            ..sample_variant(0, 10_000.0, 100.0, 4000)
        };
        let without_3d = VariantDoc {
            has_3d: false,
            ..sample_variant(1, 10_000.0, 100.0, 4000)
        };

        let doc_with = LuminaireDoc {
            variants: smallvec::smallvec![with_3d.clone()],
            ..sample_doc()
        };
        let doc_without = LuminaireDoc {
            variants: smallvec::smallvec![without_3d.clone()],
            ..sample_doc()
        };

        let want_3d = Filter {
            has_3d: Some(true),
            ..Default::default()
        };
        assert!(want_3d.matches_doc(&doc_with));
        assert!(!want_3d.matches_doc(&doc_without));

        let want_no_3d = Filter {
            has_3d: Some(false),
            ..Default::default()
        };
        assert!(!want_no_3d.matches_doc(&doc_with));
        assert!(want_no_3d.matches_doc(&doc_without));
    }

    #[test]
    fn is_text_only_true_for_pure_text_query() {
        let f = Filter {
            text: Some(CompactString::from("led")),
            ..Default::default()
        };
        assert!(f.is_text_only());

        let f = Filter {
            text: Some(CompactString::from("led")),
            locale: Some(Locale([b'd', b'e'])),
            ..Default::default()
        };
        assert!(f.is_text_only(), "locale-only narrows the same text scan");
    }

    #[test]
    fn is_text_only_false_when_any_facet_active() {
        let f = Filter {
            text: Some(CompactString::from("led")),
            applications: smallvec::smallvec![10],
            ..Default::default()
        };
        assert!(!f.is_text_only());

        let f = Filter {
            text: Some(CompactString::from("led")),
            flux_lm: Some(1000.0..=2000.0),
            ..Default::default()
        };
        assert!(!f.is_text_only());

        let f = Filter {
            text: Some(CompactString::from("led")),
            has_photometry: Some(true),
            ..Default::default()
        };
        assert!(!f.is_text_only());

        let f = Filter {
            text: Some(CompactString::from("led")),
            mounting_place: smallvec::smallvec![MountingPlace::Ceiling],
            ..Default::default()
        };
        assert!(!f.is_text_only());
    }

    #[test]
    fn is_text_only_false_when_no_text() {
        let f = Filter::default();
        assert!(!f.is_text_only());

        let f = Filter {
            applications: smallvec::smallvec![10],
            ..Default::default()
        };
        assert!(!f.is_text_only());
    }

    #[test]
    fn has_photometry_none_is_dont_care() {
        let only_ldc_doc = LuminaireDoc {
            variants: smallvec::smallvec![sample_variant(0, 10_000.0, 100.0, 4000)],
            ..sample_doc()
        };
        let no_ldc_doc = LuminaireDoc {
            variants: smallvec::smallvec![variant_without_photometry(0)],
            ..sample_doc()
        };
        let no_filter = Filter::default();
        assert!(no_filter.matches_doc(&only_ldc_doc));
        assert!(no_filter.matches_doc(&no_ldc_doc));
    }
}
