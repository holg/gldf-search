//! Orchestrator: walk a `GldfProduct` and emit a `LuminaireDoc`.
//!
//! Strategy (corpus-shape driven — see project memory):
//!
//! 1. **Typed XSD path** is always preferred when populated. Reads
//!    `<Marketing>` / `<Mechanical>` / `<Electrical>` from each
//!    variant's `<DescriptiveAttributes>`.
//! 2. **Name-mining** fills gaps. For the dominant `light-other-rs`
//!    corpus where the typed blocks are empty, mining is the only
//!    source of facet ids.
//! 3. When both sources produce a value for the same field, **typed
//!    wins**. Mined keywords still enter the keyword pool — so a
//!    user query for `"IP54"` finds the doc whether the value came
//!    from `<IngressProtectionIPCode>` or from a mined match.
//! 4. **Photometry** stays as a stub `None` here. The follow-up pass
//!    will plug in LDC parsing via `gldf_rs`'s `eulumdat` feature.
//!    Mined numerics still populate a `PhotometryStats` so range
//!    queries work without LDC.

use std::collections::BTreeSet;

use compact_str::CompactString;
use gldf_rs::gldf::{GldfProduct, Variant};
use gldf_search_schema::doc::{
    FileMeta, LuminaireDoc, PhotometricSymmetryHint, PhotometryStats, SourceRef, VariantDoc,
    VariantId,
};
use gldf_search_schema::enums::{
    adjustability_from_str, application_from_str, control_gear_interface_from_str,
    emergency_lighting_type_from_str, ik_rating_from_str, ip_code_from_str, label_from_str,
    lamp_type_from_str, light_distribution_from_str, product_form_from_str, safety_class_from_str,
    AdjustabilityId, ApplicationId, ControlGearInterfaceId, IkRatingId, IpCodeId, LabelId,
    LampTypeId, LightDistributionId, ProductFormId, SafetyClassId,
};
use smallvec::SmallVec;

use crate::doc_id::derive_doc_id;
use crate::geometry::variant_mounting;
use crate::locale::gather_descriptions;
use crate::manufacturer::normalise;
use crate::mining::{mine_variant_name, MinedFromName};

/// Extractor output: the doc plus a (possibly empty) list of warnings
/// describing what the corpus surface lacked.
#[derive(Debug, Clone)]
pub struct ExtractOutput {
    /// Indexable document.
    pub doc: LuminaireDoc,
    /// Soft failures and missing-field signals. Never `Err`-equivalent
    /// — the doc is always usable even when warnings are present.
    pub warnings: Vec<ExtractWarning>,
}

/// Soft-failure surface. Each variant is shaped so the operator can
/// triage corpus-wide issues with a simple count-by-variant histogram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractWarning {
    /// The variant had no LDC file in `<GeneralDefinitions><Files>`.
    /// The variant still indexes — facets from mining persist — but
    /// numeric photometry filters won't match it.
    PhotometryMissing {
        /// XSD `@id` of the variant.
        variant_id: String,
    },
    /// `<Header><Manufacturer>` was empty or whitespace.
    ManufacturerEmpty,
    /// `<Header><UniqueGldfId>` was absent AND the caller provided no
    /// raw bytes for the BLAKE3 fallback. The resulting DocId is the
    /// all-zeros sentinel.
    DocIdUnavailable,
    /// A variant's XSD-restricted field carried a value not in the
    /// canonical enum table. Mined as keyword but did not populate
    /// the typed field. Common when manufacturers ship pre-rc.3 enum
    /// strings.
    NonCanonicalEnumValue {
        /// XSD `@id` of the variant the violation came from.
        variant_id: String,
        /// XSD element name that carried the offending value (e.g.
        /// `"Application"`, `"IngressProtectionIPCode"`).
        field: &'static str,
        /// The non-canonical value, as written in the source GLDF.
        value: String,
    },
    /// An LDC reference existed in `<GeneralDefinitions><Files>` but
    /// the file couldn't be read from the zip or its content was not
    /// a parseable EULUMDAT/LDT. Photometry falls back to mined-only.
    PhotometryParseFailed {
        /// XSD `@id` of the variant whose LDT was unparseable.
        variant_id: String,
        /// XSD `@id` of the LDC file in `<Files>`.
        file_id: String,
    },
}

