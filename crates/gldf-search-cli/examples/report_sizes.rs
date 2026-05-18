//! Dump per-field RAM costs for the currently-built index cache,
//! plus duplicate-string statistics to predict the win of
//! string-interning and group-and-fold dedup.
//!
//! Output sections:
//! 1. Aggregate sizes per top-level `LuminaireDoc` field (total +
//!    avg/doc + share of total RAM).
//! 2. Aggregate sizes per `VariantDoc` field.
//! 3. Duplicate counts for `manufacturer`, `product`, and the
//!    full `(manufacturer, product)` family key — i.e. the
//!    intern-table dedup ceiling.
//! 4. Top-50 most-duplicated `manufacturer` strings.
//! 5. Top-25 most-duplicated `(manufacturer, product)` families.
//!
//! Usage:
//!   GLDF_SEARCH_INDEX_CACHE=$PWD/.index/gldf-search.bin \
//!     cargo run --release --example report_sizes -p gldf-search-cli

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use compact_str::CompactString;
use gldf_search_gldf::cache;
use gldf_search_schema::doc::{LuminaireDoc, VariantDoc};

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let path: PathBuf = std::env::var("GLDF_SEARCH_INDEX_CACHE")
        .map(PathBuf::from)
        .or_else(|_| {
            Ok::<_, std::io::Error>(std::env::current_dir()?.join(".index/gldf-search.bin"))
        })?;
    let folded = std::env::args().any(|a| a == "--folded");
    eprintln!("reading cache: {}", path.display());

    let raw_docs = cache::read(&path)?;
    eprintln!("loaded {} docs", raw_docs.len());

    let docs = if folded {
        let (folded_docs, _aliases, stats) = gldf_search_index::fold_docs(raw_docs);
        eprintln!(
            "folded: {} → {} ({:.1}% reduction)",
            stats.input_docs,
            stats.output_docs,
            100.0 * (stats.input_docs as f64 - stats.output_docs as f64)
                / stats.input_docs.max(1) as f64,
        );
        folded_docs
    } else {
        eprintln!("(pass --folded to measure post-fold RAM)");
        raw_docs
    };

    // ── Doc-level aggregates ────────────────────────────────────
    let mut doc_id_bytes: u64 = 0;
    let mut source_bytes: u64 = 0;
    let mut manufacturer_bytes: u64 = 0;
    let mut product_bytes: u64 = 0;
    let mut gtin_bytes: u64 = 0;
    let mut product_code_bytes: u64 = 0;
    let mut descriptions_bytes: u64 = 0;
    let mut descriptions_strings: u64 = 0;
    let mut keywords_bytes: u64 = 0;
    let mut keywords_strings: u64 = 0;
    let mut applications_bytes: u64 = 0;
    let mut labels_bytes: u64 = 0;
    let mut adjustability_bytes: u64 = 0;
    let mut variants_struct_bytes: u64 = 0;
    let mut variant_name_bytes: u64 = 0;
    let mut variant_photo_bytes: u64 = 0;
    let mut variant_cg_iface_bytes: u64 = 0;
    let mut total_variants: u64 = 0;
    let mut variants_with_phot: u64 = 0;

    let struct_overhead = std::mem::size_of::<LuminaireDoc>() as u64;
    let variant_overhead = std::mem::size_of::<VariantDoc>() as u64;

    // Duplicate-string trackers.
    let mut mfr_counts: BTreeMap<CompactString, u32> = BTreeMap::new();
    let mut product_counts: BTreeMap<CompactString, u32> = BTreeMap::new();
    let mut family_counts: BTreeMap<(CompactString, CompactString), u32> = BTreeMap::new();
    let mut variant_name_counts: BTreeMap<CompactString, u32> = BTreeMap::new();

    for doc in &docs {
        doc_id_bytes += std::mem::size_of_val(&doc.id) as u64;
        source_bytes += source_ref_bytes(&doc.source);
        manufacturer_bytes += cs_bytes(&doc.manufacturer);
        product_bytes += cs_bytes(&doc.product);
        gtin_bytes += doc.gtin.as_ref().map(cs_bytes).unwrap_or(0);
        product_code_bytes += doc.product_code.as_ref().map(cs_bytes).unwrap_or(0);

        // descriptions: SmallVec<[(Locale, String); 1]>
        for (_, s) in doc.descriptions.iter() {
            descriptions_bytes += s.capacity() as u64;
            descriptions_strings += 1;
        }
        descriptions_bytes +=
            spill_overhead::<(gldf_search_schema::Locale, String)>(&doc.descriptions);

        for k in doc.keywords.iter() {
            keywords_bytes += cs_bytes(k);
            keywords_strings += 1;
        }
        keywords_bytes += spill_overhead::<CompactString>(&doc.keywords);

        applications_bytes += spill_overhead::<u8>(&doc.applications);
        labels_bytes += spill_overhead::<u8>(&doc.labels);
        adjustability_bytes += spill_overhead::<u8>(&doc.adjustability);

        for v in &doc.variants {
            variants_struct_bytes += variant_overhead;
            variant_name_bytes += cs_bytes(&v.name);
            variant_cg_iface_bytes += spill_overhead::<u8>(&v.control_gear_interfaces);
            if v.photometry.is_some() {
                variants_with_phot += 1;
                variant_photo_bytes +=
                    std::mem::size_of::<gldf_search_schema::doc::PhotometryStats>() as u64;
            }
            total_variants += 1;
            *variant_name_counts.entry(v.name.clone()).or_default() += 1;
        }
        variants_struct_bytes += spill_overhead::<VariantDoc>(&doc.variants);

        *mfr_counts.entry(doc.manufacturer.clone()).or_default() += 1;
        *product_counts.entry(doc.product.clone()).or_default() += 1;
        *family_counts
            .entry((doc.manufacturer.clone(), doc.product.clone()))
            .or_default() += 1;
    }

    // ── Report ──────────────────────────────────────────────────
    let n = docs.len() as u64;
    println!();
    println!("=== LuminaireDoc field RAM costs ({} docs) ===", n);
    println!("{:<28}  {:>14}  {:>10}", "field", "total", "avg/doc");
    let row = |name: &str, bytes: u64| {
        println!(
            "{:<28}  {:>14}  {:>10}",
            name,
            human_bytes(bytes),
            human_bytes(if n > 0 { bytes / n } else { 0 }),
        );
    };
    row("struct overhead (sizeof)", struct_overhead * n);
    row("  .id (DocId)", doc_id_bytes);
    row("  .source", source_bytes);
    row("  .manufacturer (String)", manufacturer_bytes);
    row("  .product (String)", product_bytes);
    row("  .gtin (Option<String>)", gtin_bytes);
    row("  .product_code", product_code_bytes);
    row("  .descriptions (heap)", descriptions_bytes);
    row("  .keywords (heap)", keywords_bytes);
    row("  .applications spill", applications_bytes);
    row("  .labels spill", labels_bytes);
    row("  .adjustability spill", adjustability_bytes);
    row("variants[] structs", variants_struct_bytes);
    row("  .name", variant_name_bytes);
    row("  .photometry (some)", variant_photo_bytes);
    row("  .cg_interfaces spill", variant_cg_iface_bytes);

    let approx_total = struct_overhead * n
        + manufacturer_bytes
        + product_bytes
        + gtin_bytes
        + product_code_bytes
        + descriptions_bytes
        + keywords_bytes
        + applications_bytes
        + labels_bytes
        + adjustability_bytes
        + variants_struct_bytes
        + variant_name_bytes
        + variant_photo_bytes
        + variant_cg_iface_bytes;
    println!();
    println!("Approx total accounted: {}", human_bytes(approx_total));
    println!("(Excludes index-side structures: by_id, by_article, article_prefix.)");

    println!();
    println!(
        "Variants: {} total ({} with photometry, {:.1}%), avg {:.2}/doc",
        total_variants,
        variants_with_phot,
        100.0 * variants_with_phot as f64 / total_variants.max(1) as f64,
        total_variants as f64 / n.max(1) as f64,
    );

    // ── Dedup analysis ──────────────────────────────────────────
    println!();
    println!("=== Dedup potential ===");
    let mfr_unique = mfr_counts.len();
    let product_unique = product_counts.len();
    let family_unique = family_counts.len();
    println!(
        "Manufacturer strings: {} occurrences, {} unique (intern would collapse {})",
        n, mfr_unique, n - mfr_unique as u64
    );
    println!(
        "Product strings:      {} occurrences, {} unique (intern would collapse {})",
        n, product_unique, n - product_unique as u64
    );
    println!(
        "(manufacturer, product) families: {} unique — folding {} docs into {} groups would shrink the doc count by {:.1}%",
        family_unique,
        n,
        family_unique,
        100.0 * (n as f64 - family_unique as f64) / n as f64,
    );
    let variant_name_unique = variant_name_counts.len();
    println!(
        "Variant name strings: {} occurrences, {} unique",
        total_variants, variant_name_unique
    );

    let mfr_intern_saved = manufacturer_bytes
        .saturating_sub(estimated_intern_bytes(&mfr_counts));
    let product_intern_saved = product_bytes
        .saturating_sub(estimated_intern_bytes(&product_counts));
    println!();
    println!("Projected savings from string-interning:");
    println!(
        "  manufacturer: save {} ({:.1}% of field, {:.1}% of total)",
        human_bytes(mfr_intern_saved),
        100.0 * mfr_intern_saved as f64 / manufacturer_bytes.max(1) as f64,
        100.0 * mfr_intern_saved as f64 / approx_total.max(1) as f64,
    );
    println!(
        "  product:      save {} ({:.1}% of field, {:.1}% of total)",
        human_bytes(product_intern_saved),
        100.0 * product_intern_saved as f64 / product_bytes.max(1) as f64,
        100.0 * product_intern_saved as f64 / approx_total.max(1) as f64,
    );

    println!();
    println!("=== Top 50 manufacturers by doc count ===");
    let mut mfr_rows: Vec<_> = mfr_counts.iter().collect();
    mfr_rows.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in mfr_rows.iter().take(50) {
        println!("  {:>6}  {}", count, name);
    }

    println!();
    println!("=== Top 25 families (manufacturer, product) ===");
    let mut family_rows: Vec<_> = family_counts.iter().collect();
    family_rows.sort_by(|a, b| b.1.cmp(a.1));
    for ((mfr, prod), count) in family_rows.iter().take(25) {
        println!("  {:>6}  {} :: {}", count, mfr, prod);
    }

    Ok(())
}

