//! Dump every distinct manufacturer string in the index cache, sorted
//! by doc count (descending). Useful for picking subdomain slugs:
//! the slug nginx forwards as `X-Manufacturer:` must match the
//! corpus's exact (case-insensitive) spelling of `LuminaireDoc.manufacturer`.
//!
//! Usage:
//!   cargo run --release --example list_manufacturers -p gldf-search-cli
//!
//! The cache path comes from `GLDF_SEARCH_INDEX_CACHE` in `.env`
//! (defaults to `.index/gldf-search.bin` at the repo root).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use gldf_search_gldf::cache;

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let cache_path: PathBuf = std::env::var("GLDF_SEARCH_INDEX_CACHE")
        .map(PathBuf::from)
        .or_else(|_| {
            let repo = std::env::current_dir()?;
            Ok::<_, std::io::Error>(repo.join(".index/gldf-search.bin"))
        })?;
    eprintln!("reading cache: {}", cache_path.display());

    let docs = cache::read(&cache_path)?;
    eprintln!("loaded {} docs", docs.len());

    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for doc in &docs {
        *counts.entry(doc.manufacturer.to_string()).or_default() += 1;
    }

    let mut rows: Vec<(String, u32)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    // Slug from the first whitespace-separated word, lowercased.
    // This matches the proposed `resolve_manufacturer_by_first_word`
    // policy: subdomain `aec.gldf-search.de` → header `aec` → match
    // any corpus manufacturer whose first token is `AEC`. Show both
    // forms so the operator can audit conflicts.
    println!(
        "{:>7}  {:<22}  {}",
        "count", "first-word slug", "manufacturer"
    );
    println!(
        "{:>7}  {:<22}  {}",
        "-----", "---------------", "------------"
    );
    for (name, n) in &rows {
        let slug = name
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        println!("{:>7}  {:<22}  {}", n, slug, name);
    }
    println!();
    println!("Total distinct: {}", rows.len());

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
