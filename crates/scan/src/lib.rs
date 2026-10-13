//! Image acquisition from a scanner.
//!
//! Windows talks to the scanner through WIA, in the `pdfcraft-wia` crate; this crate holds the
//! shared types and rules and has no unsafe code. The backend's `scanners` connects and reads
//! properties; a device that fails to open is left out, and the others are still returned. It does
//! not feed paper. Its `acquire` does: it pulls sheets from the feeder, or reads the glass, and
//! returns each page as in-memory [`Page`] bytes (and optionally streams them through
//! [`ProgressToken`] while the transfer runs).
//!
//! **Buffers first.** The public surface never requires a path. Language bindings and the app
//! keep pages in memory, build a PDF from those bytes, or hand them to another consumer. Temp
//! files under the process temp directory are only an internal WIA scratch while a page is still
//! arriving; they are deleted before [`Page`] is published. Optional path helpers
//! ([`write_pages`], [`page_path`], [`listed_page_files`]) are sinks/sources for a CLI or a
//! binding that *chooses* to spill to disk — they are never called by `acquire`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub mod bmp;
pub mod bw;
mod contract;
pub mod trace;

pub use contract::{
    Compression, ErrorContext, ImageFormat, ListDetails, ParseWireError, Phase, Recovery, ScanErrorInfo, code_name, code_names, contract_json,
    kind_for_hresult, recovery,
};

/// Where the page sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Source {
    /// Feeder when [`Scanner::feeder_ready`] is set, otherwise the bed. Resolved at acquire time
    /// from a fresh status read. Some drivers leave the ready bit on after the tray is empty.
    #[default]
    Auto,
    /// The automatic document feeder. Hardware duplex is a separate flag on
    /// [`ScanSettings::duplex`] (`FEEDER|DUPLEX`); leave that off for one side.
    Feeder,
    /// The flatbed glass. One page.
    Glass,
}

/// The pixels the driver is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Color {
    /// 8-bit grey. The usual input for a text page.
    #[default]
    Gray,
    /// 24-bit color.
    Rgb,
    /// 1-bit black and white.
    BlackAndWhite,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Feeder => "feeder",
            Self::Glass => "glass",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "feeder" => Some(Self::Feeder),
            "glass" | "flatbed" | "bed" => Some(Self::Glass),
            _ => None,
        }
    }

    /// Turn [`Self::Auto`] into feeder or glass. Never returns [`Self::Auto`].
    pub fn resolve(self, feeder_ready: bool, has_feeder: bool, has_glass: bool) -> Result<Self, ScanError> {
        match self {
            Self::Auto => {
                if feeder_ready && has_feeder {
                    Ok(Self::Feeder)
                } else if has_glass {
                    Ok(Self::Glass)
                } else if has_feeder {
                    Ok(Self::Feeder)
                } else {
                    Err(ScanError::Failed("no scanner source is available".into()))
                }
            }
            Self::Feeder | Self::Glass => Ok(self),
        }
    }

    pub fn is_feeder(self) -> bool {
        matches!(self, Self::Feeder)
    }

    pub fn is_glass(self) -> bool {
        matches!(self, Self::Glass)
    }
}

impl Color {
    pub fn name(self) -> &'static str {
        match self {
            Self::Gray => "gray",
            Self::Rgb => "rgb",
            Self::BlackAndWhite => "bw",
        }
    }

    /// WIA data-type code. Threshold is `0`, grey is `2`, color is `3`.
    pub fn data_type(self) -> i32 {
        match self {
            Self::BlackAndWhite => 0,
            Self::Gray => 2,
            Self::Rgb => 3,
        }
    }

    pub fn depth(self) -> u32 {
        match self {
            Self::BlackAndWhite => 1,
            Self::Gray => 8,
            Self::Rgb => 24,
        }
    }

    pub fn from_data_type(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::BlackAndWhite),
            2 => Some(Self::Gray),
            3 => Some(Self::Rgb),
            _ => None,
        }
    }
}

/// How the page is turned. The values are the driver's rotation codes, not degrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rotation {
    #[default]
    Portrait,
    Landscape,
    UpsideDown,
    Left,
}

impl Rotation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Landscape => "landscape",
            Self::UpsideDown => "180",
            Self::Left => "270",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "portrait" | "0" => Some(Self::Portrait),
            "landscape" | "90" => Some(Self::Landscape),
            "180" | "upside-down" => Some(Self::UpsideDown),
            "270" | "left" => Some(Self::Left),
            _ => None,
        }
    }

    pub fn driver(self) -> i32 {
        match self {
            Self::Portrait => 0,
            Self::Landscape => 1,
            Self::UpsideDown => 2,
            Self::Left => 3,
        }
    }

    pub fn from_driver(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Portrait),
            1 => Some(Self::Landscape),
            2 => Some(Self::UpsideDown),
            3 => Some(Self::Left),
            _ => None,
        }
    }
}

/// A WIA range: `min`, `max`, and `step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueSpan {
    pub min: i32,
    pub max: i32,
    pub step: i32,
}

impl ValueSpan {
    pub fn contains(self, value: i32) -> bool {
        if value < self.min || value > self.max {
            return false;
        }
        let step = if self.step == 0 { 1 } else { self.step };
        (value - self.min) % step == 0
    }
}

/// Driver intent presets (`WIA_IPS_CUR_INTENT`). Fine controls still override after intent is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    None,
    Color,
    Gray,
    Text,
    BestPreview,
    MaximizeQuality,
    MinimizeSize,
}

impl Intent {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Color => "color",
            Self::Gray => "gray",
            Self::Text => "text",
            Self::BestPreview => "preview",
            Self::MaximizeQuality => "quality",
            Self::MinimizeSize => "size",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(Self::None),
            "color" | "rgb" => Some(Self::Color),
            "gray" | "grey" | "grayscale" => Some(Self::Gray),
            "text" | "ocr" => Some(Self::Text),
            "preview" | "best-preview" => Some(Self::BestPreview),
            "quality" | "max-quality" => Some(Self::MaximizeQuality),
            "size" | "min-size" => Some(Self::MinimizeSize),
            _ => None,
        }
    }

    pub fn driver(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Color => 1,
            Self::Gray => 2,
            Self::Text => 4,
            Self::BestPreview => 262_144,
            Self::MaximizeQuality => 131_072,
            Self::MinimizeSize => 65_536,
        }
    }

    pub fn from_driver(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Color),
            2 => Some(Self::Gray),
            4 => Some(Self::Text),
            65_536 => Some(Self::MinimizeSize),
            131_072 => Some(Self::MaximizeQuality),
            262_144 => Some(Self::BestPreview),
            _ => None,
        }
    }
}

/// Named page size (`WIA_IPS_PAGE_SIZE`). A region still overrides extent after this is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    A4,
    Letter,
    Legal,
    Auto,
    Custom,
    BusinessCard,
}

impl PageSize {
    pub fn name(self) -> &'static str {
        match self {
            Self::A4 => "a4",
            Self::Letter => "letter",
            Self::Legal => "legal",
            Self::Auto => "auto",
            Self::Custom => "custom",
            Self::BusinessCard => "business-card",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "a4" => Some(Self::A4),
            "letter" => Some(Self::Letter),
            "legal" => Some(Self::Legal),
            "auto" => Some(Self::Auto),
            "custom" => Some(Self::Custom),
            "business-card" | "card" => Some(Self::BusinessCard),
            _ => None,
        }
    }

    pub fn driver(self) -> i32 {
        match self {
            Self::A4 => 0,
            Self::Letter => 1,
            Self::Custom => 2,
            Self::Legal => 3,
            Self::BusinessCard => 6,
            Self::Auto => 100,
        }
    }

    pub fn from_driver(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::A4),
            1 => Some(Self::Letter),
            2 => Some(Self::Custom),
            3 => Some(Self::Legal),
            6 => Some(Self::BusinessCard),
            100 => Some(Self::Auto),
            _ => None,
        }
    }
}

/// How a scanner is attached. Derived from the driver's port string; the same hardware can appear
/// once per transport (USB minidriver vs inbox WSD) with different published capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Connection {
    Usb,
    /// Web Services for Devices / WS-Scan (network).
    Wsd,
    #[default]
    Unknown,
}

impl Connection {
    pub fn name(self) -> &'static str {
        match self {
            Self::Usb => "usb",
            Self::Wsd => "wsd",
            Self::Unknown => "unknown",
        }
    }

    /// Classify a WIA port string (`WIA_DIP_PORT_NAME`) or PnP location.
    pub fn from_port(port: &str) -> Self {
        let lower = port.to_ascii_lowercase();
        if lower.contains("usbscan") || lower.starts_with(r"\\.\") {
            Self::Usb
        } else if lower.contains("webservices")
            || lower.contains("wsd")
            || lower.contains("scannerservice")
            || lower.starts_with("urn:uuid:")
            || lower.starts_with("http://")
            || lower.starts_with("https://")
        {
            Self::Wsd
        } else if lower.contains("usb") {
            // Broad USB match after WSD checks (a URL must not become USB).
            Self::Usb
        } else {
            Self::Unknown
        }
    }
}

/// Bits from `WIA_DPS_DOCUMENT_HANDLING_STATUS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HandlingStatus {
    pub feeder_ready: bool,
    pub flat_ready: bool,
    pub duplex_ready: bool,
    pub cover_open: bool,
    pub path_cover_open: bool,
    pub jam: bool,
    /// `WIA_MULTIPLE_FEED` (0x200).
    pub multiple_feed: bool,
    /// `WIA_DEVICE_ATTENTION` (0x400).
    pub attention: bool,
    /// `WIA_LAMP_ERR` (0x800).
    pub lamp_error: bool,
}

impl HandlingStatus {
    pub fn from_bits(bits: i32) -> Self {
        let bits = u32::try_from(bits).unwrap_or(0);
        Self {
            feeder_ready: bits & 0x1 != 0,
            flat_ready: bits & 0x2 != 0,
            duplex_ready: bits & 0x4 != 0,
            cover_open: bits & 0x8 != 0,
            path_cover_open: bits & 0x10 != 0,
            jam: bits & 0x20 != 0,
            multiple_feed: bits & 0x200 != 0,
            attention: bits & 0x400 != 0,
            lamp_error: bits & 0x800 != 0,
        }
    }

    /// Plain sentence that should block `acquire` before Download, if any.
    /// The first match wins: a jam is reported ahead of a cover, a double-feed, or a lamp.
    /// Sentences match [`wia_error_message`].
    ///
    /// `DEVICE_ATTENTION` (0x400) does not block: the driver also raises it for a paused scanner
    /// and for a lamp warning, and Windows' own Transfer succeeds there. It only explains a
    /// failure afterwards (see [`explain_transfer_failure`]).
    pub fn acquire_block_reason(self) -> Option<&'static str> {
        if self.jam {
            return wia_error_message(0x8021_0002);
        }
        if self.cover_open || self.path_cover_open {
            return wia_error_message(0x8021_0010);
        }
        if self.multiple_feed {
            return wia_error_message(0x8021_0014);
        }
        if self.lamp_error {
            return wia_error_message(0x8021_0011);
        }
        None
    }

    /// The sentence for a paper-path fault the bits name, in the order jam, cover, double feed.
    /// Used to sharpen the ambiguous `PAPER_PROBLEM` HRESULT; `None` when no such bit is set.
    pub fn paper_path_reason(self) -> Option<&'static str> {
        if self.jam {
            return wia_error_message(0x8021_0002);
        }
        if self.cover_open || self.path_cover_open {
            return wia_error_message(0x8021_0010);
        }
        if self.multiple_feed {
            return wia_error_message(0x8021_0014);
        }
        None
    }
}

/// A Windows failure that does not name the fault. A later status read can.
pub fn generic_scan_failure(error: &ScanError) -> bool {
    let text = error.to_string();
    text == "The scanner reported an error."
        || text.starts_with("The scanner reported an error (")
        || text.starts_with("The scan failed (")
        || text == "Unspecified error"
        || text == "The parameter is incorrect."
        // Pre-status E_FAIL sentence from [`describe_windows_error`]; jam/cover bits still win in
        // [`explain_transfer_failure`]. Used so banded→file retry still runs when TYMED was rejected.
        || text.starts_with("The scan was interrupted.")
}

/// When Download returns a generic HRESULT, prefer a jam, cover, double-feed, lamp, or attention
/// bit read after the call. On the feeder, a clear `FEED_READY` with no block bit is an empty
/// tray (`WIA_ERROR_PAPER_EMPTY`), so a later [`after_transfer`] can end a partial stack normally
/// and [`retry_with_file_transfer`] does not start a second transfer. A sentence that already
/// names the fault is left as it is. `feeder` is false for the glass so a clear ready bit there
/// is not treated as an empty tray.
///
/// `pages_requested` is `0` when the feeder runs until empty; otherwise it is the requested
/// sheet count. A clear `FEED_READY` after every requested page has arrived is an empty tray.
/// Mid-job faults with fewer pages than requested keep the generic error for [`after_transfer`].
pub fn explain_transfer_failure(
    error: ScanError,
    status: HandlingStatus,
    feeder: bool,
    pages_transferred: usize,
    pages_requested: usize,
) -> ScanError {
    if feeder && is_busy(&error) {
        // An empty feeder is invisible to the WIA status: the driver starts the scan, then answers
        // BUSY. A bit that names a cause (jam, cover, double feed, lamp, attention) wins; with none
        // set, say both. The glass keeps the plain busy sentence.
        let named = status.acquire_block_reason().or_else(|| status.attention.then(|| wia_error_message(0x8021_0008)).flatten());
        return error.restated(named.unwrap_or(BUSY_OR_FEEDER_EMPTY));
    }
    if is_paper_problem(&error) {
        // WIA folds a double feed (and other paper-path faults) into PAPER_PROBLEM. A status bit
        // that names the cause wins; with none set the sentence stays neutral.
        return match status.paper_path_reason() {
            Some(reason) => error.restated(reason),
            None => error,
        };
    }
    if !generic_scan_failure(&error) {
        return error;
    }
    if let Some(reason) = status.acquire_block_reason() {
        return error.restated(reason);
    }
    if feeder && pages_requested == 0 && pages_transferred > 0 && generic_scan_failure(&error) {
        // Whole-stack acquire: inbox WSD often keeps FEED_READY set after the last sheet.
        return error.restated(wia_error_message(0x8021_0003).unwrap_or("The feeder has no paper."));
    }
    if status.attention
        && let Some(reason) = wia_error_message(0x8021_0008)
    {
        // Before the clear-FEED_READY empty-tray inference below: a door interlock or a full device
        // store clears `FEED_READY` too, and would otherwise be reported as "The feeder has no
        // paper." The inbox WSD driver keeps `FEED_READY` set for a genuinely empty tray, so a set
        // attention bit on a failed transfer is a device-side fault. The sentence does not guess
        // which one. (After a whole-stack scan has delivered pages the rule above still ends the
        // stack normally; that is the paused-scanner case.)
        return error.restated(reason);
    }
    if feeder && !status.feeder_ready {
        let empty_tray = pages_transferred == 0
            || (pages_requested > 0 && pages_transferred >= pages_requested)
            || (pages_requested == 0 && pages_transferred > 0);
        if empty_tray {
            return error.restated(wia_error_message(0x8021_0003).unwrap_or("The feeder has no paper."));
        }
    }
    error
}