/// Optional input parameters to [`extract`].
///
/// `source` is recorded in `LuminaireDoc.source` as-is. `raw_bytes` is
/// used **only** as the BLAKE3 fallback for `DocId` — the extractor
/// never re-opens the file.
#[derive(Debug, Clone, Default)]
pub struct ExtractInput<'a> {
    /// Where the doc came from. Defaults to [`SourceRef::ContentOnly`].
    pub source: Option<SourceRef>,
    /// Raw `.gldf` bytes for DocId BLAKE3 fallback. Pass `None` when
    /// the caller can't easily provide them — `<UniqueGldfId>` covers
    /// the entire corpus we've measured.
    pub raw_bytes: Option<&'a [u8]>,
    /// File-level metadata. Defaults to zero/None.
    pub file_meta: Option<FileMeta>,
}

/// Convert a parsed `GldfProduct` into a `LuminaireDoc`.
///
/// **Small I/O**: when an LDC file is referenced from the GLDF, the
/// extractor re-opens the zip via `gldf_rs::GldfProduct::get_ldc_by_id`
/// to parse photometry. The caller therefore must have loaded the
/// GLDF from a path-on-disk (`load_gldf(path)`) so `GldfProduct.path`
/// is populated. Buffer-based loaders (`load_gldf_from_buf`) leave
/// `path` empty; in that case `get_ldc_by_id` fails and the extractor
/// silently falls back to mining-derived photometry (with a warning).
pub fn extract(gldf: &GldfProduct, input: ExtractInput<'_>) -> ExtractOutput {
    let mut warnings = Vec::new();

    let manufacturer = normalise(&gldf.header.manufacturer);
    if manufacturer.display.is_empty() {
        warnings.push(ExtractWarning::ManufacturerEmpty);
    }

    let doc_id = {
        let uid = gldf.header.unique_gldf_id.as_deref();
        let id = derive_doc_id(uid, input.raw_bytes);
        if id.0 == [0u8; 32] {
            warnings.push(ExtractWarning::DocIdUnavailable);
        }
        id
    };

    let descriptions = gather_descriptions(gldf);

    let product = product_name(gldf);
    let gtin = product_gtin(gldf);
    let product_code = product_code(gldf);

    // Walk variants, mine their names, collect facets. Doc-scoped
    // facets are the **union** over variant-scoped XSD facets — the
    // schema models them at the doc level because manufacturers
    // usually mark a whole family with one applications list, but
    // when a multi-variant doc disagrees, union is the conservative
    // choice (matches more queries, doesn't reject results).
    let mut applications_set: BTreeSet<ApplicationId> = BTreeSet::new();
    let mut labels_set: BTreeSet<LabelId> = BTreeSet::new();
    let mut adjust_set: BTreeSet<AdjustabilityId> = BTreeSet::new();
    let mut keywords_set: BTreeSet<CompactString> = BTreeSet::new();

    let mut doc_ip_code: Option<IpCodeId> = None;
    let mut doc_safety_class: Option<SafetyClassId> = None;
    let mut doc_ik_rating: Option<IkRatingId> = None;
    let mut doc_product_form: Option<ProductFormId> = None;

    let mut out_variants: SmallVec<[VariantDoc; 1]> = SmallVec::new();

    // Doc-wide aggregates pulled from `GeneralDefinitions` once. The
    // typed reference chain that would attribute these per-variant
    // (Variant.geometry.emitter_reference → Emitter → light_source_ref
    // → LightSourceMaintenance.cie97_lamp_type, and similar for
    // ControlGear) is multi-hop and depends on optional id-string
    // matches throughout the file. For facet bucketing on a corpus
    // this size, the doc-wide aggregate is "close enough": every
    // variant inherits the union of values declared anywhere in the
    // doc's GeneralDefinitions. Refine to per-variant attribution
    // later if the corpus actually carries heterogeneous variants.
    let doc_lamp_type = pluck_cie97_lamp_type(gldf);
    let doc_control_gear_interfaces = pluck_control_gear_interfaces(gldf);

    let variants_in = gldf
        .product_definitions
        .variants
        .as_ref()
        .map(|vs| vs.variant.as_slice())
        .unwrap_or(&[]);

    for (idx, v) in variants_in.iter().enumerate() {
        let display_name = first_locale_value(v.name.as_ref()).unwrap_or_else(|| v.id.clone());
        let mined = mine_variant_name(&display_name);

        // Typed XSD path — when fields are present, they win and are
        // also pushed into the doc-scoped sets. Validate against the
        // canonical tables; non-canonical values produce a warning.
        let typed = TypedFacets::from_variant(v, &v.id, &mut warnings);

        // Merge into doc-scoped facets. Typed XSD path is the source
        // of truth when present; the name-mined applications only
        // contribute extra ids the XSD didn't carry (union semantics —
        // same conservative choice as every other doc-level facet).
        for &a in &typed.applications {
            applications_set.insert(a);
        }
        for &a in &mined.applications {
            applications_set.insert(a);
        }
        for &l in &typed.labels {
            labels_set.insert(l);
        }
        for &a in &typed.adjustability {
            adjust_set.insert(a);
        }

        // Doc-scoped single-value facets — first variant to populate
        // wins. Subsequent variants don't override (a multi-variant
        // doc with conflicting IP codes is unusual; the union approach
        // would force a doc-level "set of values" which would change
        // the schema).
        doc_ip_code = doc_ip_code.or(typed.ip_code).or(mined.ip_code);
        doc_safety_class = doc_safety_class.or(typed.safety_class);
        doc_ik_rating = doc_ik_rating.or(typed.ik_rating).or(mined.ik_rating);
        doc_product_form = doc_product_form
            .or(typed.product_form)
            .or(mined.product_form);

        // Keywords pool — every mined token + every typed canonical
        // value enters here. De-duplicated by the BTreeSet.
        for kw in &mined.keywords {
            keywords_set.insert(kw.clone());
        }
        for raw in &typed.raw_keywords {
            keywords_set.insert(raw.clone());
        }

        // Variant-scoped photometry: prefer LDT-parsed values over
        // mined ones. v0 picks the first photometric file in the GLDF;
        // multi-LDT-per-variant mapping is a follow-up.
        let from_ldt = resolve_ldt_for_variant(gldf, &v.id, &mut warnings);
        let mined_stats = build_photometry(&mined, typed.light_distribution);
        let mut photometry = merge_photometry(from_ldt, mined_stats, typed.light_distribution);
        // Drop physically impossible outliers before they enter the
        // cache. See `photometry::sanitize_photometry` for the caps.
        if let Some(p) = photometry.as_mut() {
            crate::photometry::sanitize_photometry(p);
        }
        if photometry.is_none() {
            warnings.push(ExtractWarning::PhotometryMissing {
                variant_id: v.id.clone(),
            });
        }

        let variant_id = VariantId(idx as u16);
        let mounting = variant_mounting(v);
        // Per XSD, presence of `<ModelGeometryReference>` guarantees
        // the referenced `<ModelGeometry>` has ≥1 `<GeometryFileReference>`
        // (a 3D model file inside the GLDF, typically `.l3d` or
        // `.gltf`). So a Some on the reference is the correct
        // boolean — no need to follow it.
        let has_3d = v
            .geometry
            .as_ref()
            .and_then(|g| g.model_geometry_reference.as_ref())
            .is_some();

        // Emergency block on this variant's DescriptiveAttributes.
        let emergency = v
            .descriptive_attributes
            .as_ref()
            .and_then(|d| d.emergency.as_ref());
        let has_emergency_lighting = emergency.is_some();
        let emergency_lighting_type = emergency
            .and_then(|e| e.dedicated_emergency_lighting_type.as_deref())
            .and_then(emergency_lighting_type_from_str);

        out_variants.push(VariantDoc {
            id: variant_id,
            name: CompactString::from(display_name),
            photometry,
            mounting_place: mounting.place,
            mounting_type: mounting.kind,
            recessed_depth_mm: crate::photometry::sanitize_recessed_depth(
                mounting.recessed_depth_mm,
            ),
            has_3d,
            lamp_type: doc_lamp_type,
            control_gear_interfaces: doc_control_gear_interfaces.clone(),
            has_emergency_lighting,
            emergency_lighting_type,
            emitter_count: 0, // populated by the LDC pass later
            source_index: 0,  // un-folded — single source, index 0
        });
    }

    let source = input.source.unwrap_or(SourceRef::ContentOnly);
    let mut source_paths: SmallVec<[SourceRef; 1]> = SmallVec::new();
    source_paths.push(source.clone());
    let doc = LuminaireDoc {
        id: doc_id,
        source,
        source_paths,
        manufacturer: manufacturer.display.clone(),
        product,
        gtin,
        product_code,
        descriptions,
        keywords: keywords_set.into_iter().collect(),
        applications: applications_set.into_iter().collect(),
        labels: labels_set.into_iter().collect(),
        adjustability: adjust_set.into_iter().collect(),
        ip_code: doc_ip_code,
        safety_class: doc_safety_class,
        ik_rating: doc_ik_rating,
        product_form: doc_product_form,
        variants: out_variants,
        file_meta: input.file_meta.unwrap_or(FileMeta {
            size_bytes: 0,
            mtime_epoch_s: None,
            format_version: format_version_string(gldf).map(CompactString::from),
        }),
    };

    // Suppress unused-field warning until the LDC pass uses it.
    let _ = manufacturer.key;

    ExtractOutput { doc, warnings }
}

