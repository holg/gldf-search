//! URL ↔ [`Filter`] codec.
//!
//! Encodes the active filter as a list of `(key, value)` query-string
//! pairs, suitable for `?text=led&app=10,12&flux=1000-5000`. The codec
//! is symmetric: [`Filter::to_query_pairs`] / [`Filter::from_query`].
//!
//! Design choices:
//!
//! - **One param per Filter field.** Human-readable, hand-editable.
//!   Schema-resilient: an unknown key on the receiving side is just
//!   ignored, so adding a new Filter field doesn't break old links.
//! - **Comma-separated lists.** Standard convention. Empty list = no
//!   key emitted. Lists never contain commas in practice (we
//!   url-percent-encode anyway).
//! - **Ranges as `lo-hi`.** Both ends always present (no open
//!   ranges). Negative numbers aren't possible for any of our axes,
//!   so the `-` separator is unambiguous.
//! - **Stable key names.** The wire format is the public API for
//!   shared/bookmarked links; we don't rename keys lightly. See the
//!   `KEY_*` constants below for the canonical mapping.

use core::ops::RangeInclusive;

use compact_str::CompactString;
use smallvec::SmallVec;

use crate::filter::{Filter, Locale};

// Canonical short keys. Kept short because URLs get long when many
// filters are active. Changing any of these breaks existing
// shared/bookmarked links.
const KEY_TEXT: &str = "text";
const KEY_LOCALE: &str = "locale";
const KEY_MANUFACTURER: &str = "mfr";
const KEY_APPLICATIONS: &str = "app";
const KEY_LABELS: &str = "lbl";
const KEY_ADJUSTABILITY: &str = "adj";
const KEY_IP_CODE: &str = "ip";
const KEY_SAFETY_CLASS: &str = "sc";
const KEY_IK_RATING: &str = "ik";
const KEY_PRODUCT_FORM: &str = "pf";
const KEY_FLUX: &str = "flux";
const KEY_POWER: &str = "pw";
const KEY_EFFICACY: &str = "eff";
const KEY_CCT: &str = "cct";
const KEY_CRI_MIN: &str = "cri";
const KEY_BEAM: &str = "beam";
const KEY_DEPTH: &str = "depth";
const KEY_MOUNTING_PLACE: &str = "mp";
const KEY_MOUNTING_TYPE: &str = "mt";
const KEY_LIGHT_DISTRIBUTION: &str = "ld";
const KEY_HAS_PHOTOMETRY: &str = "phot";
const KEY_HAS_3D: &str = "m3d";
const KEY_HAS_EMERGENCY: &str = "em";
const KEY_LAMP_TYPE: &str = "lt";
const KEY_CONTROL_GEAR: &str = "cg";
const KEY_EMERGENCY_TYPE: &str = "et";

impl Filter {
    /// Emit the active filter as `(key, value)` pairs ready for
    /// `?key=value&...` serialisation. Default-valued fields are
    /// omitted, so an empty filter produces an empty `Vec`.
    ///
    /// Values are *not* URL-encoded — the caller is expected to
    /// pass each value through their preferred encoder
    /// (`urlencoding::encode` for native, `js_sys::encodeURIComponent`
    /// for wasm, or just hand them to whichever router API does the
    /// encoding).
    pub fn to_query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();

        if let Some(t) = &self.text {
            if !t.is_empty() {
                out.push((KEY_TEXT, t.to_string()));
            }
        }
        if let Some(loc) = self.locale {
            out.push((KEY_LOCALE, loc.as_str().to_string()));
        }
        if let Some(list) = &self.manufacturer {
            if !list.is_empty() {
                out.push((KEY_MANUFACTURER, join_strings(list)));
            }
        }
        push_u8_list(&mut out, KEY_APPLICATIONS, &self.applications);
        push_u8_list(&mut out, KEY_LABELS, &self.labels);
        push_u8_list(&mut out, KEY_ADJUSTABILITY, &self.adjustability);
        push_u8_list(&mut out, KEY_IP_CODE, &self.ip_code);
        push_u8_list(&mut out, KEY_SAFETY_CLASS, &self.safety_class);
        push_u8_list(&mut out, KEY_IK_RATING, &self.ik_rating);
        push_u8_list(&mut out, KEY_PRODUCT_FORM, &self.product_form);

