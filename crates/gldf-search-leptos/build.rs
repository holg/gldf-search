//! Build-time generator for the 15 light-distribution glyphs.
//!
//! Each glyph is a small polar SVG — one per canonical
//! `LIGHT_DISTRIBUTIONS` entry in the GLDF XSD — used by the facet
//! panel's "Light distribution" group. Style follows the Relux LVK
//! plate: dark gray disc background, light gray axes, white-filled
//! closed polar curve. Designed to read clearly at 24–40 px in a
//! filter sidebar.
//!
//! We do NOT use `eulumdat`'s `to_svg`. That renderer is tuned for
//! full-page analytic diagrams (60 px hardcoded margin, grid circles,
//! labels) which look cramped at thumbnail sizes. Instead the
//! emitter below paints a few SVG primitives directly — no
//! build-time deps beyond `std`.
//!
//! Output: `$OUT_DIR/lvk_<slug>.svg` for each class. The component
//! side reads them with `include_str!`.

use std::f64::consts::FRAC_PI_2;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));

    // (slug-for-filename, intensity profile)
    //
    // Slugs match the canonical XSD strings via a function in the
    // component crate; do NOT renumber. Order is the XSD order so a
    // grep against `LIGHT_DISTRIBUTIONS` stays sane.
    let classes: &[(&str, ProfileFn)] = &[
        ("laterally-symmetrical-narrow", profile_narrow),
        ("laterally-symmetrical-medium", profile_medium),
        ("laterally-symmetrical-wide", profile_wide),
        ("symmetrical-in-each-quadrant", profile_quadrant),
        ("symmetric-about-0-180-plane", profile_plane_c0),
        ("symmetric-about-90-270-plane", profile_plane_c90),
        ("asymmetrical", profile_asym),
        ("asymmetrical-flood", profile_asym_flood),
        ("asymmetrical-wide-flood", profile_asym_wide_flood),
        ("diffuse-half-spherical", profile_diffuse_half),
        ("diffuse-full-spherical", profile_diffuse_full),
        ("direct", profile_direct),
        ("indirect", profile_indirect),
        ("direct-indirect", profile_direct_indirect),
        ("other", profile_other),
    ];

    for (slug, profile) in classes {
        let svg = render_glyph(*profile);
        let path = out_dir.join(format!("lvk_{slug}.svg"));
        std::fs::write(&path, svg).unwrap_or_else(|e| {
            panic!("write glyph {}: {e}", path.display());
        });
    }
}

// ── Glyph rendering ───────────────────────────────────────────────────

/// `intensity(gamma_deg, c_plane_deg) -> intensity`. `c_plane_deg` is
/// either 0 (C0–C180) or 90 (C90–C270) for the two curves we emit.
type ProfileFn = fn(f64, f64) -> f64;

/// SVG viewBox size in user units. Big enough that integer/fixed
/// stroke widths stay crisp; the CSS scales it down to display size.
const SIZE: f64 = 64.0;
/// Padding inside the disc so the curve never touches the edge.
const PAD: f64 = 4.0;

