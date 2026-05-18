//! Per-locale text gathering.
//!
//! Walks the parts of `GldfProduct` that carry language-tagged strings
//! and concatenates them by locale into `LuminaireDoc.descriptions`.
//!
//! What counts as fulltext source for this corpus:
//! - `<ProductDefinitions><ProductMetaData><Name>` per locale
//! - `<ProductDefinitions><ProductMetaData><Description>` per locale
//! - `<ProductDefinitions><ProductMetaData><TenderText>` per locale
//! - Every `<Variant><Name>` and `<Variant><Description>` per locale
//!
//! Boilerplate descriptions ("Product without accessories" / "Artikel
//! ohne Zubehör") are kept in. Filtering them out is a bad idea —
//! they're cheap to index, and some users may legitimately query
//! "luminaire without accessories" to find bare-product GLDFs.

use std::collections::BTreeMap;

use gldf_rs::gldf::header::LocaleFoo;
use gldf_rs::gldf::GldfProduct;
use gldf_search_schema::filter::Locale;
use smallvec::SmallVec;

/// One concatenated string per locale found in the document.
///
/// Locales whose 2-letter code we can't parse are dropped on the floor
/// (consistent with the schema's `Locale([u8; 2])` constraint). The
/// extractor flags this in its warning surface.
pub fn gather_descriptions(gldf: &GldfProduct) -> SmallVec<[(Locale, String); 1]> {
    let mut by_locale: BTreeMap<Locale, String> = BTreeMap::new();

    if let Some(meta) = &gldf.product_definitions.product_meta_data {
        push_from_locale_foo(meta.name.as_ref(), &mut by_locale);
        push_from_locale_foo(meta.description.as_ref(), &mut by_locale);
        push_from_locale_foo(meta.tender_text.as_ref(), &mut by_locale);
        push_from_locale_foo(meta.product_number.as_ref(), &mut by_locale);
    }

    if let Some(vs) = &gldf.product_definitions.variants {
        for v in &vs.variant {
            push_from_locale_foo(v.name.as_ref(), &mut by_locale);
            push_from_locale_foo(v.description.as_ref(), &mut by_locale);
            push_from_locale_foo(v.product_number.as_ref(), &mut by_locale);
            push_from_locale_foo(v.tender_text.as_ref(), &mut by_locale);
        }
    }

    by_locale.into_iter().collect()
}

fn push_from_locale_foo(bag: Option<&LocaleFoo>, out: &mut BTreeMap<Locale, String>) {
    let Some(bag) = bag else { return };
    for entry in &bag.locale {
        if entry.value.trim().is_empty() {
            continue;
        }
        let Some(loc) = Locale::from_str(&entry.language) else {
            // Non-2-letter code (e.g. "en-US" or "zh-Hant"). Drop with
            // no warning here — the schema doesn't model sub-tags, and
            // the corpus uses bare ISO-639-1 codes uniformly.
            continue;
        };
        let bucket = out.entry(loc).or_default();
        if !bucket.is_empty() {
            bucket.push(' ');
        }
        bucket.push_str(&entry.value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gldf_rs::gldf::header::Locale as GldfLocale;

    fn loc(lang: &str, value: &str) -> GldfLocale {
        GldfLocale {
            language: lang.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn skips_blank_values() {
        let mut out = BTreeMap::new();
        let bag = LocaleFoo {
            locale: vec![loc("en", ""), loc("en", "   "), loc("en", "real")],
        };
        push_from_locale_foo(Some(&bag), &mut out);
        let en = Locale::from_str("en").unwrap();
        assert_eq!(out.get(&en).map(String::as_str), Some("real"));
    }

    #[test]
    fn drops_non_two_letter_locales() {
        let mut out = BTreeMap::new();
        let bag = LocaleFoo {
            locale: vec![loc("en-US", "hi"), loc("zh-Hant", "no"), loc("en", "yes")],
        };
        push_from_locale_foo(Some(&bag), &mut out);
        let en = Locale::from_str("en").unwrap();
        assert_eq!(out.get(&en).map(String::as_str), Some("yes"));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn merges_multiple_sources_with_space() {
        let mut out = BTreeMap::new();
        let a = LocaleFoo {
            locale: vec![loc("en", "first")],
        };
        let b = LocaleFoo {
            locale: vec![loc("en", "second")],
        };
        push_from_locale_foo(Some(&a), &mut out);
        push_from_locale_foo(Some(&b), &mut out);
        let en = Locale::from_str("en").unwrap();
        assert_eq!(out.get(&en).map(String::as_str), Some("first second"));
    }
}
