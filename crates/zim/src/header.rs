use crate::bytes::{u16_at, u32_at, u64_at};
use crate::error::{Error, Result, corrupt};

pub const MAGIC: u32 = 72_173_914;
pub const HEADER_LEN: u64 = 80;
const NO_PAGE: u32 = u32::MAX;

/// The fixed 80-byte header at the start of every ZIM file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub major_version: u16,
    pub minor_version: u16,
    pub uuid: [u8; 16],
    pub entry_count: u32,
    pub cluster_count: u32,
    pub path_ptr_pos: u64,
    /// Position of the v0 title pointer list, if the file has one.
    pub title_ptr_pos: Option<u64>,
    pub cluster_ptr_pos: u64,
    pub mime_list_pos: u64,
    pub main_page: Option<u32>,
    pub layout_page: Option<u32>,
    pub checksum_pos: u64,
}

impl Header {
    pub fn parse(data: &[u8]) -> Result<Header> {
        if (data.len() as u64) < HEADER_LEN {
            return Err(corrupt(format!("file is {} bytes, smaller than the header", data.len())));
        }
        let magic = u32_at(data, 0, "magic")?;
        if magic != MAGIC {
            return Err(Error::BadMagic(magic));
        }
        let major_version = u16_at(data, 4, "major version")?;
        if !(5..=6).contains(&major_version) {
            return Err(Error::UnsupportedVersion(major_version));
        }
        let page = |v: u32| (v != NO_PAGE).then_some(v);
        let header = Header {
            major_version,
            minor_version: u16_at(data, 6, "minor version")?,
            uuid: data[8..24].try_into().unwrap(),
            entry_count: u32_at(data, 24, "entry count")?,
            cluster_count: u32_at(data, 28, "cluster count")?,
            path_ptr_pos: u64_at(data, 32, "path pointer position")?,
            title_ptr_pos: {
                let pos = u64_at(data, 40, "title pointer position")?;
                (pos != 0 && pos != u64::MAX).then_some(pos)
            },
            cluster_ptr_pos: u64_at(data, 48, "cluster pointer position")?,
            mime_list_pos: u64_at(data, 56, "mime list position")?,
            main_page: page(u32_at(data, 64, "main page")?),
            layout_page: page(u32_at(data, 68, "layout page")?),
            checksum_pos: u64_at(data, 72, "checksum position")?,
        };
        header.validate(data.len() as u64)?;
        Ok(header)
    }

    fn validate(&self, file_len: u64) -> Result<()> {
        let within = |pos: u64, len: u64, what: &str| -> Result<()> {
            match pos.checked_add(len) {
                Some(end) if end <= file_len => Ok(()),
                _ => Err(corrupt(format!("{what} at {pos} (+{len}) runs past the {file_len}-byte file"))),
            }
        };
        within(self.mime_list_pos, 1, "mime list")?;
        within(self.path_ptr_pos, u64::from(self.entry_count) * 8, "path pointer list")?;
        if let Some(pos) = self.title_ptr_pos {
            within(pos, u64::from(self.entry_count) * 4, "title pointer list")?;
        }
        within(self.cluster_ptr_pos, u64::from(self.cluster_count) * 8, "cluster pointer list")?;
        // checksum_pos == 0 means "no checksum" to libzim; the main page index is
        // checked where it is used, as libzim does.
        if self.checksum_pos != 0 {
            within(self.checksum_pos, 16, "checksum")?;
        }
        Ok(())
    }

    /// Files from minor version 1 on use the new namespace scheme (`C`, `M`, `W`, `X`).
    pub fn uses_new_namespaces(&self) -> bool {
        self.major_version >= 6 && self.minor_version >= 1
    }

    pub fn uuid_hex(&self) -> String {
        self.uuid.iter().map(|b| format!("{b:02x}")).collect()
    }
}
