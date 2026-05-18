//! `gldf-search corpus inspect <file>` — load a `.gldf` via `gldf-rs`
//! and print the parts of the parse tree the extractor will care about.
//!
//! This is **diagnostic only**, not production extraction. It exists to
//! answer questions like "what locales are actually present?", "how
//! dense is `<Marketing>` in the wild?", "do variants carry distinct
//! photometric data or are they re-using one LDC?". The answers shape
//! the real extractor.
//!
//! The reading path goes through `gldf_rs::GldfProduct` exclusively —
//! no shell `unzip`, no `zip` crate calls, no parallel parsing. If
//! something is hidden because the parser drops it, the inspector is
//! also blind to it, which is the right invariant: the extractor will
//! see exactly what the inspector sees.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use gldf_rs::gldf::{GldfProduct, Variant};

/// What we print. Returned (instead of writing directly to stdout) so
/// tests can assert on individual fields without shelling.
#[derive(Debug, Clone)]
pub struct Report {
    /// File path inspected.
    pub path: String,
    /// Header summary.
    pub header: HeaderSummary,
    /// Files declared in `<GeneralDefinitions><Files>`. The extractor
    /// will need to know what's there before it tries to parse
    /// photometry.
    pub files: FilesSummary,
    /// Variants discovered. Every variant gets its own entry — the
    /// schema indexes per-variant for facets like IP code and
    /// LightDistribution.
    pub variants: Vec<VariantSummary>,
    /// Distinct locale codes found anywhere in the tree (header,
    /// product meta data, variants). The extractor uses this to size
    /// the per-locale fulltext sources.
    pub locales: BTreeSet<String>,
}

/// Short header view.
#[derive(Debug, Clone)]
pub struct HeaderSummary {
    /// Manufacturer name as written in the file (no normalisation).
    pub manufacturer: String,
    /// `<UniqueGldfId>` if present — candidate for the search DocId
    /// alongside the BLAKE3 fallback.
    pub unique_gldf_id: Option<String>,
    /// `<DefaultLanguage>` if present.
    pub default_language: Option<String>,
    /// `<CreatedWithApplication>` — useful for spotting per-vendor
    /// quirks ("DIALux 11.x" vs "Vendor Tooling 1.0").
    pub created_with_application: String,
    /// Format version as a single string (`"1.0.0-rc.3"` etc).
    pub format_version: String,
}

/// File-section summary. Every file in `<GeneralDefinitions><Files>`
/// gets one entry. Same surface as `gldf_rs::File` but trimmed.
#[derive(Debug, Clone)]
pub struct FileSummary {
    /// XSD `@id` — the reference key used by Photometry/Geometry
    /// nodes elsewhere in the tree.
    pub id: String,
    /// `@contentType` (see `gldf_rs::validation::xsd_enums::CONTENT_TYPES`).
    pub content_type: String,
    /// `@type` — `localFileName` (embedded in zip) or `url` (remote).
    pub type_attr: String,
    /// `$text` — filename (or URL).
    pub file_name: String,
}

/// Aggregate file counts per kind.
#[derive(Debug, Clone, Default)]
pub struct FilesSummary {
    /// Photometric files (LDC: `ldc/eulumdat`, `ldc/ies`, ...).
    pub photometric: Vec<FileSummary>,
    /// Image files (`image/png`, `image/jpg`, ...).
    pub images: Vec<FileSummary>,
    /// Geometry files (`geo/l3d`, `geo/m3d`, `geo/r3d`).
    pub geometries: Vec<FileSummary>,
    /// Spectrum files (`spectrum/text`).
    pub spectra: Vec<FileSummary>,
    /// Anything else declared (sensor, document, symbol, other).
    pub other: Vec<FileSummary>,
}

