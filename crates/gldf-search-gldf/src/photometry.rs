//! Parse an LDT (EULUMDAT) file string into [`PhotometryStats`].
//!
//! This is the **authoritative** photometry path. When an LDC file is
//! present in a GLDF, its parsed values win over the mining-derived
//! ones (which are still useful for the keyword pool and as a fallback
//! when no LDC is reachable).
//!
//! Pure function: no I/O. The caller resolves the LDC bytes via
//! `gldf_rs::GldfProduct::get_ldc_by_id(file_id)` and hands the UTF-8
//! string in here.
//!
//! Returns `None` on parse failure — real-world LDTs are not always
//! well-formed. The extractor logs an `ExtractWarning` in that case.

use eulumdat::{Eulumdat, PhotometricCalculations, PhotometricSummary, Symmetry};
use gldf_search_schema::doc::{PhotometricSymmetryHint, PhotometryStats};
use gldf_search_schema::enums::{light_distribution_from_str, LightDistributionId};

/// Parse an LDT string and project it into the schema's [`PhotometryStats`].
///
/// Conventions:
/// - Flux + power come from the first lamp set (corpus convention —
///   single-emitter LDTs are dominant). Multi-emitter LDTs sum across
///   sets which would be a follow-up.
/// - Efficacy is computed once: `flux / power` when both are positive.
/// - CCT and CRI come from the lamp set's `color_appearance` and
///   `color_rendering_group` strings via best-effort parsing. The DIN
///   3-digit code (e.g. `"830"` → 80 CRI, 3000 K) wins when present;
///   bare Kelvin strings (`"4000"`, `"4000K"`) are a fallback.
/// - Beam angle uses the CIE half-peak definition (full angle).
/// - Symmetry maps from EULUMDAT's `Isym` enum.
pub fn parse_ldt_to_stats(ldt_str: &str) -> Option<PhotometryStats> {
    let ldt = Eulumdat::parse(ldt_str).ok()?;

    let lamp_set = ldt.lamp_sets.first();

    // Flux / power / efficacy. `total_luminous_flux` of 0 is rare but
    // legal in legacy LDTs — preserve it as Some(0.0) instead of None
    // so we don't silently confuse "we know it's zero" with "we don't
    // know". Conversely we treat a missing lamp set as None.
    let flux_lm = lamp_set.map(|l| l.total_luminous_flux as f32);
    let power_w = lamp_set.map(|l| l.wattage_with_ballast as f32);
    let efficacy_lm_w = match (flux_lm, power_w) {
        (Some(f), Some(p)) if p > 0.0 => Some(f / p),
        _ => None,
    };

    let (cct_k, cri_ra) = lamp_set
        .map(|l| parse_color_strings(&l.color_appearance, &l.color_rendering_group))
        .unwrap_or((None, None));

    // Beam angle: only meaningful when we have intensity data.
    let beam_deg = if !ldt.intensities.is_empty() && !ldt.g_angles.is_empty() {
        let v = PhotometricCalculations::beam_angle(&ldt);
        if v.is_finite() && v > 0.0 {
            Some(v as f32)
        } else {
            None
        }
    } else {
        None
    };

    // Field angle (1/10 peak).
    let field_deg = if !ldt.intensities.is_empty() && !ldt.g_angles.is_empty() {
        let v = PhotometricCalculations::field_angle(&ldt);
        if v.is_finite() && v > 0.0 {
            Some(v as f32)
        } else {
            None
        }
    } else {
        None
    };

    // DLOR — downward flux fraction is a percentage in the LDT.
    let dlor = if ldt.downward_flux_fraction > 0.0 {
        Some((ldt.downward_flux_fraction / 100.0) as f32)
    } else {
        None
    };

    // ULOR derived. Total LOR is the `light_output_ratio` percentage;
    // upward = total - downward. Both are percentages in the LDT.
    let ulor = if ldt.light_output_ratio > 0.0 {
        let total = ldt.light_output_ratio / 100.0;
        let down = ldt.downward_flux_fraction / 100.0;
        let up = (total - down).max(0.0);
        Some(up as f32)
    } else {
        None
    };

    let symmetry = match ldt.symmetry {
        Symmetry::None => PhotometricSymmetryHint::Asymmetric,
        Symmetry::VerticalAxis => PhotometricSymmetryHint::Rotational,
        Symmetry::PlaneC0C180 | Symmetry::PlaneC90C270 => PhotometricSymmetryHint::Axial,
        Symmetry::BothPlanes => PhotometricSymmetryHint::Rotational,
    };

    // Derive `light_distribution` from the LDC. The XSD-declared
    // value (if any) wins over this in `extract::merge_photometry`'s
    // `.or()` chain, so we set the LDT-derived value here without
    // worrying about overriding a real declaration.
    let light_distribution = classify_light_distribution(&ldt);

    Some(PhotometryStats {
        flux_lm,
        power_w,
        efficacy_lm_w,
        cct_k,
        cri_ra,
        r9: None,
        beam_deg,
        field_deg,
        ulor,
        dlor,
        symmetry,
        light_distribution,
    })
}

