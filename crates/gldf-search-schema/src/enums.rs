//! Index types over the canonical XSD enum tables shipped by `gldf-rs`.
//!
//! Each closed-enum schema field is a `u8` index into a
//! `&'static [&'static str]` table in
//! [`gldf_rs::validation::xsd_enums`]. The index is the storage form;
//! the canonical XSD string is recovered with [`as_str`]-style helpers.
//!
//! **Why indices, not Rust enums:** the canonical tables are the source
//! of truth, doc-commented and XSD-cited. Parallel Rust enums would
//! drift as the XSD evolves. `u8` indices give bitmap-friendly storage
//! for the future facet index, fit every closed enum in the schema (max
//! 79 for `Application`), and round-trip through the canonical string
//! losslessly.
//!
//! All `from_str` helpers do an O(n) linear scan. The tables are short
//! (≤ 79 entries) so this is fine for indexing-time work.

use gldf_rs::validation::xsd_enums as xsd;

/// Index into [`gldf_rs::validation::xsd_enums::APPLICATIONS`] (79
/// hierarchical values, "Top: Mid: Leaf"). Use [`application_str`] to
/// recover the canonical string.
pub type ApplicationId = u8;

/// Index into [`gldf_rs::validation::xsd_enums::IP_CODES`] (47 values,
/// `IP20`..`IP69K`).
pub type IpCodeId = u8;

/// Index into
/// [`gldf_rs::validation::xsd_enums::ELECTRICAL_SAFETY_CLASSES`]
/// (5 values: `0`, `I`, `0I`, `II`, `III`).
pub type SafetyClassId = u8;

/// Index into [`gldf_rs::validation::xsd_enums::LABELS`] (14 regulatory
/// / quality marks: `CE`, `ENEC`, `UL`, ...).
pub type LabelId = u8;

/// Index into [`gldf_rs::validation::xsd_enums::PRODUCT_FORMS`]
/// (10 geometric forms: `Round`, `Linear`, `Cuboid`, ...).
pub type ProductFormId = u8;

/// Index into
/// [`gldf_rs::validation::xsd_enums::LIGHT_DISTRIBUTIONS`]
/// (15 photometric distribution classes).
pub type LightDistributionId = u8;

/// Index into [`gldf_rs::validation::xsd_enums::IK_RATINGS`]
/// (12 impact-protection grades, `IK00`..`IK10+`).
pub type IkRatingId = u8;

/// Index into [`gldf_rs::validation::xsd_enums::ADJUSTABILITIES`]
/// (7 adjustability modes).
pub type AdjustabilityId = u8;

/// Index into [`gldf_rs::validation::xsd_enums::CIE97_LAMP_TYPES`].
/// Canonical CIE-97 lamp categories — Incandescent, Halogen, the
/// Fluorescent family, Mercury, Metal Halide variants, High Pressure
/// Sodium.
pub type LampTypeId = u8;

/// Index into [`gldf_rs::validation::xsd_enums::INTERFACES`].
/// Control-gear bus/wireless types — DALI / KNX / 0-10V / WiFi /
/// Bluetooth / DMX / etc. (Newer XSD values like Zigbee/Matter/
/// Thread/Bluetooth Mesh/DALI-2/DALI D4i aren't yet in the upstream
/// table, so they currently land as non-canonical.)
pub type ControlGearInterfaceId = u8;

/// Index into
/// [`gldf_rs::validation::xsd_enums::EMERGENCY_LIGHTING_TYPES`].
/// The dedicated function of an emergency luminaire (`Exit Light` /
/// `Guide Light` / `Evacuation Light` / `Reference Light` /
/// `For Signage` / `For Lighting`).
pub type EmergencyLightingTypeId = u8;

// ── Lookup helpers ────────────────────────────────────────────────────

fn index_of(table: &'static [&'static str], value: &str) -> Option<u8> {
    table
        .iter()
        .position(|v| *v == value)
        .and_then(|i| u8::try_from(i).ok())
}

fn at(table: &'static [&'static str], id: u8) -> Option<&'static str> {
    table.get(id as usize).copied()
}

/// Resolve a canonical XSD string to its `ApplicationId`. Returns
/// `None` for non-canonical values (legacy / typo / future XSD).
pub fn application_from_str(value: &str) -> Option<ApplicationId> {
    index_of(xsd::APPLICATIONS, value)
}

/// Recover the canonical XSD string for an `ApplicationId`. Returns
/// `None` if the id is out of range for the current table.
pub fn application_str(id: ApplicationId) -> Option<&'static str> {
    at(xsd::APPLICATIONS, id)
}