/// Render one glyph: gray disc, light axes, white filled polar curve.
fn render_glyph(profile: ProfileFn) -> String {
    // 5° steps from 0 to 180° give 37 samples per half-curve —
    // smooth enough at thumbnail size without padding the bundle.
    let gammas: Vec<f64> = (0..=36).map(|i| (i as f64) * 5.0).collect();

    // Build the closed polar curve in unit-intensity space. Both
    // C-planes contribute to `max_intensity` so we normalise the
    // larger lobe to the disc radius.
    let c0 = curve_points(&gammas, |g| profile(g, 0.0));
    let c90 = curve_points(&gammas, |g| profile(g, 90.0));
    let max_intensity = c0
        .iter()
        .chain(c90.iter())
        .map(|p| p.r)
        .fold(0.0_f64, f64::max)
        .max(1e-6);

    let cx = SIZE / 2.0;
    let cy = SIZE / 2.0;
    let radius = (SIZE / 2.0) - PAD;
    // Scale factor: largest intensity maps to the disc radius.
    let scale = radius / max_intensity;

    let mut svg = String::new();
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}">"#
    ));

    // Disc background (Relux uses a desaturated gray ~#d2d4d7).
    svg.push_str(&format!(
        r##"<circle cx="{cx}" cy="{cy}" r="{r:.2}" fill="#d4d6d9"/>"##,
        r = SIZE / 2.0
    ));

    // Cross axes — thin, lighter than the disc. Drawn behind the
    // curve so the curve covers them where opaque.
    let r = SIZE / 2.0;
    svg.push_str(&format!(
        r##"<line x1="{cx}" y1="{a:.2}" x2="{cx}" y2="{b:.2}" stroke="rgba(255,255,255,0.55)" stroke-width="0.9"/>"##,
        a = cy - r + PAD * 0.5,
        b = cy + r - PAD * 0.5
    ));
    svg.push_str(&format!(
        r##"<line x1="{a:.2}" y1="{cy}" x2="{b:.2}" y2="{cy}" stroke="rgba(255,255,255,0.55)" stroke-width="0.9"/>"##,
        a = cx - r + PAD * 0.5,
        b = cx + r - PAD * 0.5
    ));

    // Curve fill. C0 first (primary), C90 second only when distinct
    // (skipped for rotationally-symmetric classes where C90 is
    // empty). For visual punch we use solid white per the screenshot,
    // with a subtle outline so a fully-spherical curve has a visible
    // edge against the disc.
    if !c0.is_empty() {
        let path = path_for(&c0, cx, cy, scale);
        svg.push_str(&format!(
            r##"<path d="{path}" fill="#ffffff" stroke="rgba(0,0,0,0.18)" stroke-width="0.6" stroke-linejoin="round"/>"##
        ));
    }
    if !c90.is_empty() && distinguishable(&c0, &c90) {
        let path = path_for(&c90, cx, cy, scale);
        svg.push_str(&format!(
            r##"<path d="{path}" fill="rgba(255,255,255,0.55)" stroke="rgba(0,0,0,0.18)" stroke-width="0.6" stroke-linejoin="round"/>"##
        ));
    }

    svg.push_str("</svg>");
    svg
}

/// Polar sample. `r` is the intensity (proxy for radius in disc-
/// units before scaling), kept alongside the cartesian projection so
/// callers can find max intensity without re-walking the curve.
struct PolarSample {
    x: f64,
    y: f64,
    r: f64,
}

/// Build the closed polar curve for one C-plane: walk γ=0..180 on
/// the right side, then γ=180..0 mirrored on the left. Convention:
/// γ=0 is nadir (down), γ=180 is zenith (up). SVG y+ is down, so
/// γ=0 lands at positive y — matches photometric plots.
///
/// Returns an empty Vec when the profile is identically zero on this
/// C-plane (rotationally-symmetric classes return zero for c=90).
fn curve_points(gammas: &[f64], profile: impl Fn(f64) -> f64) -> Vec<PolarSample> {
    let mut out: Vec<PolarSample> = Vec::with_capacity(gammas.len() * 2);
    let mut any_nonzero = false;

    for &g in gammas {
        let r = profile(g).max(0.0);
        if r > 1e-9 {
            any_nonzero = true;
        }
        let angle = -g.to_radians() + FRAC_PI_2;
        out.push(PolarSample {
            x: r * angle.cos(),
            y: r * angle.sin(),
            r,
        });
    }
    for &g in gammas.iter().rev() {
        let r = profile(g).max(0.0);
        let angle = -g.to_radians() + FRAC_PI_2;
        out.push(PolarSample {
            x: -(r * angle.cos()),
            y: r * angle.sin(),
            r,
        });
    }

    if any_nonzero {
        out
    } else {
        Vec::new()
    }
}

/// SVG path `d` attribute for a closed polar curve, with the polar
/// origin translated to `(cx, cy)` and intensities scaled by `scale`.
fn path_for(points: &[PolarSample], cx: f64, cy: f64, scale: f64) -> String {
    let mut d = String::with_capacity(points.len() * 12);
    for (i, p) in points.iter().enumerate() {
        let sx = cx + p.x * scale;
        let sy = cy + p.y * scale;
        if i == 0 {
            d.push_str(&format!("M{sx:.2} {sy:.2}"));
        } else {
            d.push_str(&format!(" L{sx:.2} {sy:.2}"));
        }
    }
    d.push_str(" Z");
    d
}