/// Classify a parsed LDT into one of the canonical GLDF
/// `<LightDistribution>` enum values. Best-effort: returns `None`
/// when the LDT lacks usable intensity data, or when no rule fires
/// (e.g. total LOR ≈ 0).
///
/// Rule order (first match wins). Calibration draws on Relux/Dialux
/// conventions plus the inputs `eulumdat::PhotometricSummary` makes
/// available:
///
/// - **Indirect** — ULOR ≥ 80%. Most of the light goes up.
/// - **Direct indirect** — both DLOR ≥ 20% and ULOR ≥ 20%. Office
///   pendants, hybrid linears.
/// - **Diffuse full spherical** — DLOR roughly equal to ULOR (within
///   20 pp) AND beam very wide. Frosted globes.
/// - **Diffuse half spherical** — DLOR ≥ 80% AND beam ≥ 120° (the
///   light fills the lower hemisphere without a clear peak).
/// - **Direct** — DLOR ≥ 90% AND ULOR ≤ 5% AND symmetry is None.
/// - The "Laterally symmetrical narrow / medium / wide" axis applies
///   to direct-dominant luminaires with symmetry. We split by beam
///   angle: narrow ≤ 30°, medium ≤ 60°, wide otherwise.
/// - **Symmetric about 0-180 plane** / **Symmetric about 90-270
///   plane** / **Symmetrical in each quadrant** — derived from the
///   LDT's `Isym` field when symmetry is a single plane / both
///   planes respectively, AND the direct-dominant condition holds.
/// - **Asymmetrical** — direct-dominant with `Symmetry::None`.
/// - **Other** — fallback for the long tail when no rule above fires.
///
/// All thresholds are tunable; calibrate against the full corpus
/// before committing to anything aggressive.
pub fn classify_light_distribution(ldt: &Eulumdat) -> Option<LightDistributionId> {
    // No intensity data → no classification possible.
    if ldt.intensities.is_empty() || ldt.g_angles.is_empty() {
        return None;
    }

    let summary = PhotometricSummary::from_eulumdat(ldt);

    // Total light output too low to classify confidently — we'd be
    // staring at numerical noise.
    if summary.lor < 5.0 {
        return None;
    }

    let dlor = summary.dlor; // 0..=100
    let ulor = summary.ulor; // 0..=100
    let beam = summary.beam_angle; // degrees, full angle at 50% max

    // ── Direction-dominant axis ──────────────────────────────────────

    if ulor >= 80.0 {
        return canon("Indirect");
    }

    let both_hemispheres = dlor >= 20.0 && ulor >= 20.0;
    if both_hemispheres {
        // Could be either "Direct indirect" or a true "Diffuse full
        // spherical" (luminous sphere). Use a flat-distribution test
        // for diffuse-full.
        if (dlor - ulor).abs() <= 20.0 && beam.is_finite() && beam >= 150.0 {
            return canon("Diffuse full spherical");
        }
        return canon("Direct indirect");
    }

    // Direct-dominant from here on.
    if dlor < 50.0 {
        // Neither clearly indirect nor clearly direct — bail out.
        return canon("Other");
    }

    // Diffuse half-spherical: lots of downward light, no clear peak.
    if dlor >= 80.0 && beam.is_finite() && beam >= 120.0 {
        return canon("Diffuse half spherical");
    }

    // ── Symmetry → narrow/medium/wide refinement ─────────────────────
    //
    // The XSD's "Laterally symmetrical *" implies rotational symmetry
    // (Isym = 1 in the LDT) — that's a single C-plane sweep. The
    // "Symmetric about X plane" entries map to the half-symmetry
    // cases (Isym = 2 or 3). "Symmetrical in each quadrant" is both
    // planes (Isym = 4).
    let class_by_beam = beam_class_label(beam);
    match ldt.symmetry {
        Symmetry::VerticalAxis => canon(class_by_beam.lateral_str()),
        Symmetry::PlaneC0C180 => canon("Symmetric about 0-180 plane"),
        Symmetry::PlaneC90C270 => canon("Symmetric about 90-270 plane"),
        Symmetry::BothPlanes => canon("Symmetrical in each quadrant"),
        Symmetry::None => {
            // True asymmetric distribution.
            if dlor >= 90.0 && ulor <= 5.0 && beam.is_finite() && beam <= 30.0 {
                // Very tight beam, no symmetry — likely an accent /
                // spotlight without C-plane symmetry annotation.
                canon("Direct")
            } else {
                canon("Asymmetrical")
            }
        }
    }
}