/// Sentence for a feeder transfer that answered BUSY while no status bit names a cause.
pub const BUSY_OR_FEEDER_EMPTY: &str = "The scanner is busy, or the feeder has no paper.";

/// The WIA `BUSY` sentence, as [`describe_windows_error`] produces it.
fn is_busy(error: &ScanError) -> bool {
    wia_error_message(0x8021_0006).is_some_and(|sentence| error.to_string().contains(sentence))
}

/// The neutral `PAPER_PROBLEM` sentence, as [`describe_windows_error`] produces it.
fn is_paper_problem(error: &ScanError) -> bool {
    wia_error_message(0x8021_0004).is_some_and(|sentence| error.to_string().contains(sentence))
}

/// Banded transfer is the wrong mode when the driver rejects it without naming a device fault.
/// A jam, an open cover, or an empty feeder must not start a second transfer. Call
/// [`explain_transfer_failure`] first so status bits become named sentences.
pub fn retry_with_file_transfer(error: &ScanError) -> bool {
    generic_scan_failure(error)
}

fn source_code(source: Source) -> u8 {
    match source {
        Source::Auto => 0,
        Source::Feeder => 1,
        Source::Glass => 2,
    }
}

fn source_from_code(code: u8) -> Option<Source> {
    match code {
        1 => Some(Source::Feeder),
        2 => Some(Source::Glass),
        _ => None,
    }
}

/// Pages a failed acquire still delivered (`Incomplete` carries them; every other error has none).
pub fn pages_in_error(error: &ScanError) -> usize {
    match error {
        ScanError::Incomplete { acquired, .. } => acquired.pages.len(),
        _ => 0,
    }
}

/// Whether a failed acquire should be retried once on the glass.
///
/// Only for Auto that resolved to the feeder, when the glass exists and no page arrived, and the
/// failure is the empty-feeder classification (`BusyOrFeederEmpty` or `FeederEmpty`). Drivers keep
/// the WIA ready bit set for an empty tray, so Auto cannot tell before it tries. A jam, cover,
/// double feed, lamp or attention error, a plain busy, and any shortfall after pages stay errors.
pub fn auto_fallback_to_glass(requested: Source, resolved: Source, has_glass: bool, pages_received: usize, error: &ScanError) -> bool {
    requested == Source::Auto
        && resolved == Source::Feeder
        && has_glass
        && pages_received == 0
        && matches!(error.interrupt_kind(), InterruptKind::BusyOrFeederEmpty | InterruptKind::FeederEmpty)
}

/// Whether feeder acquire should refuse before transfer when `FEED_READY` is clear.
///
/// Inbox WSD drivers often omit the bit even when the feeder has sheets loaded (`pages=0`
/// means the whole stack; same preflight as a fixed page count).
pub fn feeder_empty_blocks_acquire(source: Source, feeder_ready: bool, wsd_port: bool) -> bool {
    source == Source::Feeder && !feeder_ready && !wsd_port
}

/// WIA `IPS_PAGES` / `DPS_PAGES` for inbox WSD when the user asked for the whole feeder (`0`).
///
/// Some WSD stacks map a zero page count to a single sheet on the WS-Scan ticket; use a large cap
/// so `ImagesToTransfer` can mean “until the feeder is empty”.
pub fn wsd_feeder_ticket_pages(page_count: i32) -> i32 {
    if page_count == 0 { i32::try_from(MAX_PAGES).unwrap_or(i32::MAX) } else { page_count }
}

/// After WIA `Download` returns.
///
/// The dialog already keeps pages streamed through [`ProgressToken`] when this is `Err`.
/// A paper jam or cover fault is therefore still an error after a partial stack. The feeder
/// running out (`no paper`) with pages in hand is a normal end. Cancel with no pages is cancel.
/// A still-generic failure with no pages becomes a mid-scan drop / rejected-image sentence.
pub fn after_transfer(page_count: usize, cancelled: bool, download_error: Option<ScanError>) -> Result<(), ScanError> {
    if cancelled && page_count == 0 {
        return Err(ScanError::Failed("the scan was cancelled".into()));
    }
    if let Some(error) = download_error {
        if page_count == 0 || !feeder_ended(&error) {
            return Err(clarify_empty_or_mid_transfer(error, page_count));
        }
    } else if page_count == 0 {
        return Err(ScanError::Failed("the scanner transferred no pages".into()));
    }
    Ok(())
}

/// Sentence for a feeder that delivered fewer pages than the caller asked for and then said it
/// had no more paper.
pub fn feeder_shortfall_message(pages_requested: usize, pages_transferred: usize) -> String {
    format!("Only {pages_transferred} of {pages_requested} pages were scanned: the feeder reported no more paper.")
}

/// [`after_transfer`] plus the page-count check for a fixed-count feeder scan.
///
/// `pages_requested` is `0` for "the whole feeder" (a short stack is a normal end). When it is
/// `N > 0` on the feeder and `0 < pages_transferred < N`, a transfer that ended without a hard
/// error, or with the scanner saying "no more paper / empty", is reported as an error so the
/// caller (via [`ScanError::Incomplete`]) keeps the pages that did arrive but does not call the
/// scan a success. Jams and other faults keep their own sentence. A user cancel with pages in
/// hand stays a normal end. Pass `pages_requested = 0` for hardware duplex (sides are not sheets)
/// and `feeder = false` for the glass.
pub fn after_transfer_checked(
    pages_requested: usize,
    feeder: bool,
    pages_transferred: usize,
    cancelled: bool,
    download_error: Option<ScanError>,
) -> Result<(), ScanError> {
    let ended_empty = download_error.as_ref().is_none_or(feeder_ended);
    after_transfer(pages_transferred, cancelled, download_error)?;
    if feeder && !cancelled && ended_empty && pages_requested > 0 && pages_transferred > 0 && pages_transferred < pages_requested {
        return Err(ScanError::Failed(feeder_shortfall_message(pages_requested, pages_transferred)));
    }
    Ok(())
}

/// When Download failed with a generic HRESULT and status bits did not rename it, say so plainly.
/// Jam / cover / empty-feeder sentences are left alone. A partial stack that ended in E_FAIL is
/// usually the device going offline mid-job or rejecting the transferred image (WSD CancelJob).
fn clarify_empty_or_mid_transfer(error: ScanError, page_count: usize) -> ScanError {
    if !generic_scan_failure(&error) {
        // Inbox WSD maps mid-job offline/stop to ATTENTION (0x80210008); partial pages should
        // read like an interrupted stack, not a preflight block.
        if page_count > 0 {
            let text = error.to_string();
            if text.contains("needs attention") {
                return error.restated_other("The scan was interrupted. Check for a paper jam, an open cover, or that the scanner stayed online.");
            }
        }
        return error;
    }
    if page_count > 0 {
        return error
            .restated_other("The scan stopped before all pages arrived. Check for a paper jam, an open cover, or that the scanner stayed online.");
    }
    error.restated_other(
        "The scanner did not deliver a usable page. Check that it stayed online; for a network scanner, remove it in Windows and add it again while it is on if this keeps happening.",
    )
}

/// True when Download stopped because the feeder had no more sheets, not because of a jam.
fn feeder_ended(error: &ScanError) -> bool {
    let text = error.to_string();
    text.contains("no paper") || text.contains("feeder is empty")
}

/// What a backend registers so [`CancelToken::request`] can stop a transfer from another thread.
pub type Canceller = Arc<dyn Fn() + Send + Sync>;

/// Cancel an in-flight `acquire`. The backend registers a [`Canceller`] while its transfer runs;
/// [`Self::request`] sets the flag and calls it.
#[derive(Clone, Default)]
pub struct CancelToken {
    requested: Arc<AtomicBool>,
    /// The running transfer's canceller. `None` when idle.
    canceller: Arc<Mutex<Option<Canceller>>>,
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken").field("requested", &self.is_requested()).finish()
    }
}

impl PartialEq for CancelToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.requested, &other.requested)
    }
}

impl Eq for CancelToken {}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        // Clone out of the lock so a slow cancel never blocks the worker that clears it.
        let canceller = self.canceller.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(cancel) = canceller {
            cancel();
        }
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Register the canceller for the transfer that is about to run (backends only).
    pub fn set_canceller(&self, canceller: Canceller) {
        *self.canceller.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(canceller);
    }

    /// Forget the canceller when the transfer is over (backends only).
    pub fn clear_canceller(&self) {
        *self.canceller.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Live page arrivals while `acquire` runs. Each finished page is a full in-memory [`Page`]
/// (no path). The dialog — or a binding — drains them with [`Self::take_pages`] as each sheet
/// finishes, so a jam or Stop still leaves what already transferred.
///
/// Banded and WIA 2.0 transfers also publish how far the **current** page has filled
/// ([`Self::page_percent`], [`Self::bytes_received`]) so a long single-page scan is not stuck at
/// "Scanning…" with a frozen bar.
#[derive(Clone, Default)]
pub struct ProgressToken {
    done: Arc<AtomicUsize>,
    pending: Arc<Mutex<Vec<Page>>>,
    /// 0 = unknown; 1..=101 means 0..=100% of the page in flight.
    page_percent: Arc<AtomicU32>,
    bytes_received: Arc<AtomicU64>,
    /// 0 = unknown expected size.
    bytes_expected: Arc<AtomicU64>,
    /// The source the engine is transferring from: 0 = not known yet, 1 = feeder, 2 = glass.
    source_used: Arc<AtomicU8>,
}

impl std::fmt::Debug for ProgressToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressToken")
            .field("pages_done", &self.pages_done())
            .field("page_percent", &self.page_percent())
            .field("bytes_received", &self.bytes_received())
            .field("bytes_expected", &self.bytes_expected())
            .finish()
    }
}

impl PartialEq for ProgressToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.done, &other.done)
    }
}

impl Eq for ProgressToken {}

impl ProgressToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pages_done(&self) -> usize {
        self.done.load(Ordering::SeqCst)
    }

    /// Driver-reported or byte-derived fill of the page currently transferring.
    pub fn page_percent(&self) -> Option<u8> {
        match self.page_percent.load(Ordering::SeqCst) {
            0 => None,
            n => Some(n.saturating_sub(1).min(100) as u8),
        }
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::SeqCst)
    }

    pub fn bytes_expected(&self) -> Option<u64> {
        match self.bytes_expected.load(Ordering::SeqCst) {
            0 => None,
            n => Some(n),
        }
    }

    /// The source the engine is transferring from now (never [`Source::Auto`]). It changes from the
    /// feeder to the glass when Auto falls back, before the first page of the glass scan arrives.
    pub fn source(&self) -> Option<Source> {
        source_from_code(self.source_used.load(Ordering::SeqCst))
    }

    pub fn set_source(&self, source: Source) {
        self.source_used.store(source_code(source), Ordering::SeqCst);
    }

    /// Pages that finished since the last take. Order is transfer order.
    pub fn take_pages(&self) -> Vec<Page> {
        self.pending.lock().map(|mut guard| std::mem::take(&mut *guard)).unwrap_or_default()
    }

    /// Begin (or refresh) the expected uncompressed size for the page about to transfer.
    pub fn set_expected_bytes(&self, expected: Option<u64>) {
        self.bytes_expected.store(expected.unwrap_or(0), Ordering::SeqCst);
        self.bytes_received.store(0, Ordering::SeqCst);
        self.page_percent.store(0, Ordering::SeqCst);
    }

    /// Update fill for the page in flight. `percent` is 0..=100 from the driver when known.
    ///
    /// When the driver percent is omitted (inbox WSD JFIF: `lPercentComplete` peaks near 40
    /// for the full stream, then snaps to 100) and no expected size is set, percent is derived
    /// from `received` with a soft ceiling so the bar climbs with the wire bytes and only
    /// claims 100 when the caller passes `Some(100)` (or a matching `expected`).
    pub fn set_transfer(&self, percent: Option<u8>, received: u64, expected: Option<u64>) {
        if let Some(exp) = expected {
            self.bytes_expected.store(exp, Ordering::SeqCst);
        } else if percent == Some(100) && received > 0 && self.bytes_expected.load(Ordering::SeqCst) == 0 {
            // Stream finished: lock expected to what actually arrived (compressed size).
            self.bytes_expected.store(received, Ordering::SeqCst);
        }
        self.bytes_received.store(received, Ordering::SeqCst);
        let pct = percent.or_else(|| {
            let exp = self.bytes_expected.load(Ordering::SeqCst);
            if let Some(done) = received.min(exp).saturating_mul(100).checked_div(exp) {
                Some(done as u8)
            } else if received > 0 {
                // Unknown total: asymptotic toward 99 (never claim done from bytes alone).
                let soft = received.saturating_add(64 * 1024).max(1);
                Some((received.saturating_mul(100) / soft).min(99) as u8)
            } else {
                None
            }
        });
        if let Some(p) = pct {
            self.page_percent.store(u32::from(p.min(100)) + 1, Ordering::SeqCst);
        }
    }

    pub fn push_page(&self, page: Page) {
        self.done.fetch_add(1, Ordering::SeqCst);
        self.page_percent.store(0, Ordering::SeqCst);
        self.bytes_received.store(0, Ordering::SeqCst);
        // Keep bytes_expected: the next page is usually the same size.
        if let Ok(mut guard) = self.pending.lock() {
            guard.push(page);
        }
    }
}

