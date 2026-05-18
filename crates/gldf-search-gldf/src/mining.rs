//! Mine searchable signal out of variant `<Name>` strings.
//!
//! The `light-other-rs` corpus that drives our test sample carries
//! almost all facet-able information in **free-form variant names**:
//!
//! ```text
//! "DL SQUARE MAXI C/EW 2x42° 15W 930 SL-10 EM1/3h + White Optic IP54 | Battery"
//! "BARIS 52 LED Z 1143mm 1900lm 840 IP44 I cl. PRM Anoda CO 16W NO DIFFUSER"
//! "MADERA 3 LED 480x480mm 3100lm 830 IP44 OPAL WHITE (26W)"
//! ```
//!
//! These encode form (Square / Round / Linear), wattage, flux, IP code,
//! CRI+CCT via the DIN 3-digit code, beam angle, and emergency-lighting
//! designation. Mining converts what's recognisable into typed schema
//! ids; the raw matched tokens also enter the keyword pool so fulltext
//! queries on `"IP54"` or `"4000K"` work before facet UI exists.
//!
//! Conventions:
//! - The mining pass is **best-effort**. False negatives (a known value
//!   the regex missed) are far better than false positives.
//! - Typed XSD fields from `<Mechanical>`/`<Electrical>` always take
//!   precedence — see `extract.rs`. Mining fills in only where the XSD
//!   path was empty.
//! - Every mined value also goes into the keyword pool, regardless of
//!   whether a typed XSD field shadowed it. This makes the fulltext
//!   index match the user's literal query even when the canonical
//!   form differs (`"IP54"` matches whether mined or from XSD).
//! - The regexes target English convention (`W`, `lm`, `IP`). German
//!   spellings like `"Watt"` are skipped — the corpus uses English
//!   shorthand exclusively.

use std::sync::OnceLock;

use compact_str::CompactString;
use gldf_search_schema::enums::{
    application_leaves_lower, application_str, ik_rating_from_str, ip_code_from_str,
    product_form_from_str, ApplicationId, IkRatingId, IpCodeId, ProductFormId,
};
use regex::Regex;
use smallvec::SmallVec;

/// Everything mined from one variant Name string. `extract.rs` merges
/// this with typed XSD data to build the final `VariantDoc`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MinedFromName {
    /// Luminous flux (lumens). Last match wins if the name has more
    /// than one (typically only one).
    pub flux_lm: Option<f32>,
    /// Electrical power (watts).
    pub power_w: Option<f32>,
    /// Correlated colour temperature in K. Decoded from DIN 3-digit
    /// codes (`830` → 3000 K, `940` → 4000 K) and explicit `4000K`
    /// forms. DIN takes priority when both appear in the same name.
    pub cct_k: Option<u16>,
    /// CRI Ra lower bound. From DIN 3-digit codes only — `830` → 80,
    /// `940` → 90. Treating the first digit as a *minimum* CRI tens
    /// is the honest reading: `830` means "Ra 80–89 at 3000 K", not a
    /// precise Ra value.
    pub cri_ra_min: Option<u8>,
    /// Half-peak beam angle in degrees. For asymmetric `MxN°` forms
    /// the smaller of the two values wins — the schema currently
    /// stores a single beam angle.
    pub beam_deg: Option<f32>,
    /// IP code id (resolved through `gldf_rs::validation::xsd_enums`).
    pub ip_code: Option<IpCodeId>,
    /// IK rating id.
    pub ik_rating: Option<IkRatingId>,
    /// Product form id (Round / Linear / Square / ...).
    pub product_form: Option<ProductFormId>,
    /// `true` when the name carries an emergency designation like
    /// `EM3h` or `EM1/3h`. Stored as a flag rather than typed data
    /// because the schema doesn't model emergency duration yet.
    pub is_emergency: bool,
    /// Tokens worth feeding into the fulltext keyword pool. Includes
    /// every matched canonical value (e.g. `"IP54"`) plus a few
    /// derived ones (e.g. `"3000K"` from `"830"`). De-duplicated by
    /// the caller.
    pub keywords: SmallVec<[CompactString; 8]>,
    /// Application taxonomy ids matched by name-mining. Filled by
    /// `scan_applications` from word-boundary probes against the XSD
    /// leaf segments. Only set when the variant's name carries an
    /// application term — most light-other-rs corpus names don't.
    pub applications: SmallVec<[ApplicationId; 4]>,
}

