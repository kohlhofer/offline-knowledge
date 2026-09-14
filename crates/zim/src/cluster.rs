//! Clusters: an info byte, then (possibly compressed) a blob offset table
//! followed by the blobs.
//!
//! A cluster's size is never taken from the next cluster pointer or the
//! checksum position: real files put dirents and pointer lists after the last
//! cluster. Like libzim, the reader starts at the cluster's first byte and
//! reads exactly as far as the offset table says, capped by hard limits so a
//! hostile file cannot make it allocate or decompress without bound.

use std::io::Read;

use crate::bytes::{slice, u32_at, u64_at};
use crate::error::{Error, Result, corrupt};

const COMPRESSION_MASK: u8 = 0x0f;
const EXTENDED_FLAG: u8 = 0x10;

/// Largest decompressed cluster the reader accepts. libzim targets 2 MiB;
/// oversized single-blob clusters exist but text ZIMs stay far below this.
/// Uncompressed clusters are read in place from the mapping and are not limited.
pub const MAX_DECODED_CLUSTER: u64 = 128 * 1024 * 1024;
const XZ_MEMLIMIT: u64 = 256 * 1024 * 1024;
const ZSTD_WINDOW_LOG_MAX: u32 = 27;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Compression {
    None,
    Xz,
    Zstd,
}

pub(crate) struct Info {
    pub compression: Compression,
    pub width: u64,
}

pub(crate) fn info(cluster: u32, byte: u8) -> Result<Info> {
    let compression = match byte & COMPRESSION_MASK {
        0 | 1 => Compression::None,
        4 => Compression::Xz,
        5 => Compression::Zstd,
        other => return Err(Error::UnsupportedCompression(cluster, other)),
    };
    Ok(Info { compression, width: if byte & EXTENDED_FLAG != 0 { 8 } else { 4 } })
}

/// A decompressed cluster held in memory.
#[derive(Debug)]
pub struct Cluster {
    data: Vec<u8>,
    offsets: Vec<u64>,
}

impl Cluster {
    pub fn blob_count(&self) -> u32 {
        (self.offsets.len() - 1) as u32
    }

    pub fn blob(&self, blob: u32) -> Option<&[u8]> {
        let i = blob as usize;
        let (start, end) = (*self.offsets.get(i)?, *self.offsets.get(i + 1)?);
        self.data.get(start as usize..end as usize)
    }

    /// Bytes held in memory, for cache accounting: capacity, not length.
    pub fn memory_size(&self) -> usize {
        self.data.capacity() + self.offsets.capacity() * 8
    }
}

/// Decodes a compressed cluster whose body (after the info byte) starts at
/// `body[0]`; `body` may run past the cluster's end.
pub(crate) fn decode_compressed(cluster: u32, info: &Info, body: &[u8]) -> Result<Cluster> {
    let fail = |e: std::io::Error| Error::Decompress(cluster, e.to_string());
    let mut reader: Box<dyn Read + '_> = match info.compression {
        Compression::Xz => {
            let stream = xz2::stream::Stream::new_stream_decoder(XZ_MEMLIMIT, 0)
                .map_err(|e| Error::Decompress(cluster, e.to_string()))?;
            Box::new(xz2::read::XzDecoder::new_stream(body, stream))
        }
        Compression::Zstd => {
            let mut d = zstd::stream::read::Decoder::with_buffer(body).map_err(fail)?.single_frame();
            d.window_log_max(ZSTD_WINDOW_LOG_MAX).map_err(fail)?;
            Box::new(d)
        }
        Compression::None => unreachable!("uncompressed clusters are read in place"),
    };

    let width = info.width as usize;
    let mut first_bytes = [0u8; 8];
    reader.read_exact(&mut first_bytes[..width]).map_err(fail)?;
    let first = parse_offset(&first_bytes[..width]);
    check_table_start(cluster, first, info.width, MAX_DECODED_CLUSTER)?;

    // Grow buffers only as decompressed bytes actually arrive, so a truncated or
    // lying stream cannot make the reader reserve memory up front.
    let mut data = first_bytes[..width].to_vec();
    read_exactly(&mut reader, &mut data, first, cluster)?;
    let offsets = parse_table(cluster, &data, info.width, MAX_DECODED_CLUSTER)?;
    let end = *offsets.last().expect("table has at least one offset");
    read_exactly(&mut reader, &mut data, end, cluster)?;
    Ok(Cluster { data, offsets })
}

fn read_exactly(reader: &mut dyn Read, data: &mut Vec<u8>, until: u64, cluster: u32) -> Result<()> {
    let want = until - data.len() as u64;
    let got = reader
        .take(want)
        .read_to_end(data)
        .map_err(|e| Error::Decompress(cluster, e.to_string()))?;
    if got as u64 != want {
        return Err(Error::Decompress(cluster, format!("stream ended {} bytes early", want - got as u64)));
    }
    Ok(())
}