/// Beam-angle bucketing for the "Laterally symmetrical *" axis.
enum BeamClass {
    Narrow,
    Medium,
    Wide,
}

impl BeamClass {
    fn lateral_str(self) -> &'static str {
        match self {
            BeamClass::Narrow => "Laterally symmetrical narrow",
            BeamClass::Medium => "Laterally symmetrical medium",
            BeamClass::Wide => "Laterally symmetrical wide",
        }
    }
}

fn beam_class_label(beam: f64) -> BeamClass {
    if !beam.is_finite() || beam <= 0.0 {
        // Degenerate beam → call it medium so we don't pretend to know.
        return BeamClass::Medium;
    }
    if beam <= 30.0 {
        BeamClass::Narrow
    } else if beam <= 60.0 {
        BeamClass::Medium
    } else {
        BeamClass::Wide
    }
}

/// Resolve a canonical GLDF light-distribution string to its index.
/// Returns `None` if the string isn't canonical — should not happen
/// for any literal in this module, but defending against a typo here
/// is cheaper than chasing one in the corpus.
fn canon(s: &str) -> Option<LightDistributionId> {
    light_distribution_from_str(s)
}

/// Parse an LDT color-appearance / color-rendering-group pair into
/// `(cct_k, cri_ra)`. Best-effort; either value may come back as
/// `None`.
///
/// Recognised forms (real corpus examples):
/// - DIN 3-digit code in either field: `"830"`, `"/940"`, `"930K"` →
///   `cri ≥ 80`, `cct ≈ 3000`. First digit is CRI tens (≥ 8 = Ra80+,
///   ≥ 9 = Ra90+); last two are CCT in hundreds (30 = 3000K).
/// - Explicit Kelvin: `"4000"`, `"4000K"`, `"3000 K"` → `cct = 4000`.
/// - Explicit Ra: `"Ra80"`, `"80"` in the `color_rendering_group`
///   field → `cri = 80`.
fn parse_color_strings(appearance: &str, rendering: &str) -> (Option<u16>, Option<u8>) {
    // DIN code first — most common in the corpus and gives us both
    // values at once.
    for s in [appearance, rendering] {
        if let Some((cct, cri)) = parse_din_code(s) {
            return (Some(cct), Some(cri));
        }
    }
    let cct = parse_explicit_kelvin(appearance).or_else(|| parse_explicit_kelvin(rendering));
    let cri = parse_explicit_ra(rendering).or_else(|| parse_explicit_ra(appearance));
    (cct, cri)
}