/// Top-level segment of the hierarchical Application string
/// (`"Interior"` / `"Exterior"`). Every canonical entry has at least
/// one segment, so this is always `Some` for a valid id.
pub fn application_top(id: ApplicationId) -> Option<&'static str> {
    let s = application_str(id)?;
    Some(s.split(':').next().unwrap_or(s).trim())
}

/// Middle (second) segment of the hierarchical Application string —
/// the category between top-level and leaf. Returns `None` for
/// two-segment entries like `"Exterior: Other"` and for any id whose
/// canonical string has only one segment. UI uses this to render a
/// `Top → Mid → Leaf` tree.
pub fn application_mid(id: ApplicationId) -> Option<&'static str> {
    let s = application_str(id)?;
    let mut parts = s.split(':');
    let _top = parts.next()?;
    let mid = parts.next()?;
    // Require a third segment — otherwise this is a 2-segment parent
    // entry (no real "middle"), and the caller should treat it as a
    // leaf directly under Top.
    parts.next()?;
    Some(mid.trim())
}

/// Last segment of the hierarchical Application string — the
/// most-specific term that would appear in a product name or
/// description. `"Interior: Office: Desk"` → `"Desk"`. For
/// two-segment entries (`"Exterior: Other"`) returns `"Other"`.
///
/// Used by the extractor's name-mining pass to map free-text product
/// names onto canonical `ApplicationId`s when the GLDF declares no
/// `DescriptiveAttributes/Marketing/Applications` (the common case
/// in the light-other-rs 270k export).
pub fn application_leaf(id: ApplicationId) -> Option<&'static str> {
    let s = application_str(id)?;
    s.rsplit(':').next().map(|s| s.trim())
}

/// All canonical `(probe-lowercase, ApplicationId)` matching pairs
/// for name-mining.
///
/// We emit **one probe per useful sub-token of each leaf** rather
/// than the full leaf string, because product names rarely contain
/// the full XSD phrasing. Example: leaf `"Office Desks"` produces
/// probes `["office desks", "office", "desks"]`, leaf
/// `"Industry/Craft: Industrial Workshops"` produces `"workshops"`,
/// `"workshop"` (singular), `"industrial"`. Each probe maps back to
/// the same `ApplicationId`.
///
/// Probes are lowercased so the matcher compares against a
/// lowercased haystack without per-call work. Sorted by length
/// descending so `"office desks"` matches before `"office"` would
/// shadow it on the same input.
///
/// Skipped:
/// - Probes shorter than 4 chars (too many manufacturer-slug hits).
/// - Generic catch-all words (`other`, `general`, `areas`, etc.).
/// - Probes that duplicate an already-emitted shorter token from a
///   different id (keeps the table compact and avoids ambiguity).
///
/// **Why a Vec, not a static**: `application_leaf` walks the XSD
/// table at runtime, and we don't want a build-time codegen step
/// just for this. The table is 79 entries; building the Vec is
/// microsecond work.
pub fn application_leaves_lower() -> Vec<(String, ApplicationId)> {
    const STOPWORDS: &[&str] = &[
        "other",
        "general",
        "areas",
        "zones",
        "lighting",
        "fields",
        "facilities",
        "places",
        "rooms",
        "stations",
        "spaces",
        "paths",
        "roads",
    ];

    let mut out: Vec<(String, ApplicationId)> = Vec::new();
    let mut seen_probes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (i, _s) in xsd::APPLICATIONS.iter().enumerate() {
        let Ok(id) = u8::try_from(i) else {
            continue;
        };
        let Some(leaf) = application_leaf(id) else {
            continue;
        };

        // Clean the leaf for splitting: drop parenthesized qualifiers
        // (`(Indoor)`, `(Exterior)`), normalize `/` and `-` to spaces,
        // then lowercase. Resulting probes are space-separated tokens
        // + the whole multi-word phrase.
        let cleaned: String = leaf
            .chars()
            .filter(|c| !matches!(*c, '(' | ')'))
            .map(|c| match c {
                '/' | '-' => ' ',
                _ => c,
            })
            .collect();
        let lowered = cleaned.to_lowercase();
        let phrase = lowered.split_whitespace().collect::<Vec<_>>().join(" ");

        let mut probes_for_id: Vec<String> = Vec::new();
        if phrase.len() >= 4 && !STOPWORDS.contains(&phrase.as_str()) && phrase.contains(' ') {
            probes_for_id.push(phrase.clone());
        }
        for tok in phrase.split_whitespace() {
            if tok.len() < 4 || STOPWORDS.contains(&tok) {
                continue;
            }
            probes_for_id.push(tok.to_string());
            // Plural→singular: product names use the singular form
            // (`"Office Pendant"`, `"Street Light"`) far more often
            // than the canonical plural (`"Offices"`, `"Streets"`).
            // English heuristic — only trims a trailing 's' when the
            // remainder is still ≥ 4 chars and not preceded by a
            // sibilant ending ("ss", "us", "is") to avoid false stems
            // ("class" → "clas").
            if let Some(stem) = simple_singular(tok) {
                if stem.len() >= 4 && !STOPWORDS.contains(&stem.as_str()) {
                    probes_for_id.push(stem);
                }
            }
        }

        for probe in probes_for_id {
            if seen_probes.insert(probe.clone()) {
                out.push((probe, id));
            }
        }
    }

    // Length-descending so multi-word phrases match before any of
    // their constituent tokens would on the same haystack span.
    out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    out
}

