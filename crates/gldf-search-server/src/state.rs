//! Server state. Held in an `Arc` and cloned freely into Axum
//! handlers; the underlying [`InMemoryIndex`] is immutable so no
//! lock is needed.

use std::path::PathBuf;
use std::sync::Arc;

use gldf_search_index::InMemoryIndex;

#[derive(Clone)]
pub struct AppState {
    /// The query backend. `Arc` clone is cheap.
    pub index: Arc<InMemoryIndex>,
    /// Where the source `.gldf` files live (for the `/gldfs/...`
    /// static route and for logging).
    #[allow(dead_code)]
    pub corpus_root: PathBuf,
}
