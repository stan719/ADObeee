//! Read-only ZIP container (GB/T 33190-2016 §6: an OFD file is a ZIP package).
//!
//! Only what OFD needs: stored and Deflate entries, read from the central directory. Every number
//! comes from the file and is checked: entry count, declared and actual sizes, the compression
//! ratio (zip bombs), the CRC, and names (no `..`, no absolute or drive paths, no duplicates).
//! ZIP64, multi-disk archives and encrypted entries are refused with an error that says so.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Read;

use crate::{Limits, OfdError};

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
const EOCD_LEN: usize = 22;
const CENTRAL_LEN: usize = 46;
const LOCAL_LEN: usize = 30;
/// Below this size an entry is never refused for its compression ratio (small, very repetitive
/// XML compresses well and is harmless).
const RATIO_FLOOR: u64 = 1 << 20;

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    b.get(at..end).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_le_bytes)
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    b.get(at..end).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_le_bytes)
}

fn corrupt(what: &str) -> OfdError {
    OfdError::Container(format!("the ZIP container is damaged ({what})"))
}

/// Whether `bytes` starts like a ZIP file (a local file header).
pub fn looks_like_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    method: u16,
    flags: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    offset: u64,
}

/// An opened ZIP package. Entries are inflated on demand by [`Package::read`]; the running total
/// of inflated bytes is bounded by [`Limits::max_total_size`].
#[derive(Debug)]
pub struct Package<'a> {
    bytes: &'a [u8],
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
    limits: Limits,
    inflated: Cell<u64>,
}