/// What one source (the bed or the feeder) offers. Empty means the driver did not publish a list.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ImageChoices {
    pub dpi: Vec<u32>,
    /// Y resolutions when the driver lists them separately from [`Self::dpi`].
    pub dpi_y: Vec<u32>,
    pub colors: Vec<Color>,
    pub brightness: Option<ValueSpan>,
    pub contrast: Option<ValueSpan>,
    pub threshold: Option<ValueSpan>,
    pub rotation: Vec<Rotation>,
    /// Transfer formats the item lists (`bmp`, `jpeg`, `png`, `tiff`, …).
    pub formats: Vec<String>,
    pub compression: Vec<String>,
    pub intents: Vec<Intent>,
    pub page_sizes: Vec<PageSize>,
    /// True when `WIA_IPS_AUTO_DESKEW` is published.
    pub auto_deskew: bool,
    /// Orientation codes when distinct from rotation.
    pub orientation: Vec<i32>,
    pub photometric: Vec<i32>,
    pub invert: bool,
    pub mirror: bool,
    pub lamp: bool,
}

/// A WIA list attribute is `[count, nominal, values...]`.
pub fn listed_values(raw: &[i32]) -> Vec<i32> {
    let Some(count) = raw.first().copied().and_then(|count| usize::try_from(count).ok()) else {
        return Vec::new();
    };
    let end = count.saturating_add(2);
    if count == 0 || raw.len() < end {
        return Vec::new();
    }
    raw[2..end].to_vec()
}

/// A WIA range attribute is `[min, nominal, max, step]`.
pub fn spanned_values(raw: &[i32]) -> Option<ValueSpan> {
    if raw.len() < 4 {
        return None;
    }
    let step = if raw[3] == 0 { 1 } else { raw[3] };
    Some(ValueSpan { min: raw[0], max: raw[2], step })
}

/// What the driver says it can do. Flags are the WIA document-handling bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanner {
    pub id: String,
    pub name: String,
    pub feeder: bool,
    pub flatbed: bool,
    pub duplex: bool,
    /// A sheet is waiting in the feeder. Independent of [`Self::feeder`]. Same as
    /// [`HandlingStatus::feeder_ready`].
    pub feeder_ready: bool,
    pub handling_status: HandlingStatus,
    /// Sheets the feeder holds, when the driver publishes capacity.
    pub feeder_capacity: Option<u32>,
    /// How the device is attached, from the driver's port string when present.
    pub connection: Connection,
    /// Driver port string (`\\\\.\\Usbscan0`, a WSD URL, …), when published.
    pub port: Option<String>,
    /// Optical resolution when the driver publishes a positive value. Missing on many WSD class drivers.
    pub optical_x: Option<u32>,
    pub optical_y: Option<u32>,
    /// How many sheets the feeder will accept. `0` in the range means the whole feeder. `None`
    /// when the driver does not publish a range.
    pub pages: Option<ValueSpan>,
    /// Bed width and height in thousandths of an inch, when the driver publishes them.
    pub bed: Option<(u32, u32)>,
    /// Sheet-feeder width and height in thousandths of an inch.
    pub sheet: Option<(u32, u32)>,
    pub glass: Option<ImageChoices>,
    pub feeder_choices: Option<ImageChoices>,
}

/// A scanner as the device list shows it. Read without opening the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannerRef {
    pub id: String,
    pub name: String,
    /// Driver port string, when the device-info storage publishes it.
    pub port: Option<String>,
    pub connection: Connection,
}

impl ScannerRef {
    pub fn new(id: String, name: String, port: Option<String>) -> Self {
        let connection = port.as_deref().map(Connection::from_port).unwrap_or(Connection::Unknown);
        Self { id, name, port, connection }
    }

    /// Case-insensitive substring match against id, name or port. Empty text matches nothing.
    pub fn matches(&self, filter: &str) -> bool {
        let needle = filter.trim().to_lowercase();
        if needle.is_empty() {
            return false;
        }
        self.id.to_lowercase().contains(&needle)
            || self.name.to_lowercase().contains(&needle)
            || self.port.as_deref().is_some_and(|port| port.to_lowercase().contains(&needle))
    }

    /// Whether the id, name or port contains a denied entry (see [`deny_list`]).
    pub fn is_denied(&self, denied: &[String]) -> bool {
        denied.iter().any(|entry| {
            contains_plain(&self.id, entry)
                || contains_plain(&self.name, entry)
                || self.port.as_deref().is_some_and(|port| contains_plain(port, entry))
        })
    }
}

/// Environment variable naming devices that must never be opened: comma-separated substrings of a
/// device uuid, id, name or port.
pub const DENY_ENV: &str = "PDFCRAFT_SCAN_DENY";

/// Lowercase with hyphens and spaces removed, so a uuid compares with or without its hyphens.
fn plain(text: &str) -> String {
    text.chars().filter(|c| *c != '-' && !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

fn contains_plain(haystack: &str, entry: &str) -> bool {
    let entry = plain(entry);
    !entry.is_empty() && plain(haystack).contains(&entry)
}

/// The entries of [`DENY_ENV`], trimmed, with empty ones dropped.
pub fn deny_list() -> Vec<String> {
    parse_deny_list(&std::env::var(DENY_ENV).unwrap_or_default())
}

/// Split a comma-separated deny list.
pub fn parse_deny_list(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|entry| !entry.is_empty()).map(str::to_string).collect()
}

/// Whether a device filter names a denied entry: it equals or contains the entry, or is a fragment
/// of it at least four characters long (hyphens and case ignored).
pub fn filter_is_denied(filter: &str, denied: &[String]) -> bool {
    let wanted = plain(filter);
    denied.iter().any(|entry| {
        let entry = plain(entry);
        !entry.is_empty() && !wanted.is_empty() && (wanted.contains(&entry) || (wanted.len() >= 4 && entry.contains(&wanted)))
    })
}

/// One property from a read-only WIA dump. No transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyInfo {
    pub id: u32,
    pub name: String,
    pub vt: u16,
    pub flags: u32,
    pub value: String,
    pub access: String,
}

/// Root or child item in a read-only dump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemDump {
    pub kind: String,
    pub name: String,
    pub properties: Vec<PropertyInfo>,
}

/// Device tree from `dump_devices`. Connects and reads; does not feed paper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceDump {
    pub id: String,
    pub name: String,
    pub items: Vec<ItemDump>,
}

/// A request that pulls paper when `acquire` runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanSettings {
    /// Device id or a unique substring of the name. Empty uses the only scanner, or the first.
    pub device: String,
    pub source: Source,
    pub dpi: u32,
    /// Y resolution when it differs from [`Self::dpi`]. `None` writes the same value on both axes.
    pub dpi_y: Option<u32>,
    pub color: Color,
    /// How many sheets to pull. `0` means until the feeder is empty, and never more than
    /// [`MAX_PAGES`]. The bed is one page.
    pub pages: u32,
    /// Left alone when `None`, so the driver's current value stands.
    pub brightness: Option<i32>,
    pub contrast: Option<i32>,
    pub threshold: Option<i32>,
    pub rotation: Option<Rotation>,
    /// A window on the page, in thousandths of an inch. `None` keeps the driver's full area.
    pub region: Option<Region>,
    /// Transfer format name (`bmp`, `jpeg`, …). `None` leaves the driver's preferred format.
    pub format: Option<String>,
    /// Compression name (`none`, `jpeg`, `g4`, …). `None` leaves the driver alone.
    pub compression: Option<String>,
    /// Intent written before fine controls when the driver lists it.
    pub intent: Option<Intent>,
    /// Page-size preset. A [`Self::region`] still overrides the extent afterward.
    pub page_size: Option<PageSize>,
    /// Automatic deskew when the driver publishes the control.
    pub auto_deskew: Option<bool>,
    /// Orientation code from the driver's list (`WIA_IPS_ORIENTATION`), when distinct from rotation.
    pub orientation: Option<i32>,
    /// Photometric interpretation code when the driver lists values.
    pub photometric: Option<i32>,
    pub invert: Option<bool>,
    pub mirror: Option<bool>,
    /// Lamp on/off when `WIA_IPS_LAMP` is published.
    pub lamp: Option<bool>,
    /// Request `FEEDER|DUPLEX` when the device reports duplex. Off by default.
    pub duplex: bool,
    /// Optional cancel token. The dialog and tests call [`CancelToken::request`].
    pub cancel: Option<CancelToken>,
    /// Optional progress token. Each finished page is pushed as the driver hands it over.
    pub progress: Option<ProgressToken>,
}

/// Origin and size on the bed or the sheet, in thousandths of an inch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
}

/// Where that window falls in pixels at one resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionPixels {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The rectangle `acquire` will ask for. A full page leaves the driver's origin alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannedFrame {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Write [`Self::x`] and [`Self::y`] as the origin. A full page does not.
    pub move_origin: bool,
}

/// One transferred page, held entirely in memory.
///
/// Bindings and the app consume [`Self::bytes`] directly (create a PDF, OCR, write elsewhere).
/// There is no associated path on disk unless a caller separately uses [`write_pages`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub bytes: Vec<u8>,
    /// `bmp`, `png`, `jpeg`, or `tiff`, from the file header.
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    /// A black-and-white request got deeper data from the driver, so this page was thresholded in
    /// software to a real 1-bit BMP (see [`bw`]). `false` for pages delivered as they came.
    pub thresholded: bool,
}

/// Pages from `acquire`, with the source that was actually used (never [`Source::Auto`]).
///
/// Always in-memory. Spilling to files is the caller's choice ([`write_pages`]), not part of acquire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acquired {
    pub source: Source,
    pub pages: Vec<Page>,
}

pub const MAX_PAGES: u32 = 200;
pub const MIN_DPI: u32 = 50;
pub const MAX_DPI: u32 = 1200;

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            device: String::new(),
            source: Source::Auto,
            dpi: 200,
            dpi_y: None,
            color: Color::Gray,
            pages: 1,
            brightness: None,
            contrast: None,
            threshold: None,
            rotation: None,
            region: None,
            format: None,
            compression: None,
            intent: None,
            page_size: None,
            auto_deskew: None,
            orientation: None,
            photometric: None,
            invert: None,
            mirror: None,
            lamp: None,
            duplex: false,
            cancel: None,
            progress: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("scanning is only supported on Windows (WIA) in this version")]
    Unsupported,
    #[error("{0}")]
    Failed(String),
    /// Pages were received before Download ended with an error (jam, offline, incomplete stack).
    /// `cause` is the HRESULT behind the stop, when the engine saw one.
    #[error("{message}")]
    Incomplete { message: String, acquired: Acquired, cause: Option<WiaCause> },
    /// A failure that came from an HRESULT. It reads like [`Self::Failed`]; it also carries the code
    /// and the cause the engine decided on, so a caller need not parse the sentence.
    #[error("{message}")]
    Wia { cause: WiaCause, message: String },
}

/// The structured identity of a failure that came from the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WiaCause {
    /// The HRESULT the driver returned.
    pub code: u32,
    /// The cause the engine settled on. Usually [`kind_for_hresult`] of `code`; a status bit or a
    /// fault named in the driver's text can sharpen it (a busy feeder with no paper, a jam behind
    /// the vague paper-problem code).
    pub kind: InterruptKind,
}

impl PartialEq for ScanError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unsupported, Self::Unsupported) => true,
            (Self::Incomplete { message: a, .. }, Self::Incomplete { message: b, .. }) => a == b,
            // A code does not change what an error says; `Failed` and `Wia` with one sentence are equal.
            (a, b) => match (a.plain_message(), b.plain_message()) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        }
    }
}

impl ScanError {
    fn plain_message(&self) -> Option<&str> {
        match self {
            Self::Failed(message) | Self::Wia { message, .. } => Some(message),
            Self::Unsupported | Self::Incomplete { .. } => None,
        }
    }

    /// An error for an HRESULT, with the engine's sentence for it ([`describe_windows_error`]) and
    /// the cause the code stands for. The cause comes from the code ([`kind_for_hresult`]); the
    /// sentence decides only when the code is unknown or is the vague `PAPER_PROBLEM`, which the
    /// driver's own text can sharpen into a jam or a double feed.
    pub fn from_hresult(code: u32, driver_message: &str) -> Self {
        let message = describe_windows_error(code, driver_message);
        let from_message = interrupt_kind(&message);
        let kind = match kind_for_hresult(code) {
            Some(_) if code == PAPER_PROBLEM => from_message,
            Some(kind) => kind,
            None => from_message,
        };
        Self::Wia { cause: WiaCause { code, kind }, message }
    }

    /// The HRESULT and cause behind this error, if it has them.
    pub fn cause(&self) -> Option<WiaCause> {
        match self {
            Self::Wia { cause, .. } => Some(*cause),
            Self::Incomplete { cause, .. } => *cause,
            Self::Unsupported | Self::Failed(_) => None,
        }
    }

