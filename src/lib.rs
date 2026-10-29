//! uring-crash: explorador exhaustivo y acotado de estados de crash para
//! protocolos de commit que escriben con io_uring.

pub mod explore;
pub mod model;
pub mod recorder;

pub use explore::{explore, Config, Report};
pub use model::{Corpus, Protocol};

/// Carga un corpus JSON.
pub fn load_corpus(json: &str) -> Result<Corpus, serde_json::Error> {
    serde_json::from_str(json)
}
