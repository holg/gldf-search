//! Derive the schema [`DocId`] for a GLDF.
//!
//! Priority order (concept doc §11 q1, settled by the 10-file sample):
//! 1. `<Header><UniqueGldfId>` if present and non-empty — always
//!    available in the `light-other-rs` corpus, used verbatim.
//! 2. BLAKE3 over the supplied raw bytes (extractor caller passes the
//!    file content) — fallback when the header field is missing.
//!
//! We hash the **UniqueGldfId string** rather than parse it as a UUID:
//! the spec allows any URN, not just `urn:uuid:`. Hashing keeps the
//! schema's `DocId([u8; 32])` shape uniform regardless of source.

use blake3::Hasher;
use gldf_search_schema::doc::DocId;

/// Derive a [`DocId`] from a GLDF's UniqueGldfId (if present) or the
/// raw file bytes (fallback).
pub fn derive_doc_id(unique_gldf_id: Option<&str>, raw_bytes: Option<&[u8]>) -> DocId {
    if let Some(s) = unique_gldf_id {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return hash_str(trimmed);
        }
    }
    if let Some(bytes) = raw_bytes {
        return hash_bytes(bytes);
    }
    // Caller gave us neither — return all-zeros so the downstream index
    // can detect "extractor had no identity input" via a single
    // comparison. This is a corpus-quality signal, not a panic case.
    DocId([0; 32])
}

fn hash_str(s: &str) -> DocId {
    hash_bytes(s.as_bytes())
}

fn hash_bytes(bytes: &[u8]) -> DocId {
    let mut h = Hasher::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_bytes());
    DocId(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_id_drives_the_hash() {
        let a = derive_doc_id(Some("urn:uuid:0001"), Some(b"different"));
        let b = derive_doc_id(Some("urn:uuid:0001"), Some(b"also-different"));
        // Same UniqueGldfId → same DocId regardless of file bytes.
        assert_eq!(a, b);
    }

    #[test]
    fn whitespace_only_unique_id_falls_back_to_bytes() {
        let a = derive_doc_id(Some("   "), Some(b"hello"));
        let b = derive_doc_id(None, Some(b"hello"));
        assert_eq!(a, b, "whitespace-only id must not affect the digest");
    }

    #[test]
    fn missing_both_yields_zeros() {
        let id = derive_doc_id(None, None);
        assert_eq!(id, DocId([0; 32]));
    }

    #[test]
    fn distinct_unique_ids_differ() {
        let a = derive_doc_id(Some("urn:uuid:0001"), None);
        let b = derive_doc_id(Some("urn:uuid:0002"), None);
        assert_ne!(a, b);
    }
}