/// Mine a variant Name. Pure — no I/O, no allocation beyond the result
/// and the SmallVec spilling.
pub fn mine_variant_name(name: &str) -> MinedFromName {
    let mut out = MinedFromName::default();

    // Flux: "1900lm", "3100 lm".
    if let Some(c) = re_flux().captures(name) {
        if let Ok(n) = c[1].parse::<f32>() {
            out.flux_lm = Some(n);
            out.keywords
                .push(CompactString::from(format!("{}lm", n as u64)));
        }
    }

    // Power: "16W", "(26W)", "2x6W" — for "MxN W" we take the product
    // (parallel emitters) which matches what most LDC files report.
    if let Some(c) = re_power_product().captures(name) {
        if let (Ok(m), Ok(n)) = (c[1].parse::<f32>(), c[2].parse::<f32>()) {
            out.power_w = Some(m * n);
            out.keywords
                .push(CompactString::from(format!("{}W", (m * n) as u64)));
        }
    } else if let Some(c) = re_power_simple().captures(name) {
        if let Ok(n) = c[1].parse::<f32>() {
            out.power_w = Some(n);
            out.keywords
                .push(CompactString::from(format!("{}W", n as u64)));
        }
    }

    // Beam: "42°", "2x42°", "10x20°".
    //
    // `MxN°` is ambiguous: it can mean asymmetric beam (10x20° = 10°
    // along one axis, 20° along the other — pick the smaller per the
    // schema's symmetric-assumption rule) OR multi-emitter notation
    // (2x42° = two emitters with 42° beam each). We distinguish by
    // size: if the smaller of the two is below 5° it's not a real
    // photometric beam angle, so we treat the pair as "count × angle"
    // and use the larger value.
    if let Some(c) = re_beam_asym().captures(name) {
        if let (Ok(a), Ok(b)) = (c[1].parse::<f32>(), c[2].parse::<f32>()) {
            let (small, large) = if a < b { (a, b) } else { (b, a) };
            let deg = if small < 5.0 { large } else { small };
            out.beam_deg = Some(deg);
            out.keywords
                .push(CompactString::from(format!("{}°", deg as u32)));
        }
    } else if let Some(c) = re_beam_simple().captures(name) {
        if let Ok(deg) = c[1].parse::<f32>() {
            out.beam_deg = Some(deg);
            out.keywords
                .push(CompactString::from(format!("{}°", deg as u32)));
        }
    }

    // IP code: "IP54", "IP69K". The canonical-set membership check
    // means a typo like "IP99" gets dropped.
    if let Some(c) = re_ip_code().captures(name) {
        let raw = &c[0];
        if let Some(id) = ip_code_from_str(raw) {
            out.ip_code = Some(id);
            out.keywords.push(CompactString::from(raw));
        }
    }

    // IK rating: "IK08", "IK10+".
    if let Some(c) = re_ik_rating().captures(name) {
        let raw = &c[0];
        if let Some(id) = ik_rating_from_str(raw) {
            out.ik_rating = Some(id);
            out.keywords.push(CompactString::from(raw));
        }
    }

    // DIN 3-digit lighting code: "830" / "840" / "930" / "940" / "965".
    // First digit ≥ 8 = CRI tens (80 / 90), last two ×100 = CCT in K.
    // We require word boundaries on both sides to avoid eating part of
    // a longer number like "1900lm". The CCT range guards against
    // catastrophes (a `699` token isn't a DIN code).
    if let Some(c) = re_din_code().captures(name) {
        if let Ok(code) = c[1].parse::<u16>() {
            let cri_tens = (code / 100) as u8;
            let cct_hundreds = code % 100;
            let cct = cct_hundreds * 100;
            // Sanity: CCT must be in the lighting range (1500..=10000 K).
            if cri_tens >= 8 && (15..=100).contains(&cct_hundreds) {
                let cri = cri_tens * 10;
                out.cct_k = Some(cct);
                out.cri_ra_min = Some(cri);
                out.keywords.push(CompactString::from(format!("{cct}K")));
                out.keywords.push(CompactString::from(format!("CRI{cri}")));
            }
        }
    } else if let Some(c) = re_explicit_cct().captures(name) {
        // Fallback: "4000K" / "3000 K".
        if let Ok(k) = c[1].parse::<u16>() {
            if (1500..=10_000).contains(&k) {
                out.cct_k = Some(k);
                out.keywords.push(CompactString::from(format!("{k}K")));
            }
        }
    }

    // Emergency: "EM1/3h", "EM3h", "EM 3h".
    if re_emergency().is_match(name) {
        out.is_emergency = true;
        out.keywords.push(CompactString::from("emergency"));
    }

    // Product form keywords: rough match against PRODUCT_FORMS canonical
    // values. Case-insensitive; we test each form against word-boundary
    // occurrences in the name.
    if let Some((id, raw)) = scan_product_form(name) {
        out.product_form = Some(id);
        out.keywords.push(CompactString::from(raw));
    }

    // Application taxonomy: word-boundary match probes from the XSD
    // leaf segments. Most corpus names declare no application, so
    // most outputs stay empty.
    scan_applications(name, &mut out);

    out
}

