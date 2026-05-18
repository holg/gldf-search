//! On-disk cache of extracted `Vec<LuminaireDoc>`.
//!
//! The server (and the `gldf-search build-index` CLI) serialises the
//! extracted corpus to a single bincode file with a short
//! magic+version prefix. On restart, the server skips re-parsing and
//! just deserialises — turning a multi-minute cold start at hundreds
//! of thousands of docs into a sub-second warm start.
//!
//! Format:
//! ```text
//! "GSCACHE\x01" || bincode::serialize(Vec<LuminaireDoc>)
//! ```
//!
//! Bump the version byte (`\x01` → `\x02`) whenever
//! [`gldf_search_schema::doc::LuminaireDoc`] changes shape. Old
//! caches fail the magic check and the server falls back to a fresh
//! extract.
//!
//! **The cache is trusted as-is.** No mtime / freshness check —
//! that's the operator's responsibility. Built locally on a fast
//! multi-core machine, the cache file is rsync'd to the (single-
//! core) server and the server boots from it. If you want to force
//! a re-extract, delete the cache file before starting the server.
//!
//! The cache is a **pure performance optimisation** — its absence or
//! corruption never blocks server startup.

use std::path::Path;

use anyhow::{Context, Result};
use gldf_search_schema::doc::LuminaireDoc;

/// Magic + version. 8 bytes for cheap O(1) detection.
///
/// - `\x02` (2026-05-14): `VariantDoc` gained `mounting_place`
///   (rename of `geometry`), `mounting_type`, `recessed_depth_mm`.
/// - `\x03` (2026-05-14): `VariantDoc` gained `has_3d`.
/// - `\x04` (2026-05-14): `VariantDoc` gained `lamp_type`,
///   `control_gear_interfaces`, `has_emergency_lighting`,
///   `emergency_lighting_type`.
/// - `\x05` (2026-05-14): `PhotometryStats` gained `ldc_symmetry`,
///   `c_angles_deg`, `g_angles_deg`, `intensities` (f32 matrix).
/// - `\x06` (2026-05-14): rolled back \x05. The f32 matrix ballooned
///   the cache to 8.8 GB on 270k docs — too big for the production
///   server's 7.7 GB RAM. Polar / cartesian / butterfly rendering
///   now happens **in the browser** via `eulumdat-rs` compiled to
///   wasm: the server fn `fetch_ldt_bytes` ships the raw LDT text
///   on demand, the hydrate bundle parses + renders.
/// - `\x07-skipped` (2026-05-15): RAM-layout pass. `LuminaireDoc` and
///   `VariantDoc` SmallVec inline sizes shrunk to match the corpus
///   (variants 4→1, keywords 8→2, descriptions 4→1) — saved
///   ~175 MB resident on the 270k corpus. SmallVec wire format is
///   unchanged (serialises as Vec regardless of inline N), so the
///   existing `\x06` cache loads cleanly into the new build —
///   **magic bump skipped**.
/// - `\x07` (2026-05-15): name-mined `ApplicationId`s now flow into
///   `LuminaireDoc.applications`. The `light-other-rs` 270k export
///   carries `DescriptiveAttributes/Marketing/Applications` on only
///   a small minority of docs, so the previous Applications facet
///   was effectively empty; the mining pass recognises XSD leaf
///   terms (Office, Industrial, Workshops, Streets, ...) in variant
///   `<Name>` strings and unions the matched ids into the doc's
///   applications. Wire format is unchanged (still
///   `SmallVec<ApplicationId>`) but the set of values changes for
///   most docs, so re-extraction is required to get coverage.
/// - `\x08` (2026-05-16): physical sanity caps applied to
///   `PhotometryStats` and `VariantDoc.recessed_depth_mm` at
///   extraction. The dual-slider rails in the UI were anchored on
///   parser-bug outliers (10 000 lm/W efficacy, 1 045° beam, 800 m
///   recessed depth). Values outside the envelopes in
///   `photometry::sanitize_photometry` / `sanitize_recessed_depth`
///   now become `None` before they hit the cache. Wire format
///   unchanged; outlier counts drop to zero so re-extraction is
///   required for the slider rails to reflect reality.
/// - `\x09` (2026-05-17): the `build-index` CLI now passes the raw
///   GLDF bytes into `ExtractInput.raw_bytes` so docs without a
///   `<UniqueGldfId>` header get a BLAKE3 fallback DocId instead of
///   the all-zeros sentinel. Without this, every Deko-Light family
///   GLDF (and any other corpus subset missing UniqueGldfId)
///   collided on the same DocId; later inserts evicted earlier ones
///   from `InMemoryIndex.by_id`. Wire format unchanged, but DocId
///   values change for every previously-colliding doc.
/// - `\x0a` (2026-05-18): DocId fallback changed from
///   `BLAKE3(file_bytes)` to `BLAKE3("path:" + relative_path)`.
///   Byte-hashing the full file doubled the rebuild wall-clock
///   (8 min → 15 min) on external storage because each rayon
///   worker slurped the whole .gldf before parsing the zip, and
///   the corpus bus serialised those reads. Path-based is free
///   AND reproducible: same file at same path = same DocId across
///   re-indexing, so URL bookmarks survive a manufacturer's v2
///   GLDF reshipping.
/// - `\x0b` (2026-05-18): group-and-fold dedup. Multiple GLDFs
///   that share `(manufacturer, product)` are folded into a single
///   `LuminaireDoc` whose `source_paths` lists every source file
///   and whose variants carry a `source_index` into that list. The
///   `by_id` map aliases each source DocId to the family doc so old
///   URLs survive. Wire format: new fields
///   `LuminaireDoc.source_paths` and `VariantDoc.source_index` (both
///   `#[serde(default)]`-friendly for forward-compat but the magic
///   bumps anyway because the values are essential for the lookup).
///
/// Old caches fail the magic check and the operator must rebuild.
pub const CACHE_MAGIC: &[u8; 8] = b"GSCACHE\x0b";

