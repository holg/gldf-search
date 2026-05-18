//! Client-side polar / LDC rendering. Compiles only for the hydrate
//! target (wasm32) because eulumdat is gated behind the `hydrate`
//! feature in our Cargo.toml.
//!
//! The flow:
//!
//! 1. Server fn `fetch_ldt(doc_id)` reads the LDT text out of the
//!    source GLDF zip and ships it as a `String`.
//! 2. This module parses it via `eulumdat::Eulumdat::parse` and
//!    renders an SVG via `eulumdat::diagram::PolarDiagram::to_svg`.
//! 3. The HitRow inlines the resulting SVG.
//!
//! Why client-side: a full per-doc intensity matrix at typical
//! angular resolution costs ~30 KB per doc — too big for a 270k-doc
//! in-memory index on a 7.7 GB box. The browser does the parsing
//! per displayed luminaire, which is bounded by what the user
//! clicks.

#![cfg(target_arch = "wasm32")]

use eulumdat::diagram::{PolarDiagram, SvgTheme};
use eulumdat::Eulumdat;

/// Parse an LDT text and render a polar diagram SVG. Returns `None`
/// when parsing fails (which can happen on the long tail of corpus
/// files with malformed metadata — eulumdat's parser is strict).
pub fn render_polar_from_ldt(ldt: &str, size_px: f64) -> Option<String> {
    let parsed = Eulumdat::parse(ldt).ok()?;
    let polar = PolarDiagram::from_eulumdat(&parsed);
    let theme = polar_theme();
    Some(polar.to_svg(size_px, size_px, &theme))
}

/// Theme used for the inline hit-row polar diagram. Light surface,
/// blue C0–C180 curve, lighter C90–C270. Matches our accent colour
/// for consistency with the rest of the UI.
fn polar_theme() -> SvgTheme {
    let mut t = SvgTheme::light();
    t.background = "transparent".into();
    t.curve_c0_c180 = "#1f6fd0".into();
    t.curve_c0_c180_fill = "rgba(31,111,208,0.18)".into();
    t.curve_c90_c270 = "#7a3911".into();
    t.curve_c90_c270_fill = "rgba(122,57,17,0.10)".into();
    t
}