// ── Regex cache — compiled once per process. ──────────────────────────

fn re_flux() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Word-boundary, then 1+ digits, optional decimal, optional
    // whitespace, "lm" case-insensitively.
    R.get_or_init(|| Regex::new(r"(?i)\b(\d{1,7})(?:\.\d+)?\s*lm\b").unwrap())
}

fn re_power_product() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(\d{1,3})\s*x\s*(\d{1,4})(?:\.\d+)?\s*W\b").unwrap())
}

fn re_power_simple() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Single wattage. Trailing word boundary excludes "16Wh", "16W2",
    // but keeps "16W" and "(26W)" via the `\b` before W.
    R.get_or_init(|| Regex::new(r"(?i)\b(\d{1,4})(?:\.\d+)?\s*W\b").unwrap())
}

fn re_beam_asym() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(\d{1,3})x(\d{1,3})°").unwrap())
}

fn re_beam_simple() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(\d{1,3})°").unwrap())
}

fn re_ip_code() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\bIP\d{2}K?\b").unwrap())
}

fn re_ik_rating() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\bIK\d{2}\+?\b").unwrap())
}

fn re_din_code() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // The DIN 3-digit code is whitespace-bounded on both sides so it
    // doesn't eat parts of "1900lm" or "480x480mm". Range gate (>=830)
    // narrowed in the parsing step.
    R.get_or_init(|| Regex::new(r"(?:^|\s)([89]\d{2})(?:\s|$|\)|,|;|\.)").unwrap())
}

fn re_explicit_cct() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(\d{4,5})\s*K\b").unwrap())
}

fn re_emergency() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\bEM\s*\d+(?:/\d+)?h\b").unwrap())
}

/// Scan for product-form keywords. Order matters: more specific
/// terms first. Returns the canonical XSD value when matched.
fn scan_product_form(name: &str) -> Option<(ProductFormId, &'static str)> {
    // Word-boundary checks via Regex would be overkill for 10 fixed
    // strings; do a manual case-insensitive substring scan instead.
    let lowered = name.to_lowercase();
    // Map plausible English keywords → canonical XSD value.
    let map: &[(&str, &str)] = &[
        ("rectangular", "Square"), // "Rectangular" → closest canonical is Square per XSD
        ("rectangle", "Square"),
        ("square", "Square"),
        ("round", "Round"),
        ("rounded", "Rounded"),
        ("linear", "Linear"),
        ("cuboid", "Cuboid"),
        ("cylinder", "Cylinder"),
        ("cylindrical", "Cylinder"),
        ("cone", "Cone"),
        ("conical", "Cone"),
        ("sphere", "Sphere"),
        ("spherical", "Sphere"),
        ("areal", "Areal"),
    ];
    for (needle, canonical) in map {
        if substr_word_match(&lowered, needle) {
            if let Some(id) = product_form_from_str(canonical) {
                return Some((id, canonical));
            }
        }
    }
    None
}