fn parse_din_code(s: &str) -> Option<(u16, u8)> {
    // Find a 3-digit run starting with 8 or 9, whose last two digits
    // are in the lighting-CCT range (15..=99 = 1500..=9900 K — covers
    // the practical span). Surrounded by non-digit chars or string
    // boundaries.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Slide to next ASCII digit boundary.
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let end = i;
        if end - start == 3 {
            let first = bytes[start] - b'0';
            if first >= 8 {
                let tail: u16 =
                    (bytes[start + 1] - b'0') as u16 * 10 + (bytes[start + 2] - b'0') as u16;
                if (15..=99).contains(&tail) {
                    return Some((tail * 100, first * 10));
                }
            }
        }
    }
    None
}

fn parse_explicit_kelvin(s: &str) -> Option<u16> {
    // Find a 4-digit run (optionally followed by `K`) in lighting
    // range 1500..=9999.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let end = i;
        if end - start == 4 {
            let n: u16 = s[start..end].parse().ok()?;
            if (1500..=9999).contains(&n) {
                return Some(n);
            }
        }
    }
    None
}

fn parse_explicit_ra(s: &str) -> Option<u8> {
    // Look for "Ra" + digits, or a bare 2-digit value in the CRI range
    // (50..=100). Bare digit is conservative because lots of CRI-group
    // fields are abbreviations like "1A".
    let lower = s.to_lowercase();
    if let Some(pos) = lower.find("ra") {
        let rest = &lower[pos + 2..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u8>() {
            if (50..=100).contains(&n) {
                return Some(n);
            }
        }
    }
    None
}

// ── Sanity caps for parser/mining outliers ────────────────────────────
//
// Real corpus files occasionally carry physically impossible values: a
// 10 000 lm/W efficacy, a 1 045° beam angle, an 800 000 mm "recessed
// depth". Causes vary — LDT files with wrong units, a stray digit in a
// variant name the mining regex picked up, a mounting block that put
// the *height* of the fixture into the recessed-depth field. We never
// want these in the cache: they skew the dual-slider rails to nonsense
// ranges that hide the real population.
//
// Strategy: clamp at extraction time. If a value parses but lands
// outside a physical envelope, drop it back to `None`. The user keeps
// the GLDF in the corpus (it might still match other facets), the
// slider just doesn't see the outlier.
//
// Envelopes are conservative — wider than any real luminaire is
// expected to be, narrower than the parser-bug values we've seen.

/// Maximum plausible luminous flux for a single luminaire (lm).
/// Stadium projectors hit ~300 000 lm; 1 000 000 is well above that
/// and below the "this can't be right" threshold.
pub const FLUX_MAX_LM: f32 = 1_000_000.0;
/// Maximum plausible electrical power (W).
pub const POWER_MAX_W: f32 = 10_000.0;
/// Maximum plausible luminous efficacy (lm/W). Best LED packages
/// reach ~220 lm/W; 300 leaves headroom but is well below the
/// 10 000 lm/W parse-bug case we've seen.
pub const EFFICACY_MAX_LM_W: f32 = 300.0;
/// Maximum plausible half-peak beam angle (degrees). Beam-angle is
/// an angle in `[0, 360]`; some LDC files report 1045° (probably a
/// degenerate intensity table). Clamp at 360.
pub const BEAM_MAX_DEG: f32 = 360.0;
/// Maximum plausible recessed installation depth (mm). One metre is
/// already deeper than any architectural cavity we'd expect.
pub const DEPTH_MAX_MM: u32 = 1_000;

/// Clamp helper: keep `value` only when finite, non-negative, and at
/// most `cap`. Returns `None` for `NaN`, `INF`, negatives, or values
/// above the cap.
fn keep_f32(value: Option<f32>, cap: f32) -> Option<f32> {
    value.filter(|v| v.is_finite() && *v >= 0.0 && *v <= cap)
}

/// Apply sanity caps to a [`PhotometryStats`] in-place. Implausible
/// outliers become `None`; efficacy is also recomputed from the
/// (possibly clamped) flux + power so the cached value stays
/// internally consistent.
///
/// Called by `extract.rs` right after `merge_photometry`.
pub fn sanitize_photometry(p: &mut PhotometryStats) {
    p.flux_lm = keep_f32(p.flux_lm, FLUX_MAX_LM);
    p.power_w = keep_f32(p.power_w, POWER_MAX_W);
    p.beam_deg = keep_f32(p.beam_deg, BEAM_MAX_DEG);
    p.field_deg = keep_f32(p.field_deg, BEAM_MAX_DEG);

    // Recompute efficacy from the sanitised flux + power so an
    // outlier doesn't sneak through via the LDT branch. If both
    // halves are present we recompute; otherwise fall back to the
    // existing value (which is then itself capped).
    p.efficacy_lm_w = match (p.flux_lm, p.power_w) {
        (Some(f), Some(pw)) if pw > 0.0 => {
            let v = f / pw;
            if v.is_finite() && v <= EFFICACY_MAX_LM_W {
                Some(v)
            } else {
                None
            }
        }
        _ => keep_f32(p.efficacy_lm_w, EFFICACY_MAX_LM_W),
    };
}

/// Apply the recessed-depth cap. Caller passes the raw mounting
/// value; returns `None` when the parsed depth is unrealistic, else
/// the value as-is.
pub fn sanitize_recessed_depth(depth: Option<u32>) -> Option<u32> {
    depth.filter(|&d| d <= DEPTH_MAX_MM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn din_code_extracts_cct_and_cri() {
        assert_eq!(parse_din_code("830"), Some((3000, 80)));
        assert_eq!(parse_din_code("/940"), Some((4000, 90)));
        assert_eq!(parse_din_code("930K"), Some((3000, 90)));
        assert_eq!(parse_din_code("Ra 80, 4000K"), None); // 4000 is not DIN
    }

    #[test]
    fn din_code_rejects_low_first_digit() {
        // 720 isn't a DIN code (first digit < 8).
        assert_eq!(parse_din_code("720"), None);
    }

    #[test]
    fn din_code_rejects_out_of_range_cct() {
        // "899" → cct 9900 OK, but "812" → cct 1200 K = below the
        // lighting range floor.
        assert_eq!(parse_din_code("899"), Some((9900, 80)));
        assert_eq!(parse_din_code("812"), None);
    }

    #[test]
    fn explicit_kelvin_finds_4digit() {
        assert_eq!(parse_explicit_kelvin("4000K"), Some(4000));
        assert_eq!(parse_explicit_kelvin("3000"), Some(3000));
        assert_eq!(parse_explicit_kelvin("color 6500 K"), Some(6500));
        assert_eq!(parse_explicit_kelvin("xx 99 yy"), None);
    }

    #[test]
    fn explicit_ra_finds_after_ra_prefix() {
        assert_eq!(parse_explicit_ra("Ra80"), Some(80));
        assert_eq!(parse_explicit_ra("ra 90"), None); // we don't tolerate space
        assert_eq!(parse_explicit_ra("RA95"), Some(95));
        assert_eq!(parse_explicit_ra("foo"), None);
    }

    #[test]
    fn color_strings_prefer_din_over_kelvin() {
        // "830" in appearance, "Ra80" in rendering → DIN wins for both.
        assert_eq!(parse_color_strings("830", "Ra80"), (Some(3000), Some(80)));
        // No DIN, explicit Kelvin in appearance, Ra in rendering.
        assert_eq!(parse_color_strings("4000K", "Ra80"), (Some(4000), Some(80)));
        // Nothing.
        assert_eq!(parse_color_strings("LED", ""), (None, None));
    }

    #[test]
    fn malformed_ldt_returns_none() {
        assert!(parse_ldt_to_stats("not an LDT file").is_none());
        assert!(parse_ldt_to_stats("").is_none());
    }

    // The end-to-end LDT-string-to-PhotometryStats path is exercised
    // by step 8 of the implementation plan: running the CLI search
    // over the sample-100 corpus and verifying the
    // "has photometry: present" count rises substantially. Hand-built
    // LDT fixtures are brittle (the parser checks many field-shape
    // invariants); real corpus files are the right test bed.

    #[test]
    fn classifier_labels_are_canonical() {
        // Every string literal the classifier returns must round-trip
        // through `light_distribution_from_str`. This catches a typo
        // here before it produces silent classification failures
        // against the corpus.
        for s in [
            "Laterally symmetrical narrow",
            "Laterally symmetrical medium",
            "Laterally symmetrical wide",
            "Symmetrical in each quadrant",
            "Symmetric about 0-180 plane",
            "Symmetric about 90-270 plane",
            "Asymmetrical",
            "Asymmetrical flood",
            "Asymmetrical wide flood",
            "Diffuse half spherical",
            "Diffuse full spherical",
            "Direct",
            "Indirect",
            "Direct indirect",
            "Other",
        ] {
            assert!(
                canon(s).is_some(),
                "non-canonical light-distribution literal: {s:?}"
            );
        }
    }

    fn stats_with(
        flux: Option<f32>,
        power: Option<f32>,
        eff: Option<f32>,
        beam: Option<f32>,
    ) -> PhotometryStats {
        PhotometryStats {
            flux_lm: flux,
            power_w: power,
            efficacy_lm_w: eff,
            cct_k: None,
            cri_ra: None,
            r9: None,
            beam_deg: beam,
            field_deg: None,
            ulor: None,
            dlor: None,
            symmetry: PhotometricSymmetryHint::Unknown,
            light_distribution: None,
            ..Default::default()
        }
    }

    #[test]
    fn sanitize_drops_impossible_efficacy() {
        // Parser bug: flux 100 lm but power 0.01 W → 10 000 lm/W.
        let mut p = stats_with(Some(100.0), Some(0.01), Some(10_000.0), Some(30.0));
        sanitize_photometry(&mut p);
        // Flux + power are within range; efficacy recomputed and
        // capped above 300, so dropped.
        assert_eq!(p.flux_lm, Some(100.0));
        assert_eq!(p.power_w, Some(0.01));
        assert_eq!(p.efficacy_lm_w, None);
    }

    #[test]
    fn sanitize_drops_huge_beam_angle() {
        let mut p = stats_with(Some(2000.0), Some(20.0), Some(100.0), Some(1045.0));
        sanitize_photometry(&mut p);
        assert_eq!(p.beam_deg, None);
    }

    #[test]
    fn sanitize_keeps_normal_values() {
        let mut p = stats_with(Some(3000.0), Some(30.0), Some(100.0), Some(120.0));
        sanitize_photometry(&mut p);
        assert_eq!(p.flux_lm, Some(3000.0));
        assert_eq!(p.power_w, Some(30.0));
        // Efficacy recomputed from 3000/30 = 100.
        assert_eq!(p.efficacy_lm_w, Some(100.0));
        assert_eq!(p.beam_deg, Some(120.0));
    }

    #[test]
    fn sanitize_drops_nan_and_negative() {
        let mut p = stats_with(Some(f32::NAN), Some(-5.0), None, Some(f32::INFINITY));
        sanitize_photometry(&mut p);
        assert_eq!(p.flux_lm, None);
        assert_eq!(p.power_w, None);
        assert_eq!(p.beam_deg, None);
    }

    #[test]
    fn sanitize_caps_huge_recessed_depth() {
        // 800 000 mm = 800 m. Drop.
        assert_eq!(sanitize_recessed_depth(Some(800_000)), None);
        // 50 mm shallow recessed → keep.
        assert_eq!(sanitize_recessed_depth(Some(50)), Some(50));
        // None passes through.
        assert_eq!(sanitize_recessed_depth(None), None);
    }
}
