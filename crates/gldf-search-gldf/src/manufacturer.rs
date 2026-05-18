//! Manufacturer name normalisation.
//!
//! GLDF lets manufacturers write `<Manufacturer>` however they want.
//! The 10-file sample showed: `A.L.S.`, `BEGA`, `Lena Lighting S.A.`,
//! `Performance in Lighting`, `Philips`, `Prolicht`, `RUCO LICHT`,
//! `RZB` — mixed case, punctuation, legal suffixes.
//!
//! The index keys facets and dedup off a **canonical key** (lowercased,
//! punctuation stripped, legal suffixes removed, whitespace collapsed)
//! while keeping the **display name** as-written. Two file orders:
//!
//! - `"Lena Lighting S.A."` and `"Lena Lighting"` collapse to the same
//!   canonical key (`lena lighting`) → grouped under one facet.
//! - `"A.L.S."` and `"A L S"` and `"als"` all collapse to `als` — same
//!   manufacturer despite different spellings.
//!
//! The display name keeps its first-seen casing for the result UI.

use compact_str::CompactString;

const LEGAL_SUFFIXES: &[&str] = &[
    "s.a.",
    "s.a",
    "sa", // Sociedad / Société Anonyme
    "gmbh",
    "g.m.b.h.",
    "ag",
    "a.g.",
    "ltd",
    "ltd.",
    "limited",
    "inc",
    "inc.",
    "incorporated",
    "llc",
    "l.l.c.",
    "co",
    "co.",
    "company",
    "kg",
    "k.g.",
    "ohg",
    "o.h.g.",
    "se", // Societas Europaea — also legitimate German for "see"; suffix-position only
    "plc",
    "n.v.",
    "nv", // Naamloze Vennootschap
    "b.v.",
    "bv", // Besloten Vennootschap
    "s.r.l.",
    "srl", // Italian Limited
    "s.p.a.",
    "spa", // Italian PLC
    "s.l.",
    "sl", // Spanish Limited
    "s.l.u.",
    "slu",
    "oy", // Finnish Limited
    "ab", // Swedish AB
    "as",
    "a.s.", // Norwegian/Danish A/S — bare "as" risks false positives; suffix-position only mitigates
];

/// Result of normalising a manufacturer string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManufacturerName {
    /// As written in the source GLDF, trimmed but otherwise unchanged.
    pub display: CompactString,
    /// Canonical lowercased + de-punctuated + suffix-stripped + space-
    /// collapsed key. Used for facet matching and dedup.
    pub key: CompactString,
}

/// Build a [`ManufacturerName`] from the raw `<Header><Manufacturer>`
/// string. Empty / whitespace-only input produces empty display + key
/// (the extractor warns separately).
pub fn normalise(raw: &str) -> ManufacturerName {
    let display = raw.trim().to_string();
    let key = canonical_key(&display);
    ManufacturerName {
        display: CompactString::from(display),
        key: CompactString::from(key),
    }
}

/// Compute the canonical key. Exposed so tests can lock its behaviour
/// down; the extractor goes through [`normalise`] instead.
pub fn canonical_key(input: &str) -> String {
    // Step 1: lowercase, replace punctuation with spaces (we keep the
    // word boundary so "A.L.S." → "a l s" → "als" after collapsing,
    // and "S.A." stays detectable as the suffix "s a").
    let lowered: String = input
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();

    // Step 2: collapse whitespace into single spaces and trim.
    let collapsed: String = {
        let mut out = String::with_capacity(lowered.len());
        let mut last_was_space = true;
        for c in lowered.chars() {
            if c == ' ' {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            } else {
                out.push(c);
                last_was_space = false;
            }
        }
        out.trim_end().to_string()
    };

    // Step 3: strip trailing legal-suffix tokens (one or more, in
    // succession). At this point dots in the input have already been
    // turned into spaces, so "Lena Lighting S.A." → "lena lighting s a"
    // and we strip the trailing " s a" suffix.
    //
    // For each suffix in the literal list, compute its canonical-step
    // form (dots → spaces, spaces collapsed) and try to strip that
    // shape from the right.
    let mut s = collapsed;
    loop {
        let original_len = s.len();
        for suffix in LEGAL_SUFFIXES {
            // Replicate steps 1+2 on the suffix itself so we compare
            // apples to apples.
            let canonical_suffix = suffix_canonical_form(suffix);
            if canonical_suffix.is_empty() {
                continue;
            }
            let needle = format!(" {canonical_suffix}");
            if s.ends_with(&needle) {
                s.truncate(s.len() - needle.len());
                break;
            }
            if s == canonical_suffix {
                s.clear();
                break;
            }
        }
        if s.len() == original_len {
            break;
        }
    }

    // Step 4: collapse any remaining single-letter runs (`a l s` → `als`)
    // so different spellings of the same acronym match.
    let collapsed_acronyms = collapse_single_letter_runs(&s);
    collapsed_acronyms.trim().to_string()
}