/// Are the two curves visually distinguishable? Used to skip the
/// secondary C90 fill when it duplicates C0 (rotationally symmetric
/// classes). Compares max-r and a few sample radii — coarse but
/// cheap and good enough for "skip the overlay".
fn distinguishable(c0: &[PolarSample], c90: &[PolarSample]) -> bool {
    if c0.is_empty() || c90.is_empty() {
        return false;
    }
    let max_c0 = c0.iter().map(|p| p.r).fold(0.0_f64, f64::max);
    let max_c90 = c90.iter().map(|p| p.r).fold(0.0_f64, f64::max);
    if (max_c0 - max_c90).abs() / max_c0.max(1e-6) > 0.05 {
        return true;
    }
    // Compare a quarter and three-eighths sample.
    let n = c0.len().min(c90.len());
    let probe = [n / 4, 3 * n / 8, n / 2];
    for i in probe {
        let a = c0[i].r;
        let b = c90[i].r;
        if (a - b).abs() / a.max(1e-6) > 0.10 {
            return true;
        }
    }
    false
}

// ── Synthetic intensity profiles ──────────────────────────────────────
//
// Returns intensity (relative — only the SHAPE matters; the renderer
// rescales) for a given gamma. Conventions: γ=0 is nadir (down),
// γ=90 is horizontal, γ=180 is zenith (up). All profiles use 1.0 as
// peak. C-plane argument lets the asymmetric classes vary by C-plane.

fn gauss(gamma: f64, center: f64, sigma: f64) -> f64 {
    let z = (gamma - center) / sigma;
    (-0.5 * z * z).exp()
}

fn profile_narrow(g: f64, _c: f64) -> f64 {
    gauss(g, 0.0, 12.0)
}
fn profile_medium(g: f64, _c: f64) -> f64 {
    gauss(g, 0.0, 25.0)
}
fn profile_wide(g: f64, _c: f64) -> f64 {
    gauss(g, 0.0, 45.0)
}

fn profile_quadrant(g: f64, c: f64) -> f64 {
    // Same shape on both C-planes — rotationally symmetric look.
    let s = if c < 45.0 { 25.0 } else { 30.0 };
    gauss(g, 0.0, s)
}

fn profile_plane_c0(g: f64, c: f64) -> f64 {
    // Symmetric about C0-C180: sharp in C0, broader in C90.
    if c < 45.0 {
        gauss(g, 0.0, 18.0)
    } else {
        gauss(g, 0.0, 40.0)
    }
}

fn profile_plane_c90(g: f64, c: f64) -> f64 {
    // Mirror of plane_c0.
    if c < 45.0 {
        gauss(g, 0.0, 40.0)
    } else {
        gauss(g, 0.0, 18.0)
    }
}

fn profile_asym(g: f64, c: f64) -> f64 {
    // Lobe tilted to one side — peak off-axis at γ=20° in the C0
    // plane, narrower on the off side.
    if c < 45.0 {
        gauss(g, 25.0, 22.0)
    } else {
        gauss(g, 10.0, 28.0)
    }
}

fn profile_asym_flood(g: f64, c: f64) -> f64 {
    // Tilted + wider than `asym`.
    if c < 45.0 {
        gauss(g, 35.0, 30.0)
    } else {
        gauss(g, 15.0, 35.0)
    }
}

fn profile_asym_wide_flood(g: f64, c: f64) -> f64 {
    // Heavily tilted, very wide — typical street-light shape.
    if c < 45.0 {
        gauss(g, 55.0, 38.0)
    } else {
        gauss(g, 25.0, 42.0)
    }
}

fn profile_diffuse_half(g: f64, _c: f64) -> f64 {
    // Constant intensity over the lower hemisphere only.
    if g <= 90.0 {
        1.0
    } else {
        0.0
    }
}

fn profile_diffuse_full(_g: f64, _c: f64) -> f64 {
    // Constant intensity everywhere — a sphere.
    1.0
}

fn profile_direct(g: f64, _c: f64) -> f64 {
    // Downward-only narrow lobe (γ < 90°).
    if g <= 90.0 {
        gauss(g, 0.0, 28.0)
    } else {
        0.0
    }
}

fn profile_indirect(g: f64, _c: f64) -> f64 {
    // Upward-only narrow lobe (γ > 90°), peak at γ=180.
    if g >= 90.0 {
        gauss(g, 180.0, 28.0)
    } else {
        0.0
    }
}

fn profile_direct_indirect(g: f64, _c: f64) -> f64 {
    // Bimodal: lobes both up and down, narrow waist at horizontal.
    gauss(g, 0.0, 25.0).max(gauss(g, 180.0, 25.0) * 0.6)
}

fn profile_other(g: f64, _c: f64) -> f64 {
    // A featureless circle — the "we don't know" glyph. Renders as
    // a uniform disc, distinct from `diffuse_full` only by intent.
    let _ = g;
    0.7
}
