//! `gldf-search` CLI entry point.
//!
//! Subcommands grow as the project lands more pieces. The first one,
//! `corpus sample`, copies a random selection of GLDF files from a
//! large source corpus into a working sample directory — used as both
//! the dev test set and the future static web root.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod corpus;
mod inspect;
mod search;

#[derive(Parser, Debug)]
#[command(
    name = "gldf-search",
    version,
    about = "Tools for the gldf-search index",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Corpus operations (sampling, future: triage, scan).
    Corpus {
        #[command(subcommand)]
        sub: CorpusCmd,
    },
}

#[derive(Subcommand, Debug)]
enum CorpusCmd {
    /// Copy N random `.gldf` files from a source corpus into an output
    /// directory. Single-pass reservoir sampling. Writes a
    /// `manifest.json` next to the copied files.
    Sample {
        /// Source corpus directory (flat, holds all `.gldf` files).
        /// Required; pass the path to the big source corpus you want
        /// to sample from. No env-var fallback — sampling is a manual
        /// dev tool, and we don't want it picking up the live corpus
        /// path (`GLDF_SEARCH_CORPUS`) by accident.
        #[arg(long)]
        root: PathBuf,
        /// Destination directory. Created if missing. Refuses to clobber
        /// a non-empty existing directory unless `--force` is given.
        #[arg(long)]
        out: PathBuf,
        /// Number of files to sample.
        #[arg(long, default_value_t = 10)]
        n: usize,
        /// Optional RNG seed for reproducible samples. Omit for a fresh
        /// random sample on every run.
        #[arg(long)]
        seed: Option<u64>,
        /// Clear `out` first if it exists and is non-empty.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Load one or more `.gldf` files via gldf-rs and print the parsed
    /// structure the extractor will work with. Diagnostic only.
    Inspect {
        /// One or more file paths. Multiple files are inspected in
        /// turn, each preceded by a blank line.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Load a `.gldf` file, run the search extractor, and print the
    /// resulting `LuminaireDoc` plus warnings as pretty JSON.
    Extract {
        /// One or more file paths.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Build an in-memory index over every `.gldf` in `--root`, run a
    /// filter, print hits and (optionally) facet counts. Native-only;
    /// `MmapIndex` / `RemoteIndex` will land later.
    Search {
        /// Directory of `.gldf` files (non-recursive).
        ///
        /// Resolution order:
        /// 1. `--root` on the command line.
        /// 2. `DEBUG_GLDF_SEARCH_CORPUS` if set (dev override).
        /// 3. `GLDF_SEARCH_CORPUS` (the live corpus path from `.env`).
        ///
        /// Missing entirely → error. Same pattern as
        /// `gldf-search-server` so the same `.env` works for both.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Free-text query (case-insensitive substring; matches
        /// descriptions, manufacturer, product, keywords).
        #[arg(long)]
        text: Option<String>,
        /// Restrict text matching to one locale ("en", "de", ...).
        #[arg(long)]
        locale: Option<String>,
        /// Manufacturer display names. Repeatable.
        #[arg(long = "manufacturer")]
        manufacturer: Vec<String>,
        /// XSD canonical IP code strings (e.g. `IP54`). Repeatable.
        #[arg(long = "ip-code")]
        ip_code: Vec<String>,
        /// XSD canonical label strings (e.g. `CE`). Repeatable.
        #[arg(long = "label")]
        labels: Vec<String>,
        /// XSD canonical product-form strings (e.g. `Round`).
        /// Repeatable.
        #[arg(long = "product-form")]
        product_form: Vec<String>,
        /// Inclusive flux range as `MIN:MAX` in lumens.
        #[arg(long = "flux-lm")]
        flux_lm: Option<String>,
        /// Inclusive CCT range as `MIN:MAX` in Kelvin.
        #[arg(long = "cct-k")]
        cct_k: Option<String>,
        /// Filter on photometry presence: `true` / `false`. Omit for
        /// don't-care.
        #[arg(long = "has-photometry")]
        has_photometry: Option<bool>,
        /// Print facet counts after the hit list.
        #[arg(long, default_value_t = false)]
        facets: bool,
        /// 0-indexed page number.
        #[arg(long, default_value_t = 0)]
        page: u32,
        /// Hits per page.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Pre-build the on-disk extracted-doc cache. The server reads
    /// this file on startup instead of re-extracting the corpus —
    /// turning multi-minute cold starts at 270k+ docs into sub-second
    /// warm starts. Safe to re-run; the cache is rewritten atomically.
    ///
    /// Extracts in parallel using all cores by default. Pass
    /// `--threads N` to bound the rayon pool. Build locally, rsync
    /// the resulting bincode file to the (single-core) server.
    BuildIndex {
        /// Corpus root directory (flat or nested layout). Defaults to
        /// `GLDF_SEARCH_CORPUS` / `DEBUG_GLDF_SEARCH_CORPUS` like the
        /// server.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Output cache file. Defaults to `GLDF_SEARCH_INDEX_CACHE`
        /// env var, falling back to `<root>/.index.bin`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Rayon worker threads. `0` (the default) means "all
        /// available cores". The walk to enumerate `.gldf` paths
        /// stays serial; only the per-file extract parallelises.
        #[arg(long, default_value_t = 0)]
        threads: usize,
    },
}

fn main() -> ExitCode {
    // Load .env so DEBUG_GLDF_SEARCH_CORPUS / GLDF_SEARCH_CORPUS are
    // visible when `corpus search --root` falls back to the env vars.
    // Silent failure: the CLI is dev-only and may run without a .env.
    let _ = dotenvy::dotenv();
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Corpus { sub } => match sub {
            CorpusCmd::Sample {
                root,
                out,
                n,
                seed,
                force,
            } => run_sample(root, out, n, seed, force),
            CorpusCmd::Inspect { files } => run_inspect(&files),
            CorpusCmd::Extract { files } => run_extract(&files),
            CorpusCmd::Search {
                root,
                text,
                locale,
                manufacturer,
                ip_code,
                labels,
                product_form,
                flux_lm,
                cct_k,
                has_photometry,
                facets,
                page,
                limit,
            } => {
                let args = search::SearchArgs {
                    root: resolve_corpus_root(root.as_deref())?,
                    text,
                    locale,
                    manufacturer,
                    ip_code,
                    labels,
                    product_form,
                    flux_lm: flux_lm.as_deref().map(parse_f32_range).transpose()?,
                    cct_k: cct_k.as_deref().map(parse_u16_range).transpose()?,
                    has_photometry,
                    facets,
                    page,
                    limit,
                };
                search::run(&args)
            }
            CorpusCmd::BuildIndex {
                root,
                out,
                threads,
            } => run_build_index(root, out, threads),
        },
    }
}

/// Parse a `MIN:MAX` range string. Either end may be empty to mean
/// unbounded (`":5000"` = `0..=5000`, `"500:"` = `500..=f32::MAX`).
fn parse_f32_range(s: &str) -> Result<std::ops::RangeInclusive<f32>> {
    let (lo, hi) = s
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("range must be MIN:MAX (got {s:?})"))?;
    let lo = if lo.is_empty() { 0.0 } else { lo.parse()? };
    let hi = if hi.is_empty() { f32::MAX } else { hi.parse()? };
    Ok(lo..=hi)
}

fn parse_u16_range(s: &str) -> Result<std::ops::RangeInclusive<u16>> {
    let (lo, hi) = s
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("range must be MIN:MAX (got {s:?})"))?;
    let lo = if lo.is_empty() { 0 } else { lo.parse()? };
    let hi = if hi.is_empty() { u16::MAX } else { hi.parse()? };
    Ok(lo..=hi)
}

fn run_extract(files: &[PathBuf]) -> Result<()> {
    use anyhow::Context;
    use gldf_rs::gldf::GldfProduct;
    use gldf_search_gldf::{extract, ExtractInput};
    use gldf_search_schema::doc::SourceRef;

    for (i, path) in files.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let path_str = path.to_string_lossy().into_owned();
        let gldf = GldfProduct::load_gldf(&path_str)
            .with_context(|| format!("load_gldf({})", path.display()))?;
        let out = extract(
            &gldf,
            ExtractInput {
                source: Some(SourceRef::Path(path_str.clone().into())),
                ..Default::default()
            },
        );
        println!("FILE  {}", path.display());
        println!(
            "{}",
            serde_json::to_string_pretty(&out.doc).with_context(|| "serialise LuminaireDoc")?
        );
        if !out.warnings.is_empty() {
            println!("WARNINGS  ({}):", out.warnings.len());
            for w in &out.warnings {
                println!("  - {w:?}");
            }
        }
    }
    Ok(())
}

