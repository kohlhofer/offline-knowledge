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
    #[error("{zim} has not been imported yet; run `ok import {zim}`", zim = .0.display())]
    NotImported(PathBuf),
    #[error("the index at {index} was built from a different file (expected ZIM {expected}, found {found})", index = .index.display())]
    IndexMismatch { index: PathBuf, expected: String, found: String },
    #[error("index format {found} is not supported (expected {expected}); re-run `ok import`")]
    IndexFormat { expected: u32, found: u32 },
    #[error("entry {0} is not an article")]
    NotArticle(u32),
}