    /// The same failure restated as `sentence` by a rule of the engine (a status bit named the
    /// fault). The HRESULT the driver returned stays; the kind is that of the new sentence.
    fn restated(&self, sentence: &str) -> Self {
        match self.cause() {
            Some(cause) => Self::Wia { cause: WiaCause { code: cause.code, kind: interrupt_kind(sentence) }, message: sentence.to_string() },
            None => Self::Failed(sentence.to_string()),
        }
    }

    /// Like [`Self::restated`] for a sentence that names no cause: the HRESULT stays, the kind is
    /// [`InterruptKind::Other`].
    fn restated_other(&self, sentence: &str) -> Self {
        match self.cause() {
            Some(cause) => Self::Wia { cause: WiaCause { code: cause.code, kind: InterruptKind::Other }, message: sentence.to_string() },
            None => Self::Failed(sentence.to_string()),
        }
    }
}

/// `WIA_ERROR_PAPER_PROBLEM`.
const PAPER_PROBLEM: u32 = 0x8021_0004;

impl Eq for ScanError {}

/// Map WS-Scan SOAP text or driver messages to the same sentences as [`wia_error_message`].
fn transfer_fault_sentence(message: &str) -> Option<&'static str> {
    let lower = message.to_ascii_lowercase();
    if lower.contains("mediajam") || lower.contains("clienterrormediajam") {
        return wia_error_message(0x8021_0002);
    }
    if lower.contains("notacceptingjobs") || lower.contains("servererrornotacceptingjobs") {
        return wia_error_message(0x8021_0005);
    }
    if lower.contains("noimagesavailable") || lower.contains("clienterrornoimagesavailable") {
        return wia_error_message(0x8021_0003);
    }
    if lower.contains("coveropen") || lower.contains("clienterrorcoveropen") {
        return wia_error_message(0x8021_0010);
    }
    if lower.contains("multifeed") || lower.contains("clienterrordocumentfeederror") {
        return wia_error_message(0x8021_0014);
    }
    None
}

/// Plain sentence for a WIA HRESULT. The dialog and the CLI show this instead of the hex code.
pub fn wia_error_message(code: u32) -> Option<&'static str> {
    match code {
        // Connect after a bad / empty WSD install often returns this; re-Add while the device stays on.
        0x8021_0001 => Some("The scanner could not be opened. If it is a network scanner, remove it in Windows and add it again while it is on."),
        0x8021_0002 => Some("The scanner has a paper jam."),
        0x8021_0003 => Some("The feeder has no paper."),
        0x8021_0004 => Some("The scanner has a paper problem."),
        // Sticky offline after a WSD device restart: power/cable first, then re-Add while it stays on.
        0x8021_0005 => Some(
            "The scanner is offline. Check that it is powered on and connected. For a network scanner, remove it in Windows and add it again while it is on.",
        ),
        0x8021_0006 => Some("The scanner is busy. Close any other program that is using it, or wait for the current scan to finish, then try again."),
        0x8021_0007 => Some("The scanner is warming up. Try again in a moment."),
        0x8021_0008 => Some("The scanner needs attention. Check it, then try again."),
        0x8021_0009 => Some("The scanner item was removed."),
        0x8021_000A => Some("The computer could not talk to the scanner. Check the cable and power, then try again."),
        0x8021_000B => Some("The scanner rejected that command."),
        0x8021_000C => Some("A scanner setting was not accepted."),
        0x8021_000D => Some("The scanner is locked. Close any other program that is using it, then try again."),
        0x8021_000E => Some("The scanner driver failed."),
        0x8021_000F => Some("The scanner driver gave an unexpected response."),
        // 0x10 and 0x11 are what the Windows WSD driver was measured to return; 0x16 and 0x17 are the documented numbers.
        0x8021_0010 | 0x8021_0016 => Some("A cover on the scanner is open."),
        0x8021_0011 | 0x8021_0017 => Some("The scanner lamp is off."),
        0x8021_0012 => Some("The scan could not be saved to that destination."),
        0x8021_0013 => Some("The network scanner could not be reserved."),
        0x8021_0014 | 0x8021_0020 => Some("The feeder took more than one sheet."),
        0x8021_0015 => Some("No scanner is available. Check that it is powered on and connected."),
        0x8021_0021 => Some("The printer endorser counter is at its maximum."),
        _ => None,
    }
}

/// What stopped a scan, for the instruction a caller shows ("close the cover", "clear the jam", ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InterruptKind {
    /// A scanner or ADF cover is open.
    CoverOpen,
    /// The driver said "paper jam" outright.
    Jam,
    /// WIA's catch-all paper fault: a jam or several sheets fed together. The cause is not known,
    /// so the instruction covers both.
    PaperProblem,
    /// The feeder ran out of sheets, including a fixed-count shortfall.
    FeederEmpty,
    /// The feeder took more than one sheet.
    MultiFeed,
    /// Offline or unreachable.
    Offline,
    /// The scanner asks for attention without saying why (a door, a full store, a panel message).
    Attention,
    /// Busy, locked, or warming up: waiting is the cure.
    Busy,
    /// A feeder transfer answered busy and nothing names a cause. An empty feeder looks like this.
    BusyOrFeederEmpty,
    /// A lamp or device fault that a power cycle may clear.
    DeviceError,
    /// Anything the engine did not name.
    Other,
}

/// The kind each classified HRESULT stands for. Order matters when matching sentences: the
/// specific faults are tried before the broad "needs attention".
pub(crate) const KIND_BY_CODE: [(u32, InterruptKind); 19] = [
    (0x8021_0002, InterruptKind::Jam),
    (0x8021_0004, InterruptKind::PaperProblem),
    (0x8021_0010, InterruptKind::CoverOpen),
    (0x8021_0014, InterruptKind::MultiFeed),
    (0x8021_0003, InterruptKind::FeederEmpty),
    (0x8021_0005, InterruptKind::Offline),
    (0x8021_000A, InterruptKind::Offline),
    (0x8021_0008, InterruptKind::Attention),
    (0x8021_0001, InterruptKind::Offline),
    (0x8021_0006, InterruptKind::Busy),
    (0x8021_0007, InterruptKind::Busy),
    (0x8021_000D, InterruptKind::Busy),
    (0x8021_0011, InterruptKind::DeviceError),
    (0x8021_000E, InterruptKind::DeviceError),
    (0x8021_0013, InterruptKind::Busy),
    // The documented numbers (the ones above 0x0F are also what WIA lists for cover, lamp and multi-feed).
    (0x8021_0015, InterruptKind::Offline),
    (0x8021_0016, InterruptKind::CoverOpen),
    (0x8021_0017, InterruptKind::DeviceError),
    (0x8021_0020, InterruptKind::MultiFeed),
];

/// Classify an engine sentence. It matches only the engine's own fixed sentences
/// ([`wia_error_message`], [`feeder_shortfall_message`], the preflight "feeder is empty"), so the
/// answer is deterministic. A sentence may carry a prefix (a device name) and still match.
pub fn interrupt_kind(message: &str) -> InterruptKind {
    if message.contains(BUSY_OR_FEEDER_EMPTY) {
        return InterruptKind::BusyOrFeederEmpty;
    }
    for (code, kind) in KIND_BY_CODE {
        if let Some(sentence) = wia_error_message(code)
            && message.contains(sentence)
        {
            return kind;
        }
    }
    // The short offline sentence the device layer builds, which opens the table's longer one.
    if message.contains("The scanner is offline.") {
        return InterruptKind::Offline;
    }
    // Same text as [`feeder_shortfall_message`] around its two numbers, and the preflight sentence.
    if message.contains("the feeder reported no more paper.") || message.contains("the feeder is empty") {
        return InterruptKind::FeederEmpty;
    }
    InterruptKind::Other
}

impl ScanError {
    /// The cause. The engine's own decision when the error came from an HRESULT; for an error with
    /// none behind it, [`interrupt_kind`] of its sentence.
    pub fn interrupt_kind(&self) -> InterruptKind {
        match self.cause() {
            Some(cause) => cause.kind,
            None => interrupt_kind(&self.to_string()),
        }
    }
}

/// Text for a Windows error from WIA. Known codes become sentences; unknown ones keep the hex.
pub fn describe_windows_error(code: u32, message: &str) -> String {
    // PAPER_PROBLEM is the driver's catch-all for the paper path. When the message carries the
    // device's own fault name (a jam or a double feed), that is the better sentence.
    if code == 0x8021_0004
        && let Some(sentence) = transfer_fault_sentence(message.trim())
    {
        return sentence.to_string();
    }
    if let Some(text) = wia_error_message(code) {
        return text.to_string();
    }
    let trimmed = message.trim();
    if !trimmed.is_empty()
        && !trimmed.eq_ignore_ascii_case("ok")
        && trimmed != "The operation completed successfully."
        && !trimmed.eq_ignore_ascii_case("Unspecified error")
    {
        if let Some(sentence) = transfer_fault_sentence(trimmed) {
            return sentence.to_string();
        }
        return trimmed.to_string();
    }
    // E_FAIL / Unspecified after Download is common for mid-scan drop or a rejected WSD image.
    if code == 0x8000_4005 {
        return "The scan was interrupted. Check for a paper jam, an open cover, or that the scanner stayed online.".into();
    }
    if (0x8021_0000..=0x8021_FFFF).contains(&code) {
        return format!("The scanner reported an error ({code:#010x}).");
    }
    format!("The scan failed ({code:#010x}).")
}

impl ScanSettings {
    pub fn checked(&self) -> Result<&Self, ScanError> {
        if !(MIN_DPI..=MAX_DPI).contains(&self.dpi) {
            return Err(ScanError::Failed(format!("dpi is {MIN_DPI} to {MAX_DPI}")));
        }
        if let Some(dpi_y) = self.dpi_y
            && !(MIN_DPI..=MAX_DPI).contains(&dpi_y)
        {
            return Err(ScanError::Failed(format!("dpi-y is {MIN_DPI} to {MAX_DPI}")));
        }
        if self.pages > MAX_PAGES {
            return Err(ScanError::Failed(format!("pages is at most {MAX_PAGES}, or 0 for the whole feeder")));
        }
        if let Some(region) = self.region
            && (region.width == 0 || region.height == 0)
        {
            return Err(ScanError::Failed("the scan region has no size".into()));
        }
        Ok(self)
    }
}

/// Pixel extent for a physical size in thousandths of an inch.
pub fn pixels_for(thousandths_of_an_inch: u32, dpi: u32) -> u32 {
    let px = u64::from(thousandths_of_an_inch) * u64::from(dpi) / 1000;
    u32::try_from(px).unwrap_or(u32::MAX)
}

/// Uncompressed BMP size for a page: each row padded to 4 bytes, plus a 54-byte header.
pub fn estimated_bmp_bytes(width: u32, height: u32, depth: u32) -> Option<u64> {
    if depth == 0 || depth > 32 {
        return None;
    }
    let stride = u64::from(width).checked_mul(u64::from(depth))?.div_ceil(32) * 4;
    stride.checked_mul(u64::from(height))?.checked_add(54)
}

/// Keep the same physical area when the resolution changes.
pub fn rescale_extent(extent: u32, from_dpi: u32, to_dpi: u32) -> u32 {
    if from_dpi == 0 {
        return extent;
    }
    let px = u64::from(extent) * u64::from(to_dpi) / u64::from(from_dpi);
    u32::try_from(px).unwrap_or(u32::MAX).max(1)
}

/// WIA document-handling capability bits: feeder, flatbed, duplex.
pub fn handling_caps(bits: i32) -> (bool, bool, bool) {
    (bits & 0x1 != 0, bits & 0x2 != 0, bits & 0x4 != 0)
}

/// Width and height of a BMP, if `bytes` is one (see [`bmp::parse`] for what is accepted).
pub fn bmp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    bmp::size(bytes)
}

pub fn image_format(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        "jpeg"
    } else if bytes.starts_with(b"II") || bytes.starts_with(b"MM") {
        "tiff"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else {
        "bin"
    }
}

pub fn page_path(folder: &Path, index: usize, format: &str) -> PathBuf {
    folder.join(format!("page-{index:03}.{format}"))
}

/// Optional disk sink: write each [`Page`]'s bytes as `page-001.{format}`, …
///
/// Not used by `acquire`. For a CLI `--out` directory or a binding that prefers files; in-memory
/// consumers skip this and keep [`Page::bytes`]. Refuses to overwrite an existing `page-*` file.
pub fn write_pages(folder: &Path, pages: &[Page]) -> Result<Vec<PathBuf>, ScanError> {
    use std::io::Write;
    std::fs::create_dir_all(folder).map_err(|error| ScanError::Failed(format!("{}: {error}", folder.display())))?;
    let mut paths = Vec::with_capacity(pages.len());
    for (index, page) in pages.iter().enumerate() {
        let path = page_path(folder, index + 1, page.format);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| ScanError::Failed(format!("{}: {error}", path.display())))?;
        file.write_all(&page.bytes).map_err(|error| ScanError::Failed(format!("{}: {error}", path.display())))?;
        paths.push(path);
    }
    Ok(paths)
}

/// `page-001.bmp` (and png/jpeg/tiff/bin) already in `folder`, sorted by index.
pub fn listed_page_files(folder: &Path) -> Result<Vec<PathBuf>, ScanError> {
    let entries = std::fs::read_dir(folder).map_err(|error| ScanError::Failed(format!("{}: {error}", folder.display())))?;
    let mut pages = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| ScanError::Failed(format!("{}: {error}", folder.display())))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(index) = stem.strip_prefix("page-").and_then(|rest| rest.parse::<usize>().ok()) else {
            continue;
        };
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        if !matches!(ext, "bmp" | "png" | "jpeg" | "jpg" | "tiff" | "tif" | "bin") {
            continue;
        }
        pages.push((index, path));
    }
    pages.sort_by_key(|(index, _)| *index);
    for (expected, (index, _)) in pages.iter().enumerate() {
        if *index != expected + 1 {
            return Err(ScanError::Failed(format!(
                "page files in {} must be page-001, page-002, … without gaps (found page-{index:03})",
                folder.display()
            )));
        }
    }
    Ok(pages.into_iter().map(|(_, path)| path).collect())
}

