//! Viewer-link configuration — wire type for the "Open in viewer"
//! affordance.
//!
//! The server reads four env vars at startup, picks dev or prod
//! variants, and exposes the resolved [`ViewerConfig`] to the Leptos
//! UI through context (SSR) or hydration (CSR). The UI consumes it
//! purely to construct an external `<a href>` — there's no fetch logic
//! at this layer.

use serde::{Deserialize, Serialize};

/// Resolved viewer configuration: which viewer to link to, where our
/// files live, and which query param the viewer expects.
///
/// Production: `viewer_base="https://gldf.icu"`, `public_base="https://gldf-search.de"`.
/// Dev: `viewer_base="http://127.0.0.1:8052"`, `public_base="http://127.0.0.1:3000"`.
///
/// The resulting "Open in viewer" link is:
///
/// `<viewer_base>?<query_param>=<urlenc(<public_base>/gldfs/<filename>)>`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerConfig {
    /// Public origin of the gldf-search instance — used to prefix
    /// `/gldfs/<filename>` so the viewer can fetch the file.
    pub public_base: String,
    /// Viewer root. The link href starts with this.
    pub viewer_base: String,
    /// Name of the viewer's URL parameter (`url`, `source`, ...).
    /// gldf-rs-wasm uses `url`.
    pub query_param: String,
}

impl ViewerConfig {
    /// Build the "Open in viewer" URL for one filename / relative
    /// path. Encoding has two layers:
    ///
    /// 1. **Path-segment encoding**: percent-encode each segment of
    ///    `filename`, but keep the `/` separators literal. A nested
    ///    corpus path like `luglight/archiline_2_35_led/article.gldf`
    ///    must round-trip through the URL as
    ///    `.../gldfs/luglight/archiline_2_35_led/article.gldf` — NOT
    ///    `.../gldfs/luglight%2Farchiline_2_35_led%2Farticle.gldf`,
    ///    which Apache / nginx would 404 because there's no file
    ///    literally named `luglight%2F…`.
    /// 2. **Query-value encoding**: the whole resulting URL is
    ///    percent-encoded once more, because it's the *value* of the
    ///    viewer's `?url=` parameter.
    ///
    /// The previous implementation called `urlencoding::encode` on the
    /// whole filename, which double-encoded path separators for nested
    /// corpora and broke "Open in viewer" for everything that wasn't
    /// at the root of `/gldfs/`.
    pub fn link_for(&self, filename: &str) -> String {
        let encoded_path = encode_path_keep_slashes(filename);
        let file_url = format!("{}/gldfs/{}", self.public_base, encoded_path);
        let encoded_file_url = urlencoding::encode(&file_url);
        format!(
            "{}?{}={}",
            self.viewer_base, self.query_param, encoded_file_url
        )
    }
}

/// Percent-encode each `/`-delimited segment but leave the `/`
/// characters between segments literal. Equivalent to
/// `parts.iter().map(urlencoding::encode).join("/")`, with the
/// edge cases (`""`, leading/trailing `/`) handled by `split` —
/// they become empty segments, which encode to empty strings.
fn encode_path_keep_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut first = true;
    for segment in path.split('/') {
        if !first {
            out.push('/');
        }
        first = false;
        out.push_str(&urlencoding::encode(segment));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(public: &str, viewer: &str, param: &str) -> ViewerConfig {
        ViewerConfig {
            public_base: public.to_string(),
            viewer_base: viewer.to_string(),
            query_param: param.to_string(),
        }
    }

    #[test]
    fn simple_filename_round_trips() {
        let c = cfg("https://gldf-search.de", "https://gldf.icu", "url");
        let link = c.link_for("acme.gldf");
        assert_eq!(
            link,
            "https://gldf.icu?url=https%3A%2F%2Fgldf-search.de%2Fgldfs%2Facme.gldf"
        );
    }

    #[test]
    fn dev_localhost_uses_dev_bases() {
        let c = cfg("http://127.0.0.1:3000", "http://127.0.0.1:8052", "url");
        let link = c.link_for("test.gldf");
        assert_eq!(
            link,
            "http://127.0.0.1:8052?url=http%3A%2F%2F127.0.0.1%3A3000%2Fgldfs%2Ftest.gldf"
        );
    }

    #[test]
    fn filename_with_unsafe_chars_is_encoded() {
        // Real corpus filenames contain `___`, `-`, `+`, parentheses.
        // Only the bytes that *need* encoding should change.
        let c = cfg(
            "https://example.invalid",
            "https://v.example.invalid",
            "url",
        );
        let link = c.link_for("3f_filippi-3ff___targetti_sankey_b56b891e.gldf");
        assert!(link.contains("3f_filippi-3ff___targetti_sankey_b56b891e.gldf"));
        // `_` and `-` are NOT in RFC 3986's reserved set, so they pass
        // through. Only the `:` and `/` of the URL itself become `%3A`
        // and `%2F`.
        assert!(link.contains("%3A%2F%2Fexample.invalid"));
    }

    #[test]
    fn query_param_name_is_respected() {
        let c = cfg("https://a", "https://b", "source");
        assert!(c.link_for("x.gldf").contains("?source="));
    }

    #[test]
    fn nested_path_keeps_slashes_literal() {
        // Regression: production corpus is nested under
        // `<manufacturer>/<family>/<file>.gldf`. The old
        // implementation called `urlencoding::encode` on the whole
        // relative path, escaping `/` to `%2F`, then encoded the
        // whole URL again, producing `%252F` — the file URL became
        // `.../gldfs/luglight%2Farchiline...`, a single component
        // name that doesn't exist on disk.
        let c = cfg("https://gldf-search.de", "https://gldf.icu", "url");
        let link = c.link_for("luglight/archiline_2_35_led_c3dc/140630_0l09_011.gldf");
        // The viewer URL value is the inner URL once-encoded. So the
        // slashes appear as %2F (single encoding), NEVER %252F.
        assert!(
            link.contains("%2Fluglight%2Farchiline_2_35_led_c3dc%2F140630_0l09_011.gldf"),
            "got: {link}"
        );
        assert!(
            !link.contains("%252F"),
            "double-encoded path separator in: {link}"
        );
    }
}