/// Typed XSD facets pulled from one variant.
struct TypedFacets {
    applications: SmallVec<[ApplicationId; 4]>,
    labels: SmallVec<[LabelId; 4]>,
    adjustability: SmallVec<[AdjustabilityId; 2]>,
    ip_code: Option<IpCodeId>,
    safety_class: Option<SafetyClassId>,
    ik_rating: Option<IkRatingId>,
    product_form: Option<ProductFormId>,
    light_distribution: Option<LightDistributionId>,
    raw_keywords: SmallVec<[CompactString; 8]>,
}

impl TypedFacets {
    fn from_variant(v: &Variant, variant_id: &str, warnings: &mut Vec<ExtractWarning>) -> Self {
        let mut out = Self {
            applications: SmallVec::new(),
            labels: SmallVec::new(),
            adjustability: SmallVec::new(),
            ip_code: None,
            safety_class: None,
            ik_rating: None,
            product_form: None,
            light_distribution: None,
            raw_keywords: SmallVec::new(),
        };

        let Some(da) = &v.descriptive_attributes else {
            return out;
        };

        if let Some(m) = &da.marketing {
            if let Some(apps) = &m.applications {
                for a in &apps.application {
                    match application_from_str(a) {
                        Some(id) => {
                            out.applications.push(id);
                            out.raw_keywords.push(CompactString::from(a));
                        }
                        None => warnings.push(ExtractWarning::NonCanonicalEnumValue {
                            variant_id: variant_id.to_string(),
                            field: "Application",
                            value: a.clone(),
                        }),
                    }
                }
            }
            if let Some(labels) = &m.labels {
                for l in &labels.label {
                    match label_from_str(l) {
                        Some(id) => {
                            out.labels.push(id);
                            out.raw_keywords.push(CompactString::from(l));
                        }
                        None => warnings.push(ExtractWarning::NonCanonicalEnumValue {
                            variant_id: variant_id.to_string(),
                            field: "Label",
                            value: l.clone(),
                        }),
                    }
                }
            }
        }

        if let Some(mech) = &da.mechanical {
            if let Some(form) = mech.product_form.as_deref().filter(|s| !s.is_empty()) {
                match product_form_from_str(form) {
                    Some(id) => {
                        out.product_form = Some(id);
                        out.raw_keywords.push(CompactString::from(form));
                    }
                    None => warnings.push(ExtractWarning::NonCanonicalEnumValue {
                        variant_id: variant_id.to_string(),
                        field: "ProductForm",
                        value: form.to_string(),
                    }),
                }
            }
            if let Some(ik) = mech.ik_rating.as_deref().filter(|s| !s.is_empty()) {
                match ik_rating_from_str(ik) {
                    Some(id) => {
                        out.ik_rating = Some(id);
                        out.raw_keywords.push(CompactString::from(ik));
                    }
                    None => warnings.push(ExtractWarning::NonCanonicalEnumValue {
                        variant_id: variant_id.to_string(),
                        field: "IKRating",
                        value: ik.to_string(),
                    }),
                }
            }
            if let Some(adj) = &mech.adjustabilities {
                for a in &adj.adjustability {
                    if let Some(id) = adjustability_from_str(a) {
                        out.adjustability.push(id);
                        out.raw_keywords.push(CompactString::from(a));
                    } else {
                        warnings.push(ExtractWarning::NonCanonicalEnumValue {
                            variant_id: variant_id.to_string(),
                            field: "Adjustability",
                            value: a.clone(),
                        });
                    }
                }
            }
        }

        if let Some(el) = &da.electrical {
            if let Some(ip) = el
                .ingress_protection_ip_code
                .as_deref()
                .filter(|s| !s.is_empty())
            {
                match ip_code_from_str(ip) {
                    Some(id) => {
                        out.ip_code = Some(id);
                        out.raw_keywords.push(CompactString::from(ip));
                    }
                    None => warnings.push(ExtractWarning::NonCanonicalEnumValue {
                        variant_id: variant_id.to_string(),
                        field: "IngressProtectionIPCode",
                        value: ip.to_string(),
                    }),
                }
            }
            if let Some(sc) = el
                .electrical_safety_class
                .as_deref()
                .filter(|s| !s.is_empty())
            {
                match safety_class_from_str(sc) {
                    Some(id) => {
                        out.safety_class = Some(id);
                        out.raw_keywords.push(CompactString::from(sc));
                    }
                    None => warnings.push(ExtractWarning::NonCanonicalEnumValue {
                        variant_id: variant_id.to_string(),
                        field: "ElectricalSafetyClass",
                        value: sc.to_string(),
                    }),
                }
            }
            if let Some(ld) = el.light_distribution.as_deref().filter(|s| !s.is_empty()) {
                match light_distribution_from_str(ld) {
                    Some(id) => {
                        out.light_distribution = Some(id);
                        out.raw_keywords.push(CompactString::from(ld));
                    }
                    None => warnings.push(ExtractWarning::NonCanonicalEnumValue {
                        variant_id: variant_id.to_string(),
                        field: "LightDistribution",
                        value: ld.to_string(),
                    }),
                }
            }
        }

        out
    }
}

