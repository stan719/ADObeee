//! The scan contract: every enum and error identity a binding may expose, defined once.
//!
//! Language bindings (CLI, MCP tools, Python, the harness) must not invent their own enums or
//! parse prose. They list a type's `ALL`, spell a value with `as_str`, read one back with
//! `FromStr`/`parse`, and take an error's identity from [`ScanErrorInfo`]. The wire strings are
//! stable lowercase snake_case (`business-card` keeps its hyphen: it is a page-size name the CLI
//! has always taken).
//!
//! Adding a variant means: add it to the type, to its `ALL`, and to `as_str`; the exhaustive
//! `match` in `as_str` fails to compile until it is named, and `contract_json` (read by the Python
//! drift test) carries the new value to every binding.

use std::fmt;
use std::str::FromStr;

use crate::{Color, Connection, HandlingStatus, Intent, InterruptKind, PageSize, ProgressToken, ScanError, ScanSettings, Source, pages_in_error};

/// A wire string that names no value of the enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseWireError {
    /// The enum that was being read (`"source"`, `"color"`, ...).
    pub what: &'static str,
    /// The text that matched nothing.
    pub got: String,
}

impl fmt::Display for ParseWireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown {} {:?}", self.what, self.got)
    }
}

impl std::error::Error for ParseWireError {}

macro_rules! wire_text {
    ($ty:ident, $what:literal) => {
        impl FromStr for $ty {
            type Err = ParseWireError;

            fn from_str(text: &str) -> Result<Self, ParseWireError> {
                Self::parse(text).ok_or_else(|| ParseWireError { what: $what, got: text.to_string() })
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

// ---- enums defined elsewhere in the crate: list and spell them here ------------------------------

impl Source {
    /// Every source, in the order a UI lists them.
    pub const ALL: &'static [Source] = &[Source::Auto, Source::Feeder, Source::Glass];

    /// Wire string: `auto`, `feeder`, `glass` (the same as [`Self::name`]).
    pub fn as_str(self) -> &'static str {
        self.name()
    }
}
wire_text!(Source, "source");

impl Color {
    /// Every color mode, in the order a UI lists them.
    pub const ALL: &'static [Color] = &[Color::Gray, Color::Rgb, Color::BlackAndWhite];

    /// Wire string: `rgb`, `gray`, `bw` (the same as [`Self::name`]).
    pub fn as_str(self) -> &'static str {
        self.name()
    }

    /// Read a wire string; `grey` and `color` are accepted as the CLI always has.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "gray" | "grey" => Some(Self::Gray),
            "rgb" | "color" => Some(Self::Rgb),
            "bw" => Some(Self::BlackAndWhite),
            _ => None,
        }
    }
}
wire_text!(Color, "color");

impl Intent {
    /// Every intent preset.
    pub const ALL: &'static [Intent] =
        &[Intent::None, Intent::Color, Intent::Gray, Intent::Text, Intent::BestPreview, Intent::MaximizeQuality, Intent::MinimizeSize];

    /// Wire string: `none`, `color`, `gray`, `text`, `preview`, `quality`, `size`.
    pub fn as_str(self) -> &'static str {
        self.name()
    }
}
wire_text!(Intent, "intent");

impl PageSize {
    /// Every named page size.
    pub const ALL: &'static [PageSize] = &[PageSize::A4, PageSize::Letter, PageSize::Legal, PageSize::Auto, PageSize::Custom, PageSize::BusinessCard];

    /// Wire string: `a4`, `letter`, `legal`, `auto`, `custom`, `business-card`.
    pub fn as_str(self) -> &'static str {
        self.name()
    }
}
wire_text!(PageSize, "page size");

impl Connection {
    /// Every connection class.
    pub const ALL: &'static [Connection] = &[Connection::Wsd, Connection::Usb, Connection::Unknown];

    /// Wire string: `wsd`, `usb`, `unknown`.
    pub fn as_str(self) -> &'static str {
        self.name()
    }

    /// Read a wire string (not a port: see [`Self::from_port`]).
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|value| value.as_str() == text)
    }
}
wire_text!(Connection, "connection");

