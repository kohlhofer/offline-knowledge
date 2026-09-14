use crate::bytes::{cstr_at, slice, u16_at, u32_at};
use crate::error::{Result, corrupt};

const MIME_REDIRECT: u16 = 0xffff;
const MIME_LINKTARGET: u16 = 0xfffe;
const MIME_DELETED: u16 = 0xfffd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirentKind {
    Content { mime_index: u16, cluster: u32, blob: u32 },
    Redirect { target: u32 },
    /// Link-target and deleted entries: obsolete, carry no content.
    Other,
}

/// A directory entry borrowed straight from the memory-mapped file.
#[derive(Debug, Clone, Copy)]
pub struct DirentRef<'a> {
    pub namespace: u8,
    pub revision: u32,
    pub kind: DirentKind,
    pub path: &'a [u8],
    /// Raw title bytes; empty means "same as path" (see [`DirentRef::title`]).
    pub raw_title: &'a [u8],
    pub parameter: &'a [u8],
}

impl<'a> DirentRef<'a> {
    pub fn parse(data: &'a [u8], offset: u64) -> Result<DirentRef<'a>> {
        let mime = u16_at(data, offset, "dirent mimetype")?;
        let parameter_len = *slice(data, offset + 2, 1, "dirent parameter length")?.first().unwrap();
        let namespace = *slice(data, offset + 3, 1, "dirent namespace")?.first().unwrap();
        let revision = u32_at(data, offset + 4, "dirent revision")?;
        let (kind, strings_at) = match mime {
            MIME_REDIRECT => {
                let target = u32_at(data, offset + 8, "redirect index")?;
                (DirentKind::Redirect { target }, offset + 12)
            }
            MIME_LINKTARGET | MIME_DELETED => (DirentKind::Other, offset + 8),
            mime_index => {
                let cluster = u32_at(data, offset + 8, "dirent cluster")?;
                let blob = u32_at(data, offset + 12, "dirent blob")?;
                (DirentKind::Content { mime_index, cluster, blob }, offset + 16)
            }
        };
        let path = cstr_at(data, strings_at, "dirent path")?;
        let title_at = strings_at + path.len() as u64 + 1;
        let raw_title = cstr_at(data, title_at, "dirent title")?;
        let param_at = title_at + raw_title.len() as u64 + 1;
        let parameter = slice(data, param_at, u64::from(parameter_len), "dirent parameter")?;
        if !namespace.is_ascii_graphic() {
            return Err(corrupt(format!("dirent at {offset} has namespace byte {namespace:#04x}")));
        }
        Ok(DirentRef { namespace, revision, kind, path, raw_title, parameter })
    }

    /// The title bytes, falling back to the path when the stored title is empty.
    pub fn title(&self) -> &'a [u8] {
        if self.raw_title.is_empty() { self.path } else { self.raw_title }
    }

    pub fn path_str(&self) -> String {
        String::from_utf8_lossy(self.path).into_owned()
    }

    pub fn title_str(&self) -> String {
        String::from_utf8_lossy(self.title()).into_owned()
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.kind, DirentKind::Redirect { .. })
    }
}
