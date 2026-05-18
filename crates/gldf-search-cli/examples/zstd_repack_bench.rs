//! Measure compression headroom from re-archiving GLDFs with Zstd
//! instead of deflate.
//!
//! Walks a corpus directory, picks N random `.gldf` files, decompresses
//! each entry, re-archives with Zstd (ZIP method 93), and reports
//! per-category size deltas plus a projection over the whole corpus.
//!
//! Run:
//!   cargo run --release --example zstd_repack_bench -p gldf-search-cli \
//!       -- --root /Volumes/tb4ssd/develeop/gldf-search-data/gldfs \
//!       --sample 50 --level 19
//!
//! Default sample=50, level=19 (max-quality Zstd). Higher level = slower
//! but tighter compression. 19 is a reasonable "production" target —
//! still fast at decompress time.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rand::seq::SliceRandom;
use rand::SeedableRng;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Category {
    Xml,
    Ldt,
    L3d,
    Model3d,
    Image,
    Other,
}

impl Category {
    fn classify(name: &str) -> Category {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".xml") {
            Category::Xml
        } else if lower.ends_with(".ldt") || lower.ends_with(".ies") {
            Category::Ldt
        } else if lower.ends_with(".l3d") {
            Category::L3d
        } else if lower.ends_with(".obj")
            || lower.ends_with(".gltf")
            || lower.ends_with(".glb")
            || lower.ends_with(".stl")
            || lower.ends_with(".mtl")
        {
            Category::Model3d
        } else if lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".webp")
        {
            Category::Image
        } else {
            Category::Other
        }
    }

    fn name(self) -> &'static str {
        match self {
            Category::Xml => "xml",
            Category::Ldt => "ldt/ies",
            Category::L3d => "l3d",
            Category::Model3d => "model",
            Category::Image => "image",
            Category::Other => "other",
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct SizeTally {
    raw: u64,
    deflate: u64,
    zstd: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Pick `sample` files uniformly at random with `seed`.
    Random,
    /// Pick the `sample` largest files in the corpus by raw byte size.
    /// Reveals whether big files (presumably 3D-heavy / image-heavy)
    /// compress meaningfully better than average.
    TopLargest,
}

struct Args {
    root: PathBuf,
    sample: usize,
    level: i32,
    seed: u64,
    mode: Mode,
}

fn parse_args() -> Args {
    let mut root: Option<PathBuf> = None;
    let mut sample: usize = 50;
    let mut level: i32 = 19;
    let mut seed: u64 = 42;
    let mut mode = Mode::Random;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                root = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--sample" => {
                sample = args[i + 1]
                    .parse()
                    .expect("sample must be a positive integer");
                i += 2;
            }
            "--level" => {
                level = args[i + 1]
                    .parse()
                    .expect("level must be an int (1..=22 for zstd)");
                i += 2;
            }
            "--seed" => {
                seed = args[i + 1].parse().expect("seed must be u64");
                i += 2;
            }
            "--mode" => {
                mode = match args[i + 1].as_str() {
                    "random" => Mode::Random,
                    "top-largest" | "top" => Mode::TopLargest,
                    other => panic!("--mode must be 'random' or 'top-largest', got {other}"),
                };
                i += 2;
            }
            other => {
                panic!("unexpected arg: {other}");
            }
        }
    }
    let root = root
        .or_else(|| {
            std::env::var("LOCAL_GLDF_SEARCH_CORPUS")
                .ok()
                .map(PathBuf::from)
        })
        .expect("pass --root /path/to/corpus or set LOCAL_GLDF_SEARCH_CORPUS");
    Args {
        root,
        sample,
        level,
        seed,
        mode,
    }
}