impl InterruptKind {
    /// Every kind, in declaration order.
    pub const ALL: &'static [InterruptKind] = &[
        InterruptKind::CoverOpen,
        InterruptKind::Jam,
        InterruptKind::PaperProblem,
        InterruptKind::FeederEmpty,
        InterruptKind::MultiFeed,
        InterruptKind::Offline,
        InterruptKind::Attention,
        InterruptKind::Busy,
        InterruptKind::BusyOrFeederEmpty,
        InterruptKind::DeviceError,
        InterruptKind::Other,
    ];

    /// Wire string (`cover_open`, `jam`, `paper_problem`, `feeder_empty`, `multi_feed`, `offline`,
    /// `attention`, `busy`, `busy_or_feeder_empty`, `device_error`, `other`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoverOpen => "cover_open",
            Self::Jam => "jam",
            Self::PaperProblem => "paper_problem",
            Self::FeederEmpty => "feeder_empty",
            Self::MultiFeed => "multi_feed",
            Self::Offline => "offline",
            Self::Attention => "attention",
            Self::Busy => "busy",
            Self::BusyOrFeederEmpty => "busy_or_feeder_empty",
            Self::DeviceError => "device_error",
            Self::Other => "other",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|value| value.as_str() == text)
    }
}
wire_text!(InterruptKind, "interrupt kind");

// ---- enums that only exist here -------------------------------------------------------------------

/// An image format a caller may ask the driver for (`ScanSettings::format`). Pages themselves are
/// labelled by [`crate::image_format`], which can also answer `bin` for bytes it does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Bmp,
    Jpeg,
    Png,
    Tiff,
    Jpeg2k,
}

impl ImageFormat {
    pub const ALL: &'static [ImageFormat] = &[ImageFormat::Bmp, ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::Tiff, ImageFormat::Jpeg2k];

    /// Wire string: `bmp`, `jpeg`, `png`, `tiff`, `jpeg2k`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bmp => "bmp",
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Tiff => "tiff",
            Self::Jpeg2k => "jpeg2k",
        }
    }

    /// Read a name or extension: `jpg`, `tif`, `jp2` and a leading dot are accepted.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().trim_start_matches('.').to_ascii_lowercase().as_str() {
            "bmp" => Some(Self::Bmp),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "tiff" | "tif" => Some(Self::Tiff),
            "jpeg2k" | "jp2" => Some(Self::Jpeg2k),
            _ => None,
        }
    }
}
wire_text!(ImageFormat, "image format");

/// A compression the driver may offer (`ScanSettings::compression`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Compression {
    None,
    Rle4,
    Rle8,
    G3,
    G4,
    Jpeg,
    Jbig,
    Jpeg2k,
    Png,
}

impl Compression {
    pub const ALL: &'static [Compression] = &[
        Compression::None,
        Compression::Rle4,
        Compression::Rle8,
        Compression::G3,
        Compression::G4,
        Compression::Jpeg,
        Compression::Jbig,
        Compression::Jpeg2k,
        Compression::Png,
    ];

    /// Wire string: `none`, `rle4`, `rle8`, `g3`, `g4`, `jpeg`, `jbig`, `jpeg2k`, `png`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rle4 => "rle4",
            Self::Rle8 => "rle8",
            Self::G3 => "g3",
            Self::G4 => "g4",
            Self::Jpeg => "jpeg",
            Self::Jbig => "jbig",
            Self::Jpeg2k => "jpeg2k",
            Self::Png => "png",
        }
    }

    /// Read a name; `jpg` and `jp2` are accepted.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "rle4" => Some(Self::Rle4),
            "rle8" => Some(Self::Rle8),
            "g3" => Some(Self::G3),
            "g4" => Some(Self::G4),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "jbig" => Some(Self::Jbig),
            "jpeg2k" | "jp2" => Some(Self::Jpeg2k),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}
wire_text!(Compression, "compression");

/// The `details` field of a scanner listing row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListDetails {
    /// The device was listed and not opened.
    Skipped,
    /// The device was opened and its capabilities read.
    Loaded,
    /// The device would not open; the row carries the reason.
    Error,
    /// The device is named in `PDFCRAFT_SCAN_DENY` and was never opened.
    Denied,
}

impl ListDetails {
    pub const ALL: &'static [ListDetails] = &[ListDetails::Skipped, ListDetails::Loaded, ListDetails::Error, ListDetails::Denied];