/// Apply the canonical-key normalisation (steps 1+2) to a legal-suffix
/// literal so it matches the shape of the manufacturer string we'd
/// already normalised. `"S.A."` → `"s a"`, `"GmbH"` → `"gmbh"`.
fn suffix_canonical_form(suffix: &str) -> String {
    let lowered: String = suffix
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_space = true;
    for c in lowered.chars() {
        if c == ' ' {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out.trim_end().to_string()
}

/// Collapse runs of ≥2 single-letter words into one token. So
/// `"a l s"` → `"als"`, but `"performance in lighting"` is left alone
/// because no consecutive 1-letter tokens. The legacy `RUCO LICHT` →
/// `"ruco licht"` also stays as two words.
fn collapse_single_letter_runs(s: &str) -> String {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        if tok.chars().count() == 1 {
            // Start of a single-letter run. Find how far it extends.
            let mut j = i;
            while j < tokens.len() && tokens[j].chars().count() == 1 {
                j += 1;
            }
            if j - i >= 2 {
                // Concatenate the run into one token.
                if !out.is_empty() {
                    out.push(' ');
                }
                for t in &tokens[i..j] {
                    out.push_str(t);
                }
                i = j;
                continue;
            }
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(tok);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punctuation_acronym_collapses() {
        assert_eq!(canonical_key("A.L.S."), "als");
        assert_eq!(canonical_key("a.l.s"), "als");
        assert_eq!(canonical_key("A L S"), "als");
        assert_eq!(canonical_key("als"), "als");
    }

    #[test]
    fn legal_suffix_strips() {
        assert_eq!(canonical_key("Lena Lighting S.A."), "lena lighting");
        assert_eq!(canonical_key("Lena Lighting"), "lena lighting");
        assert_eq!(canonical_key("Acme GmbH"), "acme");
        assert_eq!(canonical_key("Foo Bar AG"), "foo bar");
        assert_eq!(canonical_key("Bar Ltd."), "bar");
    }

    #[test]
    fn case_is_collapsed() {
        assert_eq!(canonical_key("RUCO LICHT"), "ruco licht");
        assert_eq!(canonical_key("Ruco Licht"), "ruco licht");
        assert_eq!(canonical_key("RUCO LiChT"), "ruco licht");
    }

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(
            canonical_key("  Performance   in    Lighting  "),
            "performance in lighting"
        );
    }

    #[test]
    fn empty_input_yields_empty_key() {
        assert_eq!(canonical_key(""), "");
        assert_eq!(canonical_key("   "), "");
    }

    #[test]
    fn corpus_sample_round_trip() {
        // From the 10-file sample. Two are useful equivalence checks
        // (same physical manufacturer should produce the same key).
        for (raw, expected) in [
            ("A.L.S.", "als"),
            ("BEGA", "bega"),
            ("Lena Lighting S.A.", "lena lighting"),
            ("Performance in Lighting", "performance in lighting"),
            ("Philips", "philips"),
            ("Prolicht", "prolicht"),
            ("RUCO LICHT", "ruco licht"),
            ("RZB", "rzb"),
        ] {
            assert_eq!(canonical_key(raw), expected, "for raw = {raw:?}");
        }
    }

    #[test]
    fn display_preserves_casing() {
        let m = normalise("  RUCO LICHT  ");
        assert_eq!(m.display, "RUCO LICHT");
        assert_eq!(m.key, "ruco licht");
    }
}
