//! Load every `.gldf` in a directory and extract into
//! [`LuminaireDoc`]s, with an on-disk cache so server restarts don't
//! re-parse the whole corpus.
//!
//! Two entry points:
//! - [`load_and_extract`] — always re-extract from the corpus on disk.
//!   Used by the indexer CLI (where freshness *is* the point).
//! - [`load_with_cache`] — uses the bincode cache via
//!   `gldf_search_gldf::cache` when fresh; falls through to a fresh
//!   extract + cache rewrite otherwise.
//!
//! The cache is a **pure performance optimisation**. Any deserialise
//! failure (older bincode payload, schema change, truncated file)
//! triggers a fresh extract — the server never refuses to start
//! because the cache is stale or broken.
//!
//! The walk is **recursive**. The corpus may be flat (one .gldf per
//! file at the root) or nested (`slug/family/article.gldf`); the
//! `SourceRef::Path` stored on each doc carries the relative path
//! from the corpus root so URLs and the cache survive either shape.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gldf_rs::gldf::GldfProduct;
use gldf_search_gldf::{cache, extract, ExtractInput};
use gldf_search_schema::doc::{LuminaireDoc, SourceRef};

/// Load the corpus, preferring a cached `Vec<LuminaireDoc>` on disk.
///
/// **Trust the cache unconditionally.** If `cache_path` exists and
/// decodes cleanly (magic byte check + bincode deserialise), the
/// docs come from there — regardless of the corpus directory's
/// mtime. This lets us build the index on a fast multi-core
/// machine, rsync the bincode file to a single-core production
/// host, and have the server boot in seconds without re-extracting.
///
/// Cache write-back happens only when we had to extract — we don't
/// blindly rewrite a cache we just read.
///
/// If `cache_path` is `None`, this is equivalent to
/// [`load_and_extract`] — no read attempt, no write.
pub fn load_with_cache(root: &Path, cache_path: Option<&Path>) -> Result<Vec<LuminaireDoc>> {
    let Some(cache_path) = cache_path else {
        return load_and_extract(root);
    };

    if cache_path.exists() {
        match cache::read(cache_path) {
            Ok(docs) => {
                tracing::info!(
                    cache = %cache_path.display(),
                    docs = docs.len(),
                    "loaded corpus from cache"
                );
                return Ok(docs);
            }
            Err(err) => {
                tracing::warn!(
                    cache = %cache_path.display(),
                    error = %err,
                    "cache present but unreadable; re-extracting"
                );
            }
        }
    } else {
        tracing::info!(cache = %cache_path.display(), "cache absent; extracting");
    }

    let docs = load_and_extract(root)?;
    if let Err(err) = cache::write(cache_path, &docs) {
        // Non-fatal: we have the docs in memory.
        tracing::warn!(
            cache = %cache_path.display(),
            error = %err,
            "failed to write extract cache; will re-extract next start"
        );
    } else {
        tracing::info!(
            cache = %cache_path.display(),
            docs = docs.len(),
            "wrote extract cache"
        );
    }
    Ok(docs)
}

/// Walk `root` (recursively), load every `.gldf` via `gldf-rs`, run
/// the extractor, and collect into a `Vec<LuminaireDoc>`. Per-file
/// errors are logged at WARN and skipped — partial indexing is better
/// than refusing to start.
pub fn load_and_extract(root: &Path) -> Result<Vec<LuminaireDoc>> {
    let mut docs = Vec::new();
    let mut skipped = 0usize;
    let root_canon = root
        .canonicalize()
        .with_context(|| format!("canonicalize({})", root.display()))?;
    visit(&root_canon, &root_canon, &mut docs, &mut skipped)?;
    if skipped > 0 {
        tracing::warn!(skipped, "files were not loaded");
    }
    Ok(docs)
}

fn visit(dir: &Path, root: &Path, docs: &mut Vec<LuminaireDoc>, skipped: &mut usize) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("read_dir({})", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ftype = entry.file_type()?;
        if ftype.is_dir() {
            visit(&path, root, docs, skipped)?;
            continue;
        }
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gldf"))
        {
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        // `SourceRef::Path` carries the path relative to the corpus
        // root so the viewer link reconstructs the right URL whether
        // the corpus is flat or nested. The absolute filesystem path
        // is server-internal and shouldn't leak to clients.
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
                tracing::warn!(file = %path.display(), error = %err, "skipping unparseable .gldf");
                *skipped += 1;
            }
        }
    }
    Ok(())
}

/// Resolve the cache-file path from env + corpus root. Precedence:
/// 1. `GLDF_SEARCH_INDEX_CACHE` env var if set & non-empty
/// 2. `<corpus_root>/.index.bin` (hidden file inside the corpus dir
///    so an operator's `ls` doesn't trip on it)
///
/// Explicit empty string disables caching entirely.
pub fn resolve_cache_path(corpus_root: &Path) -> Option<PathBuf> {
    if let Ok(s) = std::env::var("GLDF_SEARCH_INDEX_CACHE") {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }
        return Some(PathBuf::from(trimmed));
    }
    Some(corpus_root.join(".index.bin"))
}
