//! Group-and-fold duplicates into family rollups.
//!
//! Background: the corpus contains many GLDFs that are really just
//! per-SKU variants of one product family. Manufacturers ship them
//! as separate `.gldf` files even though the GLDF format supports
//! variants natively. Measured 2026-05-18 on the 449 333-doc
//! corpus: 449 333 occurrences across 141 896 unique
//! `(manufacturer, product)` pairs — folding cuts the doc count by
//! 68.4% and frees most of the duplicated `descriptions` payload
//! (the 2.65 GB heap field that dominates RAM).
//!
//! **Fold key**: exact `(manufacturer, product)` match,
//! case-sensitive. Audited zero-collision; this is what the
//! `--report-sizes` example measures. Loosen to case-insensitive
//! only if you see meaningful under-folding (e.g. `"Compar linear"`
//! vs `"Compar Linear"`).
//!
//! **DocId policy**: each family gets a deterministic canonical
//! DocId (BLAKE3 of `"family:" + manufacturer + ":" + product`).
//! The source DocIds remain valid because `InMemoryIndex::from_docs`
//! enters each one into `by_id` pointing at the same family doc.
//! Old bookmarks survive.
//!
//! **Source provenance**: each family doc's `source_paths` lists
//! every contributing GLDF's path. Each variant inside the family
//! carries a `source_index` pointing into that list. Server fns
//! that need to open the right .gldf for a variant
//! (`fetch_ldt_for_variant`, `fetch_product_image_for_variant`)
//! dereference via `doc.source_paths[variant.source_index]`.

use std::collections::BTreeSet;
use std::collections::HashMap;

use compact_str::CompactString;
use gldf_search_schema::doc::{DocId, LuminaireDoc, SourceRef, VariantDoc, VariantId};
use smallvec::SmallVec;

/// Outcome counters from a single `fold_docs` pass. The CLI prints
/// these so the operator sees what happened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FoldStats {
    /// Docs in before folding.
    pub input_docs: usize,
    /// Distinct family groups (= docs out).
    pub output_docs: usize,
    /// Total variants in the input (sum of `doc.variants.len()` over
    /// every source doc).
    pub input_variants: usize,
    /// Total variants in the output (same count — folding doesn't
    /// drop variants, only collapses parents).
    pub output_variants: usize,
    /// Number of source-DocId → family-DocId aliases created.
    pub aliases: usize,
}

/// Run the group-and-fold pass. Returns `(folded_docs,
/// id_aliases, stats)`.
///
/// `id_aliases` maps every source DocId to the canonical family
/// DocId it landed in. The index builder consumes this when
/// populating `by_id` — every alias becomes a key that resolves to
/// the same family doc index. Aliases include the canonical
/// DocId-for-itself so callers can build the map in one loop.
pub fn fold_docs(
    docs: Vec<LuminaireDoc>,
) -> (Vec<LuminaireDoc>, HashMap<DocId, DocId>, FoldStats) {
    let mut stats = FoldStats {
        input_docs: docs.len(),
        ..Default::default()
    };
    let mut by_key: HashMap<(CompactString, CompactString), Vec<LuminaireDoc>> =
        HashMap::with_capacity(docs.len());
    for doc in docs {
        stats.input_variants += doc.variants.len();
        let key = (doc.manufacturer.clone(), doc.product.clone());
        by_key.entry(key).or_default().push(doc);
    }

    let mut folded: Vec<LuminaireDoc> = Vec::with_capacity(by_key.len());
    let mut aliases: HashMap<DocId, DocId> = HashMap::with_capacity(stats.input_docs);

    for ((manufacturer, product), group) in by_key {
        let family = merge_group(&manufacturer, &product, group, &mut aliases);
        stats.output_variants += family.variants.len();
        folded.push(family);
    }
    stats.output_docs = folded.len();
    stats.aliases = aliases.len();
    (folded, aliases, stats)
}