/// The file range of blob `blob` in an uncompressed cluster whose offset table
/// starts at `table_pos` in `file`. Reads two offsets, copies nothing.
pub(crate) fn uncompressed_blob_range(
    file: &[u8],
    cluster: u32,
    info: &Info,
    table_pos: u64,
    blob: u32,
) -> Result<(u64, u64)> {
    let read = |i: u64| -> Result<u64> {
        let at = table_pos + i * info.width;
        if info.width == 8 { u64_at(file, at, "cluster offset") } else { u32_at(file, at, "cluster offset").map(u64::from) }
    };
    let available = (file.len() as u64).saturating_sub(table_pos);
    let first = read(0)?;
    check_table_start(cluster, first, info.width, available)?;
    let count = first / info.width;
    let index = u64::from(blob);
    if index + 1 >= count {
        return Err(corrupt(format!("blob {blob} out of range in cluster {cluster} ({} blobs)", count - 1)));
    }
    let (start, end) = (read(index)?, read(index + 1)?);
    if start < first || end < start || end > available {
        return Err(corrupt(format!("cluster {cluster} blob {blob} has offsets {start}..{end}")));
    }
    // Make sure the range is really inside the file before handing it out.
    slice(file, table_pos + start, end - start, "blob")?;
    Ok((table_pos + start, table_pos + end))
}

fn parse_offset(bytes: &[u8]) -> u64 {
    match bytes.len() {
        8 => u64::from_le_bytes(bytes.try_into().unwrap()),
        _ => u64::from(u32::from_le_bytes(bytes.try_into().unwrap())),
    }
}

fn check_table_start(cluster: u32, first: u64, width: u64, limit: u64) -> Result<()> {
    if first < width || first % width != 0 {
        return Err(corrupt(format!("cluster {cluster} first offset {first} is not a positive multiple of {width}")));
    }
    if first > limit {
        return Err(corrupt(format!("cluster {cluster} offset table of {first} bytes exceeds {limit}")));
    }
    Ok(())
}

fn parse_table(cluster: u32, table: &[u8], width: u64, limit: u64) -> Result<Vec<u64>> {
    let offsets: Vec<u64> = table.chunks_exact(width as usize).map(parse_offset).collect();
    let first = offsets[0];
    let mut previous = first;
    for (i, &offset) in offsets.iter().enumerate() {
        if offset < previous || offset > limit {
            return Err(corrupt(format!("cluster {cluster} offset {i} = {offset} is out of order or above {limit}")));
        }
        previous = offset;
    }
    Ok(offsets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zstd_body(table: &[u32], payload: &mut dyn Read) -> Vec<u8> {
        let mut enc = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
        for o in table {
            std::io::Write::write_all(&mut enc, &o.to_le_bytes()).unwrap();
        }
        std::io::copy(payload, &mut enc).unwrap();
        enc.finish().unwrap()
    }

    const ZSTD: Info = Info { compression: Compression::Zstd, width: 4 };

    #[test]
    fn decodes_exactly_the_declared_bytes_and_ignores_trailing_data() {
        let mut body = zstd_body(&[12, 15, 17], &mut &b"abcde"[..]);
        body.extend_from_slice(b"dirents and pointer lists follow here");
        let c = decode_compressed(0, &ZSTD, &body).unwrap();
        assert_eq!(c.blob(0), Some(&b"abc"[..]));
        assert_eq!(c.blob(1), Some(&b"de"[..]));
        assert_eq!(c.blob(2), None);
    }

    #[test]
    fn refuses_a_decompression_bomb_quickly() {
        let size = MAX_DECODED_CLUSTER as u32 + 1024;
        let body = zstd_body(&[8, 8 + size], &mut std::io::repeat(0).take(u64::from(size)));
        let started = std::time::Instant::now();
        assert!(matches!(decode_compressed(0, &ZSTD, &body), Err(Error::Corrupt(_))));
        assert!(started.elapsed().as_millis() < 500);
    }

    #[test]
    fn refuses_huge_or_malformed_offset_tables() {
        for table in [&[0u32][..], &[6], &[u32::MAX, 0], &[12, 8, 20]] {
            let body = zstd_body(table, &mut &[0u8; 32][..]);
            assert!(decode_compressed(0, &ZSTD, &body).is_err(), "{table:?}");
        }
        // Uncompressed, extended, first offset near u64::MAX: no allocation, an error.
        let mut file = vec![0u8; 8];
        file.extend_from_slice(&0x8000_0000_0000_0000u64.to_le_bytes());
        let extended = Info { compression: Compression::None, width: 8 };
        assert!(uncompressed_blob_range(&file, 0, &extended, 8, 0).is_err());
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let body = zstd_body(&[12, 15, 17], &mut &b"abcde"[..]);
        assert!(decode_compressed(0, &ZSTD, &body[..body.len() - 4]).is_err());
    }
}
