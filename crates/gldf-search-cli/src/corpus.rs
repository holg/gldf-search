//! Corpus sampling. Walks a flat directory of `.gldf` files, picks N at
//! random (reservoir sampling, single pass, constant memory), copies the
//! selected files into an output directory, and writes a manifest so the
//! sample can be reproduced or audited later.
//!
//! The sampler is deliberately independent of `clap` so the logic is
//! straightforward to unit-test.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context, Result};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

/// Configuration for one sampling run.
#[derive(Debug, Clone)]
pub struct SampleArgs {
    /// Source directory holding the `.gldf` corpus. Must exist; must
    /// be a directory; is read non-recursively (the corpus is flat).
    pub root: PathBuf,
    /// Destination directory. Created if missing. Refuses to overwrite
    /// non-empty existing dirs unless `force` is set.
    pub out: PathBuf,
    /// Sample size. Must be > 0.
    pub n: usize,
    /// Optional deterministic seed for the RNG. `None` uses
    /// `rand::thread_rng()` (fresh sample every run).
    pub seed: Option<u64>,
    /// If `out` exists and is non-empty, clear it first. Otherwise
    /// the run aborts so we never silently mix two samples.
    pub force: bool,
}

/// Manifest persisted to `<out>/manifest.json`. Round-trips through
/// `serde_json` so a future `corpus reproduce` subcommand can rebuild
/// an identical sample given the same source corpus + seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// `gldf-search-cli` version that produced this sample.
    pub generator_version: String,
    /// Source corpus root, absolute path at sampling time.
    pub source_root: PathBuf,
    /// Total number of `.gldf` files seen in `source_root` (the
    /// reservoir's denominator).
    pub source_count: usize,
    /// Sample size requested.
    pub requested_n: usize,
    /// Seed used (`None` for non-deterministic runs). Reproducing a
    /// non-deterministic sample is impossible by design.
    pub seed: Option<u64>,
    /// Sampling timestamp (Unix epoch seconds). For audit only.
    pub sampled_at_epoch_s: u64,
    /// One entry per copied file.
    pub entries: Vec<ManifestEntry>,
}

/// One sample entry. `source_path` plus `blake3` together uniquely
/// identify the file; `dest_name` is preserved verbatim from the source
/// so manufacturer/series info encoded in the filename survives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Absolute path of the file in the source corpus.
    pub source_path: PathBuf,
    /// Filename (basename) under the destination directory. Equal to
    /// `source_path.file_name()` — we preserve the corpus naming.
    pub dest_name: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// BLAKE3 hash of the file contents (hex). Doubles as the future
    /// `DocId` once the extractor lands, so this is committed-to.
    pub blake3: String,
    /// Source mtime (Unix epoch seconds), if the platform supports it.
    pub source_mtime_epoch_s: Option<u64>,
}

/// Reservoir-sample N filenames from a flat directory of `.gldf` files.
///
/// Single pass over `read_dir`, O(n) memory in the sample size, O(C)
/// runtime in the corpus size. Returns the sampled paths plus the total
/// `.gldf` count seen (the reservoir's denominator — recorded in the
/// manifest).
pub fn reservoir_sample(root: &Path, n: usize, seed: Option<u64>) -> Result<(Vec<PathBuf>, usize)> {
    if n == 0 {
        bail!("sample size must be > 0");
    }
    if !root.is_dir() {
        bail!("source root is not a directory: {}", root.display());
    }

    let entries = fs::read_dir(root).with_context(|| format!("read_dir({})", root.display()))?;

    // Two RNG paths so the seeded code stays easy to reason about.
    // The seeded path is reproducible verbatim across machines; the
    // unseeded path uses thread-local entropy and is deliberately
    // un-reproducible.
    let mut reservoir: Vec<PathBuf> = Vec::with_capacity(n);
    let mut total = 0usize;

    if let Some(s) = seed {
        let mut rng = StdRng::seed_from_u64(s);
        scan(entries, n, &mut reservoir, &mut total, &mut rng)?;
    } else {
        let mut rng = rand::thread_rng();
        scan(entries, n, &mut reservoir, &mut total, &mut rng)?;
    }

    if total == 0 {
        bail!(
            "no .gldf files found under {} (sampled 0 of 0)",
            root.display()
        );
    }
    Ok((reservoir, total))
}

fn scan(
    entries: fs::ReadDir,
    n: usize,
    reservoir: &mut Vec<PathBuf>,
    total: &mut usize,
    rng: &mut impl Rng,
) -> Result<()> {
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gldf"))
        {
            continue;
        }
        // Algorithm R: keep first n outright, then for item i (1-indexed
        // beyond the reservoir) replace a random reservoir slot with
        // probability n / i.
        *total += 1;
        if reservoir.len() < n {
            reservoir.push(path);
        } else {
            let j = rng.gen_range(0..*total);
            if j < n {
                reservoir[j] = path;
            }
        }
    }
    Ok(())
}