/// Merge N source docs sharing one `(manufacturer, product)` key
/// into a single family doc. Caller has already ensured `group` is
/// non-empty (every entry came from the input `docs` vector).
fn merge_group(
    manufacturer: &CompactString,
    product: &CompactString,
    mut group: Vec<LuminaireDoc>,
    aliases: &mut HashMap<DocId, DocId>,
) -> LuminaireDoc {
    // Single-doc groups: no-op fold. Just record the alias and
    // pass-through.
    if group.len() == 1 {
        let mut doc = group.pop().unwrap();
        // Canonical-id-for-itself so the by_id builder doesn't need
        // a separate code path.
        aliases.insert(doc.id, doc.id);
        // Migration: an un-folded doc that came from an older cache
        // missed `source_paths` (the field is brand-new at \x0b);
        // populate it from `source` for consistency.
        if doc.source_paths.is_empty() {
            doc.source_paths.push(doc.source.clone());
        }
        return doc;
    }

    let canonical_id = family_doc_id(manufacturer, product);

    // Order group by source DocId for determinism — re-running on
    // the same input must produce byte-identical output (the cache
    // is content-addressed at hash level).
    group.sort_by(|a, b| a.id.0.cmp(&b.id.0));

    // Build `source_paths` in source-doc order. Each source doc's
    // existing source becomes one entry. The variant `source_index`
    // is then the position in this vec.
    let mut source_paths: SmallVec<[SourceRef; 1]> = SmallVec::with_capacity(group.len());
    for doc in &group {
        source_paths.push(doc.source.clone());
    }

    // Take the first source's `source` field as the family's
    // primary source — used by the viewer-link path and any other
    // "doc-level / first source" caller.
    let primary_source = source_paths[0].clone();

    // Aggregate variants, applications, labels, etc. across the
    // group. Variants get a fresh sequential `VariantId` and the
    // `source_index` pointing back at their source doc.
    let mut variants: SmallVec<[VariantDoc; 1]> = SmallVec::with_capacity(group.len());
    let mut applications_set: BTreeSet<u8> = BTreeSet::new();
    let mut labels_set: BTreeSet<u8> = BTreeSet::new();
    let mut adjustability_set: BTreeSet<u8> = BTreeSet::new();
    let mut keyword_set: BTreeSet<CompactString> = BTreeSet::new();
    let mut description_set: BTreeSet<(gldf_search_schema::Locale, String)> = BTreeSet::new();
    // First-seen wins for single-valued doc-level facets.
    let mut ip_code: Option<u8> = None;
    let mut safety_class: Option<u8> = None;
    let mut ik_rating: Option<u8> = None;
    let mut product_form: Option<u8> = None;
    // First-seen wins for product_code / gtin too — the corpus
    // ships per-SKU codes, and after folding the family-level code
    // is just "one of the SKUs"; the SKU column on the variant row
    // is the actually-useful display.
    let mut gtin: Option<CompactString> = None;
    let mut product_code: Option<CompactString> = None;
    let mut size_bytes: u64 = 0;
    let mut mtime_max: Option<u64> = None;
    let mut format_version: Option<CompactString> = None;

    let mut next_variant_id: u16 = 0;

    for (source_index, doc) in group.into_iter().enumerate() {
        aliases.insert(doc.id, canonical_id);
        applications_set.extend(doc.applications.iter().copied());
        labels_set.extend(doc.labels.iter().copied());
        adjustability_set.extend(doc.adjustability.iter().copied());
        keyword_set.extend(doc.keywords.iter().cloned());
        description_set.extend(doc.descriptions.iter().cloned());
        ip_code = ip_code.or(doc.ip_code);
        safety_class = safety_class.or(doc.safety_class);
        ik_rating = ik_rating.or(doc.ik_rating);
        product_form = product_form.or(doc.product_form);
        gtin = gtin.or(doc.gtin);
        product_code = product_code.or(doc.product_code);
        size_bytes = size_bytes.saturating_add(doc.file_meta.size_bytes);
        mtime_max = match (mtime_max, doc.file_meta.mtime_epoch_s) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
        format_version = format_version.or(doc.file_meta.format_version);

        // For every variant from this source doc, rewrite its id
        // and source_index. The original VariantId is lost — the
        // doc-level URL didn't carry it anyway. If a caller had a
        // (DocId, VariantId) pair, only the DocId alias survives.
        for mut v in doc.variants {
            v.id = VariantId(next_variant_id);
            v.source_index = source_index as u16;
            next_variant_id = next_variant_id.wrapping_add(1);
            variants.push(v);
        }
    }

    let applications: SmallVec<[u8; 4]> = applications_set.into_iter().collect();
    let labels: SmallVec<[u8; 4]> = labels_set.into_iter().collect();
    let adjustability: SmallVec<[u8; 2]> = adjustability_set.into_iter().collect();
    let keywords: SmallVec<[CompactString; 2]> = keyword_set.into_iter().collect();
    let descriptions: SmallVec<[(gldf_search_schema::Locale, String); 1]> =
        description_set.into_iter().collect();

    LuminaireDoc {
        id: canonical_id,
        source: primary_source,
        source_paths,
        manufacturer: manufacturer.clone(),
        product: product.clone(),
        gtin,
        product_code,
        descriptions,
        keywords,
        applications,
        labels,
        adjustability,
        ip_code,
        safety_class,
        ik_rating,
        product_form,
        variants,
        file_meta: gldf_search_schema::doc::FileMeta {
            size_bytes,
            mtime_epoch_s: mtime_max,
            format_version,
        },
    }
}