        if let Some(r) = &self.flux_lm {
            out.push((KEY_FLUX, fmt_range_f32(r)));
        }
        if let Some(r) = &self.power_w {
            out.push((KEY_POWER, fmt_range_f32(r)));
        }
        if let Some(r) = &self.efficacy {
            out.push((KEY_EFFICACY, fmt_range_f32(r)));
        }
        if let Some(r) = &self.cct_k {
            out.push((KEY_CCT, format!("{}-{}", r.start(), r.end())));
        }
        if let Some(c) = self.cri_min {
            out.push((KEY_CRI_MIN, c.to_string()));
        }
        if let Some(r) = &self.beam_deg {
            out.push((KEY_BEAM, fmt_range_f32(r)));
        }
        if let Some(r) = &self.recessed_depth_mm {
            out.push((KEY_DEPTH, format!("{}-{}", r.start(), r.end())));
        }

        if !self.mounting_place.is_empty() {
            out.push((
                KEY_MOUNTING_PLACE,
                self.mounting_place
                    .iter()
                    .map(|m| format!("{m:?}"))
                    .collect::<Vec<_>>()
                    .join(","),
            ));
        }
        if !self.mounting_type.is_empty() {
            out.push((
                KEY_MOUNTING_TYPE,
                self.mounting_type
                    .iter()
                    .map(|m| format!("{m:?}"))
                    .collect::<Vec<_>>()
                    .join(","),
            ));
        }
        push_u8_list(&mut out, KEY_LIGHT_DISTRIBUTION, &self.light_distribution);

        if let Some(b) = self.has_photometry {
            out.push((KEY_HAS_PHOTOMETRY, bool_str(b).to_string()));
        }
        if let Some(b) = self.has_3d {
            out.push((KEY_HAS_3D, bool_str(b).to_string()));
        }
        if let Some(b) = self.has_emergency_lighting {
            out.push((KEY_HAS_EMERGENCY, bool_str(b).to_string()));
        }

        push_u8_list(&mut out, KEY_LAMP_TYPE, &self.lamp_type);
        push_u8_list(&mut out, KEY_CONTROL_GEAR, &self.control_gear_interfaces);
        push_u8_list(&mut out, KEY_EMERGENCY_TYPE, &self.emergency_lighting_type);

        out
    }

    /// Build a [`Filter`] from a sequence of `(key, value)` query
    /// pairs. Unknown keys are silently ignored. Malformed values for
    /// a known key cause that field to fall back to its default —
    /// **the rest of the filter still applies**. Lenient on purpose:
    /// a copy-pasted URL with one stale-format value shouldn't blow
    /// up the whole search.
    pub fn from_query<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut f = Filter::default();
        for (k, v) in pairs {
            apply_pair(&mut f, k.as_ref(), v.as_ref());
        }
        f
    }
}

fn apply_pair(f: &mut Filter, key: &str, value: &str) {
    match key {
        KEY_TEXT => {
            if !value.is_empty() {
                f.text = Some(CompactString::from(value));
            }
        }
        KEY_LOCALE => {
            f.locale = Locale::from_str(value);
        }
        KEY_MANUFACTURER => {
            let mut list: SmallVec<[CompactString; 4]> = SmallVec::new();
            for s in value.split(',').filter(|s| !s.is_empty()) {
                list.push(CompactString::from(s));
            }
            if !list.is_empty() {
                f.manufacturer = Some(list);
            }
        }
        KEY_APPLICATIONS => parse_u8_list(value, &mut f.applications),
        KEY_LABELS => parse_u8_list(value, &mut f.labels),
        KEY_ADJUSTABILITY => parse_u8_list(value, &mut f.adjustability),
        KEY_IP_CODE => parse_u8_list(value, &mut f.ip_code),
        KEY_SAFETY_CLASS => parse_u8_list(value, &mut f.safety_class),
        KEY_IK_RATING => parse_u8_list(value, &mut f.ik_rating),
        KEY_PRODUCT_FORM => parse_u8_list(value, &mut f.product_form),
        KEY_FLUX => {
            f.flux_lm = parse_range_f32(value);
        }
        KEY_POWER => {
            f.power_w = parse_range_f32(value);
        }
        KEY_EFFICACY => {
            f.efficacy = parse_range_f32(value);
        }
        KEY_CCT => {
            f.cct_k = parse_range_u16(value);
        }
        KEY_CRI_MIN => {
            f.cri_min = value.parse().ok();
        }
        KEY_BEAM => {
            f.beam_deg = parse_range_f32(value);
        }
        KEY_DEPTH => {
            f.recessed_depth_mm = parse_range_u32(value);
        }
        KEY_MOUNTING_PLACE => {
            use crate::doc::MountingPlace;
            for s in value.split(',').filter(|s| !s.is_empty()) {
                let p = match s {
                    "Ceiling" => MountingPlace::Ceiling,
                    "Wall" => MountingPlace::Wall,
                    "WorkingPlane" => MountingPlace::WorkingPlane,
                    "Ground" => MountingPlace::Ground,
                    "Unknown" => MountingPlace::Unknown,
                    _ => continue,
                };
                if !f.mounting_place.contains(&p) {
                    f.mounting_place.push(p);
                }
            }
        }
        KEY_MOUNTING_TYPE => {
            use crate::doc::MountingType;
            for s in value.split(',').filter(|s| !s.is_empty()) {
                let m = match s {
                    "Recessed" => MountingType::Recessed,
                    "SurfaceMounted" => MountingType::SurfaceMounted,
                    "Pendant" => MountingType::Pendant,
                    "FreeStanding" => MountingType::FreeStanding,
                    "PoleTop" => MountingType::PoleTop,
                    "PoleIntegrated" => MountingType::PoleIntegrated,
                    "Unknown" => MountingType::Unknown,
                    _ => continue,
                };
                if !f.mounting_type.contains(&m) {
                    f.mounting_type.push(m);
                }
            }
        }
        KEY_LIGHT_DISTRIBUTION => parse_u8_list(value, &mut f.light_distribution),
        KEY_HAS_PHOTOMETRY => {
            f.has_photometry = parse_bool(value);
        }
        KEY_HAS_3D => {
            f.has_3d = parse_bool(value);
        }
        KEY_HAS_EMERGENCY => {
            f.has_emergency_lighting = parse_bool(value);
        }
        KEY_LAMP_TYPE => parse_u8_list(value, &mut f.lamp_type),
        KEY_CONTROL_GEAR => parse_u8_list(value, &mut f.control_gear_interfaces),
        KEY_EMERGENCY_TYPE => parse_u8_list(value, &mut f.emergency_lighting_type),
        _ => {
            // Unknown key — ignore for forwards-compatibility.
        }
    }
}