/// Manual duplex without a hardware duplex bit: `fronts` from the first pass in order; the stack
/// was flipped so the last sheet is on top; `backs_as_scanned` is the reverse-side pass (first
/// image is the back of the last front). Reverses the backs and returns front0, back0, ….
pub fn interleave_manual_duplex<T>(fronts: Vec<T>, backs_as_scanned: Vec<T>) -> Result<Vec<T>, ScanError> {
    if fronts.is_empty() {
        return Err(ScanError::Failed("manual duplex needs the front pages from the first pass".into()));
    }
    if backs_as_scanned.len() != fronts.len() {
        return Err(ScanError::Failed(format!(
            "manual duplex needs one back for each front ({} front(s), {} back(s))",
            fronts.len(),
            backs_as_scanned.len()
        )));
    }
    let mut backs = backs_as_scanned;
    backs.reverse();
    let mut out = Vec::with_capacity(fronts.len() * 2);
    for (front, back) in fronts.into_iter().zip(backs) {
        out.push(front);
        out.push(back);
    }
    Ok(out)
}

/// The smallest listed resolution, or 100 when the driver publishes none.
pub fn preview_dpi(listed: &[u32]) -> u32 {
    listed.iter().copied().filter(|dpi| *dpi > 0).min().unwrap_or(100)
}

/// Pixel origin and size of `region` at `dpi`. A zero side is an error.
pub fn region_pixels(region: Region, dpi: u32) -> Result<RegionPixels, ScanError> {
    if region.width == 0 || region.height == 0 || dpi == 0 {
        return Err(ScanError::Failed("the scan region has no size".into()));
    }
    let width = pixels_for(region.width, dpi);
    let height = pixels_for(region.height, dpi);
    if width == 0 || height == 0 {
        return Err(ScanError::Failed("the scan region has no size".into()));
    }
    Ok(RegionPixels { x: pixels_for(region.left, dpi), y: pixels_for(region.top, dpi), width, height })
}

/// Keep `region` inside `max` (thousandths of an inch). `None` leaves the request as it is.
pub fn fit_region(region: Region, max: Option<(u32, u32)>) -> Result<Region, ScanError> {
    if region.width == 0 || region.height == 0 {
        return Err(ScanError::Failed("the scan region has no size".into()));
    }
    let Some((max_w, max_h)) = max.filter(|(w, h)| *w > 0 && *h > 0) else {
        return Ok(region);
    };
    if region.left >= max_w || region.top >= max_h {
        return Err(ScanError::Failed("the scan region is outside the page".into()));
    }
    let width = region.width.min(max_w - region.left);
    let height = region.height.min(max_h - region.top);
    if width == 0 || height == 0 {
        return Err(ScanError::Failed("the scan region has no size".into()));
    }
    Ok(Region { left: region.left, top: region.top, width, height })
}

/// Snap `value` onto a published range. Extent stays positive when `must_be_positive` is set.
pub fn align_to_span(value: i32, span: ValueSpan, must_be_positive: bool) -> Result<i32, ScanError> {
    let step = if span.step <= 0 { 1 } else { span.step };
    if span.max < span.min {
        return Err(ScanError::Failed("the scanner published an empty range".into()));
    }
    let mut aligned = value.clamp(span.min, span.max);
    let offset = i64::from(aligned) - i64::from(span.min);
    let rem = offset.rem_euclid(i64::from(step));
    if rem != 0 {
        let down = i64::from(aligned) - rem;
        let next = if down >= i64::from(span.min) { down } else { down + i64::from(step) };
        aligned = i32::try_from(next).map_err(|_| ScanError::Failed("a scanner value does not fit".into()))?;
        if aligned > span.max {
            let back = i64::from(span.max) - i64::from(span.min);
            let snapped = i64::from(span.min) + back - back.rem_euclid(i64::from(step));
            aligned = i32::try_from(snapped).map_err(|_| ScanError::Failed("a scanner value does not fit".into()))?;
        }
    }
    if must_be_positive && aligned <= 0 {
        return Err(ScanError::Failed("the scan region has no size".into()));
    }
    if !span.contains(aligned) {
        return Err(ScanError::Failed(format!("a scanner value {value} is outside {} to {}, step {step}", span.min, span.max)));
    }
    Ok(aligned)
}

/// Full-page rescaling, or a region clamped to `max` and turned into pixels at `to_dpi`.
pub fn planned_frame(
    old_width: u32,
    old_height: u32,
    from_dpi: u32,
    to_dpi: u32,
    region: Option<Region>,
    max: Option<(u32, u32)>,
) -> Result<PlannedFrame, ScanError> {
    if let Some(region) = region {
        let fitted = fit_region(region, max)?;
        let px = region_pixels(fitted, to_dpi)?;
        return Ok(PlannedFrame { x: px.x, y: px.y, width: px.width, height: px.height, move_origin: true });
    }
    if old_width == 0 || old_height == 0 {
        return Err(ScanError::Failed("the scanner did not report a page size".into()));
    }
    Ok(PlannedFrame {
        x: 0,
        y: 0,
        width: rescale_extent(old_width, from_dpi, to_dpi),
        height: rescale_extent(old_height, from_dpi, to_dpi),
        move_origin: false,
    })
}

fn thousandths_down(pixels: u32, dpi: u32) -> u32 {
    u32::try_from(u64::from(pixels) * 1000 / u64::from(dpi)).unwrap_or(u32::MAX)
}

fn thousandths_up(pixels: u32, dpi: u32) -> u32 {
    u32::try_from((u64::from(pixels) * 1000).div_ceil(u64::from(dpi))).unwrap_or(u32::MAX)
}

/// Map a drag rectangle in preview pixels to thousandths of an inch.
///
/// The origin rounds down and the far edge rounds up, then one extra thousandth, so a later
/// [`pixels_for`] truncation still covers the selection. A zero-area drag is the full page.
pub fn region_from_preview(x0: u32, y0: u32, x1: u32, y1: u32, dpi: u32) -> Result<Option<Region>, ScanError> {
    if dpi == 0 {
        return Err(ScanError::Failed("the preview has no resolution".into()));
    }
    let (left_px, right_px) = (x0.min(x1), x0.max(x1));
    let (top_px, bottom_px) = (y0.min(y1), y0.max(y1));
    if left_px == right_px || top_px == bottom_px {
        return Ok(None);
    }
    let left = thousandths_down(left_px, dpi);
    let top = thousandths_down(top_px, dpi);
    let right = thousandths_up(right_px, dpi).saturating_add(1);
    let bottom = thousandths_up(bottom_px, dpi).saturating_add(1);
    let width = right.saturating_sub(left);
    let height = bottom.saturating_sub(top);
    if width == 0 || height == 0 {
        return Ok(None);
    }
    Ok(Some(Region { left, top, width, height }))
}

fn thousandths_from_inches(inches: f64) -> Result<u32, ScanError> {
    if !inches.is_finite() || inches < 0.0 {
        return Err(ScanError::Failed("the scan region cannot be negative".into()));
    }
    let n = (inches * 1000.0).round();
    if n > f64::from(u32::MAX) {
        return Err(ScanError::Failed("the scan region is too large".into()));
    }
    Ok(n as u32)
}

/// `--left`, `--top`, `--width` and `--height` in inches. Width and height must be positive.
pub fn region_from_inches(left: f64, top: f64, width: f64, height: f64) -> Result<Region, ScanError> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(ScanError::Failed("the scan region width and height must be greater than zero".into()));
    }
    let left = thousandths_from_inches(left)?;
    let top = thousandths_from_inches(top)?;
    let width = thousandths_from_inches(width)?;
    let height = thousandths_from_inches(height)?;
    if width == 0 || height == 0 {
        return Err(ScanError::Failed("the scan region width and height must be greater than zero".into()));
    }
    Ok(Region { left, top, width, height })
}

/// The operations on a platform with no scanner backend. The Windows backend is `pdfcraft-wia`;
/// callers pick it on Windows and these everywhere else.
pub mod unsupported {
    use super::{Acquired, DeviceDump, ScanError, ScanSettings, Scanner, ScannerRef};

    pub fn scanner_list() -> Result<Vec<ScannerRef>, ScanError> {
        Err(ScanError::Unsupported)
    }

    pub fn scanner_details(_id: &str) -> Result<Scanner, ScanError> {
        Err(ScanError::Unsupported)
    }

    pub fn scanners() -> Result<Vec<Scanner>, ScanError> {
        Err(ScanError::Unsupported)
    }

    pub fn dump_devices() -> Result<Vec<DeviceDump>, ScanError> {
        Err(ScanError::Unsupported)
    }