/// Match application-taxonomy probes against the variant name. Probes
/// are precomputed once per process from
/// `application_leaves_lower()` — lowercase phrases and tokens
/// derived from XSD leaf segments, sorted longest-first so multi-word
/// matches win.
///
/// We stop at the first match for each id (one variant might mention
/// several distinct applications; that's allowed). Each accepted
/// match pushes the canonical XSD string into the keyword pool so
/// fulltext queries on the canonical form still work.
fn scan_applications(name: &str, out: &mut MinedFromName) {
    static PROBES: OnceLock<Vec<(String, ApplicationId)>> = OnceLock::new();
    let probes = PROBES.get_or_init(application_leaves_lower);

    let lowered = name.to_lowercase();
    let mut seen: SmallVec<[ApplicationId; 4]> = SmallVec::new();
    for (probe, id) in probes {
        if seen.contains(id) {
            continue;
        }
        if substr_word_match(&lowered, probe) {
            seen.push(*id);
            if let Some(canonical) = application_str(*id) {
                out.keywords.push(CompactString::from(canonical));
            }
        }
    }
    out.applications = seen;
}

/// Word-boundary-aware substring match over an already-lowercased
/// haystack. We only treat ASCII alphanumeric / underscore as
/// word-char which is fine for the lighting English we're scanning.
fn substr_word_match(hay: &str, needle: &str) -> bool {
    let hay_bytes = hay.as_bytes();
    let needle_bytes = needle.as_bytes();
    let n = needle_bytes.len();
    if n == 0 || n > hay_bytes.len() {
        return false;
    }
    for start in 0..=(hay_bytes.len() - n) {
        if &hay_bytes[start..start + n] != needle_bytes {
            continue;
        }
        let before_ok = start == 0 || !is_word_byte(hay_bytes[start - 1]);
        let after_ok = start + n == hay_bytes.len() || !is_word_byte(hay_bytes[start + n]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pil_dl_square_maxi() {
        // Real corpus name. Asymmetric beam, single wattage, IP54,
        // DIN 930 (90 CRI, 3000K), emergency, square form.
        let r = mine_variant_name(
            "DL SQUARE MAXI C/EW 2x42° 15W 930 SL-10 EM1/3h + White Optic IP54 | Battery",
        );
        assert_eq!(r.power_w, Some(15.0));
        assert_eq!(r.beam_deg, Some(42.0));
        assert_eq!(
            r.ip_code.and_then(gldf_search_schema::enums::ip_code_str),
            Some("IP54")
        );
        assert_eq!(r.cct_k, Some(3000));
        assert_eq!(r.cri_ra_min, Some(90));
        assert!(r.is_emergency);
        assert_eq!(
            r.product_form
                .and_then(gldf_search_schema::enums::product_form_str),
            Some("Square")
        );
    }

    #[test]
    fn lena_baris() {
        // "BARIS 52 LED Z 1143mm 1900lm 840 IP44 I cl. PRM Anoda CO 16W NO DIFFUSER"
        let r = mine_variant_name(
            "BARIS 52 LED Z 1143mm 1900lm 840 IP44 I cl. PRM Anoda CO 16W NO DIFFUSER",
        );
        assert_eq!(r.flux_lm, Some(1900.0));
        assert_eq!(r.power_w, Some(16.0));
        assert_eq!(r.cct_k, Some(4000));
        assert_eq!(r.cri_ra_min, Some(80));
        assert_eq!(
            r.ip_code.and_then(gldf_search_schema::enums::ip_code_str),
            Some("IP44")
        );
        assert!(!r.is_emergency);
    }

    #[test]
    fn lena_madera() {
        // "MADERA 3 LED 480x480mm 3100lm 830 IP44 OPAL WHITE (26W)"
        let r = mine_variant_name("MADERA 3 LED 480x480mm 3100lm 830 IP44 OPAL WHITE (26W)");
        assert_eq!(r.flux_lm, Some(3100.0));
        assert_eq!(r.power_w, Some(26.0));
        assert_eq!(r.cct_k, Some(3000));
        assert_eq!(r.cri_ra_min, Some(80));
        assert_eq!(
            r.ip_code.and_then(gldf_search_schema::enums::ip_code_str),
            Some("IP44")
        );
    }

    #[test]
    fn rejects_non_canonical_ip() {
        let r = mine_variant_name("Some Luminaire IP99 weird");
        assert!(r.ip_code.is_none(), "IP99 is not in the canonical set");
    }

    #[test]
    fn product_form_round_matches_rectangular_kw() {
        let r = mine_variant_name("SlimBlend Rectangular, suspended");
        assert_eq!(
            r.product_form
                .and_then(gldf_search_schema::enums::product_form_str),
            Some("Square")
        );
    }

    #[test]
    fn empty_name_yields_default() {
        let r = mine_variant_name("");
        assert_eq!(r, MinedFromName::default());
    }

    #[test]
    fn din_code_does_not_eat_lumens() {
        // The DIN regex must not match "900" inside "1900lm".
        let r = mine_variant_name("Foo 1900lm Bar");
        assert_eq!(r.flux_lm, Some(1900.0));
        assert_eq!(r.cct_k, None);
        assert_eq!(r.cri_ra_min, None);
    }

    #[test]
    fn explicit_cct_fallback() {
        let r = mine_variant_name("LED panel 4000K 30W");
        assert_eq!(r.cct_k, Some(4000));
        // No DIN code so cri_ra_min stays absent.
        assert_eq!(r.cri_ra_min, None);
        assert_eq!(r.power_w, Some(30.0));
    }

    #[test]
    fn asymmetric_beam_picks_smaller_when_both_real_angles() {
        let r = mine_variant_name("Spot 10x60° 20W");
        assert_eq!(r.beam_deg, Some(10.0));
    }

    #[test]
    fn count_times_beam_uses_the_angle_not_the_count() {
        // "2x42°" — two emitters with 42° beam. Without this rule,
        // we'd pick `2.0` which is not a real beam angle.
        let r = mine_variant_name("Spot 2x42° 15W");
        assert_eq!(r.beam_deg, Some(42.0));
    }

    #[test]
    fn keywords_include_canonical_forms() {
        let r = mine_variant_name("Light 22W 1500lm IP65 4000K");
        let kw: Vec<&str> = r.keywords.iter().map(|s| s.as_str()).collect();
        assert!(kw.contains(&"22W"), "missing 22W in {kw:?}");
        assert!(kw.contains(&"1500lm"), "missing 1500lm in {kw:?}");
        assert!(kw.contains(&"IP65"), "missing IP65 in {kw:?}");
        assert!(kw.contains(&"4000K"), "missing 4000K in {kw:?}");
    }

    #[test]
    fn applications_mine_office_from_name() {
        let r = mine_variant_name("LedFlex Office Pendant 38W 4000K");
        assert!(
            !r.applications.is_empty(),
            "expected at least one applications match"
        );
        // Any office-related ApplicationId is fine for this assertion;
        // the canonical string must mention "Office".
        let canonical_hit = r.applications.iter().any(|id| {
            gldf_search_schema::enums::application_str(*id)
                .map(|s| s.contains("Office"))
                .unwrap_or(false)
        });
        assert!(canonical_hit, "expected an Office-family application id");
    }

    #[test]
    fn applications_mine_streets_from_name() {
        // Probes include simple singular stems, so "Street" in a
        // product slug matches the canonical "Streets" leaf via the
        // `simple_singular` helper.
        let r = mine_variant_name("UrbanPro Street LED 80W 5000K");
        let canonical_hit = r.applications.iter().any(|id| {
            gldf_search_schema::enums::application_str(*id)
                .map(|s| s.contains("Streets"))
                .unwrap_or(false)
        });
        assert!(canonical_hit, "expected a Streets-family application id");
    }

    #[test]
    fn applications_no_match_on_generic_names() {
        // Manufacturer product slug with no application term in it.
        let r = mine_variant_name("BARIS 52 LED Z 1143mm 1900lm 840 IP44 16W");
        assert!(
            r.applications.is_empty(),
            "unexpected matches: {:?}",
            r.applications
        );
    }

    #[test]
    fn applications_skip_short_and_stopword_probes() {
        // Probes shorter than 4 chars and stopword leaves are dropped.
        // The token "areas" appears in many leaves but is a stopword.
        let probes = application_leaves_lower();
        assert!(!probes.iter().any(|(p, _)| p == "areas"));
        assert!(!probes.iter().any(|(p, _)| p.len() < 4));
    }
}
