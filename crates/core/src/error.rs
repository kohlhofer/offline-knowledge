use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Zim(#[from] ok_zim::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("search index error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("title index error: {0}")]
    Fst(#[from] fst::Error),
    #[error("index metadata error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{zim} has not been imported yet; run `ok --zim {zim} import`", zim = .0.display())]
    NotImported(PathBuf),
    #[error("the index at {index} was built from a different file (expected ZIM {expected}, found {found})", index = .index.display())]
    IndexMismatch { index: PathBuf, expected: String, found: String },
    #[error("index format {found} is not supported (expected {expected}); re-run `ok import`")]
    IndexFormat { expected: u32, found: u32 },
    #[error("entry {0} is not an article")]
    NotArticle(u32),
    #[error("\"{0}\" is not a usable collection label: 1-24 characters, a lowercase letter then lowercase letters, digits or -")]
    Label(String),
    #[error("{zim} was not written by mwoffliner ({scraper}); ok reads mwoffliner's HTML only", zim = .zim.display())]
    UnsupportedScraper { zim: PathBuf, scraper: String },
    #[error("no collection could be loaded")]
    NoCollections,
    #[error("no collection is labeled \"{label}\"; loaded: {loaded}")]
    UnknownCollection { label: String, loaded: String },
    /// A collection whose label or library failed to resolve. The reason is
    /// the original error's message: it is cached and handed out again on
    /// every later use, and an [`Error`] is not `Clone`.
    #[error("{reason}")]
    CollectionFailed { reason: String },
}