/// Read and decode a cache file. Returns the docs on success.
///
/// Caller is expected to have already decided the cache is fresh
/// (see [`is_fresh`]) — this function just deserialises.
pub fn read(path: &Path) -> Result<Vec<LuminaireDoc>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if bytes.len() < CACHE_MAGIC.len() || &bytes[..CACHE_MAGIC.len()] != CACHE_MAGIC {
        anyhow::bail!("cache magic mismatch (file: {})", path.display());
    }
    let payload = &bytes[CACHE_MAGIC.len()..];
    let docs: Vec<LuminaireDoc> =
        bincode::deserialize(payload).context("deserialize cache payload")?;
    Ok(docs)
}

/// Encode and write atomically. Serialises to `<path>.tmp` first then
/// renames over `path` so a half-written file never shadows a prior
/// good one. Creates the parent directory if absent.
pub fn write(path: &Path, docs: &[LuminaireDoc]) -> Result<()> {
    let payload = bincode::serialize(docs).context("serialize cache payload")?;
    let mut buf = Vec::with_capacity(CACHE_MAGIC.len() + payload.len());
    buf.extend_from_slice(CACHE_MAGIC);
    buf.extend_from_slice(&payload);

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir -p {}", parent.display()))?;
        }
    }
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, &buf).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;
    use gldf_search_schema::doc::{
        DocId, FileMeta, LuminaireDoc, MountingPlace, MountingType, SourceRef, VariantDoc,
        VariantId,
    };
    use smallvec::smallvec;

    fn sample_doc() -> LuminaireDoc {
        LuminaireDoc {
            id: DocId([7; 32]),
            source: SourceRef::ContentOnly,
            source_paths: smallvec![SourceRef::ContentOnly],
            manufacturer: CompactString::from("TestCo"),
            product: CompactString::from("TestProduct"),
            gtin: None,
            product_code: None,
            descriptions: smallvec![],
            keywords: smallvec![],
            applications: smallvec![],
            labels: smallvec![],
            adjustability: smallvec![],
            ip_code: None,
            safety_class: None,
            ik_rating: None,
            product_form: None,
            variants: smallvec![VariantDoc {
                id: VariantId(0),
                name: CompactString::from("v"),
                photometry: None,
                mounting_place: MountingPlace::Ceiling,
                mounting_type: MountingType::SurfaceMounted,
                recessed_depth_mm: None,
                has_3d: false,
                lamp_type: None,
                control_gear_interfaces: smallvec![],
                has_emergency_lighting: false,
                emergency_lighting_type: None,
                emitter_count: 1,
                source_index: 0,
            }],
            file_meta: FileMeta {
                size_bytes: 0,
                mtime_epoch_s: None,
                format_version: None,
            },
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let tmp = std::env::temp_dir().join(format!(
            "gldf-cache-test-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let docs = vec![sample_doc(), sample_doc()];
        write(&tmp, &docs).expect("write");
        let read_back = read(&tmp).expect("read");
        assert_eq!(read_back.len(), docs.len());
        assert_eq!(read_back[0].manufacturer, docs[0].manufacturer);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn read_rejects_missing_magic() {
        let tmp = std::env::temp_dir().join(format!(
            "gldf-cache-bad-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&tmp, b"not a cache").expect("write");
        let err = read(&tmp).expect_err("should reject");
        assert!(err.to_string().contains("magic mismatch"));
        std::fs::remove_file(&tmp).ok();
    }

}