/// Result of one sampling run — manifest plus the resolved (canonical)
/// output directory, so callers can print it without re-deriving it.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// The persisted manifest.
    pub manifest: Manifest,
    /// Canonical absolute path of the destination directory.
    pub out_dir: PathBuf,
}

/// End-to-end: validate args, sample, copy, hash, write manifest.
pub fn run(args: &SampleArgs) -> Result<RunOutcome> {
    if args.n == 0 {
        bail!("sample size must be > 0");
    }
    let root = args
        .root
        .canonicalize()
        .with_context(|| format!("canonicalize source root: {}", args.root.display()))?;

    prepare_out_dir(&args.out, args.force)?;
    let out = args
        .out
        .canonicalize()
        .with_context(|| format!("canonicalize destination: {}", args.out.display()))?;

    let (sampled, source_count) = reservoir_sample(&root, args.n, args.seed)?;
    if sampled.is_empty() {
        bail!("no .gldf files matched in {}", root.display());
    }

    let mut entries = Vec::with_capacity(sampled.len());
    for src in &sampled {
        let entry = copy_one(src, &out)?;
        entries.push(entry);
    }

    let manifest = Manifest {
        generator_version: env!("CARGO_PKG_VERSION").to_string(),
        source_root: root,
        source_count,
        requested_n: args.n,
        seed: args.seed,
        sampled_at_epoch_s: epoch_seconds_now(),
        entries,
    };

    write_manifest(&out, &manifest)?;
    Ok(RunOutcome {
        manifest,
        out_dir: out,
    })
}

fn prepare_out_dir(out: &Path, force: bool) -> Result<()> {
    if !out.exists() {
        fs::create_dir_all(out).with_context(|| format!("create_dir_all({})", out.display()))?;
        return Ok(());
    }
    if !out.is_dir() {
        bail!(
            "destination exists but is not a directory: {}",
            out.display()
        );
    }
    let is_empty = fs::read_dir(out)
        .with_context(|| format!("read_dir({})", out.display()))?
        .next()
        .is_none();
    if is_empty {
        return Ok(());
    }
    if !force {
        bail!(
            "destination {} is not empty (pass --force to clear it)",
            out.display()
        );
    }
    // --force: clear contents but keep the directory itself.
    for entry in fs::read_dir(out)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("remove_dir_all({})", path.display()))?;
        } else {
            fs::remove_file(&path).with_context(|| format!("remove_file({})", path.display()))?;
        }
    }
    Ok(())
}

fn copy_one(src: &Path, out_dir: &Path) -> Result<ManifestEntry> {
    let dest_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("non-UTF-8 filename: {}", src.display()))?
        .to_string();
    let dest = out_dir.join(&dest_name);

    // Stream-copy + hash in one pass, avoids reading the file twice on
    // a 270k-file scan and a 90KB-average payload.
    let mut reader =
        fs::File::open(src).with_context(|| format!("open source: {}", src.display()))?;
    let mut writer =
        fs::File::create(&dest).with_context(|| format!("create dest: {}", dest.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size_bytes = 0u64;
    loop {
        let read = reader.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        writer.write_all(&buf[..read])?;
        size_bytes += read as u64;
    }
    writer.flush()?;
    drop(writer);

    let blake3 = hasher.finalize().to_hex().to_string();
    let source_mtime_epoch_s = src
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());

    Ok(ManifestEntry {
        source_path: src.to_path_buf(),
        dest_name,
        size_bytes,
        blake3,
        source_mtime_epoch_s,
    })
}

fn write_manifest(out: &Path, manifest: &Manifest) -> Result<()> {
    let path = out.join("manifest.json");
    let json = serde_json::to_string_pretty(manifest)?;
    fs::write(&path, json).with_context(|| format!("write manifest: {}", path.display()))?;
    Ok(())
}

