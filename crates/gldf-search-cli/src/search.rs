//! `gldf-search corpus search` — load every `.gldf` in a directory,
//! extract each into a `LuminaireDoc`, build an `InMemoryIndex`, run a
//! filter, print hits and (optionally) facet counts.
//!
//! Native-only path: file system traversal, gldf-rs parsing, extractor
//! orchestration, then the index. The `IndexSource` trait surface is
//! the same one Leptos / browser code will target.

use std::ops::RangeInclusive;
use std::path::Path;

use anyhow::{Context, Result};
use compact_str::CompactString;
use gldf_rs::gldf::GldfProduct;
use gldf_search_gldf::{extract, ExtractInput};
use gldf_search_index::{
    FacetCounts, FacetField, FacetValueCount, InMemoryIndex, IndexSource, Pagination,
};
use gldf_search_schema::doc::{LuminaireDoc, SourceRef};
use gldf_search_schema::enums::{ip_code_from_str, label_from_str, product_form_from_str};
use gldf_search_schema::filter::{Filter, Locale};

/// CLI inputs for `corpus search`. The `clap`-level struct lives in
/// `main.rs`; this is the parsed/resolved form ready for execution.
#[derive(Debug, Clone, Default)]
pub struct SearchArgs {
    /// Directory holding `.gldf` files. Read non-recursively.
    pub root: std::path::PathBuf,
    /// Free-text query.
    pub text: Option<String>,
    /// Restrict text matching to one locale (2-letter ISO code).
    pub locale: Option<String>,
    /// Manufacturer display names (OR-within).
    pub manufacturer: Vec<String>,
    /// XSD canonical IP code strings (OR-within); e.g. `["IP54", "IP65"]`.
    pub ip_code: Vec<String>,
    /// XSD canonical label strings (OR-within); e.g. `["CE", "ENEC"]`.
    pub labels: Vec<String>,
    /// XSD canonical product-form strings (OR-within).
    pub product_form: Vec<String>,
    /// Inclusive flux range (lm). `Some(min)..=Some(max)`; pass `0`
    /// or `f32::INFINITY` for unbounded ends.
    pub flux_lm: Option<RangeInclusive<f32>>,
    /// Inclusive CCT range (K).
    pub cct_k: Option<RangeInclusive<u16>>,
    /// Photometry presence tristate: `Some(true)`, `Some(false)`,
    /// `None`.
    pub has_photometry: Option<bool>,
    /// Print facet counts after the hit list.
    pub facets: bool,
    /// 0-indexed page number; `offset = page * limit`.
    pub page: u32,
    /// Hits per page.
    pub limit: u32,
}

/// Run a `corpus search` against `args.root`. Returns nothing — output
/// is printed to stdout. Errors bubble up from gldf-rs loading and
/// filter-string parsing.
pub fn run(args: &SearchArgs) -> Result<()> {
    let docs = load_and_extract(&args.root)?;
    let total_docs = docs.len();
    let idx = InMemoryIndex::from_docs(docs);

    let filter = build_filter(args)?;
    let pagination = Pagination {
        offset: args.page.saturating_mul(args.limit),
        limit: args.limit,
    };
    let result = idx.search(&filter, pagination);

    println!(
        "{} hits of {} total (page {}, limit {})",
        result.hits.len(),
        result.total,
        args.page,
        args.limit,
    );
    println!("indexed {} docs from {}", total_docs, args.root.display());
    println!();

    for hit in &result.hits {
        if let Some(doc) = idx.doc(&hit.doc) {
            print_hit(doc);
        }
    }

    if args.facets {
        let counts = idx.facets(
            &filter,
            &[
                FacetField::Manufacturer,
                FacetField::IpCode,
                FacetField::ProductForm,
                FacetField::Labels,
                FacetField::MountingPlace,
                FacetField::MountingType,
                FacetField::HasPhotometry,
            ],
        );
        println!();
        print_facets(&counts);
    }

    Ok(())
}

/// Walk `root` recursively, load every `.gldf`, run the extractor,
/// and collect into a `Vec<LuminaireDoc>`. Errors on individual files
/// are printed to stderr and the file skipped — partial indexing is
/// better than failing the whole run.
///
/// `SourceRef::Path` is the relative path from `root` so the schema
/// survives both flat and nested corpora.
pub(crate) fn load_and_extract(root: &Path) -> Result<Vec<LuminaireDoc>> {
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("canonicalize({})", root.display()))?;
    let mut docs = Vec::new();
    visit(&root_canon, &root_canon, &mut docs)?;
    Ok(docs)
}