/// Build a [`PhotometryStats`] from mined values. Returns `None` when
/// nothing useful was mined — that propagates into the schema's
/// "no photometry" branch.
fn build_photometry(
    mined: &MinedFromName,
    light_distribution: Option<LightDistributionId>,
) -> Option<PhotometryStats> {
    let has_anything = mined.flux_lm.is_some()
        || mined.power_w.is_some()
        || mined.cct_k.is_some()
        || mined.cri_ra_min.is_some()
        || mined.beam_deg.is_some()
        || light_distribution.is_some();
    if !has_anything {
        return None;
    }

    let efficacy = match (mined.flux_lm, mined.power_w) {
        (Some(f), Some(p)) if p > 0.0 => Some(f / p),
        _ => None,
    };

    Some(PhotometryStats {
        flux_lm: mined.flux_lm,
        power_w: mined.power_w,
        efficacy_lm_w: efficacy,
        cct_k: mined.cct_k,
        cri_ra: mined.cri_ra_min,
        r9: None,
        beam_deg: mined.beam_deg,
        field_deg: None,
        ulor: None,
        dlor: None,
        symmetry: light_distribution
            .map(PhotometricSymmetryHint::from_light_distribution_id)
            .unwrap_or(PhotometricSymmetryHint::Unknown),
        light_distribution,
    })
}

