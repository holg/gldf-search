//! Static LVK (Lichtverteilungskurve / light-distribution-curve)
//! polar glyphs.
//!
//! Generated at build time by `build.rs` from synthetic intensity
//! curves using `eulumdat::diagram::PolarDiagram::to_svg`. One SVG
//! per canonical `LIGHT_DISTRIBUTIONS` XSD entry; the slugs match
//! `build.rs` and the lookup keys here are the verbatim XSD strings
//! (the same strings the index emits as facet values).
//!
//! All glyphs are `include_str!`'d so they end up as bytes in the
//! crate binary — no runtime asset fetch, no extra HTTP roundtrip.

macro_rules! glyph {
    ($slug:literal) => {
        include_str!(concat!(env!("OUT_DIR"), "/lvk_", $slug, ".svg"))
    };
}

/// XSD canonical string → inline SVG markup. Order matches
/// `gldf_rs::validation::xsd_enums::LIGHT_DISTRIBUTIONS`.
pub const LVK_GLYPHS: &[(&str, &str)] = &[
    ("Laterally symmetrical narrow", glyph!("laterally-symmetrical-narrow")),
    ("Laterally symmetrical medium", glyph!("laterally-symmetrical-medium")),
    ("Laterally symmetrical wide", glyph!("laterally-symmetrical-wide")),
    ("Symmetrical in each quadrant", glyph!("symmetrical-in-each-quadrant")),
    ("Symmetric about 0-180 plane", glyph!("symmetric-about-0-180-plane")),
    ("Symmetric about 90-270 plane", glyph!("symmetric-about-90-270-plane")),
    ("Asymmetrical", glyph!("asymmetrical")),
    ("Asymmetrical flood", glyph!("asymmetrical-flood")),
    ("Asymmetrical wide flood", glyph!("asymmetrical-wide-flood")),
    ("Diffuse half spherical", glyph!("diffuse-half-spherical")),
    ("Diffuse full spherical", glyph!("diffuse-full-spherical")),
    ("Direct", glyph!("direct")),
    ("Indirect", glyph!("indirect")),
    ("Direct indirect", glyph!("direct-indirect")),
    ("Other", glyph!("other")),
];

/// Look up the polar-curve SVG for an LVK class by its canonical
/// XSD string. Returns `None` for unknown strings; callers should
/// fall back to a label-only render.
pub fn lvk_glyph(canonical: &str) -> Option<&'static str> {
    LVK_GLYPHS
        .iter()
        .find(|(key, _)| *key == canonical)
        .map(|(_, svg)| *svg)
}