fn main() -> Result<()> {
    let _ = dotenvy::dotenv(); // Pick up LOCAL_GLDF_SEARCH_CORPUS from .env.
    let args = parse_args();
    let Args {
        root,
        sample: sample_size,
        level,
        seed,
        mode,
    } = args;

    println!("corpus root: {}", root.display());
    println!("sample size: {sample_size}");
    println!("zstd level:  {level}");
    println!("mode:        {:?}", mode);

    let all_gldfs: Vec<PathBuf> = walk_gldfs(&root)?;
    println!("found {} .gldf files in corpus", all_gldfs.len());

    if all_gldfs.is_empty() {
        anyhow::bail!("no .gldf files found under {}", root.display());
    }

    let sample_paths: Vec<PathBuf> = match mode {
        Mode::Random => {
            // Deterministic sample so reruns compare apples-to-apples.
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let mut shuffled = all_gldfs.clone();
            shuffled.shuffle(&mut rng);
            shuffled.into_iter().take(sample_size).collect()
        }
        Mode::TopLargest => {
            // Stat every file, pick the N largest. Streaming
            // max-heap would be cheaper for huge corpora; the full
            // sort is fine for 270k entries.
            eprintln!(
                "stat'ing {} files to find top-{sample_size} by size…",
                all_gldfs.len()
            );
            let mut sized: Vec<(u64, PathBuf)> = all_gldfs
                .iter()
                .filter_map(|p| std::fs::metadata(p).ok().map(|m| (m.len(), p.clone())))
                .collect();
            sized.sort_by(|a, b| b.0.cmp(&a.0));
            sized
                .into_iter()
                .take(sample_size)
                .map(|(_, p)| p)
                .collect()
        }
    };
    let sample: Vec<&PathBuf> = sample_paths.iter().collect();

    let mut per_cat: BTreeMap<&'static str, SizeTally> = BTreeMap::new();
    let mut per_file_deflate: u64 = 0;
    let mut per_file_zstd: u64 = 0;
    let mut per_file_raw: u64 = 0;
    let mut failures: u32 = 0;
    let mut bench_files: u32 = 0;

    for path in &sample {
        match process_one(path, level) {
            Ok((file_raw, file_deflate, file_zstd, per_entry)) => {
                per_file_raw += file_raw;
                per_file_deflate += file_deflate;
                per_file_zstd += file_zstd;
                for (cat, tally) in per_entry {
                    let entry = per_cat.entry(cat.name()).or_default();
                    entry.raw += tally.raw;
                    entry.deflate += tally.deflate;
                    entry.zstd += tally.zstd;
                }
                bench_files += 1;
            }
            Err(e) => {
                eprintln!("skip {}: {e:#}", path.display());
                failures += 1;
            }
        }
    }

    println!();
    println!("=== per-category totals across sample ===");
    println!(
        "{:<8}  {:>14}  {:>14}  {:>14}  {:>8}  {:>8}",
        "category", "raw", "deflate", "zstd", "z/d %", "z/raw %"
    );
    for (cat, tally) in &per_cat {
        let zd = pct(tally.zstd as f64, tally.deflate as f64);
        let zr = pct(tally.zstd as f64, tally.raw as f64);
        println!(
            "{:<8}  {:>14}  {:>14}  {:>14}  {:>7.1}%  {:>7.1}%",
            cat,
            human_bytes(tally.raw),
            human_bytes(tally.deflate),
            human_bytes(tally.zstd),
            zd,
            zr,
        );
    }

    println!();
    println!("=== per-file totals ===");
    println!(
        "{:<14}  {:>14}",
        "raw (unzipped)",
        human_bytes(per_file_raw)
    );
    println!(
        "{:<14}  {:>14}",
        "deflate (.gldf today)",
        human_bytes(per_file_deflate)
    );
    println!(
        "{:<14}  {:>14}",
        "zstd (re-archived)",
        human_bytes(per_file_zstd)
    );
    if per_file_deflate > 0 {
        let ratio = per_file_zstd as f64 / per_file_deflate as f64;
        let savings_pct = (1.0 - ratio) * 100.0;
        println!(
            "{:<14}  {:>13.1}% smaller than deflate (ratio {:.3})",
            "zstd savings", savings_pct, ratio
        );
    }

    if failures > 0 {
        println!("{failures} files failed to repack (excluded from totals)");
    }

    // Whole-corpus projection.
    if bench_files > 0 && !all_gldfs.is_empty() {
        let avg_deflate = per_file_deflate as f64 / bench_files as f64;
        let avg_zstd = per_file_zstd as f64 / bench_files as f64;
        let corpus_n = all_gldfs.len() as f64;
        let projected_now = avg_deflate * corpus_n;
        let projected_zstd = avg_zstd * corpus_n;
        let savings = projected_now - projected_zstd;
        println!();
        println!(
            "=== whole-corpus projection ({} files) ===",
            all_gldfs.len()
        );
        println!("current (deflate):  {}", human_bytes(projected_now as u64));
        println!("after zstd repack:  {}", human_bytes(projected_zstd as u64));
        println!(
            "saved:              {}  ({:.1}%)",
            human_bytes(savings as u64),
            (savings / projected_now) * 100.0
        );

        // Also project to 450k corpus.
        let target = 450_000.0_f64;
        let projected_now_target = avg_deflate * target;
        let projected_zstd_target = avg_zstd * target;
        let saved_target = projected_now_target - projected_zstd_target;
        println!();
        println!("=== projection to 450 000 files ===");
        println!(
            "current (deflate):  {}",
            human_bytes(projected_now_target as u64)
        );
        println!(
            "after zstd repack:  {}",
            human_bytes(projected_zstd_target as u64)
        );
        println!(
            "saved:              {}  ({:.1}%)",
            human_bytes(saved_target as u64),
            (saved_target / projected_now_target) * 100.0
        );
    }

    Ok(())
}