    /// Wire string: `skipped`, `loaded`, `error`, `denied`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Loaded => "loaded",
            Self::Error => "error",
            Self::Denied => "denied",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|value| value.as_str() == text)
    }
}
wire_text!(ListDetails, "list details");

/// Where in a scan a failure happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// No page had arrived, and none was in flight.
    BeforeFirstPage,
    /// At least one page had arrived and no page was part-way across.
    BetweenPages,
    /// A page was part-way across (bytes received, page not finished).
    MidPage,
}

impl Phase {
    pub const ALL: &'static [Phase] = &[Phase::BeforeFirstPage, Phase::BetweenPages, Phase::MidPage];

    /// Wire string: `before_first_page`, `between_pages`, `mid_page`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeFirstPage => "before_first_page",
            Self::BetweenPages => "between_pages",
            Self::MidPage => "mid_page",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|value| value.as_str() == text)
    }

    /// The phase for what is known at the moment of failure.
    ///
    /// `mid_page` is true when bytes of a page had arrived and the page had not finished; it is
    /// only knowable when a [`ProgressToken`] watched the transfer ([`ProgressToken::in_page`]).
    /// Without one, a failure after pages arrived reads as [`Self::BetweenPages`]: a part-way page
    /// is not told apart from a clean gap.
    pub fn classify(pages_received: usize, mid_page: bool) -> Self {
        if mid_page {
            Self::MidPage
        } else if pages_received > 0 {
            Self::BetweenPages
        } else {
            Self::BeforeFirstPage
        }
    }
}
wire_text!(Phase, "phase");

// ---- recovery --------------------------------------------------------------------------------------

/// What a caller can do about a stopped scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Recovery {
    /// Nothing was kept and every wanted page is still wanted: start again with the same settings.
    pub retryable: bool,
    /// Pages were kept and some are still wanted: scan on from where it stopped.
    pub resumable: bool,
}

/// The one rule behind "Continue" versus "Try again" (the desktop dialog and every binding use it).
///
/// `pages_expected` is the wanted page count, `None` when the whole feeder was asked for. With
/// nothing left to scan neither flag is set. Otherwise: pages kept means resumable, none kept
/// means retryable. The failure's kind and phase choose the *wording* of the advice, not the
/// action, so they are not inputs.
pub fn recovery(pages_received: usize, pages_expected: Option<u32>) -> Recovery {
    let received = u32::try_from(pages_received).unwrap_or(u32::MAX);
    let wanted_more = pages_expected.is_none_or(|expected| expected > received);
    Recovery { retryable: wanted_more && pages_received == 0, resumable: wanted_more && pages_received > 0 }
}

// ---- WIA codes ---------------------------------------------------------------------------------------

/// HRESULTs with a name Microsoft documents (the WIA error code list), by that exact name. Codes the
/// engine classifies but Microsoft does not list (0x80210010 to 0x80210014: the Windows WSD driver was
/// measured returning 0x80210010 for a cover and 0x80210011 for the lamp) have no name here; they keep
/// their kind in the kind table and their hresult, and `code_name` is `None`. `E_FAIL` and `E_INVALIDARG`
/// are the standard Windows names.
const CODE_NAMES: [(u32, &str); 22] = [
    (0x8021_0001, "WIA_ERROR_GENERAL_ERROR"),
    (0x8021_0002, "WIA_ERROR_PAPER_JAM"),
    (0x8021_0003, "WIA_ERROR_PAPER_EMPTY"),
    (0x8021_0004, "WIA_ERROR_PAPER_PROBLEM"),
    (0x8021_0005, "WIA_ERROR_OFFLINE"),
    (0x8021_0006, "WIA_ERROR_BUSY"),
    (0x8021_0007, "WIA_ERROR_WARMING_UP"),
    (0x8021_0008, "WIA_ERROR_USER_INTERVENTION"),
    (0x8021_0009, "WIA_ERROR_ITEM_DELETED"),
    (0x8021_000A, "WIA_ERROR_DEVICE_COMMUNICATION"),
    (0x8021_000B, "WIA_ERROR_INVALID_COMMAND"),
    (0x8021_000C, "WIA_ERROR_INCORRECT_HARDWARE_SETTING"),
    (0x8021_000D, "WIA_ERROR_DEVICE_LOCKED"),
    (0x8021_000E, "WIA_ERROR_EXCEPTION_IN_DRIVER"),
    (0x8021_000F, "WIA_ERROR_INVALID_DRIVER_RESPONSE"),
    (0x8021_0015, "WIA_S_NO_DEVICE_AVAILABLE"),
    (0x8021_0016, "WIA_ERROR_COVER_OPEN"),
    (0x8021_0017, "WIA_ERROR_LAMP_OFF"),
    (0x8021_0020, "WIA_ERROR_MULTI_FEED"),
    (0x8021_0021, "WIA_ERROR_MAXIMUM_PRINTER_ENDORSER_COUNTER"),
    (0x8000_4005, "E_FAIL"),
    (0x8007_0057, "E_INVALIDARG"),
];