/// Variant view. The extractor will compose a `VariantDoc` from this
/// information plus the LDT/IES content (parsed via the `eulumdat`
/// feature, not yet enabled in this inspector).
#[derive(Debug, Clone)]
pub struct VariantSummary {
    /// XSD `@id` of the variant.
    pub id: String,
    /// Variant name string per locale, if `<Variant><Name>` is present.
    pub name_per_locale: Vec<(String, String)>,
    /// Variant description string per locale.
    pub description_per_locale: Vec<(String, String)>,
    /// GTIN at the variant level.
    pub gtin: Option<String>,
    /// Which `<Mountings>` child is present, if any. The extractor
    /// collapses these to [`gldf_search_schema::doc::VariantGeometry`].
    pub mountings_kind: Option<&'static str>,
    /// Marketing-derived facet sources.
    pub marketing: VariantMarketing,
    /// Mechanical-derived facet sources.
    pub mechanical: VariantMechanical,
    /// Electrical-derived facet sources.
    pub electrical: VariantElectrical,
}

/// Marketing slice of a variant's `DescriptiveAttributes`. All fields
/// are XSD canonical strings (or absent).
#[derive(Debug, Clone, Default)]
pub struct VariantMarketing {
    /// `<Applications><Application>` list — XSD canonical hierarchical
    /// strings. May be empty.
    pub applications: Vec<String>,
    /// `<Labels><Label>` list — XSD canonical regulatory marks
    /// (`CE`, `ENEC`, `UL`, ...).
    pub labels: Vec<String>,
    /// Designer name (free-form).
    pub designer: Option<String>,
}

/// Mechanical slice.
#[derive(Debug, Clone, Default)]
pub struct VariantMechanical {
    /// `<ProductForm>` — XSD canonical (`Round`, `Linear`, ...).
    pub product_form: Option<String>,
    /// `<IKRating>` — XSD canonical (`IK00`..`IK10+`).
    pub ik_rating: Option<String>,
    /// `<Adjustabilities><Adjustability>` list.
    pub adjustabilities: Vec<String>,
    /// `<ProtectiveAreas><Area>` list.
    pub protective_areas: Vec<String>,
    /// `<Weight>` in kg.
    pub weight_kg: Option<f64>,
}

/// Electrical slice.
#[derive(Debug, Clone, Default)]
pub struct VariantElectrical {
    /// `<IngressProtectionIPCode>` — XSD canonical (`IP20`..`IP69K`).
    pub ip_code: Option<String>,
    /// `<ElectricalSafetyClass>` — XSD canonical (`0`, `I`, `0I`,
    /// `II`, `III`).
    pub safety_class: Option<String>,
    /// `<LightDistribution>` — XSD canonical descriptive English.
    pub light_distribution: Option<String>,
}

/// Inspect a GLDF file by path.
pub fn inspect(path: &Path) -> Result<Report> {
    let path_str = path.to_string_lossy().into_owned();
    let gldf = GldfProduct::load_gldf(&path_str)
        .with_context(|| format!("load_gldf({})", path.display()))?;
    Ok(report(&gldf, path_str))
}

fn report(gldf: &GldfProduct, path: String) -> Report {
    let mut locales: BTreeSet<String> = BTreeSet::new();
    let header = header_summary(gldf, &mut locales);
    let files = files_summary(gldf);
    let variants = match &gldf.product_definitions.variants {
        Some(vs) => vs
            .variant
            .iter()
            .map(|v| variant_summary(v, &mut locales))
            .collect(),
        None => Vec::new(),
    };

    // ProductMetaData lives on ProductDefinitions; it carries
    // `<Description>`/`<Name>`/`<TenderText>` as `LocaleFoo` (a list
    // of language-tagged strings). Walk it for locale-coverage.
    if let Some(meta) = &gldf.product_definitions.product_meta_data {
        record_locale_foo(meta.name.as_ref(), &mut locales);
        record_locale_foo(meta.description.as_ref(), &mut locales);
        record_locale_foo(meta.tender_text.as_ref(), &mut locales);
        record_locale_foo(meta.product_number.as_ref(), &mut locales);
    }

    Report {
        path,
        header,
        files,
        variants,
        locales,
    }
}