    pub fn acquire(_settings: &ScanSettings) -> Result<Acquired, ScanError> {
        Err(ScanError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::trace::{TraceDest, trace_dest_from_str};
    use super::{
        CancelToken, Color, Connection, HandlingStatus, Intent, Page, PageSize, ProgressToken, Region, RegionPixels, Rotation, ScanError,
        ScanSettings, Source, ValueSpan, align_to_span, bmp_size, estimated_bmp_bytes, fit_region, handling_caps, image_format,
        interleave_manual_duplex, listed_values, page_path, pixels_for, planned_frame, preview_dpi, region_from_inches, region_from_preview,
        region_pixels, rescale_extent, spanned_values,
    };

    #[test]
    fn scan_trace_env_values_match_stderr_or_path() {
        assert_eq!(trace_dest_from_str("1"), TraceDest::Stderr);
        assert_eq!(trace_dest_from_str("off"), TraceDest::Off);
        assert!(matches!(trace_dest_from_str("/tmp/trace.jsonl"), TraceDest::File(_)));
    }

    #[test]
    fn letter_width_at_200_dpi_matches_the_driver_extent() {
        assert_eq!(pixels_for(8500, 200), 1700);
        assert_eq!(rescale_extent(1700, 200, 300), 2550);
    }

    #[test]
    fn a_busy_scanner_is_explained_without_the_hex_code() {
        let text = super::wia_error_message(0x8021_0006).expect("busy");
        assert!(text.contains("busy"));
        assert!(!text.contains("8021"));
        assert_eq!(super::describe_windows_error(0x8021_0006, "0x80210006"), text);
        let e_fail = super::describe_windows_error(0x8000_4005, "");
        assert!(e_fail.contains("interrupted") || e_fail.contains("online"), "{e_fail}");
        assert!(!e_fail.contains("80004005"), "{e_fail}");
        assert_eq!(super::wia_error_message(0x8021_0002), Some("The scanner has a paper jam."));
        assert_eq!(super::wia_error_message(0x8021_0003), Some("The feeder has no paper."));
        assert_eq!(super::wia_error_message(0x8021_0004), Some("The scanner has a paper problem."));
        let offline = super::wia_error_message(0x8021_0005).expect("offline");
        assert!(offline.contains("offline") && offline.contains("network"), "{offline}");
        let open_fail = super::wia_error_message(0x8021_0001).expect("open");
        assert!(open_fail.contains("opened") && open_fail.contains("network"), "{open_fail}");
        assert_eq!(super::wia_error_message(0x8021_0007), Some("The scanner is warming up. Try again in a moment."));
        assert_eq!(super::wia_error_message(0x8021_0008), Some("The scanner needs attention. Check it, then try again."));
        assert_eq!(super::wia_error_message(0x8021_0010), Some("A cover on the scanner is open."));
        assert_eq!(super::wia_error_message(0x8021_0011), Some("The scanner lamp is off."));
        assert_eq!(super::wia_error_message(0x8021_0014), Some("The feeder took more than one sheet."));
        assert_eq!(super::describe_windows_error(0x8021_0002, "ignored hex"), "The scanner has a paper jam.");
        let unknown_wia = super::describe_windows_error(0x8021_00FE, "ok");
        assert!(unknown_wia.contains("0x802100fe") && unknown_wia.contains("scanner"), "{unknown_wia}");
        assert!(super::describe_windows_error(0x8021_00FE, "The operation completed successfully.").contains("0x802100fe"));
        let jam_soap = super::describe_windows_error(0x8000_4005, "ClientErrorMediaJam: jam_after_n=2");
        assert!(jam_soap.contains("paper jam"), "{jam_soap}");
        let offline_soap = super::describe_windows_error(0x8000_4005, "ServerErrorNotAcceptingJobs: offline_after_n=1");
        assert!(offline_soap.contains("offline"), "{offline_soap}");
    }

    #[test]
    fn a_1200_dpi_color_letter_page_is_about_410_megabytes() {
        let width = pixels_for(8500, 1200);
        let height = pixels_for(11693, 1200);
        assert!(estimated_bmp_bytes(width, height, 24).is_some_and(|n| n > 256 * 1024 * 1024));
        assert!(estimated_bmp_bytes(width, height, 8).is_some_and(|n| n < 256 * 1024 * 1024));
        assert!(estimated_bmp_bytes(10, 10, 0).is_none());
    }

    #[test]
    fn auto_picks_the_feeder_only_when_it_is_ready() {
        assert_eq!(Source::Auto.resolve(true, true, true), Ok(Source::Feeder));
        assert_eq!(Source::Auto.resolve(false, true, true), Ok(Source::Glass));
        assert_eq!(Source::Auto.resolve(true, false, true), Ok(Source::Glass));
        assert_eq!(Source::Auto.resolve(false, true, false), Ok(Source::Feeder));
        assert_eq!(Source::Auto.resolve(false, false, false), Err(ScanError::Failed("no scanner source is available".into())));
        // Sticky FEED_READY with an empty tray is handled in acquire (auto retries the glass).
        assert_eq!(Source::Feeder.resolve(false, false, false), Ok(Source::Feeder));
        assert_eq!(Source::parse("flatbed"), Some(Source::Glass));
        assert_eq!(Source::parse("bed"), Some(Source::Glass));
        assert_eq!(Source::parse("auto").map(Source::name), Some("auto"));
        assert_eq!(Source::parse("sheet"), None);
    }

    #[test]
    fn color_rotation_intent_and_page_size_round_trip_through_the_driver_codes() {
        for color in [Color::BlackAndWhite, Color::Gray, Color::Rgb] {
            assert_eq!(Color::from_data_type(color.data_type()), Some(color));
        }
        assert_eq!((Color::Gray.name(), Color::Rgb.name(), Color::BlackAndWhite.name()), ("gray", "rgb", "bw"));
        assert_eq!((Color::Gray.depth(), Color::Rgb.depth(), Color::BlackAndWhite.depth()), (8, 24, 1));
        for turn in [Rotation::Portrait, Rotation::Landscape, Rotation::UpsideDown, Rotation::Left] {
            assert_eq!(Rotation::from_driver(turn.driver()), Some(turn));
            assert_eq!(Rotation::parse(turn.name()), Some(turn));
        }
        assert_eq!(Rotation::parse("90"), Some(Rotation::Landscape));
        assert_eq!(Rotation::parse("upside-down"), Some(Rotation::UpsideDown));
        for intent in [Intent::None, Intent::Color, Intent::Gray, Intent::Text, Intent::BestPreview, Intent::MaximizeQuality, Intent::MinimizeSize] {
            assert_eq!(Intent::from_driver(intent.driver()), Some(intent));
            assert_eq!(Intent::parse(intent.name()), Some(intent));
        }
        assert_eq!(Intent::parse("ocr"), Some(Intent::Text));
        assert_eq!(Intent::parse("min-size"), Some(Intent::MinimizeSize));
        for size in [PageSize::A4, PageSize::Letter, PageSize::Legal, PageSize::Auto, PageSize::Custom, PageSize::BusinessCard] {
            assert_eq!(PageSize::from_driver(size.driver()), Some(size));
            assert_eq!(PageSize::parse(size.name()), Some(size));
        }
        assert_eq!(PageSize::parse("card"), Some(PageSize::BusinessCard));
    }

    #[test]
    fn handling_bits_name_feeder_glass_and_duplex() {
        assert_eq!(handling_caps(3), (true, true, false));
        assert_eq!(handling_caps(7), (true, true, true));
        let status = HandlingStatus::from_bits(0x3F);
        assert!(status.feeder_ready && status.flat_ready && status.duplex_ready && status.cover_open && status.path_cover_open && status.jam);
        assert!(!status.multiple_feed && !status.attention && !status.lamp_error);
        let empty = HandlingStatus::from_bits(0);
        assert!(!empty.feeder_ready && !empty.jam);
        assert_eq!(status.acquire_block_reason(), Some("The scanner has a paper jam."));
        assert_eq!(HandlingStatus { cover_open: true, ..HandlingStatus::default() }.acquire_block_reason(), Some("A cover on the scanner is open."));
        assert_eq!(HandlingStatus::from_bits(0x10).acquire_block_reason(), Some("A cover on the scanner is open."));
        assert_eq!(HandlingStatus::from_bits(0x200).acquire_block_reason(), Some("The feeder took more than one sheet."));
        assert_eq!(HandlingStatus::from_bits(0x800).acquire_block_reason(), Some("The scanner lamp is off."));
        // Attention alone is informational (paused, lamp warning): Windows still transfers.
        assert_eq!(HandlingStatus::from_bits(0x400).acquire_block_reason(), None);
        assert_eq!(HandlingStatus::from_bits(0x400 | 0x800).acquire_block_reason(), Some("The scanner lamp is off."));
        // Jam outranks every other bit.
        assert_eq!(HandlingStatus::from_bits(0x200 | 0x20).acquire_block_reason(), Some("The scanner has a paper jam."));
        assert_eq!(empty.acquire_block_reason(), None);
    }

    #[test]
    fn a_generic_download_error_uses_the_status_bit_when_one_is_set() {
        let generic = || ScanError::Failed("The scanner reported an error.".into());
        let jam = HandlingStatus { jam: true, ..HandlingStatus::default() };
        assert!(super::explain_transfer_failure(generic(), jam, true, 0, 0).to_string().contains("paper jam"));
        let unspecified = ScanError::Failed("Unspecified error".into());
        let cover = HandlingStatus { cover_open: true, ..HandlingStatus::default() };
        assert!(super::explain_transfer_failure(unspecified, cover, false, 0, 0).to_string().contains("cover"));
        let multi = HandlingStatus { multiple_feed: true, ..HandlingStatus::default() };
        assert!(super::explain_transfer_failure(generic(), multi, true, 0, 0).to_string().contains("more than one"));
        let lamp = HandlingStatus { lamp_error: true, ..HandlingStatus::default() };
        assert!(
            super::explain_transfer_failure(ScanError::Failed("The scan failed (0x80004005).".into()), lamp, false, 0, 0)
                .to_string()
                .contains("lamp")
        );
        let named = || ScanError::Failed("The feeder has no paper.".into());
        assert_eq!(super::explain_transfer_failure(named(), jam, true, 0, 0).to_string(), named().to_string());
        assert!(super::retry_with_file_transfer(&ScanError::Failed("The parameter is incorrect.".into())));
        assert!(!super::retry_with_file_transfer(&named()));
        assert_eq!(super::explain_transfer_failure(generic(), HandlingStatus::default(), false, 0, 0).to_string(), "The scanner reported an error.");
    }

    #[test]
    fn an_attention_bit_beats_the_empty_tray_inference() {
        // A door interlock sets the attention bit and clears FEED_READY; it is not an empty tray.
        let interlock = HandlingStatus { attention: true, feeder_ready: false, ..HandlingStatus::default() };
        for (transferred, requested) in [(0, 0), (0, 4), (2, 4)] {
            let explained =
                super::explain_transfer_failure(ScanError::Failed("The scan failed (0x80004005).".into()), interlock, true, transferred, requested);
            assert_eq!(super::interrupt_kind(&explained.to_string()), super::InterruptKind::Attention, "{explained}");
        }
        // Without the attention bit a clear FEED_READY is still an empty tray.
        let empty = HandlingStatus { feeder_ready: false, ..HandlingStatus::default() };
        let explained = super::explain_transfer_failure(ScanError::Failed("Unspecified error".into()), empty, true, 0, 0);
        assert!(explained.to_string().contains("no paper"), "{explained}");
    }

    #[test]
    fn retry_with_file_transfer_only_for_unnamed_mode_rejection() {
        for text in [
            "The scanner reported an error.",
            "The scanner reported an error (0x802100fe).",
            "The scan failed (0x80004005).",
            "Unspecified error",
            "The parameter is incorrect.",
            "The scan was interrupted. Check for a paper jam, an open cover, or that the scanner stayed online.",
        ] {
            assert!(super::retry_with_file_transfer(&ScanError::Failed(text.into())), "{text}");
        }
        for text in [
            "The scanner has a paper jam.",
            "A cover on the scanner is open.",
            "The feeder has no paper.",
            "the feeder is empty",
            "The feeder took more than one sheet.",
            "The scanner lamp is off.",
            "The scanner needs attention. Check it, then try again.",
            "The scanner has a paper problem.",
        ] {
            assert!(!super::retry_with_file_transfer(&ScanError::Failed(text.into())), "{text}");
        }
        // Generic HRESULT + clear FEED_READY on the feeder becomes paper-empty, so no file retry.
        let empty_tray = HandlingStatus { feeder_ready: false, ..HandlingStatus::default() };
        let explained = super::explain_transfer_failure(ScanError::Failed("Unspecified error".into()), empty_tray, true, 0, 0);
        assert!(explained.to_string().contains("no paper"), "{explained}");
        assert!(!super::retry_with_file_transfer(&explained));
        // Glass must not treat a clear FEED_READY as an empty tray (status remap leaves it generic).
        assert!(super::generic_scan_failure(&super::explain_transfer_failure(
            ScanError::Failed("Unspecified error".into()),
            empty_tray,
            false,
            0,
            0,
        )));
        // Still-ready feeder keeps the generic error so banded→file can retry.
        let ready = HandlingStatus { feeder_ready: true, ..HandlingStatus::default() };
        let keep = super::explain_transfer_failure(ScanError::Failed("The parameter is incorrect.".into()), ready, true, 0, 4);
        assert!(super::retry_with_file_transfer(&keep));
        // Jam still outranks a clear ready bit.
        let jammed = HandlingStatus { feeder_ready: false, jam: true, ..HandlingStatus::default() };
        assert!(
            super::explain_transfer_failure(ScanError::Failed("The scan failed (0x80004005).".into()), jammed, true, 2, 4)
                .to_string()
                .contains("paper jam")
        );
        // Mid-job with fewer pages than requested: do not call it an empty tray.
        let explained_mid = super::explain_transfer_failure(ScanError::Failed("Unspecified error".into()), empty_tray, true, 2, 4);
        assert!(super::generic_scan_failure(&explained_mid), "{explained_mid}");
        assert!(super::after_transfer(2, false, Some(explained_mid)).unwrap_err().to_string().contains("paper jam"));
    }

    #[test]
    fn a_mid_job_jam_is_still_an_error_after_pages_arrive() {
        let jam = || ScanError::Failed("The scanner has a paper jam.".into());
        let empty = || ScanError::Failed("The feeder has no paper.".into());
        let cover = || ScanError::Failed("A cover on the scanner is open.".into());
        let multi = || ScanError::Failed("The feeder took more than one sheet.".into());
        let lamp = || ScanError::Failed("The scanner lamp is off.".into());
        let busy = || ScanError::Failed(super::wia_error_message(0x8021_0006).expect("busy").into());
        let offline = || ScanError::Failed(super::wia_error_message(0x8021_0005).expect("offline").into());
        assert!(super::after_transfer(0, false, Some(jam())).unwrap_err().to_string().contains("paper jam"));
        // Pages stay in the progress token; the dialog shows the jam.
        assert!(super::after_transfer(2, false, Some(jam())).unwrap_err().to_string().contains("paper jam"));
        assert!(super::after_transfer(2, false, Some(cover())).unwrap_err().to_string().contains("cover"));
        assert!(super::after_transfer(1, false, Some(multi())).unwrap_err().to_string().contains("more than one"));
        assert!(super::after_transfer(2, false, Some(lamp())).unwrap_err().to_string().contains("lamp"));
        // Named busy / offline keep their sentences (not success, not the generic mid-stop rewrite).
        let busy_mid = super::after_transfer(2, false, Some(busy())).unwrap_err().to_string();
        assert!(busy_mid.contains("busy"), "{busy_mid}");
        assert!(!busy_mid.contains("stopped before"), "{busy_mid}");
        let offline_mid = super::after_transfer(1, false, Some(offline())).unwrap_err().to_string();
        assert!(offline_mid.contains("offline"), "{offline_mid}");
        assert!(!offline_mid.contains("stopped before"), "{offline_mid}");
        let attention = || ScanError::Failed(super::wia_error_message(0x8021_0008).expect("attention").into());
        let attention_mid = super::after_transfer(1, false, Some(attention())).unwrap_err().to_string();
        assert!(attention_mid.contains("interrupted"), "{attention_mid}");
        assert!(attention_mid.contains("stayed online"), "{attention_mid}");
        assert!(super::after_transfer(0, false, Some(attention())).unwrap_err().to_string().contains("needs attention"));
        assert!(super::after_transfer(2, false, Some(empty())).is_ok());
        assert!(super::after_transfer(3, false, Some(ScanError::Failed("the feeder is empty".into()))).is_ok());
        assert!(super::after_transfer(0, false, Some(empty())).unwrap_err().to_string().contains("no paper"));
        assert!(super::after_transfer(0, true, None).unwrap_err().to_string().contains("cancelled"));
        assert!(super::after_transfer(1, true, None).is_ok());
        assert!(super::after_transfer(0, false, None).unwrap_err().to_string().contains("no pages"));
        // Explained empty-tray end with pages already streamed is success.
        let empty_tray = HandlingStatus { feeder_ready: false, ..HandlingStatus::default() };
        let explained = super::explain_transfer_failure(ScanError::Failed("The scanner reported an error.".into()), empty_tray, true, 2, 2);
        assert!(super::after_transfer(2, false, Some(explained)).is_ok());
        // Whole feeder (`pages_requested == 0`): empty tray after some pages is a normal end.
        let explained_whole = super::explain_transfer_failure(ScanError::Failed("Unspecified error".into()), empty_tray, true, 3, 0);
        assert!(explained_whole.to_string().contains("no paper"), "{explained_whole}");
        assert!(super::after_transfer(3, false, Some(explained_whole)).is_ok());
        // Sticky FEED_READY after the stack is gone still ends whole-stack acquire.
        let sticky_ready = HandlingStatus { feeder_ready: true, ..HandlingStatus::default() };
        let explained_sticky = super::explain_transfer_failure(ScanError::Failed("The scan failed (0x80004005).".into()), sticky_ready, true, 3, 0);
        assert!(explained_sticky.to_string().contains("no paper"), "{explained_sticky}");
        assert!(super::after_transfer(3, false, Some(explained_sticky)).is_ok());
        // Generic E_FAIL with no pages → usable-page / re-Add hint (not raw Unspecified).
        let clarified = super::after_transfer(0, false, Some(ScanError::Failed("Unspecified error".into()))).unwrap_err();
        assert!(clarified.to_string().contains("usable page") || clarified.to_string().contains("online"), "{clarified}");
        // Generic E_FAIL after some pages → mid-scan drop sentence.
        let mid = super::after_transfer(2, false, Some(ScanError::Failed("The scan failed (0x80004005).".into()))).unwrap_err();
        assert!(mid.to_string().contains("stopped") || mid.to_string().contains("online"), "{mid}");
    }

    #[test]
    fn a_short_fixed_count_feeder_scan_is_incomplete_not_success() {
        let no_paper = || ScanError::Failed("The feeder has no paper.".into());
        let no_images = || ScanError::Failed("the feeder is empty".into());
        let jam = || ScanError::Failed("The scanner has a paper jam.".into());
        // 3 requested, 1 arrived, driver said no more paper / no images: incomplete, with the words.
        for end in [Some(no_paper()), Some(no_images()), None] {
            let text = super::after_transfer_checked(3, true, 1, false, end).unwrap_err().to_string();
            assert_eq!(text, "Only 1 of 3 pages were scanned: the feeder reported no more paper.");
            assert!(text.contains("feeder") && text.contains("paper"));
        }
        // Whole feeder (0 / "all"): a short stack is a normal end.
        assert!(super::after_transfer_checked(0, true, 1, false, Some(no_paper())).is_ok());
        assert!(super::after_transfer_checked(0, true, 5, false, None).is_ok());
        // Everything requested arrived (or more): success.
        assert!(super::after_transfer_checked(3, true, 3, false, Some(no_paper())).is_ok());
        assert!(super::after_transfer_checked(3, true, 3, false, None).is_ok());
        // Nothing arrived keeps today's errors.
        assert!(super::after_transfer_checked(3, true, 0, false, Some(no_paper())).unwrap_err().to_string().contains("no paper"));
        assert!(super::after_transfer_checked(3, true, 0, false, None).unwrap_err().to_string().contains("no pages"));
        // A jam keeps its own sentence; a user cancel with pages stays a normal end.
        assert!(super::after_transfer_checked(3, true, 1, false, Some(jam())).unwrap_err().to_string().contains("paper jam"));
        assert!(super::after_transfer_checked(3, true, 1, true, None).is_ok());
        // Glass (not the feeder) and the duplex opt-out (requested 0) are unaffected.
        assert!(super::after_transfer_checked(1, false, 1, false, None).is_ok());
        assert!(super::after_transfer_checked(3, false, 1, false, None).is_ok());
    }

    #[test]
    fn wsd_feeder_skips_empty_tray_preflight() {
        assert!(!super::feeder_empty_blocks_acquire(Source::Feeder, false, true));
        assert!(super::feeder_empty_blocks_acquire(Source::Feeder, false, false));
        assert!(!super::feeder_empty_blocks_acquire(Source::Feeder, true, false));
        assert!(!super::feeder_empty_blocks_acquire(Source::Glass, false, false));
    }

    #[test]
    fn wsd_whole_feeder_uses_a_large_ticket_page_count() {
        assert_eq!(super::wsd_feeder_ticket_pages(0), i32::try_from(super::MAX_PAGES).unwrap_or(i32::MAX));
        assert_eq!(super::wsd_feeder_ticket_pages(3), 3);
    }

    #[test]
    fn settings_reject_a_dpi_or_a_page_count_outside_the_range() {
        assert!(ScanSettings { dpi: 49, ..ScanSettings::default() }.checked().is_err());
        assert!(ScanSettings { dpi: 1201, ..ScanSettings::default() }.checked().is_err());
        assert!(ScanSettings { dpi_y: Some(49), ..ScanSettings::default() }.checked().is_err());
        assert!(ScanSettings { dpi_y: Some(300), ..ScanSettings::default() }.checked().is_ok());
        assert!(ScanSettings { pages: 201, ..ScanSettings::default() }.checked().is_err());
        assert!(ScanSettings { pages: 0, ..ScanSettings::default() }.checked().is_ok());
        assert_eq!(ScanSettings::default().source, Source::Auto);
        assert_eq!(ScanSettings::default().dpi, 200);
    }

    #[test]
    fn cancel_token_records_a_request_before_any_transfer() {
        let token = CancelToken::new();
        assert!(!token.is_requested());
        token.request();
        assert!(token.is_requested());
        let clone = token.clone();
        assert!(clone.is_requested());
        assert_eq!(token, clone);
        assert_ne!(token, CancelToken::new());
    }

    #[test]
    fn progress_token_queues_finished_pages() {
        let token = ProgressToken::new();
        assert_eq!(token.pages_done(), 0);
        assert_eq!(token.page_percent(), None);
        token.set_expected_bytes(Some(1_000));
        token.set_transfer(None, 250, None);
        assert_eq!(token.page_percent(), Some(25));
        assert_eq!(token.bytes_received(), 250);
        assert_eq!(token.bytes_expected(), Some(1_000));
        token.set_transfer(Some(80), 800, None);
        assert_eq!(token.page_percent(), Some(80));
        token.push_page(Page { bytes: vec![1], format: "bmp", width: 1, height: 1, dpi: 200, thresholded: false });
        token.push_page(Page { bytes: vec![2], format: "bmp", width: 1, height: 1, dpi: 200, thresholded: false });
        assert_eq!(token.pages_done(), 2);
        assert_eq!(token.page_percent(), None, "a finished page clears in-page fill");
        let taken = token.take_pages();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].bytes, vec![1]);
        assert!(token.take_pages().is_empty());
        assert_eq!(token.pages_done(), 2);
        assert_ne!(token, ProgressToken::new());
    }