/// Symbolic name of an HRESULT the engine knows (`WIA_ERROR_BUSY`), else `None`.
pub fn code_name(code: u32) -> Option<&'static str> {
    CODE_NAMES.iter().find(|(known, _)| *known == code).map(|(_, name)| *name)
}

/// Every HRESULT with a name, as `(code, name)`.
pub fn code_names() -> &'static [(u32, &'static str)] {
    &CODE_NAMES
}

/// The kind an HRESULT stands for, when the engine classifies it. `None` for a code it does not
/// (including `E_FAIL`, which names no cause).
pub fn kind_for_hresult(code: u32) -> Option<InterruptKind> {
    crate::KIND_BY_CODE.iter().find(|(known, _)| *known == code).map(|(_, kind)| *kind)
}

// ---- HandlingStatus ---------------------------------------------------------------------------------

impl HandlingStatus {
    /// Every flag, by wire name, in bit order.
    pub const FLAG_NAMES: [&'static str; 9] =
        ["feeder_ready", "flat_ready", "duplex_ready", "cover_open", "path_cover_open", "jam", "multiple_feed", "attention", "lamp_error"];

    /// The flags that mean the scanner is blocked or abnormal (a ready bit is not one).
    pub const CONDITION_NAMES: [&'static str; 6] = ["jam", "cover_open", "path_cover_open", "multiple_feed", "attention", "lamp_error"];

    /// All nine flags as `(name, set)`, in [`Self::FLAG_NAMES`] order.
    pub fn flags(&self) -> [(&'static str, bool); 9] {
        [
            ("feeder_ready", self.feeder_ready),
            ("flat_ready", self.flat_ready),
            ("duplex_ready", self.duplex_ready),
            ("cover_open", self.cover_open),
            ("path_cover_open", self.path_cover_open),
            ("jam", self.jam),
            ("multiple_feed", self.multiple_feed),
            ("attention", self.attention),
            ("lamp_error", self.lamp_error),
        ]
    }

    /// Names of the blocking or abnormal flags that are set, in [`Self::CONDITION_NAMES`] order.
    pub fn conditions(&self) -> Vec<&'static str> {
        let set = self.flags();
        Self::CONDITION_NAMES.iter().copied().filter(|name| set.iter().any(|(flag, on)| flag == name && *on)).collect()
    }
}

// ---- structured error -------------------------------------------------------------------------------

/// What is known about a failure besides the error itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ErrorContext {
    /// Pages that had arrived (an `Incomplete` error's own pages count too; the larger wins).
    pub pages_received: usize,
    /// Pages wanted; `None` for the whole feeder (and for hardware duplex, which counts sides).
    pub pages_expected: Option<u32>,
    /// A page was part-way across ([`ProgressToken::in_page`]).
    pub mid_page: bool,
}

impl ErrorContext {
    /// The context of a failed `acquire`: page counts from the settings and, when the
    /// settings carry a progress token, how far the transfer had got.
    pub fn from_settings(settings: &ScanSettings) -> Self {
        let progress = settings.progress.as_ref();
        Self {
            pages_received: progress.map_or(0, ProgressToken::pages_done),
            pages_expected: (settings.pages > 0 && !settings.duplex).then_some(settings.pages),
            mid_page: progress.is_some_and(ProgressToken::in_page),
        }
    }
}

/// A [`ScanError`] as data: the identity a binding reports, without parsing the sentence.
///
/// `ScanError`'s `Display` text is unchanged; this sits beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanErrorInfo {
    /// The HRESULT the driver returned, when the engine saw one. A sentence the engine decided on
    /// from the status bits (a jam, an empty tray) keeps the HRESULT that triggered it.
    pub hresult: Option<u32>,
    /// [`code_name`] of `hresult` (for example `WIA_ERROR_BUSY`).
    pub code_name: Option<&'static str>,
    /// The cause. Taken from the structured source (the engine's own decision at the point it built
    /// the error); only an error with no HRESULT behind it falls back to its sentence.
    pub kind: InterruptKind,
    pub phase: Phase,
    /// Nothing was kept: try again with the same settings.
    pub retryable: bool,
    /// Pages were kept: scan on from where it stopped.
    pub resumable: bool,
    /// The same text as the error's `Display`.
    pub message: String,
}