fn visit(dir: &Path, root: &Path, docs: &mut Vec<LuminaireDoc>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read_dir({})", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ftype = entry.file_type()?;
        if ftype.is_dir() {
            visit(&path, root, docs)?;
            continue;
        }
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gldf"))
        {
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path_str.clone())
            });
        match GldfProduct::load_gldf(&path_str) {
            Ok(gldf) => {
                let out = extract(
                    &gldf,
                    ExtractInput {
                        source: Some(SourceRef::Path(rel.into())),
                        ..Default::default()
                    },
                );
                docs.push(out.doc);
            }
            Err(err) => {
                eprintln!("skip {}: {err:#}", path.display());
            }
        }
    }
    Ok(())
}

/// Build a [`Filter`] from CLI args. Each closed-enum string is
/// resolved through the canonical-set helpers; non-canonical values
/// are skipped with a warning rather than failing the whole search.
fn build_filter(args: &SearchArgs) -> Result<Filter> {
    let mut f = Filter::default();

    if let Some(text) = &args.text {
        f.text = Some(CompactString::from(text.as_str()));
    }
    if let Some(lang) = &args.locale {
        match Locale::from_str(lang) {
            Some(l) => f.locale = Some(l),
            None => eprintln!("ignoring non-2-letter locale: {lang}"),
        }
    }
    if !args.manufacturer.is_empty() {
        f.manufacturer = Some(args.manufacturer.iter().map(CompactString::from).collect());
    }

    for s in &args.ip_code {
        match ip_code_from_str(s) {
            Some(id) => f.ip_code.push(id),
            None => eprintln!("ignoring non-canonical IP code: {s}"),
        }
    }
    for s in &args.labels {
        match label_from_str(s) {
            Some(id) => f.labels.push(id),
            None => eprintln!("ignoring non-canonical label: {s}"),
        }
    }
    for s in &args.product_form {
        match product_form_from_str(s) {
            Some(id) => f.product_form.push(id),
            None => eprintln!("ignoring non-canonical product form: {s}"),
        }
    }

    f.flux_lm = args.flux_lm.clone();
    f.cct_k = args.cct_k.clone();
    f.has_photometry = args.has_photometry;

    Ok(f)
}

/// Print one hit to stdout. Designed for grep-and-eyeball, not JSON
/// (which `corpus extract` already covers).
fn print_hit(doc: &LuminaireDoc) {
    println!(
        "• [{manufacturer}] {product}",
        manufacturer = doc.manufacturer,
        product = doc.product,
    );
    let mut kws: Vec<&str> = doc.keywords.iter().map(|s| s.as_str()).collect();
    kws.sort();
    if !kws.is_empty() {
        println!("    keywords: {}", kws.join(", "));
    }
    if let Some(id) = doc.ip_code {
        if let Some(s) = gldf_search_schema::enums::ip_code_str(id) {
            print!("    ip_code: {s}");
        }
        println!();
    }
    let any_phot = doc.variants.iter().any(|v| v.photometry.is_some());
    if !any_phot {
        println!("    photometry: missing");
    }
}

/// Print the requested facet counts. Keeps the format scannable: each
/// facet's value list trimmed to the top 10 to avoid drowning the
/// terminal on large corpora.
fn print_facets(counts: &FacetCounts) {
    let sections: &[(&str, &[FacetValueCount])] = &[
        ("Manufacturer", &counts.manufacturer),
        ("IP code", &counts.ip_code),
        ("Product form", &counts.product_form),
        ("Labels", &counts.labels),
        ("Mounting place", &counts.mounting_place),
        ("Mounting type", &counts.mounting_type),
        ("Has photometry", &counts.has_photometry),
    ];
    for (name, list) in sections {
        if list.is_empty() {
            continue;
        }
        println!("== {name} ==");
        for c in list.iter().take(10) {
            println!("  {:>4}  {}", c.count, c.value);
        }
        if list.len() > 10 {
            println!("  …  ({} more)", list.len() - 10);
        }
    }
}
