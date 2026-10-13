//! The Windows WIA 2.0 backend for `pdfcraft-scan`. [`scanner_list`] is cheap (no open);
//! [`scanners`] connects to every device and reads properties; a device that fails to open is
//! omitted. [`acquire`] transfers, which feeds paper.
//!
//! WIA is COM, and the `windows` crate's COM calls are `unsafe`. All of that is confined to two
//! modules of this crate: `ffi` (small wrappers around single COM calls and `PROPVARIANT` reads)
//! and `sinks` (the COM callback objects WIA calls back into). Everything else here, and the whole
//! of `pdfcraft-scan`, is safe code. `docs/unsafe-audit.md` lists every category and how to review it.
//!
//! The crate is empty on other platforms.

#![cfg(windows)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod backend;
mod ffi;
mod sinks;

pub use backend::{acquire, dump_devices, scanner_details, scanner_list, scanners};