    #[test]
    fn progress_token_soft_percent_without_expected_size() {
        let token = ProgressToken::new();
        token.set_expected_bytes(None);
        token.set_transfer(None, 32_768, None);
        let mid = token.page_percent().expect("soft fill");
        assert!((1..100).contains(&mid), "mid={mid}");
        token.set_transfer(None, 316_180, None);
        let late = token.page_percent().expect("late fill");
        assert!(late >= mid && late < 100, "late={late} mid={mid}");
        token.set_transfer(Some(100), 316_180, None);
        assert_eq!(token.page_percent(), Some(100));
        assert_eq!(token.bytes_expected(), Some(316_180));
    }

    #[test]
    fn page_paths_and_image_formats_follow_the_file_header() {
        assert_eq!(page_path(Path::new("out"), 1, "bmp"), Path::new("out").join("page-001.bmp"));
        assert_eq!(page_path(Path::new("out"), 12, "jpeg"), Path::new("out").join("page-012.jpeg"));
        assert_eq!(image_format(b"\x89PNG\r\n\x1a\n"), "png");
        assert_eq!(image_format(&[0xff, 0xd8, 0xff]), "jpeg");
        assert_eq!(image_format(b"II*\0"), "tiff");
        assert_eq!(image_format(b"MM\0*"), "tiff");
        assert_eq!(image_format(b"BM"), "bmp");
        assert_eq!(image_format(b"????"), "bin");
    }

    #[test]
    fn write_pages_is_an_optional_sink_and_refuses_overwrite() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-write-pages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pages = [
            Page { bytes: b"BM-a".to_vec(), format: "bmp", width: 1, height: 1, dpi: 200, thresholded: false },
            Page { bytes: b"BM-b".to_vec(), format: "bmp", width: 1, height: 1, dpi: 200, thresholded: false },
        ];
        let paths = super::write_pages(&dir, &pages).expect("write");
        assert_eq!(paths.len(), 2);
        assert_eq!(std::fs::read(&paths[0]).expect("read"), b"BM-a");
        assert_eq!(std::fs::read(&paths[1]).expect("read"), b"BM-b");
        assert!(super::write_pages(&dir, &pages[..1]).is_err(), "existing page-001 must not be overwritten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_list_attribute_drops_the_count_and_the_nominal_value() {
        assert_eq!(listed_values(&[7, 200, 100, 150, 200, 300, 400, 600, 1200]), vec![100, 150, 200, 300, 400, 600, 1200]);
        assert_eq!(listed_values(&[]), Vec::<i32>::new());
        assert_eq!(listed_values(&[0, 200]), Vec::<i32>::new());
        assert_eq!(listed_values(&[3, 200, 100]), Vec::<i32>::new());
        assert_eq!(spanned_values(&[-50, 0, 50, 1]), Some(ValueSpan { min: -50, max: 50, step: 1 }));
        assert_eq!(spanned_values(&[0, 1, 30, 0]), Some(ValueSpan { min: 0, max: 30, step: 1 }));
        assert_eq!(spanned_values(&[1, 2, 3]), None);
        let pages = ValueSpan { min: 0, max: 30, step: 1 };
        assert!(pages.contains(0) && pages.contains(30) && !pages.contains(31));
        let stepped = ValueSpan { min: 0, max: 10, step: 0 };
        assert!(stepped.contains(3), "a zero step means every integer in the range");
    }

    #[test]
    fn a_two_by_one_inch_region_at_1200_dpi_is_2400_by_1200() {
        let region = Region { left: 0, top: 0, width: 2000, height: 1000 };
        assert_eq!(region_pixels(region, 1200), Ok(RegionPixels { x: 0, y: 0, width: 2400, height: 1200 }));
        assert_eq!(region_from_inches(0.0, 0.5, 2.0, 1.0), Ok(Region { left: 0, top: 500, width: 2000, height: 1000 }));
        assert!(region_pixels(Region { left: 0, top: 500, width: 2000, height: 1000 }, 1200).is_ok_and(|px| px.width == 2400 && px.height == 1200));
    }

    #[test]
    fn a_full_page_leaves_the_rescaled_extent_and_a_zero_region_is_refused() {
        let full = planned_frame(1700, 2200, 200, 300, None, Some((8500, 11693)));
        assert!(full.is_ok_and(|frame| {
            !frame.move_origin && frame.width == rescale_extent(1700, 200, 300) && frame.height == rescale_extent(2200, 200, 300)
        }));
        let window = Region { left: 1000, top: 0, width: 2000, height: 1000 };
        assert!(
            planned_frame(1700, 2200, 200, 1200, Some(window), None)
                .is_ok_and(|frame| { frame.move_origin && frame.x == 1200 && frame.width == 2400 && frame.height == 1200 })
        );
        assert!(region_pixels(Region { left: 0, top: 0, width: 0, height: 1000 }, 1200).is_err());
        assert!(ScanSettings { region: Some(Region { left: 0, top: 0, width: 0, height: 1 }), ..ScanSettings::default() }.checked().is_err());
        assert!(region_from_inches(0.0, 0.0, 0.0, 1.0).is_err());
        assert!(region_from_inches(0.0, 0.0, -1.0, 1.0).is_err());
        assert_eq!(
            fit_region(Region { left: 8000, top: 0, width: 2000, height: 1000 }, Some((8500, 11693))),
            Ok(Region { left: 8000, top: 0, width: 500, height: 1000 })
        );
        assert_eq!(fit_region(window, None), Ok(window));
        assert!(fit_region(Region { left: 9000, top: 0, width: 100, height: 100 }, Some((8500, 11693))).is_err());
    }

    #[test]
    fn a_preview_rectangle_covers_the_same_area_at_1200_dpi() {
        assert!(region_from_preview(0, 0, 200, 100, 100).is_ok_and(|region| {
            region.is_some_and(|region| region_pixels(region, 1200).is_ok_and(|px| px.x == 0 && px.y == 0 && px.width >= 2400 && px.height >= 1200))
        }));
        assert!(region_from_preview(10, 10, 10, 40, 100).is_ok_and(|region| region.is_none()));
        let preview_px = 1u32;
        let start = preview_px * 1200 / 75;
        let end = (2_u32 * 1200).div_ceil(75);
        assert!(region_from_preview(1, 0, 2, 1, 75).is_ok_and(|region| {
            region.is_some_and(|region| region_pixels(region, 1200).is_ok_and(|px| px.x <= start && px.x + px.width >= end))
        }));
        assert_eq!(preview_dpi(&[]), 100);
        assert_eq!(preview_dpi(&[300, 100, 200]), 100);
        assert_eq!(align_to_span(20, ValueSpan { min: 0, max: 64, step: 8 }, true), Ok(16));
        assert!(align_to_span(0, ValueSpan { min: 0, max: 100, step: 1 }, true).is_err());
        assert!(region_from_preview(0, 0, 1, 1, 0).is_err());
    }