fn header_summary(gldf: &GldfProduct, locales: &mut BTreeSet<String>) -> HeaderSummary {
    let h = &gldf.header;
    if let Some(lang) = &h.default_language {
        if !lang.is_empty() {
            locales.insert(lang.clone());
        }
    }
    HeaderSummary {
        manufacturer: h.manufacturer.clone(),
        unique_gldf_id: h.unique_gldf_id.clone().filter(|s| !s.is_empty()),
        default_language: h.default_language.clone().filter(|s| !s.is_empty()),
        created_with_application: h.created_with_application.clone(),
        format_version: format!(
            "{}.{}.{}{}",
            h.format_version.major,
            h.format_version.minor,
            h.format_version
                .pre_release
                .map(|n| format!("-rc.{n}"))
                .unwrap_or_default(),
            "" // placeholder if we ever want a build suffix
        ),
    }
}

fn files_summary(gldf: &GldfProduct) -> FilesSummary {
    let mut out = FilesSummary::default();
    for f in &gldf.general_definitions.files.file {
        let s = FileSummary {
            id: f.id.clone(),
            content_type: f.content_type.clone(),
            type_attr: f.type_attr.clone(),
            file_name: f.file_name.clone(),
        };
        let ct = f.content_type.as_str();
        if ct.starts_with("ldc/") {
            out.photometric.push(s);
        } else if ct.starts_with("image/") {
            out.images.push(s);
        } else if ct.starts_with("geo/") {
            out.geometries.push(s);
        } else if ct.starts_with("spectrum/") {
            out.spectra.push(s);
        } else {
            out.other.push(s);
        }
    }
    out
}

fn variant_summary(v: &Variant, locales: &mut BTreeSet<String>) -> VariantSummary {
    let mountings_kind = v.mountings.as_ref().map(|m| {
        // XSD: `Mountings` is a choice of (Ceiling | Wall | WorkingPlane
        // | Ground). The deeper variants (Recessed, Pendant, PoleTop,
        // ...) are nested inside those. The inspector reports the
        // top-level choice; the real extractor will look at the inner
        // structure to populate `VariantGeometry` more precisely.
        if m.ceiling.is_some() {
            "Ceiling"
        } else if m.wall.is_some() {
            "Wall"
        } else if m.working_plane.is_some() {
            "WorkingPlane"
        } else if m.ground.is_some() {
            "Ground"
        } else {
            "Empty"
        }
    });

    let mut name_per_locale = Vec::new();
    let mut description_per_locale = Vec::new();
    record_locale_foo_into(&v.name, locales, &mut name_per_locale);
    record_locale_foo_into(&v.description, locales, &mut description_per_locale);

    let mut marketing = VariantMarketing::default();
    let mut mechanical = VariantMechanical::default();
    let mut electrical = VariantElectrical::default();

    if let Some(da) = &v.descriptive_attributes {
        if let Some(m) = &da.marketing {
            if let Some(apps) = &m.applications {
                marketing.applications = apps.application.clone();
            }
            if let Some(labels) = &m.labels {
                marketing.labels = labels.label.clone();
            }
            marketing.designer = m.designer.clone();
        }
        if let Some(mech) = &da.mechanical {
            mechanical.product_form = mech.product_form.clone();
            mechanical.ik_rating = mech.ik_rating.clone();
            if let Some(adj) = &mech.adjustabilities {
                mechanical.adjustabilities = adj.adjustability.clone();
            }
            if let Some(pa) = &mech.protective_areas {
                mechanical.protective_areas = pa.area.clone();
            }
            mechanical.weight_kg = mech.weight;
        }
        if let Some(el) = &da.electrical {
            electrical.ip_code = el.ingress_protection_ip_code.clone();
            electrical.safety_class = el.electrical_safety_class.clone();
            electrical.light_distribution = el.light_distribution.clone();
        }
    }

    VariantSummary {
        id: v.id.clone(),
        name_per_locale,
        description_per_locale,
        gtin: v.gtin.clone(),
        mountings_kind,
        marketing,
        mechanical,
        electrical,
    }
}

/// `gldf_rs::gldf::header::LocaleFoo` is a wrapper over
/// `Vec<gldf_rs::gldf::header::Locale>` (each with `language` + `value`).
/// We accept `Option<&LocaleFoo>` so the call sites stay flat.
fn record_locale_foo(
    bag: Option<&gldf_rs::gldf::header::LocaleFoo>,
    locales: &mut BTreeSet<String>,
) {
    let Some(bag) = bag else { return };
    for loc in &bag.locale {
        if !loc.language.is_empty() {
            locales.insert(loc.language.clone());
        }
    }
}