impl ScanErrorInfo {
    pub fn new(error: &ScanError, context: ErrorContext) -> Self {
        let pages = context.pages_received.max(pages_in_error(error));
        let phase = Phase::classify(pages, context.mid_page);
        // Scanning that cannot run at all is not a thing to retry.
        let recoverable = !matches!(error, ScanError::Unsupported);
        let recover = if recoverable { recovery(pages, context.pages_expected) } else { Recovery::default() };
        let hresult = error.hresult();
        Self {
            hresult,
            code_name: hresult.and_then(code_name),
            kind: error.interrupt_kind(),
            phase,
            retryable: recover.retryable,
            resumable: recover.resumable,
            message: error.to_string(),
        }
    }

    /// [`Self::new`] with the context of a failed acquire taken from its settings.
    pub fn from_acquire(error: &ScanError, settings: &ScanSettings) -> Self {
        Self::new(error, ErrorContext::from_settings(settings))
    }

    /// `hresult` spelled `0x80210006`, the way the CLI and the tools print it.
    pub fn hresult_hex(&self) -> Option<String> {
        self.hresult.map(|code| format!("{code:#010x}"))
    }
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl ScanError {
    /// The structured identity of this error. See [`ScanErrorInfo`].
    pub fn info(&self, context: ErrorContext) -> ScanErrorInfo {
        ScanErrorInfo::new(self, context)
    }

    /// The HRESULT behind this error, when the engine saw one.
    pub fn hresult(&self) -> Option<u32> {
        self.cause().map(|cause| cause.code)
    }

    /// The symbolic name of [`Self::hresult`].
    pub fn code_name(&self) -> Option<&'static str> {
        self.hresult().and_then(code_name)
    }
}

impl ProgressToken {
    /// A page is part-way across: bytes of it have arrived and it has not finished.
    pub fn in_page(&self) -> bool {
        self.bytes_received() > 0
    }
}

// ---- one JSON dump of every list (read by the binding drift tests) ------------------------------------

fn list_json(values: impl Iterator<Item = &'static str>) -> String {
    let items: Vec<String> = values.map(json_string).collect();
    format!("[{}]", items.join(","))
}