/// Walk a directory tree, collecting every `.gldf` file.
fn walk_gldfs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let read =
            std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))?;
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("gldf"))
                .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// Read one GLDF, decompress entries, write a fresh Zstd-compressed
/// archive into a Vec<u8>. Returns `(raw_bytes_total,
/// deflate_archive_size, zstd_archive_size, per_category_tally)`.
fn process_one(
    path: &Path,
    zstd_level: i32,
) -> Result<(u64, u64, u64, BTreeMap<Category, SizeTally>)> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let deflate_size = bytes.len() as u64;

    let mut zip_in = zip::ZipArchive::new(Cursor::new(bytes.as_slice()))
        .with_context(|| format!("ZipArchive::new {}", path.display()))?;

    let mut raw_total: u64 = 0;
    let mut per_cat: BTreeMap<Category, SizeTally> = BTreeMap::new();
    let mut entries: Vec<(String, Vec<u8>, Category)> = Vec::with_capacity(zip_in.len());

    for i in 0..zip_in.len() {
        let mut entry = zip_in
            .by_index(i)
            .with_context(|| format!("by_index {i} in {}", path.display()))?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let compressed_size = entry.compressed_size();
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buf)
            .with_context(|| format!("read entry {name} in {}", path.display()))?;
        let cat = Category::classify(&name);
        let tally = per_cat.entry(cat).or_default();
        tally.raw += buf.len() as u64;
        tally.deflate += compressed_size;
        raw_total += buf.len() as u64;
        entries.push((name, buf, cat));
    }

    // Write re-archived ZIP with Zstd.
    let mut out = Cursor::new(Vec::<u8>::with_capacity(deflate_size as usize));
    {
        let mut zip_out = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Zstd)
            .compression_level(Some(zstd_level.into()));
        for (name, data, cat) in &entries {
            zip_out
                .start_file(name, opts)
                .with_context(|| format!("start_file {name}"))?;
            zip_out.write_all(data)?;
            // We don't know the per-entry compressed size until the
            // writer is finalised — measure after.
            let _ = cat;
        }
        zip_out.finish().context("ZipWriter finish")?;
    }
    let zstd_archive = out.into_inner();
    let zstd_archive_size = zstd_archive.len() as u64;

    // Now read the new archive back to fill per-entry zstd sizes.
    let mut zip_back = zip::ZipArchive::new(Cursor::new(zstd_archive.as_slice()))?;
    for i in 0..zip_back.len() {
        let entry = zip_back.by_index(i)?;
        if !entry.is_file() {
            continue;
        }
        let cat = Category::classify(entry.name());
        let tally = per_cat.entry(cat).or_default();
        tally.zstd += entry.compressed_size();
    }

    Ok((raw_total, deflate_size, zstd_archive_size, per_cat))
}

fn pct(num: f64, den: f64) -> f64 {
    if den == 0.0 {
        0.0
    } else {
        (num / den) * 100.0
    }
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

// Implement Default for SizeTally manually — derived already done; keep
// for clarity.
impl Default for &SizeTally {
    fn default() -> Self {
        &SizeTally {
            raw: 0,
            deflate: 0,
            zstd: 0,
        }
    }
}

// Quiet unused imports if any.
#[allow(dead_code)]
fn _force_link(_: &File) {}
