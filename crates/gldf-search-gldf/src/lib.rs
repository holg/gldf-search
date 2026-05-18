//! `gldf-search-gldf` — extractor from `gldf_rs::GldfProduct` to
//! `gldf_search_schema::LuminaireDoc`.
//!
//! The extractor is **pure**: no I/O, no zip access. Callers load the
//! GLDF via `gldf_rs::GldfProduct::load_gldf` (file path) or
//! `load_gldf_from_buf` (raw bytes — required for the browser path)
//! and pass the result here.
//!
//! Strategy and corpus background are documented in [`extract`].
//!
//! Submodules can be used directly for fine-grained operations:
//! - [`mining`] — name-mining (regex pass) for one variant string
//! - [`photometry`] — parse an LDT/EULUMDAT string into `PhotometryStats`
//! - [`manufacturer`] — manufacturer display/key normalisation
//! - [`geometry`] — `<Mountings>` → schema geometry collapse
//! - [`doc_id`] — DocId derivation rules
//! - [`locale`] — per-locale text gathering
//! - [`cache`] — bincode-backed on-disk cache so restarts don't re-extract

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod cache;
pub mod doc_id;
pub mod extract;
pub mod geometry;
pub mod locale;
pub mod manufacturer;
pub mod mining;
pub mod photometry;

pub use extract::{extract, ExtractInput, ExtractOutput, ExtractWarning};

/// Read the first photometric file (LDT / IES text) out of a `.gldf`
/// file on disk. Used by the server's lazy LDT-fetch endpoint so the
/// browser can parse + render polar diagrams client-side without the
/// index needing to carry the intensity matrices.
///
/// Returns `Ok(None)` when the GLDF has no photometric file; `Err`
/// when the zip can't be opened or the LDC entry can't be read.
pub fn read_first_ldt(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    read_ldt_for_variant(path, None)
}

/// Read the LDT for a specific variant out of a `.gldf` on disk.
///
/// `variant_index` is the 0-based position of the variant in the
/// GLDF's `<Variants>` block — same ordering the extractor uses to
/// assign `VariantDoc.id`. `None` picks the first photometric file
/// (matches `read_first_ldt`).
///
/// **Heuristic for multi-LDT families**: when the GLDF has the same
/// number of `<Variant>` entries as photometric `<File>` entries in
/// `<GeneralDefinitions><Files>`, we pair them positionally. That's
/// the convention for family catalogs (Deko-Light, Philips Luma)
/// where each variant ships its own LDT. When counts disagree we
/// fall back to the first file — better than guessing wrong.
///
/// The proper XSD traversal (Variant → Geometry → EmitterReference
/// → Emitter → LightSource → Photometry → PhotometryFileReference)
/// is a follow-up; the count-equality heuristic covers every
/// family-style file in the current corpus (audited 2026-05-17).
pub fn read_ldt_for_variant(
    path: &std::path::Path,
    variant_index: Option<usize>,
) -> anyhow::Result<Option<String>> {
    use anyhow::Context;
    let path_str = path.to_string_lossy();
    let gldf = gldf_rs::gldf::GldfProduct::load_gldf(&path_str)
        .with_context(|| format!("load_gldf({path_str})"))?;
    let phot_files = match gldf.get_phot_files() {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if phot_files.is_empty() {
        return Ok(None);
    }
    let variant_count = gldf
        .product_definitions
        .variants
        .as_ref()
        .map(|vs| vs.variant.len())
        .unwrap_or(0);
    let file_id = match variant_index {
        Some(idx) if variant_count == phot_files.len() && idx < phot_files.len() => {
            phot_files[idx].id.clone()
        }
        _ => phot_files[0].id.clone(),
    };
    let ldt = gldf
        .get_ldc_by_id(file_id)
        .map_err(|e| anyhow::anyhow!("get_ldc_by_id: {e:?}"))?;
    Ok(Some(ldt))
}

/// Read the first product picture entry out of a `.gldf` on disk.
///
/// Returns `Ok(Some((mime, bytes)))` with the raw image bytes and the
/// MIME type declared in the manifest (`image/png`, `image/jpeg`,
/// `image/svg`, ...). `Ok(None)` when the GLDF declares no image
/// file. `Err` when the zip can't be opened or the image entry can't
/// be read.
///
/// Conventions match `read_first_ldt`:
/// - "first" = whatever `get_image_def_files` returns first. The
///   corpus convention is one product image per GLDF; multi-image
///   GLDFs would need a per-variant selector (future task).
/// - We don't decode the image — the browser does. We just pipe the
///   bytes through, with the MIME the manifest declared.
/// - The server calls this from a `spawn_blocking` task because the
///   `gldf-rs` reader reopens the zip from disk (blocking I/O).
pub fn read_product_image(path: &std::path::Path) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    read_image_for_variant(path, None)
}

/// Read the product picture for a specific variant out of a `.gldf`.
///
/// Same positional heuristic as `read_ldt_for_variant`: if the GLDF
/// has one image per variant (same counts), the variant-index'd
/// image wins; otherwise we fall back to the first image (the
/// canonical "doc-level" product shot).
///
/// Most corpus files ship a single product photo for the whole
/// family — that's the fallback. Manufacturers who ship per-variant
/// renders (some Philips, ERCO families) get the per-variant image
/// automatically when the count matches.
pub fn read_image_for_variant(
    path: &std::path::Path,
    variant_index: Option<usize>,
) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    use anyhow::Context;
    let path_str = path.to_string_lossy();
    let gldf = gldf_rs::gldf::GldfProduct::load_gldf(&path_str)
        .with_context(|| format!("load_gldf({path_str})"))?;
    let image_files = match gldf.get_image_def_files() {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if image_files.is_empty() {
        return Ok(None);
    }
    let variant_count = gldf
        .product_definitions
        .variants
        .as_ref()
        .map(|vs| vs.variant.len())
        .unwrap_or(0);
    let image_file = match variant_index {
        Some(idx) if variant_count == image_files.len() && idx < image_files.len() => {
            image_files[idx]
        }
        _ => image_files[0],
    };
    let entry_path = if image_file.file_name.contains('/') {
        image_file.file_name.clone()
    } else {
        format!("image/{}", image_file.file_name)
    };
    let bytes = gldf
        .load_gldf_file(entry_path.clone())
        .with_context(|| format!("load_gldf_file({entry_path})"))?;
    Ok(Some((image_file.content_type.clone(), bytes)))
}