/// Try to resolve and parse an LDT for one variant. v0 takes the
/// first photometric file in the GLDF — corpus convention has one LDT
/// per file. Returns `None` if there's no LDC reference, the zip
/// couldn't be read, or the LDT didn't parse. The last case adds an
/// [`ExtractWarning::PhotometryParseFailed`].
fn resolve_ldt_for_variant(
    gldf: &GldfProduct,
    variant_id: &str,
    warnings: &mut Vec<ExtractWarning>,
) -> Option<PhotometryStats> {
    let file_id = gldf
        .get_phot_files()
        .ok()
        .and_then(|files| files.first().map(|f| f.id.clone()))?;
    // The non-http `get_ldc_by_id` re-opens the zip via the
    // `GldfProduct.path` field. If the GLDF was loaded from bytes
    // (path empty) this fails; we silently fall through (mined values
    // remain). Real loads via `load_gldf(path)` always work.
    let ldt_str = match gldf.get_ldc_by_id(file_id.clone()) {
        Ok(s) => s,
        Err(_) => return None,
    };
    match crate::photometry::parse_ldt_to_stats(&ldt_str) {
        Some(stats) => Some(stats),
        None => {
            warnings.push(ExtractWarning::PhotometryParseFailed {
                variant_id: variant_id.to_string(),
                file_id,
            });
            None
        }
    }
}