/// Deterministic DocId for a folded family — BLAKE3 of
/// `"family:<manufacturer>:<product>"`. Stable across runs.
fn family_doc_id(manufacturer: &str, product: &str) -> DocId {
    use blake3::Hasher;
    let mut h = Hasher::new();
    h.update(b"family:");
    h.update(manufacturer.as_bytes());
    h.update(b":");
    h.update(product.as_bytes());
    let digest = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_bytes());
    DocId(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gldf_search_schema::doc::{FileMeta, MountingPlace, MountingType, PhotometricSymmetryHint};
    use smallvec::smallvec;

    fn mk_variant(id: u16, name: &str) -> VariantDoc {
        VariantDoc {
            id: VariantId(id),
            name: CompactString::from(name),
            photometry: Some(gldf_search_schema::doc::PhotometryStats {
                flux_lm: Some(2000.0),
                power_w: Some(20.0),
                efficacy_lm_w: Some(100.0),
                cct_k: Some(3000),
                cri_ra: Some(80),
                r9: None,
                beam_deg: Some(60.0),
                field_deg: None,
                ulor: None,
                dlor: None,
                symmetry: PhotometricSymmetryHint::Unknown,
                light_distribution: None,
                ..Default::default()
            }),
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
        }
    }

    fn mk_doc(id_seed: u8, mfr: &str, product: &str, source_name: &str) -> LuminaireDoc {
        let source = SourceRef::Path(CompactString::from(source_name));
        LuminaireDoc {
            id: DocId([id_seed; 32]),
            source: source.clone(),
            source_paths: smallvec![source],
            manufacturer: CompactString::from(mfr),
            product: CompactString::from(product),
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
            variants: smallvec![mk_variant(0, "v")],
            file_meta: FileMeta {
                size_bytes: 1000,
                mtime_epoch_s: None,
                format_version: None,
            },
        }
    }

    #[test]
    fn single_doc_passes_through() {
        let docs = vec![mk_doc(1, "ERCO", "Compar", "erco/compar/1.gldf")];
        let (folded, aliases, stats) = fold_docs(docs);
        assert_eq!(folded.len(), 1);
        assert_eq!(stats.input_docs, 1);
        assert_eq!(stats.output_docs, 1);
        // Single-doc groups: aliases the doc id to itself for the by_id builder.
        assert_eq!(aliases.get(&DocId([1; 32])), Some(&DocId([1; 32])));
        // source_paths populated from source.
        assert_eq!(folded[0].source_paths.len(), 1);
    }

    #[test]
    fn two_docs_same_family_fold_into_one() {
        let docs = vec![
            mk_doc(1, "ERCO", "Compar", "erco/compar/1.gldf"),
            mk_doc(2, "ERCO", "Compar", "erco/compar/2.gldf"),
        ];
        let (folded, aliases, stats) = fold_docs(docs);
        assert_eq!(folded.len(), 1);
        assert_eq!(stats.output_variants, 2, "both variants survive");
        assert_eq!(folded[0].variants.len(), 2);
        assert_eq!(folded[0].source_paths.len(), 2);
        // Each variant's source_index points at its source file.
        let indices: Vec<u16> = folded[0].variants.iter().map(|v| v.source_index).collect();
        assert!(indices.contains(&0));
        assert!(indices.contains(&1));
        // Both source DocIds alias to the canonical family DocId.
        let canonical = folded[0].id;
        assert_eq!(aliases.get(&DocId([1; 32])), Some(&canonical));
        assert_eq!(aliases.get(&DocId([2; 32])), Some(&canonical));
    }

    #[test]
    fn fold_key_is_case_sensitive() {
        let docs = vec![
            mk_doc(1, "ERCO", "Compar", "a.gldf"),
            mk_doc(2, "erco", "Compar", "b.gldf"),
        ];
        let (folded, _, _) = fold_docs(docs);
        assert_eq!(folded.len(), 2, "different case = different family");
    }

    #[test]
    fn different_products_dont_fold() {
        let docs = vec![
            mk_doc(1, "ERCO", "Compar", "a.gldf"),
            mk_doc(2, "ERCO", "Optec", "b.gldf"),
        ];
        let (folded, _, _) = fold_docs(docs);
        assert_eq!(folded.len(), 2);
    }

    #[test]
    fn family_doc_id_is_deterministic() {
        let a = family_doc_id("ERCO", "Compar");
        let b = family_doc_id("ERCO", "Compar");
        assert_eq!(a, b);
        let c = family_doc_id("ERCO", "Optec");
        assert_ne!(a, c);
    }

    #[test]
    fn folded_doc_applications_are_union() {
        let mut a = mk_doc(1, "ERCO", "Compar", "a.gldf");
        let mut b = mk_doc(2, "ERCO", "Compar", "b.gldf");
        a.applications = smallvec![10, 20];
        b.applications = smallvec![20, 30];
        let (folded, _, _) = fold_docs(vec![a, b]);
        assert_eq!(folded.len(), 1);
        let mut apps = folded[0].applications.to_vec();
        apps.sort();
        assert_eq!(apps, vec![10, 20, 30]);
    }

    #[test]
    fn descriptions_dedupe_across_sources() {
        use gldf_search_schema::Locale;
        let mut a = mk_doc(1, "ERCO", "Compar", "a.gldf");
        let mut b = mk_doc(2, "ERCO", "Compar", "b.gldf");
        let de = Locale::from_str("de").unwrap();
        a.descriptions = smallvec![(de, "Same text".to_string())];
        b.descriptions = smallvec![(de, "Same text".to_string())];
        let (folded, _, _) = fold_docs(vec![a, b]);
        // Identical descriptions collapse to one entry.
        assert_eq!(folded[0].descriptions.len(), 1);
    }
}