fn run_build_index(root: Option<PathBuf>, out: Option<PathBuf>, threads: usize) -> Result<()> {
    use anyhow::Context;

    let root = resolve_corpus_root(root.as_deref())?;
    let cache_path = match out {
        Some(p) => p,
        None => match std::env::var("GLDF_SEARCH_INDEX_CACHE")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            Some(p) => PathBuf::from(p),
            None => root.join(".index.bin"),
        },
    };

    if threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .context("rayon ThreadPoolBuilder::build_global")?;
    }
    let pool_size = rayon::current_num_threads();

    let root_canon = root
        .canonicalize()
        .with_context(|| format!("canonicalize({})", root.display()))?;

    // Step 1: walk to collect every .gldf path. Serial — directory
    // I/O is fast and rayon can't help with stat() bottlenecks.
    let walk_started = std::time::Instant::now();
    let mut paths: Vec<PathBuf> = Vec::new();
    walk_collect_gldfs(&root_canon, &mut paths)?;
    let n_files = paths.len();
    eprintln!(
        "found {n_files} .gldf files under {} in {:.1?}",
        root_canon.display(),
        walk_started.elapsed()
    );
    if n_files == 0 {
        anyhow::bail!("no .gldf files under {}", root_canon.display());
    }

    // Step 2: parallel extract. Each thread opens its own file, parses,
    // and runs the extractor. Errors are logged + the doc dropped —
    // partial indexing is better than failing the whole build.
    eprintln!("extracting with {pool_size} worker threads …");
    let extract_started = std::time::Instant::now();
    let docs = parallel_extract(&paths, &root_canon);
    eprintln!(
        "extracted {} of {n_files} docs in {:.1?}",
        docs.len(),
        extract_started.elapsed()
    );

    // Step 3: serialise + atomically write. The cache stores the
    // **un-folded** per-SKU docs — fold-and-alias happens fresh at
    // server boot. We keep it this way so source DocIds aren't lost
    // (the alias map isn't persisted in the cache format), and so
    // re-folding on boot is idempotent against the on-disk shape.
    // RAM savings come at boot, not on disk.
    let write_started = std::time::Instant::now();
    let n = docs.len();
    gldf_search_gldf::cache::write(&cache_path, &docs)?;
    eprintln!(
        "wrote cache to {} in {:.1?} ({} docs)",
        cache_path.display(),
        write_started.elapsed(),
        n
    );
    Ok(())
}