/// Every list a binding exposes, as one JSON object. Keys: `kinds`, `phases`, `sources`, `colors`,
/// `intents`, `page_sizes`, `formats`, `compressions`, `connections`, `list_details`,
/// `handling_flags`, `conditions`, and `hresults` (hex string -> symbolic name).
pub fn contract_json() -> String {
    let hresults: Vec<String> =
        CODE_NAMES.iter().map(|(code, name)| format!("{}:{}", json_string(&format!("{code:#010x}")), json_string(name))).collect();
    format!(
        "{{\"kinds\":{},\"phases\":{},\"sources\":{},\"colors\":{},\"intents\":{},\"page_sizes\":{},\"formats\":{},\"compressions\":{},\"connections\":{},\"list_details\":{},\"handling_flags\":{},\"conditions\":{},\"hresults\":{{{}}}}}",
        list_json(InterruptKind::ALL.iter().map(|v| v.as_str())),
        list_json(Phase::ALL.iter().map(|v| v.as_str())),
        list_json(Source::ALL.iter().map(|v| v.as_str())),
        list_json(Color::ALL.iter().map(|v| v.as_str())),
        list_json(Intent::ALL.iter().map(|v| v.as_str())),
        list_json(PageSize::ALL.iter().map(|v| v.as_str())),
        list_json(ImageFormat::ALL.iter().map(|v| v.as_str())),
        list_json(Compression::ALL.iter().map(|v| v.as_str())),
        list_json(Connection::ALL.iter().map(|v| v.as_str())),
        list_json(ListDetails::ALL.iter().map(|v| v.as_str())),
        list_json(HandlingStatus::FLAG_NAMES.iter().copied()),
        list_json(HandlingStatus::CONDITION_NAMES.iter().copied()),
        hresults.join(","),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{interrupt_kind, wia_error_message};

    fn round_trips<T: Copy + PartialEq + fmt::Debug + FromStr<Err = ParseWireError> + fmt::Display>(all: &[T], as_str: fn(T) -> &'static str) {
        let mut seen = Vec::new();
        for value in all {
            let text = as_str(*value);
            assert!(!text.is_empty() && !seen.contains(&text), "{value:?}: duplicate or empty wire string {text:?}");
            seen.push(text);
            assert_eq!(text.parse::<T>().ok(), Some(*value), "{text}");
            assert_eq!(value.to_string(), text);
        }
        assert!("definitely-not-a-value".parse::<T>().is_err());
    }

    #[test]
    fn every_enum_round_trips_through_its_wire_string() {
        round_trips(Source::ALL, Source::as_str);
        round_trips(Color::ALL, Color::as_str);
        round_trips(Intent::ALL, Intent::as_str);
        round_trips(PageSize::ALL, PageSize::as_str);
        round_trips(Connection::ALL, Connection::as_str);
        round_trips(InterruptKind::ALL, InterruptKind::as_str);
        round_trips(ImageFormat::ALL, ImageFormat::as_str);
        round_trips(Compression::ALL, Compression::as_str);
        round_trips(ListDetails::ALL, ListDetails::as_str);
        round_trips(Phase::ALL, Phase::as_str);
    }

    #[test]
    fn the_wire_strings_are_the_ones_the_bindings_have_always_used() {
        let names = |all: Vec<&'static str>| all.join(",");
        assert_eq!(names(Source::ALL.iter().map(|v| v.as_str()).collect()), "auto,feeder,glass");
        assert_eq!(names(Color::ALL.iter().map(|v| v.as_str()).collect()), "gray,rgb,bw");
        assert_eq!(names(Intent::ALL.iter().map(|v| v.as_str()).collect()), "none,color,gray,text,preview,quality,size");
        assert_eq!(names(PageSize::ALL.iter().map(|v| v.as_str()).collect()), "a4,letter,legal,auto,custom,business-card");
        assert_eq!(names(Connection::ALL.iter().map(|v| v.as_str()).collect()), "wsd,usb,unknown");
        assert_eq!(
            names(InterruptKind::ALL.iter().map(|v| v.as_str()).collect()),
            "cover_open,jam,paper_problem,feeder_empty,multi_feed,offline,attention,busy,busy_or_feeder_empty,device_error,other"
        );
        assert_eq!(names(ListDetails::ALL.iter().map(|v| v.as_str()).collect()), "skipped,loaded,error,denied");
        assert_eq!(names(Phase::ALL.iter().map(|v| v.as_str()).collect()), "before_first_page,between_pages,mid_page");
    }

    #[test]
    fn the_aliases_the_cli_accepts_still_parse() {
        assert_eq!(Source::parse("flatbed"), Some(Source::Glass));
        assert_eq!(Color::parse("grey"), Some(Color::Gray));
        assert_eq!(Color::parse("color"), Some(Color::Rgb));
        assert_eq!(Intent::parse("ocr"), Some(Intent::Text));
        assert_eq!(PageSize::parse("card"), Some(PageSize::BusinessCard));
        assert_eq!(ImageFormat::parse(".JPG"), Some(ImageFormat::Jpeg));
        assert_eq!(Compression::parse("jp2"), Some(Compression::Jpeg2k));
    }

    #[test]
    fn handling_status_names_all_nine_flags_and_lists_only_the_abnormal_ones() {
        let all = HandlingStatus::from_bits(0xE3F);
        assert!(all.flags().iter().all(|(_, on)| *on));
        assert_eq!(all.flags().map(|(name, _)| name), HandlingStatus::FLAG_NAMES);
        assert_eq!(all.conditions(), ["jam", "cover_open", "path_cover_open", "multiple_feed", "attention", "lamp_error"]);
        let ready = HandlingStatus::from_bits(0x1 | 0x2 | 0x4);
        assert!(ready.conditions().is_empty());
        assert_eq!(HandlingStatus::from_bits(0x20).conditions(), ["jam"]);
        for name in HandlingStatus::CONDITION_NAMES {
            assert!(HandlingStatus::FLAG_NAMES.contains(&name));
        }
    }

    #[test]
    fn the_recovery_rule_is_continue_with_pages_and_try_again_without() {
        assert_eq!(recovery(0, Some(3)), Recovery { retryable: true, resumable: false });
        assert_eq!(recovery(0, None), Recovery { retryable: true, resumable: false });
        assert_eq!(recovery(2, Some(3)), Recovery { retryable: false, resumable: true });
        assert_eq!(recovery(2, None), Recovery { retryable: false, resumable: true });
        assert_eq!(recovery(3, Some(3)), Recovery::default());
        assert_eq!(recovery(5, Some(3)), Recovery::default());
    }

    #[test]
    fn the_phase_follows_pages_and_the_page_in_flight() {
        assert_eq!(Phase::classify(0, false), Phase::BeforeFirstPage);
        assert_eq!(Phase::classify(2, false), Phase::BetweenPages);
        assert_eq!(Phase::classify(0, true), Phase::MidPage);
        assert_eq!(Phase::classify(2, true), Phase::MidPage);
    }

    #[test]
    fn a_hresult_error_carries_its_code_name_and_kind_without_reading_the_sentence() {
        let error = ScanError::from_hresult(0x8021_0006, "some driver text that names nothing");
        let info = error.info(ErrorContext::default());
        assert_eq!(info.hresult, Some(0x8021_0006));
        assert_eq!(info.code_name, Some("WIA_ERROR_BUSY"));
        assert_eq!(info.kind, InterruptKind::Busy);
        assert_eq!(info.phase, Phase::BeforeFirstPage);
        assert!(info.retryable && !info.resumable);
        assert_eq!(info.message, error.to_string());
        assert_eq!(info.hresult_hex().as_deref(), Some("0x80210006"));
    }

    #[test]
    fn a_sentence_the_engine_chose_keeps_the_driver_code_and_its_own_kind() {
        let busy = ScanError::from_hresult(0x8021_0006, "");
        let explained = crate::explain_transfer_failure(busy, HandlingStatus::default(), true, 0, 1);
        let info = explained.info(ErrorContext::default());
        assert_eq!(info.kind, InterruptKind::BusyOrFeederEmpty);
        assert_eq!(info.code_name, Some("WIA_ERROR_BUSY"));
        let jam = crate::explain_transfer_failure(ScanError::from_hresult(0x8021_0004, ""), HandlingStatus::from_bits(0x20), true, 0, 1);
        assert_eq!(jam.info(ErrorContext::default()).kind, InterruptKind::Jam);
        assert_eq!(jam.hresult(), Some(0x8021_0004));
    }

    #[test]
    fn an_error_with_no_hresult_falls_back_to_its_sentence() {
        let error = ScanError::Failed("Only 1 of 3 pages were scanned: the feeder reported no more paper.".into());
        let info = error.info(ErrorContext { pages_received: 1, pages_expected: Some(3), mid_page: false });
        assert_eq!((info.hresult, info.code_name), (None, None));
        assert_eq!(info.kind, InterruptKind::FeederEmpty);
        assert_eq!(info.phase, Phase::BetweenPages);
        assert!(!info.retryable && info.resumable);
        assert!(!ScanError::Unsupported.info(ErrorContext::default()).retryable);
    }

    #[test]
    fn the_phase_of_a_part_page_failure_is_mid_page() {
        let error = ScanError::from_hresult(0x8021_0002, "");
        let info = error.info(ErrorContext { pages_received: 1, pages_expected: Some(2), mid_page: true });
        assert_eq!(info.phase, Phase::MidPage);
        assert!(info.resumable);
    }

    #[test]
    fn the_json_dump_lists_every_enum() {
        let dump = contract_json();
        for kind in InterruptKind::ALL {
            assert!(dump.contains(&format!("\"{}\"", kind.as_str())), "{kind:?}");
        }
        for name in HandlingStatus::FLAG_NAMES {
            assert!(dump.contains(name));
        }
        assert!(dump.contains("\"0x80210006\":\"WIA_ERROR_BUSY\"") && !dump.contains("0x80210010"));
        assert!(dump.starts_with("{\"kinds\":[") && dump.ends_with('}'));
    }

    #[test]
    fn json_strings_escape_quotes_and_control_characters() {
        assert_eq!(json_string("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
    }

    #[test]
    fn only_documented_codes_have_a_name_and_the_names_are_microsofts() {
        let documented = [
            (0x8021_0001, "WIA_ERROR_GENERAL_ERROR"),
            (0x8021_0002, "WIA_ERROR_PAPER_JAM"),
            (0x8021_0003, "WIA_ERROR_PAPER_EMPTY"),
            (0x8021_0004, "WIA_ERROR_PAPER_PROBLEM"),
            (0x8021_0005, "WIA_ERROR_OFFLINE"),
            (0x8021_0006, "WIA_ERROR_BUSY"),
            (0x8021_0007, "WIA_ERROR_WARMING_UP"),
            (0x8021_0008, "WIA_ERROR_USER_INTERVENTION"),
            (0x8021_0009, "WIA_ERROR_ITEM_DELETED"),
            (0x8021_000A, "WIA_ERROR_DEVICE_COMMUNICATION"),
            (0x8021_000B, "WIA_ERROR_INVALID_COMMAND"),
            (0x8021_000C, "WIA_ERROR_INCORRECT_HARDWARE_SETTING"),
            (0x8021_000D, "WIA_ERROR_DEVICE_LOCKED"),
            (0x8021_000E, "WIA_ERROR_EXCEPTION_IN_DRIVER"),
            (0x8021_000F, "WIA_ERROR_INVALID_DRIVER_RESPONSE"),
            (0x8021_0015, "WIA_S_NO_DEVICE_AVAILABLE"),
            (0x8021_0016, "WIA_ERROR_COVER_OPEN"),
            (0x8021_0017, "WIA_ERROR_LAMP_OFF"),
            (0x8021_0020, "WIA_ERROR_MULTI_FEED"),
            (0x8021_0021, "WIA_ERROR_MAXIMUM_PRINTER_ENDORSER_COUNTER"),
        ];
        for (code, name) in documented {
            assert_eq!(code_name(code), Some(name), "{code:#x}");
            assert!(wia_error_message(code).is_some(), "{code:#x} has no sentence");
        }
        assert_eq!(code_names().len(), documented.len() + 2);
        // Driver-observed, undocumented: no name, but the kind and the sentence stay.
        for (code, kind) in
            [(0x8021_0010, InterruptKind::CoverOpen), (0x8021_0011, InterruptKind::DeviceError), (0x8021_0014, InterruptKind::MultiFeed)]
        {
            assert_eq!(code_name(code), None, "{code:#x}");
            let info = ScanError::from_hresult(code, "").info(ErrorContext::default());
            assert_eq!((info.hresult, info.code_name, info.kind), (Some(code), None, kind));
        }
        for (code, kind) in
            [(0x8021_0016, InterruptKind::CoverOpen), (0x8021_0017, InterruptKind::DeviceError), (0x8021_0020, InterruptKind::MultiFeed)]
        {
            let info = ScanError::from_hresult(code, "").info(ErrorContext::default());
            assert_eq!(info.kind, kind, "{code:#x}");
            assert!(info.code_name.is_some());
        }
        assert_eq!(code_name(0x8021_0016), Some("WIA_ERROR_COVER_OPEN"));
        assert_eq!(kind_for_hresult(0x8021_0015), Some(InterruptKind::Offline));
    }

    #[test]
    fn interrupt_kind_of_a_sentence_matches_the_code_it_came_from() {
        for (code, kind) in crate::KIND_BY_CODE {
            if let Some(sentence) = wia_error_message(code) {
                assert_eq!(interrupt_kind(sentence), kind, "{code:#x}");
                assert_eq!(kind_for_hresult(code), Some(kind));
            }
        }
    }
}