/// Heuristic English singularization for application-leaf probes.
///
/// Trims a trailing `s` to recover the singular form, with two
/// guards: (a) skip non-plural `-s` endings (`-ss`, `-us`, `-is`,
/// `-ous`); (b) `-ies` → `-y`. Conservative on purpose — we'd rather
/// miss a valid plural than emit a garbage stem that matches noise
/// in 270k product names.
fn simple_singular(s: &str) -> Option<String> {
    if s.len() < 5 || !s.ends_with('s') {
        return None;
    }
    // Skip common non-plural -s endings to avoid false stems.
    if s.ends_with("ss") || s.ends_with("us") || s.ends_with("is") || s.ends_with("ous") {
        return None;
    }
    // Handle "-ies" → "-y" (e.g. "facilities" → "facility").
    if s.ends_with("ies") && s.len() > 4 {
        let mut out = String::with_capacity(s.len() - 2);
        out.push_str(&s[..s.len() - 3]);
        out.push('y');
        return Some(out);
    }
    Some(s[..s.len() - 1].to_string())
}

/// Resolve a canonical XSD string to its `IpCodeId`.
pub fn ip_code_from_str(value: &str) -> Option<IpCodeId> {
    index_of(xsd::IP_CODES, value)
}

/// Recover the canonical XSD string for an `IpCodeId`.
pub fn ip_code_str(id: IpCodeId) -> Option<&'static str> {
    at(xsd::IP_CODES, id)
}

/// Resolve a canonical XSD string to its `SafetyClassId`.
pub fn safety_class_from_str(value: &str) -> Option<SafetyClassId> {
    index_of(xsd::ELECTRICAL_SAFETY_CLASSES, value)
}

/// Recover the canonical XSD string for a `SafetyClassId`.
pub fn safety_class_str(id: SafetyClassId) -> Option<&'static str> {
    at(xsd::ELECTRICAL_SAFETY_CLASSES, id)
}

/// Resolve a canonical XSD string to its `LabelId`.
pub fn label_from_str(value: &str) -> Option<LabelId> {
    index_of(xsd::LABELS, value)
}

/// Recover the canonical XSD string for a `LabelId`.
pub fn label_str(id: LabelId) -> Option<&'static str> {
    at(xsd::LABELS, id)
}

/// Resolve a canonical XSD string to its `ProductFormId`.
pub fn product_form_from_str(value: &str) -> Option<ProductFormId> {
    index_of(xsd::PRODUCT_FORMS, value)
}

/// Recover the canonical XSD string for a `ProductFormId`.
pub fn product_form_str(id: ProductFormId) -> Option<&'static str> {
    at(xsd::PRODUCT_FORMS, id)
}

/// Resolve a canonical XSD string to its `LightDistributionId`.
pub fn light_distribution_from_str(value: &str) -> Option<LightDistributionId> {
    index_of(xsd::LIGHT_DISTRIBUTIONS, value)
}

/// Recover the canonical XSD string for a `LightDistributionId`.
pub fn light_distribution_str(id: LightDistributionId) -> Option<&'static str> {
    at(xsd::LIGHT_DISTRIBUTIONS, id)
}

/// Resolve a canonical XSD string to its `IkRatingId`.
pub fn ik_rating_from_str(value: &str) -> Option<IkRatingId> {
    index_of(xsd::IK_RATINGS, value)
}

/// Recover the canonical XSD string for an `IkRatingId`.
pub fn ik_rating_str(id: IkRatingId) -> Option<&'static str> {
    at(xsd::IK_RATINGS, id)
}