/// Merge LDT-parsed and mining-derived photometry. LDT wins per-field
/// when present; mining fills gaps. Either source can contribute
/// `light_distribution` (typed XSD path) — that's the explicit
/// argument since it comes from neither source directly.
///
/// Returns `None` only when both sources are empty.
fn merge_photometry(
    ldt: Option<PhotometryStats>,
    mined: Option<PhotometryStats>,
    light_distribution: Option<LightDistributionId>,
) -> Option<PhotometryStats> {
    match (ldt, mined) {
        (None, None) => {
            // Even if both sources are empty, build a stub when the
            // typed XSD path gave us a LightDistribution — that's a
            // useful facet on its own.
            light_distribution.map(|ld| PhotometryStats {
                flux_lm: None,
                power_w: None,
                efficacy_lm_w: None,
                cct_k: None,
                cri_ra: None,
                r9: None,
                beam_deg: None,
                field_deg: None,
                ulor: None,
                dlor: None,
                symmetry: PhotometricSymmetryHint::from_light_distribution_id(ld),
                light_distribution: Some(ld),
            })
        }
        (Some(l), None) => Some(l),
        (None, Some(m)) => Some(m),
        (Some(l), Some(m)) => Some(PhotometryStats {
            flux_lm: l.flux_lm.or(m.flux_lm),
            power_w: l.power_w.or(m.power_w),
            efficacy_lm_w: l.efficacy_lm_w.or(m.efficacy_lm_w),
            cct_k: l.cct_k.or(m.cct_k),
            cri_ra: l.cri_ra.or(m.cri_ra),
            r9: l.r9.or(m.r9),
            beam_deg: l.beam_deg.or(m.beam_deg),
            field_deg: l.field_deg.or(m.field_deg),
            ulor: l.ulor.or(m.ulor),
            dlor: l.dlor.or(m.dlor),
            // LDT's symmetry comes from real intensity data; trust it.
            symmetry: l.symmetry,
            // light_distribution only ever comes from the typed XSD
            // path (m has it set by build_photometry when present).
            light_distribution: m.light_distribution.or(l.light_distribution),
        }),
    }
}

// ── Small helpers ─────────────────────────────────────────────────────

fn product_name(gldf: &GldfProduct) -> CompactString {
    let meta = match &gldf.product_definitions.product_meta_data {
        Some(m) => m,
        None => return CompactString::default(),
    };
    first_locale_value(meta.name.as_ref())
        .map(CompactString::from)
        .unwrap_or_default()
}

fn product_gtin(gldf: &GldfProduct) -> Option<CompactString> {
    // The schema's doc-level GTIN aggregates from the first variant
    // that declares one. Variant-level GTINs are common in the
    // sample corpus, doc-level GTINs are not.
    let vs = gldf.product_definitions.variants.as_ref()?;
    vs.variant
        .iter()
        .find_map(|v| v.gtin.clone())
        .filter(|s| !s.is_empty())
        .map(CompactString::from)
}

fn product_code(gldf: &GldfProduct) -> Option<CompactString> {
    let meta = gldf.product_definitions.product_meta_data.as_ref()?;
    first_locale_value(meta.product_number.as_ref()).map(CompactString::from)
}