    #[test]
    fn a_bmp_header_has_a_size() {
        let mut bytes = vec![0u8; 54];
        bytes[0] = b'B';
        bytes[1] = b'M';
        bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&1700i32.to_le_bytes());
        bytes[22..26].copy_from_slice(&(-2338i32).to_le_bytes());
        bytes[26] = 1;
        bytes[28] = 24;
        assert_eq!(bmp_size(&bytes), Some((1700, 2338)));
        assert_eq!(bmp_size(b"BM"), None);
        // A cut-off header is not a BMP, and neither is one with no planes or a zero width.
        assert_eq!(bmp_size(&bytes[..53]), None);
        bytes[26] = 0;
        assert_eq!(bmp_size(&bytes), None);
    }

    #[test]
    fn manual_duplex_puts_the_first_flipped_back_with_the_last_front() {
        // Fronts F1,F2,F3 then flip the stack: backs arrive B3,B2,B1.
        let interleaved = interleave_manual_duplex(vec!["F1", "F2", "F3"], vec!["B3", "B2", "B1"]).expect("pair");
        assert_eq!(interleaved, vec!["F1", "B1", "F2", "B2", "F3", "B3"]);
        assert!(interleave_manual_duplex(vec!["F1"], vec!["B1", "B2"]).is_err());
        assert!(interleave_manual_duplex(Vec::<&str>::new(), Vec::new()).is_err());
    }

    #[test]
    fn connection_follows_the_port_string() {
        assert_eq!(Connection::from_port(r"\\.\Usbscan0"), Connection::Usb);
        assert_eq!(Connection::from_port("http://192.0.2.10:80/WebServices/Device"), Connection::Wsd);
        assert_eq!(Connection::from_port("WSD-43c6ea17"), Connection::Wsd);
        assert_eq!(
            Connection::from_port("urn:uuid:00000000-0000-4000-8000-0000000000aa/uri:00000000-0000-4000-8000-0000000000aa/ScannerService"),
            Connection::Wsd
        );
        assert_eq!(Connection::from_port(""), Connection::Unknown);
    }

    #[test]
    fn scanner_ref_derives_connection_from_port() {
        let wsd = super::ScannerRef::new("{a}".into(), "Fake".into(), Some("http://127.0.0.1:3920/x".into()));
        assert_eq!(wsd.connection, Connection::Wsd);
        let usb = super::ScannerRef::new("{b}".into(), "U".into(), Some(r"\.\Usbscan0".into()));
        assert_eq!(usb.connection, Connection::Usb);
        assert_eq!(super::ScannerRef::new("{c}".into(), "N".into(), None).connection, Connection::Unknown);
    }

    #[test]
    fn scanner_ref_filter_matches_id_name_or_port_case_insensitively() {
        let item = super::ScannerRef::new("{ABC-123}".into(), "My Scanner".into(), Some("urn:uuid:0000F2/ScannerService".into()));
        assert!(item.matches("abc-123"));
        assert!(item.matches("scanner"));
        assert!(item.matches("0000f2"));
        assert!(!item.matches("other"));
        assert!(!item.matches(""));
        assert!(!item.matches("   "));
    }

    #[test]
    fn deny_list_parses_and_matches_filters_and_devices() {
        let denied = super::parse_deny_list(" 11111111-2222-4333-8444-555555555555 , ,Secret Printer");
        assert_eq!(denied.len(), 2);
        for text in [
            "11111111-2222-4333-8444-555555555555",
            "11111111222243338444555555555555",
            "urn:uuid:11111111-2222-4333-8444-555555555555/x",
            "11111111",
            "555555555555",
            "SECRET PRINTER",
        ] {
            assert!(super::filter_is_denied(text, &denied), "{text}");
        }
        for text in ["111", "other", "", "  "] {
            assert!(!super::filter_is_denied(text, &denied), "{text}");
        }
        assert!(!super::filter_is_denied("anything", &[]));
        let by_port = super::ScannerRef::new("{x}".into(), "A".into(), Some("urn:uuid:11111111-2222-4333-8444-555555555555/svc".into()));
        let by_name = super::ScannerRef::new("{y}".into(), "My Secret Printer".into(), None);
        let other = super::ScannerRef::new("{z}".into(), "Fake".into(), Some("http://127.0.0.1:3920/x".into()));
        assert!(by_port.is_denied(&denied) && by_name.is_denied(&denied) && !other.is_denied(&denied));
        assert!(!by_port.is_denied(&[]));
    }

    #[test]
    fn listed_page_files_sort_and_refuse_gaps() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-listed-pages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        std::fs::write(dir.join("page-002.bmp"), b"BM").expect("write");
        std::fs::write(dir.join("page-001.bmp"), b"BM").expect("write");
        std::fs::write(dir.join("notes.txt"), b"x").expect("write");
        let listed = super::listed_page_files(&dir).expect("pages");
        assert_eq!(listed, vec![dir.join("page-001.bmp"), dir.join("page-002.bmp")]);
        std::fs::write(dir.join("page-004.bmp"), b"BM").expect("write");
        let gap = super::listed_page_files(&dir).expect_err("gap");
        assert!(gap.to_string().contains("without gaps"), "{gap}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn interrupt_kind_follows_the_engine_sentences() {
        use super::{InterruptKind as K, interrupt_kind, wia_error_message as w};
        for (code, kind) in [
            (0x8021_0002, K::Jam),
            (0x8021_0004, K::PaperProblem),
            (0x8021_0010, K::CoverOpen),
            (0x8021_0014, K::MultiFeed),
            (0x8021_0003, K::FeederEmpty),
            (0x8021_0005, K::Offline),
            (0x8021_000A, K::Offline),
            (0x8021_0008, K::Attention),
            (0x8021_0006, K::Busy),
            (0x8021_0007, K::Busy),
            (0x8021_000D, K::Busy),
            (0x8021_0011, K::DeviceError),
            (0x8021_000E, K::DeviceError),
        ] {
            assert_eq!(interrupt_kind(w(code).expect("sentence")), kind, "{code:#x}");
        }
    }

    #[test]
    fn interrupt_kind_reads_shortfall_prefix_and_generic_sentences() {
        use super::{HandlingStatus, InterruptKind as K, ScanError, feeder_shortfall_message, interrupt_kind};
        assert_eq!(interrupt_kind(&feeder_shortfall_message(3, 1)), K::FeederEmpty);
        assert_eq!(interrupt_kind("the feeder is empty"), K::FeederEmpty);
        assert_eq!(interrupt_kind("USB scanner (usb-2): The scanner is offline. Check that it is powered on and connected."), K::Offline);
        assert_eq!(interrupt_kind("The scan was interrupted. Check for a paper jam, an open cover, or that the scanner stayed online."), K::Other);
        assert_eq!(interrupt_kind("The scan stopped before all pages arrived. Check for a paper jam."), K::Other);
        assert_eq!(interrupt_kind(""), K::Other);
        assert_eq!(ScanError::Failed("The scanner has a paper jam.".into()).interrupt_kind(), K::Jam);
        let block = HandlingStatus::from_bits(0x10).acquire_block_reason().expect("cover");
        assert_eq!(interrupt_kind(block), K::CoverOpen);
        let block = HandlingStatus::from_bits(0x200).acquire_block_reason().expect("multi");
        assert_eq!(interrupt_kind(block), K::MultiFeed);
    }

    #[test]
    fn a_double_feed_after_page_one_is_not_called_a_jam() {
        let problem = || ScanError::Failed(super::describe_windows_error(0x8021_0004, ""));
        assert_eq!(problem().to_string(), "The scanner has a paper problem.");
        // No bit names a cause: stay neutral.
        let none = super::explain_transfer_failure(problem(), HandlingStatus::default(), true, 1, 3);
        assert_eq!(super::interrupt_kind(&none.to_string()), super::InterruptKind::PaperProblem);
        // The double-feed bit read after the transfer is used.
        let multi = super::explain_transfer_failure(problem(), HandlingStatus::from_bits(0x200 | 0x1), true, 1, 3);
        assert_eq!(super::interrupt_kind(&multi.to_string()), super::InterruptKind::MultiFeed);
        // A real jam bit still says jam; a cover says cover.
        let jam = super::explain_transfer_failure(problem(), HandlingStatus::from_bits(0x20), true, 1, 3);
        assert_eq!(super::interrupt_kind(&jam.to_string()), super::InterruptKind::Jam);
        let cover = super::explain_transfer_failure(problem(), HandlingStatus::from_bits(0x8), true, 1, 3);
        assert_eq!(super::interrupt_kind(&cover.to_string()), super::InterruptKind::CoverOpen);
        // The wire message names the fault: it beats the folded HRESULT.
        assert_eq!(
            super::interrupt_kind(&super::describe_windows_error(0x8021_0004, "ClientErrorMultiFeed: sheets stuck")),
            super::InterruptKind::MultiFeed
        );
        assert_eq!(
            super::interrupt_kind(&super::describe_windows_error(0x8021_0004, "ClientErrorMediaJam: jam_after_n=2")),
            super::InterruptKind::Jam
        );
    }

    #[test]
    fn feeder_busy_with_no_naming_bit_is_busy_or_empty() {
        use super::{BUSY_OR_FEEDER_EMPTY, InterruptKind as K};
        let busy = || ScanError::Failed(super::wia_error_message(0x8021_0006).expect("busy").into());
        let kind = |e: ScanError| e.interrupt_kind();
        // Feeder, busy, ready bit set, nothing else: the new sentence, with or without earlier pages.
        let none = super::explain_transfer_failure(busy(), HandlingStatus::from_bits(0x1), true, 0, 0);
        assert_eq!(none.to_string(), BUSY_OR_FEEDER_EMPTY);
        assert_eq!(kind(none), K::BusyOrFeederEmpty);
        let some = super::explain_transfer_failure(busy(), HandlingStatus::default(), true, 2, 5);
        assert_eq!(kind(some), K::BusyOrFeederEmpty);
        // With pages in hand the stack ends normally for a whole-feeder scan; a fixed count reports the shortfall.
        assert!(super::after_transfer_checked(0, true, 2, false, Some(ScanError::Failed(BUSY_OR_FEEDER_EMPTY.into()))).is_ok());
        let short = super::after_transfer_checked(5, true, 2, false, Some(ScanError::Failed(BUSY_OR_FEEDER_EMPTY.into())));
        assert_eq!(short.err().map(|e| e.interrupt_kind()), Some(K::FeederEmpty));
        // With no pages it stays an error.
        assert!(super::after_transfer_checked(0, true, 0, false, Some(ScanError::Failed(BUSY_OR_FEEDER_EMPTY.into()))).is_err());
        // A naming bit wins.
        let jam = super::explain_transfer_failure(busy(), HandlingStatus::from_bits(0x20), true, 0, 0);
        assert_eq!(kind(jam), K::Jam);
        let attention = super::explain_transfer_failure(busy(), HandlingStatus::from_bits(0x400 | 0x1), true, 0, 0);
        assert_eq!(kind(attention), K::Attention);
        // The glass is unchanged.
        let glass = super::explain_transfer_failure(busy(), HandlingStatus::default(), false, 0, 0);
        assert_eq!(glass.to_string(), busy().to_string());
        assert_eq!(kind(glass), K::Busy);
        assert_eq!(super::interrupt_kind(BUSY_OR_FEEDER_EMPTY), K::BusyOrFeederEmpty);
        assert_eq!(super::interrupt_kind("Example: The scanner is busy, or the feeder has no paper."), K::BusyOrFeederEmpty);
    }

    #[test]
    fn attention_alone_never_blocks_but_explains_a_failure() {
        let attention = HandlingStatus::from_bits(0x400 | 0x3);
        assert_eq!(attention.acquire_block_reason(), None);
        // A generic failure with the attention bit set (door interlock, full store) says so.
        let generic = ScanError::Failed(super::describe_windows_error(0x8000_4005, ""));
        let explained = super::explain_transfer_failure(generic, attention, true, 0, 3);
        assert_eq!(super::interrupt_kind(&explained.to_string()), super::InterruptKind::Attention);
        assert!(!super::retry_with_file_transfer(&explained));
        // Without the bit the failure stays generic.
        let plain = ScanError::Failed(super::describe_windows_error(0x8000_4005, ""));
        assert!(super::generic_scan_failure(&super::explain_transfer_failure(plain, HandlingStatus::from_bits(0x3), true, 0, 3)));
        // A finished whole-stack scan with the bit set (paused) still ends normally.
        let ended = ScanError::Failed("The scan failed (0x80004005).".into());
        assert!(super::explain_transfer_failure(ended, attention, true, 2, 0).to_string().contains("no paper"));
    }

    #[test]
    fn auto_falls_back_to_the_glass_only_for_an_empty_feeder_with_no_pages() {
        use super::{Acquired, BUSY_OR_FEEDER_EMPTY, auto_fallback_to_glass as fallback, wia_error_message};
        let failed = |text: &str| ScanError::Failed(text.into());
        let busy_empty = failed(BUSY_OR_FEEDER_EMPTY);
        let empty = failed(wia_error_message(0x8021_0003).unwrap_or_default());
        let (auto, feeder, glass) = (Source::Auto, Source::Feeder, Source::Glass);
        // Auto resolved to the feeder, glass exists, nothing arrived, empty-feeder classification.
        assert!(fallback(auto, feeder, true, 0, &busy_empty));
        assert!(fallback(auto, feeder, true, 0, &empty));
        assert!(fallback(auto, feeder, true, 0, &failed("the feeder is empty")));
        assert!(fallback(auto, feeder, true, 0, &failed("Example: The scanner is busy, or the feeder has no paper.")));
        // The user chose the feeder or the glass.
        assert!(!fallback(feeder, feeder, true, 0, &busy_empty));
        assert!(!fallback(glass, glass, true, 0, &busy_empty));
        // Auto already resolved to the glass.
        assert!(!fallback(auto, glass, true, 0, &busy_empty));
        // No glass to fall back to.
        assert!(!fallback(auto, feeder, false, 0, &busy_empty));
        // Pages arrived: a shortfall stays an error.
        assert!(!fallback(auto, feeder, true, 2, &busy_empty));
        let incomplete = ScanError::Incomplete {
            message: "the feeder reported no more paper.".into(),
            acquired: Acquired {
                source: Source::Feeder,
                pages: vec![Page { bytes: Vec::new(), format: "bmp", width: 1, height: 1, dpi: 1, thresholded: false }],
            },
            cause: None,
        };
        assert_eq!(super::pages_in_error(&incomplete), 1);
        assert_eq!(super::pages_in_error(&busy_empty), 0);
        assert!(!fallback(auto, feeder, true, super::pages_in_error(&incomplete), &incomplete));
        // A named fault, a plain busy, offline, or an unknown failure.
        for code in [0x8021_0002, 0x8021_0004, 0x8021_0010, 0x8021_0014, 0x8021_0008, 0x8021_0011, 0x8021_0005, 0x8021_0006] {
            let text = wia_error_message(code).unwrap_or_default();
            assert!(!fallback(auto, feeder, true, 0, &failed(text)), "{text}");
        }
        assert!(!fallback(auto, feeder, true, 0, &failed("the scan was cancelled")));
        assert!(!fallback(auto, feeder, true, 0, &ScanError::Unsupported));
    }

    #[test]
    fn progress_token_reports_the_source_in_use() {
        let token = ProgressToken::new();
        assert_eq!(token.source(), None);
        token.set_source(Source::Feeder);
        assert_eq!(token.source(), Some(Source::Feeder));
        // A clone shares it, as the dialog's copy does; the fallback flips it to the glass.
        let seen = token.clone();
        token.set_source(Source::Glass);
        assert_eq!(seen.source(), Some(Source::Glass));
        token.set_source(Source::Auto);
        assert_eq!(seen.source(), None);
    }
}