/// Resolve a canonical XSD string to its `AdjustabilityId`.
pub fn adjustability_from_str(value: &str) -> Option<AdjustabilityId> {
    index_of(xsd::ADJUSTABILITIES, value)
}

/// Recover the canonical XSD string for an `AdjustabilityId`.
pub fn adjustability_str(id: AdjustabilityId) -> Option<&'static str> {
    at(xsd::ADJUSTABILITIES, id)
}

/// Resolve a canonical XSD string to its `LampTypeId`.
pub fn lamp_type_from_str(value: &str) -> Option<LampTypeId> {
    index_of(xsd::CIE97_LAMP_TYPES, value)
}

/// Recover the canonical XSD string for a `LampTypeId`.
pub fn lamp_type_str(id: LampTypeId) -> Option<&'static str> {
    at(xsd::CIE97_LAMP_TYPES, id)
}

/// Resolve a canonical XSD string to its `ControlGearInterfaceId`.
pub fn control_gear_interface_from_str(value: &str) -> Option<ControlGearInterfaceId> {
    index_of(xsd::INTERFACES, value)
}

/// Recover the canonical XSD string for a `ControlGearInterfaceId`.
pub fn control_gear_interface_str(id: ControlGearInterfaceId) -> Option<&'static str> {
    at(xsd::INTERFACES, id)
}

/// Resolve a canonical XSD string to its `EmergencyLightingTypeId`.
pub fn emergency_lighting_type_from_str(value: &str) -> Option<EmergencyLightingTypeId> {
    index_of(xsd::EMERGENCY_LIGHTING_TYPES, value)
}

/// Recover the canonical XSD string for an `EmergencyLightingTypeId`.
pub fn emergency_lighting_type_str(id: EmergencyLightingTypeId) -> Option<&'static str> {
    at(xsd::EMERGENCY_LIGHTING_TYPES, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_round_trips() {
        let id =
            application_from_str("Interior: Office: Office Desks").expect("canonical Application");
        assert_eq!(application_str(id), Some("Interior: Office: Office Desks"));
        assert_eq!(application_top(id), Some("Interior"));
    }

    #[test]
    fn application_top_handles_no_separator() {
        // The canonical table has parent entries with no `:` deeper
        // than top-level, e.g. "Exterior: Other".
        let id = application_from_str("Exterior: Other").expect("canonical");
        assert_eq!(application_top(id), Some("Exterior"));
    }

    #[test]
    fn application_mid_returns_middle_segment() {
        let id =
            application_from_str("Interior: Office: Office Desks").expect("canonical Application");
        assert_eq!(application_mid(id), Some("Office"));
    }

    #[test]
    fn application_mid_none_for_two_segment_entries() {
        // 2-segment entries have a top + leaf, no real middle.
        let id = application_from_str("Exterior: Other").expect("canonical");
        assert_eq!(application_mid(id), None);
    }

    #[test]
    fn ip_code_round_trips() {
        let id = ip_code_from_str("IP65").expect("canonical IP65");
        assert_eq!(ip_code_str(id), Some("IP65"));
    }

    #[test]
    fn safety_class_round_trips() {
        let id = safety_class_from_str("0I").expect("canonical 0I");
        assert_eq!(safety_class_str(id), Some("0I"));
    }

    #[test]
    fn unknown_string_is_none() {
        assert_eq!(application_from_str("not a category"), None);
        assert_eq!(ip_code_from_str("IP99"), None);
    }

    #[test]
    fn out_of_range_index_is_none() {
        assert_eq!(application_str(255), None);
        assert_eq!(ip_code_str(200), None);
    }

    #[test]
    fn every_table_fits_u8() {
        // Sanity: u8 has 256 slots; even if the XSD grows, we have headroom
        // for the foreseeable future. If any table reaches 250+ entries we
        // switch to u16 — guardrail tested here.
        assert!(xsd::APPLICATIONS.len() <= 250);
        assert!(xsd::IP_CODES.len() <= 250);
        assert!(xsd::LABELS.len() <= 250);
        assert!(xsd::PRODUCT_FORMS.len() <= 250);
        assert!(xsd::LIGHT_DISTRIBUTIONS.len() <= 250);
        assert!(xsd::IK_RATINGS.len() <= 250);
        assert!(xsd::ADJUSTABILITIES.len() <= 250);
        assert!(xsd::ELECTRICAL_SAFETY_CLASSES.len() <= 250);
    }
}
