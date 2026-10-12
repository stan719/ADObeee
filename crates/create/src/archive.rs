//! Comic archives are read in entry order, entirely in memory; no path is ever extracted.

use std::io::{Cursor, Read};

use pdfcraft_cos::{Document, ObjRef};

use crate::{CreateError, ImageResolution, append_image_pages, set_title};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
}

const MAX_ENTRIES: usize = 10_000;
const MAX_PAGES: usize = 4096;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;

fn error(message: impl std::fmt::Display) -> CreateError {
    CreateError::Invalid(format!("comic archive: {message}"))
}

fn image_entry(name: &str) -> Result<bool, CreateError> {
    let name = name.replace('\\', "/");
    if name.split('/').any(|part| (part != "." && part.starts_with('.')) || part == "__MACOSX") {
        return Ok(false);
    }
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "gif" | "bmp" | "jp2" | "j2k" | "jpx" => Ok(true),
        "webp" | "avif" | "heic" | "heif" | "jxl" | "svg" => Err(error(format!("{name}: unsupported image format"))),
        _ => Ok(false), // ComicInfo.xml, notes and other non-page metadata.
    }
}

struct Pages {
    doc: Document,
    profiles: Vec<(Vec<u8>, ObjRef)>,
    remaining: u64,
    count: usize,
}

impl Pages {
    fn read(&mut self, name: &str, size: u64, reader: impl Read) -> Result<(), CreateError> {
        let limit = MAX_IMAGE_BYTES.min(self.remaining);
        if size > limit || self.count >= MAX_PAGES {
            return Err(error(format!("{name}: archive page or byte limit exceeded")));
        }
        let mut bytes = Vec::new();
        reader.take(limit + 1).read_to_end(&mut bytes).map_err(|e| error(format!("{name}: {e}")))?;
        if bytes.len() as u64 > limit || bytes.len() as u64 != size {
            return Err(error(format!("{name}: invalid or excessive decompressed size")));
        }
        self.remaining -= bytes.len() as u64;
        self.count += append_image_pages(&mut self.doc, &mut self.profiles, name, &bytes, ImageResolution::Embedded)?;
        if self.count > MAX_PAGES {
            return Err(error("archive page limit exceeded"));
        }
        Ok(())
    }
}

/// Convert CBZ/ZIP or CBT/TAR images into a new PDF, preserving archive entry order and
/// image resolution. Directories and non-image metadata are ignored. A corrupt image fails
/// the whole conversion. Nothing is extracted to the filesystem or returned on partial failure.
pub fn from_comic_archive(name: &str, bytes: &[u8], format: ArchiveFormat) -> Result<Document, CreateError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(error("archive exceeds 512 MiB"));
    }
    let mut pages = Pages { doc: Document::new_empty(), profiles: Vec::new(), remaining: MAX_ARCHIVE_BYTES as u64, count: 0 };
    match format {
        ArchiveFormat::Zip => {
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(error)?;
            if archive.len() > MAX_ENTRIES {
                return Err(error("too many archive entries"));
            }
            for index in 0..archive.len() {
                // Inspect without decoding: non-page metadata may use a different codec.
                let metadata = archive.by_index_data(index).map_err(error)?;
                let name = metadata.name().map_err(error)?.into_owned();
                if metadata.is_dir() || !image_entry(&name)? {
                    continue;
                }
                if metadata.is_symlink() || metadata.encrypted() {
                    return Err(error(format!("{name}: linked or encrypted image entries are unsupported")));
                }
                let size = metadata.size();
                let entry = archive.by_index(index).map_err(|e| error(format!("{name}: {e}")))?;
                pages.read(&name, size, entry)?;
            }
        }
        ArchiveFormat::Tar => {
            let mut archive = tar::Archive::new(Cursor::new(bytes));
            for (index, entry) in archive.entries().map_err(error)?.enumerate() {
                if index >= MAX_ENTRIES {
                    return Err(error("too many archive entries"));
                }
                let entry = entry.map_err(error)?;
                let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
                if entry.header().entry_type().is_dir() || !image_entry(&name)? {
                    continue;
                }
                if !entry.header().entry_type().is_file() {
                    return Err(error(format!("{name}: linked or special image entries are unsupported")));
                }
                pages.read(&name, entry.size(), entry)?;
            }
        }
    }
    if pages.count == 0 {
        return Err(error("no supported page images"));
    }
    set_title(&mut pages.doc, name.rsplit_once('.').map_or(name, |(stem, _)| stem));
    Ok(pages.doc)
}

#[cfg(test)]
mod tests;