fn push_u8_list<const N: usize>(
    out: &mut Vec<(&'static str, String)>,
    key: &'static str,
    list: &SmallVec<[u8; N]>,
) {
    if list.is_empty() {
        return;
    }
    let s = list
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    out.push((key, s));
}

fn parse_u8_list<const N: usize>(value: &str, target: &mut SmallVec<[u8; N]>) {
    for s in value.split(',').filter(|s| !s.is_empty()) {
        if let Ok(n) = s.parse::<u8>() {
            if !target.contains(&n) {
                target.push(n);
            }
        }
    }
}

fn fmt_range_f32(r: &RangeInclusive<f32>) -> String {
    // Drop trailing `.0` for nicer URLs. `1500.0..=2000.0` →
    // `"1500-2000"`; `0.5..=1.5` → `"0.5-1.5"`.
    let lo = fmt_f32(*r.start());
    let hi = fmt_f32(*r.end());
    format!("{lo}-{hi}")
}

fn fmt_f32(v: f32) -> String {
    if (v - v.round()).abs() < 1e-3 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v}")
    }
}

fn parse_range_f32(value: &str) -> Option<RangeInclusive<f32>> {
    let (lo, hi) = split_range(value)?;
    let lo: f32 = lo.parse().ok()?;
    let hi: f32 = hi.parse().ok()?;
    if !lo.is_finite() || !hi.is_finite() || lo > hi {
        return None;
    }
    Some(lo..=hi)
}

fn parse_range_u16(value: &str) -> Option<RangeInclusive<u16>> {
    let (lo, hi) = split_range(value)?;
    let lo: u16 = lo.parse().ok()?;
    let hi: u16 = hi.parse().ok()?;
    if lo > hi {
        return None;
    }
    Some(lo..=hi)
}

fn parse_range_u32(value: &str) -> Option<RangeInclusive<u32>> {
    let (lo, hi) = split_range(value)?;
    let lo: u32 = lo.parse().ok()?;
    let hi: u32 = hi.parse().ok()?;
    if lo > hi {
        return None;
    }
    Some(lo..=hi)
}

/// Split a `"lo-hi"` string at the first `-` after position 0
/// (negative leading sign isn't possible for any of our axes, so
/// the first `-` is always the separator).
fn split_range(value: &str) -> Option<(&str, &str)> {
    let dash = value.find('-')?;
    if dash == 0 {
        return None;
    }
    Some((&value[..dash], &value[dash + 1..]))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn bool_str(b: bool) -> &'static str {
    if b {
        "1"
    } else {
        "0"
    }
}