fn first_locale_value(bag: Option<&gldf_rs::gldf::header::LocaleFoo>) -> Option<String> {
    let bag = bag?;
    // Prefer "en" if present, otherwise first non-empty entry.
    let en = bag
        .locale
        .iter()
        .find(|loc| loc.language.eq_ignore_ascii_case("en") && !loc.value.is_empty());
    let first_nonempty = bag.locale.iter().find(|loc| !loc.value.is_empty());
    en.or(first_nonempty).map(|loc| loc.value.clone())
}

fn format_version_string(gldf: &GldfProduct) -> Option<String> {
    let fv = &gldf.header.format_version;
    // Match the inspector's rendering.
    let rc = fv
        .pre_release
        .map(|n| format!("-rc.{n}"))
        .unwrap_or_default();
    Some(format!("{}.{}.{}{}", fv.major, fv.minor, "", rc))
}

/// Walk every `LightSource` declared under `GeneralDefinitions` and
/// return the first canonical `Cie97LampType` value we encounter.
/// Returns `None` for docs that declare nothing.
///
/// The walk visits both `ChangeableLightSource` and `FixedLightSource`
/// (XSD lets a doc carry either / both). Multi-source variants with
/// different lamp technologies are rare in the corpus and would need
/// per-variant attribution to disambiguate — out of scope for this
/// pass.
fn pluck_cie97_lamp_type(gldf: &GldfProduct) -> Option<LampTypeId> {
    let ls = gldf.general_definitions.light_sources.as_ref()?;
    let from_changeable = ls
        .changeable_light_source
        .iter()
        .filter_map(|s| s.light_source_maintenance.as_ref())
        .filter_map(|m| m.cie97_lamp_type.as_deref())
        .find_map(lamp_type_from_str);
    let from_fixed = ls
        .fixed_light_source
        .iter()
        .filter_map(|s| s.light_source_maintenance.as_ref())
        .filter_map(|m| m.cie97_lamp_type.as_deref())
        .find_map(lamp_type_from_str);
    from_changeable.or(from_fixed)
}