fn epoch_seconds_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Convenience wrapper used by the io path; kept here so unit tests can
// call it without depending on the std::io re-export at the call site.
#[allow(dead_code)]
fn _io_marker(_: io::Result<()>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn write_dummy_gldf(dir: &Path, name: &str, contents: &[u8]) {
        let p = dir.join(name);
        fs::write(&p, contents).unwrap();
    }

    /// Reservoir over a tiny synthetic corpus. Same seed → same sample.
    #[test]
    fn seeded_sample_is_deterministic() {
        let tmp = tempdir();
        let root = tmp.join("src");
        fs::create_dir_all(&root).unwrap();
        for i in 0..50 {
            write_dummy_gldf(&root, &format!("file{i:02}.gldf"), &[i as u8; 16]);
        }

        let (a, total_a) = reservoir_sample(&root, 5, Some(42)).unwrap();
        let (b, total_b) = reservoir_sample(&root, 5, Some(42)).unwrap();
        assert_eq!(total_a, 50);
        assert_eq!(total_b, 50);
        assert_eq!(
            a.iter().map(|p| p.file_name().unwrap()).collect::<Vec<_>>(),
            b.iter().map(|p| p.file_name().unwrap()).collect::<Vec<_>>(),
            "seeded reservoir must be reproducible"
        );
    }

    /// Different seeds produce different samples on a corpus large
    /// enough that overlap is unlikely. Probabilistic but safe at this
    /// ratio (5 of 200, two seeds collide on all 5 ≈ 10^-9).
    #[test]
    fn different_seeds_likely_differ() {
        let tmp = tempdir();
        let root = tmp.join("src");
        fs::create_dir_all(&root).unwrap();
        for i in 0..200 {
            write_dummy_gldf(&root, &format!("f{i:03}.gldf"), b"x");
        }

        let (a, _) = reservoir_sample(&root, 5, Some(1)).unwrap();
        let (b, _) = reservoir_sample(&root, 5, Some(2)).unwrap();
        let set_a: HashSet<_> = a
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        let set_b: HashSet<_> = b
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        assert_ne!(
            set_a, set_b,
            "two random seeds happened to match — extremely unlikely"
        );
    }

    /// Files without a .gldf extension are skipped. The total reflects
    /// only matched files.
    #[test]
    fn ignores_non_gldf_files() {
        let tmp = tempdir();
        let root = tmp.join("src");
        fs::create_dir_all(&root).unwrap();
        for i in 0..10 {
            write_dummy_gldf(&root, &format!("hit{i}.gldf"), b"x");
        }
        write_dummy_gldf(&root, "skip.txt", b"x");
        write_dummy_gldf(&root, "skip.json", b"x");

        let (sampled, total) = reservoir_sample(&root, 3, Some(7)).unwrap();
        assert_eq!(total, 10);
        assert_eq!(sampled.len(), 3);
        for p in sampled {
            assert!(p.extension().unwrap().eq_ignore_ascii_case("gldf"));
        }
    }

    /// Reservoir size cap: if the corpus is smaller than n, return all
    /// of it (no padding, no error).
    #[test]
    fn corpus_smaller_than_n_returns_all() {
        let tmp = tempdir();
        let root = tmp.join("src");
        fs::create_dir_all(&root).unwrap();
        for i in 0..3 {
            write_dummy_gldf(&root, &format!("f{i}.gldf"), b"x");
        }
        let (sampled, total) = reservoir_sample(&root, 10, Some(1)).unwrap();
        assert_eq!(total, 3);
        assert_eq!(sampled.len(), 3);
    }

    /// End-to-end run: copies, hashes, manifest is round-trippable.
    #[test]
    fn run_copies_and_hashes() {
        let tmp = tempdir();
        let root = tmp.join("src");
        let out = tmp.join("out");
        fs::create_dir_all(&root).unwrap();
        for i in 0..20 {
            write_dummy_gldf(&root, &format!("f{i:02}.gldf"), &[i as u8; 256]);
        }

        let outcome = run(&SampleArgs {
            root: root.clone(),
            out: out.clone(),
            n: 5,
            seed: Some(99),
            force: false,
        })
        .unwrap();

        let manifest = &outcome.manifest;
        assert_eq!(manifest.entries.len(), 5);
        assert_eq!(manifest.source_count, 20);
        assert_eq!(manifest.seed, Some(99));

        for entry in &manifest.entries {
            let copied = outcome.out_dir.join(&entry.dest_name);
            assert!(copied.is_file(), "missing copy {}", copied.display());
            assert_eq!(fs::metadata(&copied).unwrap().len(), entry.size_bytes);
            assert_eq!(entry.blake3.len(), 64, "blake3 hex length");
        }

        // Manifest round-trip.
        let json = fs::read_to_string(outcome.out_dir.join("manifest.json")).unwrap();
        let parsed: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entries.len(), manifest.entries.len());
    }

    /// Refuses non-empty out dir without --force.
    #[test]
    fn refuses_nonempty_out_without_force() {
        let tmp = tempdir();
        let root = tmp.join("src");
        let out = tmp.join("out");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("preexisting.txt"), b"x").unwrap();
        for i in 0..5 {
            write_dummy_gldf(&root, &format!("f{i}.gldf"), b"x");
        }
        let err = run(&SampleArgs {
            root,
            out,
            n: 2,
            seed: Some(1),
            force: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("--force"), "got: {err}");
    }

    /// --force clears existing contents.
    #[test]
    fn force_clears_existing() {
        let tmp = tempdir();
        let root = tmp.join("src");
        let out = tmp.join("out");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("stale.txt"), b"x").unwrap();
        for i in 0..5 {
            write_dummy_gldf(&root, &format!("f{i}.gldf"), b"x");
        }
        run(&SampleArgs {
            root,
            out: out.clone(),
            n: 2,
            seed: Some(1),
            force: true,
        })
        .unwrap();
        assert!(
            !out.join("stale.txt").exists(),
            "--force did not clear stale file"
        );
    }

    /// Tiny tempdir helper; std::env::temp_dir() with a per-test suffix
    /// so parallel test runs don't collide.
    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "gldf-search-cli-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }
}
