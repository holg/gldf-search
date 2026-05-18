//! One-shot sanity check for a freshly-added manufacturer.
//!
//! Walks `<root>/<mfr>/**/*.gldf`, runs the same extractor the
//! `build-index` subcommand uses, and reports per-file what came
//! out: manufacturer name as the corpus will key it, product name,
//! variant count, # with photometry, mounting types, applications.
//!
//! Helps catch issues *before* burning a full corpus re-extract:
//! - Does the corpus name fold to a sensible subdomain slug?
//! - Are variants showing up at all?
//! - Is the photometric file resolvable per variant?
//! - Do `Application` / `Mounting` come through cleanly?
//!
//! Usage:
//!   cargo run --release --example probe_deko -p gldf-search-cli -- \
//!       /Volumes/tb4ssd/.../gldfs/deko-light
//!
//! Or pass a single .gldf file to introspect just that one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use gldf_rs::gldf::GldfProduct;
use gldf_search_gldf::{extract, ExtractInput};
use gldf_search_schema::doc::SourceRef;
use gldf_search_schema::enums::application_str;

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .cloned()
        .map(PathBuf::from)
        .expect("usage: probe_deko <gldf-or-dir>");

    let files: Vec<PathBuf> = if path.is_dir() {
        let mut out = Vec::new();
        let mut stack = vec![path.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)?.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("gldf"))
                {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    } else {
        vec![path.clone()]
    };

    eprintln!("Probing {} GLDF file(s)", files.len());

    let mut mfr_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut total_variants: u64 = 0;
    let mut variants_with_phot: u64 = 0;
    let mut docs_with_app: u32 = 0;
    let mut warnings: u64 = 0;
    let mut failures: u32 = 0;
    let mut first_failure: Option<String> = None;
    let mut sample_printed = 0u32;

    for path in &files {
        let path_str = path.to_string_lossy().to_string();
        let gldf = match GldfProduct::load_gldf(&path_str) {
            Ok(g) => g,
            Err(e) => {
                failures += 1;
                if first_failure.is_none() {
                    first_failure = Some(format!("{}: {e}", path.display()));
                }
                continue;
            }
        };

        let input = ExtractInput {
            source: Some(SourceRef::Path(path_str.clone().into())),
            raw_bytes: None,
            file_meta: Some(gldf_search_schema::doc::FileMeta {
                size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                mtime_epoch_s: None,
                format_version: None,
            }),
        };
        let outcome = extract(&gldf, input);

        let doc = outcome.doc;
        *mfr_counts.entry(doc.manufacturer.to_string()).or_default() += 1;
        total_variants += doc.variants.len() as u64;
        variants_with_phot += doc
            .variants
            .iter()
            .filter(|v| v.photometry.is_some())
            .count() as u64;
        if !doc.applications.is_empty() {
            docs_with_app += 1;
        }
        warnings += outcome.warnings.len() as u64;

        // Print first 3 docs verbatim so the operator can eyeball them.
        if sample_printed < 3 {
            sample_printed += 1;
            println!("\n=== {} ===", path.display());
            println!("  manufacturer = {:?}", doc.manufacturer);
            println!("  product      = {:?}", doc.product);
            println!("  variants     = {}", doc.variants.len());
            println!(
                "  with phot    = {}",
                doc.variants
                    .iter()
                    .filter(|v| v.photometry.is_some())
                    .count()
            );
            let apps: Vec<&'static str> = doc
                .applications
                .iter()
                .filter_map(|id| application_str(*id))
                .collect();
            println!("  applications = {apps:?}");
            println!(
                "  mounting     = place {:?}, type {:?}",
                doc.variants
                    .first()
                    .map(|v| format!("{:?}", v.mounting_place))
                    .unwrap_or_default(),
                doc.variants
                    .first()
                    .map(|v| format!("{:?}", v.mounting_type))
                    .unwrap_or_default(),
            );
            if !outcome.warnings.is_empty() {
                println!("  warnings ({}):", outcome.warnings.len());
                for w in outcome.warnings.iter().take(3) {
                    println!("    - {w:?}");
                }
            }
        }
    }

    println!();
    println!("=== Summary ===");
    println!("Files processed: {}", files.len());
    println!("Successful:      {}", files.len() as u32 - failures);
    println!("Failures:        {failures}");
    if let Some(e) = first_failure {
        println!("First failure:   {e}");
    }
    println!("Total variants:  {total_variants}");
    println!(
        "  with photometry: {variants_with_phot} ({:.1}%)",
        if total_variants > 0 {
            100.0 * variants_with_phot as f64 / total_variants as f64
        } else {
            0.0
        }
    );
    println!("Docs with apps:  {docs_with_app}");
    println!("Total warnings:  {warnings}");
    println!();
    println!("Manufacturer names seen ({}):", mfr_counts.len());
    let mut rows: Vec<_> = mfr_counts.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (name, n) in rows {
        let slug = name
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        println!("  {n:>5}  slug={slug:<20}  name={name}");
    }
    Ok(())
}