fn join_strings(list: &SmallVec<[CompactString; 4]>) -> String {
    list.iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{MountingPlace, MountingType};
    use smallvec::smallvec;

    #[test]
    fn empty_filter_emits_no_pairs() {
        assert!(Filter::default().to_query_pairs().is_empty());
    }

    #[test]
    fn text_round_trip() {
        let f = Filter {
            text: Some(CompactString::from("led office")),
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        assert_eq!(pairs, vec![("text", "led office".to_string())]);

        let back = Filter::from_query(pairs);
        assert_eq!(back.text.as_deref(), Some("led office"));
    }

    #[test]
    fn flux_range_round_trip() {
        let f = Filter {
            flux_lm: Some(1500.0..=2000.0),
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        assert_eq!(pairs, vec![("flux", "1500-2000".to_string())]);

        let back = Filter::from_query(pairs);
        assert_eq!(back.flux_lm, Some(1500.0..=2000.0));
    }

    #[test]
    fn applications_round_trip() {
        let f = Filter {
            applications: smallvec![10, 12, 14],
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        assert_eq!(pairs, vec![("app", "10,12,14".to_string())]);

        let back = Filter::from_query(pairs);
        assert_eq!(back.applications.as_slice(), &[10, 12, 14]);
    }

    #[test]
    fn mounting_place_round_trip() {
        let f = Filter {
            mounting_place: smallvec![MountingPlace::Ceiling, MountingPlace::Wall],
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        assert_eq!(pairs, vec![("mp", "Ceiling,Wall".to_string())]);

        let back = Filter::from_query(pairs);
        assert!(back.mounting_place.contains(&MountingPlace::Ceiling));
        assert!(back.mounting_place.contains(&MountingPlace::Wall));
    }

    #[test]
    fn mounting_type_round_trip() {
        let f = Filter {
            mounting_type: smallvec![MountingType::Recessed],
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        assert_eq!(pairs, vec![("mt", "Recessed".to_string())]);

        let back = Filter::from_query(pairs);
        assert!(back.mounting_type.contains(&MountingType::Recessed));
    }

    #[test]
    fn tristates_round_trip() {
        let f = Filter {
            has_photometry: Some(true),
            has_3d: Some(false),
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        let mut by_key: std::collections::HashMap<&str, String> = pairs.into_iter().collect();
        assert_eq!(by_key.remove("phot"), Some("1".to_string()));
        assert_eq!(by_key.remove("m3d"), Some("0".to_string()));
        assert!(by_key.is_empty());

        let back = Filter::from_query(vec![("phot", "1"), ("m3d", "0")]);
        assert_eq!(back.has_photometry, Some(true));
        assert_eq!(back.has_3d, Some(false));
    }

    #[test]
    fn cri_min_round_trip() {
        let f = Filter {
            cri_min: Some(80),
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        assert_eq!(pairs, vec![("cri", "80".to_string())]);

        let back = Filter::from_query(pairs);
        assert_eq!(back.cri_min, Some(80));
    }

    #[test]
    fn manufacturer_with_special_chars_round_trip() {
        let f = Filter {
            manufacturer: Some(smallvec![
                CompactString::from("ERCO"),
                CompactString::from("Zumtobel")
            ]),
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        assert_eq!(pairs, vec![("mfr", "ERCO,Zumtobel".to_string())]);

        let back = Filter::from_query(pairs);
        let mfr = back.manufacturer.expect("manufacturer set");
        assert_eq!(
            mfr.as_slice(),
            &[CompactString::from("ERCO"), CompactString::from("Zumtobel"),]
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let back = Filter::from_query(vec![
            ("text", "hi"),
            ("future_field", "whatever"),
            ("flux", "1000-2000"),
        ]);
        assert_eq!(back.text.as_deref(), Some("hi"));
        assert_eq!(back.flux_lm, Some(1000.0..=2000.0));
    }

    #[test]
    fn malformed_range_falls_back_to_none() {
        let back = Filter::from_query(vec![("text", "led"), ("flux", "not-a-range")]);
        assert_eq!(back.text.as_deref(), Some("led"));
        assert_eq!(back.flux_lm, None);
    }

    #[test]
    fn full_round_trip_keeps_filter_equal() {
        let f = Filter {
            text: Some(CompactString::from("led")),
            manufacturer: Some(smallvec![CompactString::from("ERCO")]),
            applications: smallvec![10, 12],
            flux_lm: Some(1000.0..=5000.0),
            cct_k: Some(3000..=4000),
            cri_min: Some(80),
            beam_deg: Some(30.0..=60.0),
            has_photometry: Some(true),
            mounting_place: smallvec![MountingPlace::Ceiling],
            ..Default::default()
        };
        let pairs = f.to_query_pairs();
        let back = Filter::from_query(pairs);
        assert_eq!(f, back);
    }
}
