use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("not a ZIM file (magic number {0:#010x})")]
    BadMagic(u32),
    #[error("unsupported ZIM major version {0}")]
    UnsupportedVersion(u16),
    #[error("corrupt ZIM file: {0}")]
    Corrupt(String),
    #[error("entry index {index} out of range (entry count {count})")]
    EntryOutOfRange { index: u32, count: u32 },
    #[error("cluster {0} uses unsupported compression type {1}")]
    UnsupportedCompression(u32, u8),
    #[error("failed to decompress cluster {0}: {1}")]
    Decompress(u32, String),
    #[error("redirect chain starting at entry {0} does not end")]
    RedirectLoop(u32),
    #[error("entry {0} is not a content entry")]
    NotContent(u32),
}

pub(crate) fn corrupt(msg: impl Into<String>) -> Error {
    Error::Corrupt(msg.into())
}