fn record_locale_foo_into(
    bag: &Option<gldf_rs::gldf::header::LocaleFoo>,
    locales: &mut BTreeSet<String>,
    out: &mut Vec<(String, String)>,
) {
    let Some(bag) = bag else { return };
    for loc in &bag.locale {
        if !loc.language.is_empty() {
            locales.insert(loc.language.clone());
        }
        out.push((loc.language.clone(), loc.value.clone()));
    }
}

/// Pretty-print the report to stdout in a stable, grep-friendly form.
/// Not JSON because we want fast eyeballing across many files; a
/// `--json` flag lands later if structured output becomes useful.
pub fn print_report(r: &Report) {
    println!("FILE  {}", r.path);
    println!("HEADER");
    println!("  manufacturer:             {}", r.header.manufacturer);
    println!(
        "  unique_gldf_id:           {}",
        r.header.unique_gldf_id.as_deref().unwrap_or("(absent)")
    );
    println!(
        "  default_language:         {}",
        r.header.default_language.as_deref().unwrap_or("(absent)")
    );
    println!(
        "  created_with_application: {}",
        r.header.created_with_application
    );
    println!("  format_version:           {}", r.header.format_version);

    println!("LOCALES SEEN  {:?}", r.locales);

    println!(
        "FILES   photometric={}, images={}, geometries={}, spectra={}, other={}",
        r.files.photometric.len(),
        r.files.images.len(),
        r.files.geometries.len(),
        r.files.spectra.len(),
        r.files.other.len(),
    );
    for f in &r.files.photometric {
        println!(
            "  ldc id={} type={}/{} → {}",
            f.id, f.content_type, f.type_attr, f.file_name
        );
    }
    for f in &r.files.images {
        println!(
            "  img id={} type={}/{} → {}",
            f.id, f.content_type, f.type_attr, f.file_name
        );
    }

    println!("VARIANTS  count={}", r.variants.len());
    for v in &r.variants {
        println!(
            "  -- variant id={}  gtin={}  mounting={}",
            v.id,
            v.gtin.as_deref().unwrap_or("(none)"),
            v.mountings_kind.unwrap_or("(none)")
        );
        if !v.name_per_locale.is_empty() {
            println!("     name:");
            for (lang, value) in &v.name_per_locale {
                println!("       [{lang}] {value}");
            }
        }
        if !v.description_per_locale.is_empty() {
            println!("     description:");
            for (lang, value) in &v.description_per_locale {
                let trimmed: String = value.chars().take(120).collect();
                let ellipsis = if value.chars().count() > 120 {
                    "…"
                } else {
                    ""
                };
                println!("       [{lang}] {trimmed}{ellipsis}");
            }
        }
        if !v.marketing.applications.is_empty() {
            println!("     applications:");
            for a in &v.marketing.applications {
                println!("       {a}");
            }
        }
        if !v.marketing.labels.is_empty() {
            println!("     labels:                  {:?}", v.marketing.labels);
        }
        if let Some(d) = &v.marketing.designer {
            println!("     designer:                {d}");
        }
        let any_mech = v.mechanical.product_form.is_some()
            || v.mechanical.ik_rating.is_some()
            || !v.mechanical.adjustabilities.is_empty()
            || !v.mechanical.protective_areas.is_empty()
            || v.mechanical.weight_kg.is_some();
        if any_mech {
            println!(
                "     mechanical: form={:?} ik={:?} adj={:?} areas={:?} weight={:?}",
                v.mechanical.product_form,
                v.mechanical.ik_rating,
                v.mechanical.adjustabilities,
                v.mechanical.protective_areas,
                v.mechanical.weight_kg,
            );
        }
        let any_el = v.electrical.ip_code.is_some()
            || v.electrical.safety_class.is_some()
            || v.electrical.light_distribution.is_some();
        if any_el {
            println!(
                "     electrical: ip={:?} safety_class={:?} light_distribution={:?}",
                v.electrical.ip_code, v.electrical.safety_class, v.electrical.light_distribution,
            );
        }
    }
}
