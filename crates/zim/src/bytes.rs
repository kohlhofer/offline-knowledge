//! Bounds-checked little-endian readers over a byte slice.

use crate::error::{Result, corrupt};

pub(crate) fn slice<'a>(data: &'a [u8], offset: u64, len: u64, what: &str) -> Result<&'a [u8]> {
    let start = usize::try_from(offset).map_err(|_| corrupt(format!("{what}: offset overflow")))?;
    let len = usize::try_from(len).map_err(|_| corrupt(format!("{what}: length overflow")))?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| corrupt(format!("{what}: range overflow")))?;
    data.get(start..end)
        .ok_or_else(|| corrupt(format!("{what}: range {start}..{end} beyond {} bytes", data.len())))
}

pub(crate) fn u16_at(data: &[u8], offset: u64, what: &str) -> Result<u16> {
    let b = slice(data, offset, 2, what)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn u32_at(data: &[u8], offset: u64, what: &str) -> Result<u32> {
    let b = slice(data, offset, 4, what)?;
    Ok(u32::from_le_bytes(b.try_into().unwrap()))
}

pub(crate) fn u64_at(data: &[u8], offset: u64, what: &str) -> Result<u64> {
    let b = slice(data, offset, 8, what)?;
    Ok(u64::from_le_bytes(b.try_into().unwrap()))
}

/// Longest path, title or MIME type the reader accepts. Real ones are a few
/// hundred bytes; the cap keeps a hostile file from making every lookup scan
/// megabytes.
pub(crate) const MAX_STRING: usize = 64 * 1024;

/// A NUL-terminated string starting at `offset`; returns the bytes without the NUL.
pub(crate) fn cstr_at<'a>(data: &'a [u8], offset: u64, what: &str) -> Result<&'a [u8]> {
    let start = usize::try_from(offset).map_err(|_| corrupt(format!("{what}: offset overflow")))?;
    let rest = data
        .get(start..)
        .ok_or_else(|| corrupt(format!("{what}: offset {start} beyond end")))?;
    let window = &rest[..rest.len().min(MAX_STRING + 1)];
    let nul = memchr::memchr(0, window)
        .ok_or_else(|| corrupt(format!("{what}: no terminator within {MAX_STRING} bytes of {start}")))?;
    Ok(&rest[..nul])
}