fn cs_bytes(s: &CompactString) -> u64 {
    // CompactString inline ≤ 24 bytes (on 64-bit) — those cost 0
    // extra heap. Otherwise heap allocation = capacity().
    if s.is_heap_allocated() {
        s.capacity() as u64
    } else {
        0
    }
}

fn source_ref_bytes(src: &gldf_search_schema::doc::SourceRef) -> u64 {
    use gldf_search_schema::doc::SourceRef;
    match src {
        SourceRef::Path(p) | SourceRef::Url(p) => cs_bytes(p),
        SourceRef::ContentOnly => 0,
    }
}

fn spill_overhead<T>(sv: &smallvec::SmallVec<impl smallvec::Array>) -> u64 {
    // SmallVec only allocates when it spills past the inline cap.
    if sv.spilled() {
        (sv.capacity() * std::mem::size_of::<T>()) as u64
    } else {
        0
    }
}

/// Estimate the heap bytes that survive interning: one copy of each
/// unique string (no inline savings from CompactString counted because
/// the index would store one shared Arc<str> per unique value).
fn estimated_intern_bytes(counts: &BTreeMap<CompactString, u32>) -> u64 {
    counts.keys().map(|s| s.len() as u64).sum()
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[i])
    } else {
        format!("{:.2} {}", v, UNITS[i])
    }
}