/// Recursive serial walk that fills `out` with the absolute path of
/// every `.gldf` under `dir`. Splitting walk from extract lets us
/// parallelise the expensive half (parse) without paying for parallel
/// directory traversal — `read_dir` is cheap.
fn walk_collect_gldfs(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> Result<()> {
    use anyhow::Context;

    let entries = std::fs::read_dir(dir).with_context(|| format!("read_dir({})", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ftype = entry.file_type()?;
        if ftype.is_dir() {
            walk_collect_gldfs(&path, out)?;
            continue;
        }
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gldf"))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Parallel extract over a list of absolute `.gldf` paths. Returns
/// `LuminaireDoc`s in arbitrary order — the in-memory index doesn't
/// care, and skipping the order constraint avoids a global sync.
fn parallel_extract(
    paths: &[PathBuf],
    root: &std::path::Path,
) -> Vec<gldf_search_schema::doc::LuminaireDoc> {
    use gldf_rs::gldf::GldfProduct;
    use gldf_search_gldf::{extract, ExtractInput};
    use gldf_search_schema::doc::SourceRef;
    use rayon::prelude::*;

    paths
        .par_iter()
        .filter_map(|path| {
            let path_str = path.to_string_lossy().into_owned();
            // Path relative to the corpus root for `SourceRef::Path`.
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| {
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path_str.clone())
                });
            // DocId fallback: BLAKE3 the relative corpus path. Many
            // GLDFs (e.g. Deko-Light's family.gldf exports) omit
            // `<UniqueGldfId>` — without a fallback every such doc
            // collides on the all-zero sentinel.
            //
            // Path-based (not byte-hashed) intentionally:
            // - **Reproducible** across re-indexing — same file at
            //   same path keeps the same DocId, so URL bookmarks and
            //   search ranks don't ghost when a manufacturer ships
            //   a v2 of the GLDF with the same filename.
            // - **Free** — no second `std::fs::read` per doc. The
            //   previous content-hash fallback doubled the wall-clock
            //   (~8 min → 15 min) because each rayon worker was
            //   slurping the whole file before the zip parse, and on
            //   external storage the bus serialises.
            // - **Unique** — filesystem paths are unique by
            //   construction. We feed the relative path (e.g.
            //   `deko-light/aaron/aaron.gldf`) so the same corpus on
            //   different machines / mount points produces the same
            //   DocId.
            let path_hash_input = format!("path:{}", rel);
            match GldfProduct::load_gldf(&path_str) {
                Ok(gldf) => {
                    let out = extract(
                        &gldf,
                        ExtractInput {
                            source: Some(SourceRef::Path(rel.into())),
                            raw_bytes: Some(path_hash_input.as_bytes()),
                            ..Default::default()
                        },
                    );
                    Some(out.doc)
                }
                Err(err) => {
                    eprintln!("skip {}: {err:#}", path.display());
                    None
                }
            }
        })
        .collect()
}

fn run_inspect(files: &[PathBuf]) -> Result<()> {
    for (i, path) in files.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let report = inspect::inspect(path)?;
        inspect::print_report(&report);
    }
    Ok(())
}

fn run_sample(root: PathBuf, out: PathBuf, n: usize, seed: Option<u64>, force: bool) -> Result<()> {
    let outcome = corpus::run(&corpus::SampleArgs {
        root,
        out,
        n,
        seed,
        force,
    })?;

    let m = &outcome.manifest;
    let total_bytes: u64 = m.entries.iter().map(|e| e.size_bytes).sum();
    let seed_part = match m.seed {
        Some(s) => format!(" (seed={s})"),
        None => String::new(),
    };
    println!(
        "sampled {} of {} .gldf files{seed_part} → {}",
        m.entries.len(),
        m.source_count,
        outcome.out_dir.display(),
    );
    println!(
        "total {} bytes across {} files (manifest at {})",
        total_bytes,
        m.entries.len(),
        outcome.out_dir.join("manifest.json").display(),
    );
    Ok(())
}

/// Mirror of `gldf-search-server::resolve_corpus_root`. Resolution
/// order: `--root` arg → `DEBUG_GLDF_SEARCH_CORPUS` → `GLDF_SEARCH_CORPUS`
/// → error. Same `.env` keys work for both CLI search and the server.
fn resolve_corpus_root(cli_value: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = cli_value {
        return Ok(p.to_path_buf());
    }
    let pick = |key: &str| -> Option<PathBuf> {
        std::env::var(key)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = pick("DEBUG_GLDF_SEARCH_CORPUS") {
        return Ok(p);
    }
    if let Some(p) = pick("GLDF_SEARCH_CORPUS") {
        return Ok(p);
    }
    anyhow::bail!(
        "no corpus root configured: pass `--root <DIR>`, or set \
         `GLDF_SEARCH_CORPUS` / `DEBUG_GLDF_SEARCH_CORPUS` in .env"
    )
}