/// A ZIP entry name made safe: `\` read as `/`, `.` and empty segments dropped. `None` (refused)
/// for `..`, an absolute path, a drive letter or a NUL. Directory entries come back empty.
pub(crate) fn clean_entry_name(raw: &str) -> Option<String> {
    let name = raw.replace('\\', "/");
    if name.starts_with('/') || name.contains('\0') {
        return None;
    }
    let mut parts = Vec::new();
    for (i, seg) in name.split('/').enumerate() {
        match seg {
            "" | "." => {}
            ".." => return None,
            s if i == 0 && s.len() == 2 && s.ends_with(':') => return None,
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

impl<'a> Package<'a> {
    /// Read the central directory of `bytes`. Nothing is inflated yet.
    pub fn open(bytes: &'a [u8], limits: &Limits) -> Result<Self, OfdError> {
        if !looks_like_zip(bytes) || bytes.len() < EOCD_LEN {
            return Err(OfdError::NotOfd("the file is not a ZIP package".into()));
        }
        // The end-of-central-directory record sits in the last 22 + 65,535 (comment) bytes.
        let last = bytes.len() - EOCD_LEN;
        let first = last.saturating_sub(0xFFFF);
        let eocd = (first..=last)
            .rev()
            .find(|&at| {
                u32_at(bytes, at) == Some(EOCD_SIG)
                    && u16_at(bytes, at.saturating_add(20)).is_some_and(|c| at.saturating_add(EOCD_LEN).saturating_add(usize::from(c)) <= bytes.len())
            })
            .ok_or_else(|| corrupt("no end of central directory"))?;
        let field16 = |off: usize| u16_at(bytes, eocd.saturating_add(off)).ok_or_else(|| corrupt("short end record"));
        let field32 = |off: usize| u32_at(bytes, eocd.saturating_add(off)).ok_or_else(|| corrupt("short end record"));
        let (disk, cd_disk, count_here, count) = (field16(4)?, field16(6)?, field16(8)?, field16(10)?);
        let (cd_size, cd_offset) = (field32(12)?, field32(16)?);
        if count == 0xFFFF || cd_size == u32::MAX || cd_offset == u32::MAX {
            return Err(OfdError::Unsupported("ZIP64 packages (over 65,535 entries or 4 GB)".into()));
        }
        if disk != 0 || cd_disk != 0 || count_here != count {
            return Err(OfdError::Unsupported("ZIP packages split over several disks".into()));
        }
        let count = usize::from(count);
        if count > limits.max_entries {
            return Err(OfdError::Limit(format!("the package has {count} entries; at most {} are read", limits.max_entries)));
        }
        let mut pos = usize::try_from(cd_offset).map_err(|_| corrupt("central directory offset"))?;
        let mut entries = Vec::with_capacity(count);
        let mut index = HashMap::with_capacity(count);
        let mut declared_total: u64 = 0;
        for _ in 0..count {
            if u32_at(bytes, pos) != Some(CENTRAL_SIG) {
                return Err(corrupt("bad central directory entry"));
            }
            let f16 = |off: usize| u16_at(bytes, pos.saturating_add(off)).ok_or_else(|| corrupt("short central directory entry"));
            let f32 = |off: usize| u32_at(bytes, pos.saturating_add(off)).ok_or_else(|| corrupt("short central directory entry"));
            let (flags, method) = (f16(8)?, f16(10)?);
            let (crc, compressed, size) = (f32(16)?, f32(20)?, f32(24)?);
            let (name_len, extra_len, comment_len) = (usize::from(f16(28)?), usize::from(f16(30)?), usize::from(f16(32)?));
            let offset = f32(42)?;
            if compressed == u32::MAX || size == u32::MAX || offset == u32::MAX {
                return Err(OfdError::Unsupported("ZIP64 entries (over 4 GB)".into()));
            }
            let name_start = pos.saturating_add(CENTRAL_LEN);
            let raw = bytes.get(name_start..name_start.saturating_add(name_len)).ok_or_else(|| corrupt("entry name past the end"))?;
            let raw = String::from_utf8_lossy(raw);
            if raw.len() > 1024 {
                return Err(OfdError::Limit("an entry name is longer than 1,024 bytes".into()));
            }
            let name =
                clean_entry_name(&raw).ok_or_else(|| OfdError::Container(format!("unsafe entry name {raw:?} (absolute, drive or `..` path)")))?;
            pos = name_start.saturating_add(name_len).saturating_add(extra_len).saturating_add(comment_len);
            if name.is_empty() || raw.ends_with('/') || raw.ends_with('\\') {
                continue; // a directory
            }
            let (compressed, size) = (u64::from(compressed), u64::from(size));
            if size > limits.max_entry_size {
                return Err(OfdError::Limit(format!("{name} unpacks to {size} bytes; at most {} are read per entry", limits.max_entry_size)));
            }
            if method == 8 && size > RATIO_FLOOR && size > compressed.saturating_mul(limits.max_ratio) {
                return Err(OfdError::Limit(format!(
                    "{name} unpacks to {size} bytes from {compressed}: more than {}× (a ZIP bomb?)",
                    limits.max_ratio
                )));
            }
            declared_total = declared_total.saturating_add(size);
            if declared_total > limits.max_total_size {
                return Err(OfdError::Limit(format!("the package unpacks to more than {} bytes", limits.max_total_size)));
            }
            if index.insert(name.clone(), entries.len()).is_some() {
                return Err(OfdError::Container(format!("the entry {name} appears twice")));
            }
            entries.push(Entry { name, method, flags, crc, compressed, size, offset: u64::from(offset) });
        }
        Ok(Package { bytes, entries, index, limits: limits.clone(), inflated: Cell::new(0) })
    }

    /// The entry names, in central-directory order (directories left out).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// Whether the package has an entry `name` (exactly, or differing only in ASCII case).
    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        if let Some(&i) = self.index.get(name) {
            return self.entries.get(i);
        }
        // Some writers disagree with their own XML about case ("doc_0" vs "Doc_0"): accept a
        // case-insensitive match only when it is unique.
        let mut found = self.entries.iter().filter(|e| e.name.eq_ignore_ascii_case(name));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// The bytes of entry `name` (a path from the package root, without a leading `/`).
    pub fn read(&self, name: &str) -> Result<Vec<u8>, OfdError> {
        let e = self.find(name).ok_or_else(|| OfdError::Missing(name.to_string()))?;
        if e.flags & 1 != 0 {
            return Err(OfdError::Unsupported(format!("{}: encrypted ZIP entries", e.name)));
        }
        let total = self.inflated.get().saturating_add(e.size);
        if total > self.limits.max_total_size {
            return Err(OfdError::Limit(format!("the package unpacks to more than {} bytes", self.limits.max_total_size)));
        }
        let at = usize::try_from(e.offset).map_err(|_| corrupt("entry offset"))?;
        if u32_at(self.bytes, at) != Some(LOCAL_SIG) {
            return Err(corrupt("bad local header"));
        }
        let name_len = usize::from(u16_at(self.bytes, at.saturating_add(26)).ok_or_else(|| corrupt("short local header"))?);
        let extra_len = usize::from(u16_at(self.bytes, at.saturating_add(28)).ok_or_else(|| corrupt("short local header"))?);
        let start = at.checked_add(LOCAL_LEN + name_len + extra_len).ok_or_else(|| corrupt("entry offset"))?;
        let len = usize::try_from(e.compressed).map_err(|_| corrupt("entry size"))?;
        let end = start.checked_add(len).ok_or_else(|| corrupt("entry size"))?;
        let data = self.bytes.get(start..end).ok_or_else(|| corrupt(&format!("{} runs past the end of the file", e.name)))?;
        let size = usize::try_from(e.size).map_err(|_| corrupt("entry size"))?;
        let out = match e.method {
            0 => data.to_vec(),
            8 => {
                // Read one byte more than declared: a stream that is longer than its header says
                // is refused instead of trusted.
                let mut out = Vec::with_capacity(size.min(1 << 20));
                flate2::read::DeflateDecoder::new(data)
                    .take(e.size.saturating_add(1))
                    .read_to_end(&mut out)
                    .map_err(|err| OfdError::Container(format!("{} can't be unpacked: {err}", e.name)))?;
                out
            }
            m => return Err(OfdError::Unsupported(format!("{}: ZIP compression method {m} (only stored and Deflate)", e.name))),
        };
        if out.len() != size {
            return Err(corrupt(&format!("{} unpacks to {} bytes, not the {size} its header says", e.name, out.len())));
        }
        if crc32fast::hash(&out) != e.crc {
            return Err(corrupt(&format!("{} fails its checksum", e.name)));
        }
        self.inflated.set(total);
        Ok(out)
    }
}
