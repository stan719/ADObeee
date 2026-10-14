//! OFD (Open Fixed-layout Document, GB/T 33190-2016) reading. Layer L3, no workspace
//! dependencies.
//!
//! OFD is China's national fixed-layout format (electronic invoices, government documents,
//! archives; issue #1036). An OFD file is a ZIP package (§6) of XML parts (§7): `OFD.xml` points
//! to each document's `Document.xml`, which lists pages, template pages and resource files; each
//! page's `Content.xml` holds layers of path, text and image objects.
//!
//! This crate is the foundation only: it opens the package safely ([`zip::Package`], with
//! [`Limits`]) and reads it into a typed model ([`OfdDocument`]) in OFD's own units. It does not
//! draw or convert anything yet.
//!
//! ```text
//! if pdfcraft_ofd::is_ofd(&bytes) {
//!     let doc = pdfcraft_ofd::parse(&bytes)?;          // or parse_with_limits
//!     for page in &doc.pages { let (w, h) = page.area.physical.size_pt(); … }
//! }
//! ```
//!
//! **Phase 2 (not here):** `pdfcraft-create` gains an OFD source kind, and
//! `Session::convert_to_pdf` turns this model into PDF pages (paths, images, text with embedded
//! fonts; name-only fonts such as SimSun as non-embedded CID fonts), so Open, Insert, Create and
//! Combine accept `.ofd` without other changes. See the README.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod geom;
pub mod model;
mod parse;
mod values;
pub mod zip;

pub use geom::{Matrix, OfdBox, PT_PER_MM, mm_to_pt};
pub use model::*;

#[cfg(test)]
mod tests;

/// What went wrong reading an OFD file. Every message says which part and what was wrong.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OfdError {
    #[error("this is not an OFD file: {0}")]
    NotOfd(String),
    #[error("{0}")]
    Container(String),
    #[error("OFD files using {0} can't be read")]
    Unsupported(String),
    #[error("the OFD file is too large or too complex: {0}")]
    Limit(String),
    #[error("{0} is missing from the OFD package")]
    Missing(String),
    #[error("{part} is not valid XML: {msg}")]
    Xml { part: String, msg: String },
    #[error("{part}: {msg}")]
    Invalid { part: String, msg: String },
}

/// Bounds on what a file may make the reader do. The defaults are far above real documents (an
/// e-invoice is a few dozen entries and kilobytes; the largest file in the ofdrw test set has
/// 2 MB of XML, 25,000 nodes in one part and 1,815 objects) while keeping the model of a small
/// but hostile file to a few hundred megabytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Limits {
    /// ZIP entries in the package.
    pub max_entries: usize,
    /// Unpacked size of one entry, in bytes.
    pub max_entry_size: u64,
    /// Unpacked size of everything read from the package, in bytes.
    pub max_total_size: u64,
    /// Largest unpacked/packed ratio for an entry over 1 MiB (zip-bomb guard).
    pub max_ratio: u64,
    /// Size of one XML part, in bytes.
    pub max_xml_size: u64,
    /// XML read for the whole document, in bytes. A part read again (pages may share one
    /// content file) counts again.
    pub max_xml_total: u64,
    /// Elements, attributes and text nodes in one XML part.
    pub max_xml_nodes: u32,
    pub max_pages: usize,
    /// Nesting of page blocks.
    pub max_depth: usize,
    /// Numbers in one attribute (an array, a path, a `DeltaX` after `g` expansion).
    pub max_numbers: usize,
    /// Page objects in the whole document (pages and templates, page blocks included).
    pub max_objects: usize,
    /// Memory the whole document's page content takes in the model, in bytes (objects, path
    /// commands, colours, glyph positions, text).
    pub max_model_bytes: usize,
    /// Characters in all the document's `TextCode`s. Each becomes a positioned glyph when
    /// converted, so this bounds the converter's output too.
    pub max_text_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_entries: 20_000,
            max_entry_size: 256 << 20,
            max_total_size: 1 << 30,
            max_ratio: 200,
            max_xml_size: 64 << 20,
            max_xml_total: 256 << 20,
            max_xml_nodes: 2_000_000,
            max_pages: 20_000,
            max_depth: 64,
            max_numbers: 1_000_000,
            max_objects: 1_000_000,
            max_model_bytes: 256 << 20,
            max_text_chars: 4_000_000,
        }
    }
}

/// Whether `bytes` is an OFD package: a ZIP whose root holds `OFD.xml`. A Word or other ZIP-based
/// file is not. Only the central directory is read.
pub fn is_ofd(bytes: &[u8]) -> bool {
    zip::looks_like_zip(bytes) && zip::Package::open(bytes, &Limits::default()).is_ok_and(|p| p.contains("OFD.xml"))
}

/// Read an OFD file with the default [`Limits`].
pub fn parse(bytes: &[u8]) -> Result<OfdDocument, OfdError> {
    parse_with_limits(bytes, &Limits::default())
}

/// Read an OFD file. Any malformed input is an error; nothing panics. This is also the entry
/// point for fuzzing.
pub fn parse_with_limits(bytes: &[u8], limits: &Limits) -> Result<OfdDocument, OfdError> {
    let pkg = zip::Package::open(bytes, limits)?;
    parse::document(&pkg, limits)
}

/// Read the OFD document in a package opened with [`zip::Package::open`]. A caller that goes on
/// to read images and fonts from the same package shares one unpacking budget
/// ([`Limits::max_total_size`]) with the reader.
pub fn parse_package(pkg: &zip::Package, limits: &Limits) -> Result<OfdDocument, OfdError> {
    parse::document(pkg, limits)
}