/// Walk every `ControlGear` declared under `GeneralDefinitions` and
/// collect the canonical-only union of declared interfaces. Dedupes
/// while preserving first-seen order. Drops non-canonical strings
/// silently (gldf-rs's INTERFACES table lags the XSD by 6 entries
/// — see project memory).
fn pluck_control_gear_interfaces(gldf: &GldfProduct) -> SmallVec<[ControlGearInterfaceId; 4]> {
    let mut out: SmallVec<[ControlGearInterfaceId; 4]> = SmallVec::new();
    let Some(gears) = gldf.general_definitions.control_gears.as_ref() else {
        return out;
    };
    for gear in &gears.control_gear {
        let Some(ifaces) = gear.interfaces.as_ref() else {
            continue;
        };
        for raw in &ifaces.interface {
            if let Some(id) = control_gear_interface_from_str(raw.as_str()) {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gldf_rs::gldf::header::{Header, Locale as GldfLocale, LocaleFoo};
    use gldf_rs::gldf::product_definitions::{ProductDefinitions, ProductMetaData};
    use gldf_rs::gldf::{GeneralDefinitions, GldfProduct, Variant, Variants};

    fn make_variant(id: &str, name_en: &str) -> Variant {
        Variant {
            id: id.to_string(),
            name: Some(LocaleFoo {
                locale: vec![GldfLocale {
                    language: "en".to_string(),
                    value: name_en.to_string(),
                }],
            }),
            ..Default::default()
        }
    }

    fn make_gldf(
        manufacturer: &str,
        unique_id: Option<&str>,
        variants: Vec<Variant>,
    ) -> GldfProduct {
        GldfProduct {
            path: String::new(),
            xmlns_xsi: String::new(),
            xsnonamespaceschemalocation: String::new(),
            header: Header {
                manufacturer: manufacturer.to_string(),
                unique_gldf_id: unique_id.map(String::from),
                ..Default::default()
            },
            general_definitions: GeneralDefinitions::default(),
            product_definitions: ProductDefinitions {
                product_meta_data: Some(ProductMetaData {
                    name: Some(LocaleFoo {
                        locale: vec![GldfLocale {
                            language: "en".to_string(),
                            value: "Test Product".to_string(),
                        }],
                    }),
                    ..Default::default()
                }),
                variants: Some(Variants { variant: variants }),
            },
        }
    }

    #[test]
    fn extract_mines_facets_from_variant_name() {
        let gldf = make_gldf(
            "Test Co.",
            Some("urn:uuid:test-0001"),
            vec![make_variant("v0", "TEST 16W 1500lm IP65 840")],
        );
        let out = extract(&gldf, ExtractInput::default());
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);
        assert_eq!(out.doc.manufacturer, "Test Co.");
        assert_eq!(out.doc.product, "Test Product");
        assert!(
            out.doc.ip_code.is_some(),
            "IP65 should populate doc.ip_code"
        );
        let variant = &out.doc.variants[0];
        let p = variant.photometry.as_ref().expect("mined photometry");
        assert_eq!(p.flux_lm, Some(1500.0));
        assert_eq!(p.power_w, Some(16.0));
        assert_eq!(p.cct_k, Some(4000));
        assert_eq!(p.cri_ra, Some(80));
    }

    #[test]
    fn empty_manufacturer_warns() {
        let gldf = make_gldf(
            "",
            Some("urn:uuid:test-0002"),
            vec![make_variant("v0", "name")],
        );
        let out = extract(&gldf, ExtractInput::default());
        assert!(out
            .warnings
            .iter()
            .any(|w| matches!(w, ExtractWarning::ManufacturerEmpty)));
    }

    #[test]
    fn variant_without_minable_info_has_no_photometry() {
        let gldf = make_gldf(
            "Test Co.",
            Some("urn:uuid:test-0003"),
            vec![make_variant("v0", "Just A Name")],
        );
        let out = extract(&gldf, ExtractInput::default());
        let v = &out.doc.variants[0];
        assert!(v.photometry.is_none());
        assert!(out.warnings.iter().any(
            |w| matches!(w, ExtractWarning::PhotometryMissing { variant_id } if variant_id == "v0")
        ));
    }

    #[test]
    fn doc_id_falls_back_to_blake3_when_unique_id_missing() {
        let gldf = make_gldf("Acme", None, vec![make_variant("v0", "x")]);
        let bytes = b"fake gldf bytes";
        let out = extract(
            &gldf,
            ExtractInput {
                raw_bytes: Some(bytes),
                ..Default::default()
            },
        );
        // Non-zero DocId means BLAKE3 filled in.
        assert_ne!(out.doc.id.0, [0u8; 32]);
        assert!(!out
            .warnings
            .iter()
            .any(|w| matches!(w, ExtractWarning::DocIdUnavailable)));
    }

    #[test]
    fn missing_both_doc_id_inputs_warns() {
        let gldf = make_gldf("Acme", None, vec![make_variant("v0", "x")]);
        let out = extract(&gldf, ExtractInput::default());
        assert_eq!(out.doc.id.0, [0u8; 32]);
        assert!(out
            .warnings
            .iter()
            .any(|w| matches!(w, ExtractWarning::DocIdUnavailable)));
    }

    #[test]
    fn keywords_include_mined_tokens() {
        let gldf = make_gldf(
            "Acme",
            Some("urn:uuid:kw"),
            vec![make_variant("v0", "Round 22W 4000K IP65")],
        );
        let out = extract(&gldf, ExtractInput::default());
        let kws: Vec<&str> = out.doc.keywords.iter().map(|s| s.as_str()).collect();
        assert!(kws.contains(&"IP65"), "kws: {kws:?}");
        assert!(kws.contains(&"4000K"), "kws: {kws:?}");
        assert!(kws.contains(&"22W"), "kws: {kws:?}");
        assert!(kws.contains(&"Round"), "kws: {kws:?}");
    }

    #[test]
    fn two_variants_union_keywords() {
        let gldf = make_gldf(
            "Acme",
            Some("urn:uuid:multi"),
            vec![
                make_variant("v0", "Round 10W IP44"),
                make_variant("v1", "Round 20W IP65"),
            ],
        );
        let out = extract(&gldf, ExtractInput::default());
        let kws: Vec<&str> = out.doc.keywords.iter().map(|s| s.as_str()).collect();
        assert!(kws.contains(&"IP44"));
        assert!(kws.contains(&"IP65"));
        assert!(kws.contains(&"10W"));
        assert!(kws.contains(&"20W"));
    }
}
