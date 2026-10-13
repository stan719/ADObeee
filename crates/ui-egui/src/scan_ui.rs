//! Scan & OCR ▸ From scanner: bed or feeder, a preview, and a region.
//!
//! Listing scanners does not feed paper. [`PdfCraftApp::start_scan`] is the only path that calls
//! [`pdfcraft_platform::scan::acquire`], and it does that on a background thread.

use std::sync::{Arc, Mutex};

use egui::{Align, Color32, Layout, Pos2, Rect, Sense, Vec2};
use pdfcraft_engine::DocId;
use pdfcraft_platform::scan::{
    self, CancelToken, Color, Connection, HandlingStatus, ImageChoices, Intent, InterruptKind, Page, PageSize, ProgressToken, Region, Rotation,
    ScanSettings, Scanner, ScannerRef, Source, ValueSpan, estimated_bmp_bytes, interrupt_kind, preview_dpi, region_from_inches, region_from_preview,
    region_pixels,
};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, widgets};

/// What the dialog's buttons asked for this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanClick {
    None,
    Close,
    /// Stop an in-flight transfer without closing the dialog.
    Stop,
    Preview,
    Create,
    Insert,
    /// Second pass of manual duplex: reverse sides after the stack was flipped.
    ReverseSides,
}

#[derive(Clone, Debug)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    pub rgba: Vec<u8>,
}

/// Choices the dialog keeps. Auto (feeder if a sheet is waiting, else the bed) is the default.
#[derive(Clone, Debug)]
pub struct ScanDraft {
    pub device: String,
    pub source: Source,
    pub dpi: u32,
    pub dpi_y: Option<u32>,
    pub color: Color,
    pub pages: u32,
    pub brightness: Option<i32>,
    pub contrast: Option<i32>,
    pub threshold: Option<i32>,
    pub rotation: Option<Rotation>,
    pub format: Option<String>,
    pub compression: Option<String>,
    pub intent: Option<Intent>,
    pub page_size: Option<PageSize>,
    pub auto_deskew: Option<bool>,
    pub orientation: Option<i32>,
    pub photometric: Option<i32>,
    pub invert: Option<bool>,
    pub mirror: Option<bool>,
    pub lamp: Option<bool>,
    pub duplex: bool,
    /// Two-pass reverse-side scan when the feeder has no hardware duplex.
    pub manual_duplex: bool,
    /// Create PDF: `0` is one document; `N` opens a new document every `N` finished pages.
    pub pages_per_file: u32,
    pub left_in: f64,
    pub top_in: f64,
    pub width_in: f64,
    pub height_in: f64,
    pub preview: Option<PreviewImage>,
    /// Drag rectangle in preview pixels. The far edge is exclusive. `None` is the full area.
    pub selection: Option<(u32, u32, u32, u32)>,
    anchor: Option<(u32, u32)>,
    pub error: Option<String>,
    snapped_device: String,
    snapped_source: Source,
    /// Last bed/feeder [`effective_source`] used for snapping choices (never Auto after a snap).
    snapped_resolved: Source,
}

impl Default for ScanDraft {
    fn default() -> Self {
        Self {
            device: String::new(),
            source: Source::Auto,
            dpi: 300,
            dpi_y: None,
            color: Color::Gray,
            // Whole feeder (0). The bed still takes one page in final_settings.
            pages: 0,
            brightness: None,
            contrast: None,
            threshold: None,
            rotation: None,
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
            manual_duplex: false,
            pages_per_file: 0,
            left_in: 0.0,
            top_in: 0.0,
            width_in: 0.0,
            height_in: 0.0,
            preview: None,
            selection: None,
            anchor: None,
            error: None,
            snapped_device: String::new(),
            snapped_source: Source::Auto,
            snapped_resolved: Source::Auto,
        }
    }
}

impl ScanDraft {
    /// Bed or feeder. The preview belongs to the previous source, so it goes away.
    pub fn set_source(&mut self, source: Source) {
        if self.source == source {
            return;
        }
        self.source = source;
        // Feeder default is the whole stack; the bed is always one page at acquire time.
        self.pages = if source == Source::Glass { 1 } else { 0 };
        self.clear_preview();
        self.brightness = None;
        self.contrast = None;
        self.threshold = None;
        self.rotation = None;
        self.format = None;
        self.compression = None;
        self.intent = None;
        self.page_size = None;
        self.auto_deskew = None;
        self.orientation = None;
        self.photometric = None;
        self.invert = None;
        self.mirror = None;
        self.lamp = None;
        self.dpi_y = None;
        self.snapped_device.clear();
    }

    pub fn clear_preview(&mut self) {
        self.preview = None;
        self.clear_region();
    }

    /// Drop the drag and the inch fields. The preview stays; the next scan covers the whole area.
    pub fn clear_region(&mut self) {
        self.selection = None;
        self.anchor = None;
        self.left_in = 0.0;
        self.top_in = 0.0;
        self.width_in = 0.0;
        self.height_in = 0.0;
    }

    /// 300 dpi gray with text intent and neutral brightness and contrast, when offered.
    pub fn apply_text_ocr(&mut self, choices: &ImageChoices) {
        self.apply_preset(choices, Color::Gray, preferred_dpi(&choices.dpi), &[Intent::Text, Intent::Gray]);
    }

    /// 300 dpi colour with colour intent — mixed text and pictures on a page.
    pub fn apply_color_document(&mut self, choices: &ImageChoices) {
        self.apply_preset(choices, Color::Rgb, preferred_dpi(&choices.dpi), &[Intent::Color, Intent::MaximizeQuality]);
    }

    /// Higher-resolution colour for photographs, when the driver lists it.
    pub fn apply_photo(&mut self, choices: &ImageChoices) {
        self.apply_preset(choices, Color::Rgb, photo_dpi(&choices.dpi), &[Intent::MaximizeQuality, Intent::Color]);
    }

    /// 300 dpi black-and-white with text intent — line art and printed text.
    pub fn apply_black_and_white(&mut self, choices: &ImageChoices) {
        self.apply_preset(choices, Color::BlackAndWhite, preferred_dpi(&choices.dpi), &[Intent::Text, Intent::Gray]);
        if let Some(span) = choices.threshold {
            self.threshold = Some(if span.contains(128) { 128 } else { span.min });
        }
    }

    fn apply_preset(&mut self, choices: &ImageChoices, color: Color, dpi: Option<u32>, intents: &[Intent]) {
        if let Some(dpi) = dpi {
            self.dpi = dpi;
        }
        if choices.colors.contains(&color) {
            self.color = color;
        } else if let Some(fallback) = choices.colors.first().copied() {
            self.color = fallback;
        }
        self.intent = intents.iter().copied().find(|intent| choices.intents.contains(intent));
        self.brightness = choices.brightness.map(|span| if span.contains(0) { 0 } else { span.min });
        self.contrast = choices.contrast.map(|span| if span.contains(0) { 0 } else { span.min });
        self.threshold = None;
        self.rotation = if choices.rotation.contains(&Rotation::Portrait) { Some(Rotation::Portrait) } else { choices.rotation.first().copied() };
        self.clear_region();
    }

    /// The window to scan, in thousandths of an inch. A drag wins over the inch fields.
    /// An empty selection is the full area.
    pub fn region(&self) -> Option<Region> {
        if let (Some(preview), Some((x0, y0, x1, y1))) = (&self.preview, self.selection) {
            return region_from_preview(x0, y0, x1, y1, preview.dpi).ok().flatten();
        }
        if self.width_in <= 0.0 || self.height_in <= 0.0 {
            return None;
        }
        region_from_inches(self.left_in, self.top_in, self.width_in, self.height_in).ok()
    }

    fn remember_selection(&mut self, selection: Option<(u32, u32, u32, u32)>) {
        self.selection = selection.filter(|(x0, y0, x1, y1)| x1.saturating_sub(*x0) >= 2 && y1.saturating_sub(*y0) >= 2);
        if let Some(region) = self.region() {
            self.left_in = f64::from(region.left) / 1000.0;
            self.top_in = f64::from(region.top) / 1000.0;
            self.width_in = f64::from(region.width) / 1000.0;
            self.height_in = f64::from(region.height) / 1000.0;
        } else if self.selection.is_none() {
            self.left_in = 0.0;
            self.top_in = 0.0;
            self.width_in = 0.0;
            self.height_in = 0.0;
        }
    }
}

struct ListBox {
    generation: u64,
    result: Option<Result<Vec<ScannerRef>, String>>,
}

struct DetailsBox {
    generation: u64,
    id: String,
    result: Option<Result<Scanner, String>>,
}

/// What is known about the selected device's capabilities. Keyed by [`DeviceList::details_id`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum DetailsState {
    #[default]
    Idle,
    Loading,
    Ready(Box<Scanner>),
    /// The device could not be opened. Holds the engine's own sentence.
    Failed(String),
}

/// The device list (cheap, nothing is opened) and the details of the one selected device.
#[derive(Default)]
pub struct DeviceList {
    pub loading: bool,
    /// A list load has finished (successfully or not) since the dialog first asked.
    listed: bool,
    pub refs: Vec<ScannerRef>,
    pub error: Option<String>,
    details_id: String,
    details: DetailsState,
    generation: u64,
    details_generation: u64,
    slot: Option<Arc<Mutex<ListBox>>>,
    details_slot: Option<Arc<Mutex<DetailsBox>>>,
}

impl DeviceList {
    /// Start a list load. `None` while one is already running; otherwise its generation.
    fn begin_list(&mut self) -> Option<u64> {
        if self.loading {
            return None;
        }
        self.generation = self.generation.saturating_add(1);
        self.loading = true;
        self.error = None;
        Some(self.generation)
    }

    /// Apply a list result. A stale generation is ignored. Returns the device id the dialog should
    /// select now (`current` when it is still listed, else the first device, else empty).
    fn finish_list(&mut self, generation: u64, result: Result<Vec<ScannerRef>, String>, current: &str) -> Option<String> {
        if generation != self.generation {
            return None;
        }
        self.loading = false;
        self.listed = true;
        self.slot = None;
        match result {
            Ok(list) => {
                self.error = None;
                let keep = list.iter().any(|item| item.id == current);
                let chosen = if keep {
                    current.to_string()
                } else if current.is_empty() {
                    default_device_id(&list).unwrap_or_default()
                } else {
                    list.first().map(|item| item.id.clone()).unwrap_or_default()
                };
                self.refs = list;
                // A refresh also forgets old details, so the selected device is checked again.
                self.details = DetailsState::Idle;
                self.details_id.clear();
                self.details_slot = None;
                Some(chosen)
            }
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }

    /// The listed device for `id`, falling back to the first one.
    pub fn selected_ref(&self, id: &str) -> Option<&ScannerRef> {
        self.refs.iter().find(|item| item.id == id).or_else(|| self.refs.first())
    }

    /// Ask for details of `id`. Returns the generation when a load must start (new selection or
    /// nothing loaded yet); `None` when `id` is already loading, loaded or failed.
    fn want_details(&mut self, id: &str) -> Option<u64> {
        if id.is_empty() || (self.details_id == id && self.details != DetailsState::Idle) {
            return None;
        }
        self.details_id = id.to_string();
        self.details = DetailsState::Loading;
        self.details_generation = self.details_generation.saturating_add(1);
        Some(self.details_generation)
    }

    /// Load the failed device again.
    fn retry_details(&mut self) -> Option<u64> {
        if !matches!(self.details, DetailsState::Failed(_)) {
            return None;
        }
        self.details = DetailsState::Idle;
        let id = self.details_id.clone();
        self.want_details(&id)
    }

    /// Apply a details result. Ignored when the generation or id no longer match the selection.
    fn finish_details(&mut self, generation: u64, id: &str, result: Result<Scanner, String>) -> bool {
        if generation != self.details_generation || id != self.details_id {
            return false;
        }
        self.details_slot = None;
        self.details = match result {
            Ok(scanner) => DetailsState::Ready(Box::new(scanner)),
            Err(error) => DetailsState::Failed(error),
        };
        true
    }

    pub fn checking(&self) -> bool {
        self.details == DetailsState::Loading
    }

    pub fn details_error(&self) -> Option<&str> {
        match &self.details {
            DetailsState::Failed(error) => Some(error),
            _ => None,
        }
    }

    /// Full details, only once they were read for `item`.
    fn ready(&self, item: &ScannerRef) -> Option<&Scanner> {
        match &self.details {
            DetailsState::Ready(scanner) if scanner.id == item.id => Some(scanner.as_ref()),
            _ => None,
        }
    }

    /// [`Self::scanner_for`] the selected device.
    pub fn scanner_selected(&self, id: &str) -> Option<Scanner> {
        self.selected_ref(id).and_then(|item| self.scanner_for(item))
    }

    /// The scanner to draw and scan with: the real details, or conservative defaults when they
    /// could not be read (bed and feeder both offered, no ranges, no duplex). `None` while the
    /// details are still loading, so nothing is scanned with a guess that is about to be replaced.
    pub fn scanner_for(&self, item: &ScannerRef) -> Option<Scanner> {
        if let Some(scanner) = self.ready(item) {
            return Some(scanner.clone());
        }
        if self.details_id == item.id && matches!(self.details, DetailsState::Failed(_)) {
            return Some(Scanner {
                id: item.id.clone(),
                name: item.name.clone(),
                connection: item.connection,
                port: item.port.clone(),
                flatbed: true,
                feeder: true,
                duplex: false,
                feeder_ready: false,
                handling_status: HandlingStatus::default(),
                feeder_capacity: None,
                optical_x: None,
                optical_y: None,
                pages: None,
                bed: None,
                sheet: None,
                glass: None,
                feeder_choices: None,
            });
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanKind {
    Preview,
    Create,
    Insert(DocId),
    /// Reverse-side pass; fronts are already held in [`ScanUi::duplex`].
    ReverseSides,
}

/// Fronts of the first manual-duplex pass. They are already in the document (contiguous from
/// `first`); the reverse-side pass inserts each back after its front as it arrives.
#[derive(Clone, Copy, Debug)]
struct DuplexAwait {
    kind: ScanKind,
    /// Bed or feeder used for the fronts; the reverse pass uses the same source.
    source: Source,
    /// Document the fronts went into.
    doc: DocId,
    /// Page index of the first front in `doc`.
    first: usize,
    /// Fronts inserted so far.
    fronts: usize,
    /// Reverse sides inserted so far; fewer than the fronts after an interruption.
    backs: usize,
}

struct JobSlot {
    /// Set when [`scan::acquire`] returns. Create/Insert pages are applied earlier from [`ScanJob::live`].
    result: Option<Result<Vec<Page>, String>>,
}

pub struct ScanJob {
    kind: ScanKind,
    /// Document Create opened from the first finished page.
    create_doc: Option<DocId>,
    /// Pages already in [`Self::create_doc`] when splitting Create into multiple files.
    pages_in_create: u32,
    /// Next Create file index (`0` → Scan.pdf, `1` → Scan (2).pdf, …).
    create_file_index: u32,
    /// Create PDF split: `None` keeps one document; `Some(n)` starts a new one every `n` pages.
    pages_per_file: Option<u32>,
    /// Requested page count when known (`0` / whole feeder is unknown).
    expected: Option<u32>,
    /// Source locked for the first (fronts) pass of a manual duplex (never Auto).
    duplex_source: Option<Source>,
    /// Pages scanned for this scan so far, over every Continue.
    received: u32,
    /// Pages wanted in all, over every Continue; `None` for the whole feeder or hardware duplex.
    expected_total: Option<u32>,
    /// Insert: the page index the next scanned page goes to.
    insert_cursor: Option<usize>,
    /// Bed or feeder the job resolved to.
    source: Source,
    live: ProgressToken,
    progress: Arc<Mutex<JobSlot>>,
    pub cancel: CancelToken,
}

/// Working copy while the Advanced scanner settings modal is open.
#[derive(Clone, Debug)]
struct AdvancedEdit {
    draft: ScanDraft,
}

#[derive(Default)]
pub struct ScanUi {
    pub draft: ScanDraft,
    pub devices: DeviceList,
    pub job: Option<ScanJob>,
    advanced: Option<AdvancedEdit>,
    duplex: Option<DuplexAwait>,
    /// A scan that stopped early; it waits for Continue or Finish ([`crate::Dialog::ScanInterrupted`]).
    pub(crate) interrupted: Option<Interrupted>,
    /// Documents opened by the Create job that just finished (for the toast).
    last_create_files: u32,
}

/// Which pass of a scan an interruption cut short.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    /// Pages go straight into the document.
    Direct,
    /// Manual duplex, first pass: the fronts go into the document as they arrive.
    Fronts,
    /// Manual duplex, second pass: each back goes in after its front.
    Backs,
}

/// What the user can choose in the Scan interrupted dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InterruptAction {
    /// Scan on from where the job stopped.
    Continue,
    /// Nothing was scanned yet: start again with the same settings.
    TryAgain,
    /// Manual duplex: go to the flip step with the fronts scanned so far.
    ReverseNow,
    /// Keep what was scanned and close.
    Finish,
}

/// A scan that stopped early (cover open, jam, empty feeder, offline). It keeps what a Continue
/// needs: which document, where the next page goes, and how many pages are still wanted.
#[derive(Clone, Debug)]
pub(crate) struct Interrupted {
    kind: InterruptKind,
    /// The engine's own sentence.
    message: String,
    /// Pages scanned so far in this scan, over every Continue.
    received: u32,
    /// Pages wanted in all; `None` when the whole feeder was asked for.
    expected: Option<u32>,
    scan_kind: ScanKind,
    pass: Pass,
    /// Create: the document the first pages went into.
    doc: Option<DocId>,
    pages_in_create: u32,
    create_file_index: u32,
    pages_per_file: Option<u32>,
    /// Insert: the page index the next scanned page goes to.
    insert_at: Option<usize>,
    /// Bed or feeder the job used.
    source: Source,
}

/// What a Continue hands to the next acquire.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Resume {
    scan_kind: ScanKind,
    /// Pages still wanted when a fixed count was asked for.
    pages: Option<u32>,
    /// Pages already scanned before this leg.
    carried: u32,
    expected_total: Option<u32>,
    create_doc: Option<DocId>,
    pages_in_create: u32,
    create_file_index: u32,
    pages_per_file: Option<u32>,
    insert_at: Option<usize>,
    /// Source to keep when pages already came in (Auto would pick the bed once the tray is empty).
    source: Option<Source>,
    feeder: bool,
}

impl Interrupted {
    /// Pages still wanted: expected minus received, for a fixed count.
    fn remaining(&self) -> Option<u32> {
        self.expected.map(|expected| expected.saturating_sub(self.received))
    }

    fn has_pages(&self) -> bool {
        self.received > 0
    }

    /// The engine's rule for what can be done about this stop (`pdfcraft_scan::recovery`).
    fn recovery(&self) -> scan::Recovery {
        scan::recovery(usize::try_from(self.received).unwrap_or(usize::MAX), self.expected)
    }

    /// False once every wanted page is in: there is nothing left to continue with.
    #[cfg(test)]
    fn can_continue(&self) -> bool {
        let recovery = self.recovery();
        recovery.retryable || recovery.resumable
    }

    /// The primary action: Try again when nothing was scanned, otherwise Continue.
    fn primary(&self) -> Option<InterruptAction> {
        let recovery = self.recovery();
        if recovery.resumable {
            Some(InterruptAction::Continue)
        } else if recovery.retryable {
            Some(InterruptAction::TryAgain)
        } else {
            None
        }
    }

    /// The buttons, primary first.
    fn actions(&self) -> Vec<InterruptAction> {
        let mut actions: Vec<InterruptAction> = self.primary().into_iter().collect();
        if self.pass == Pass::Fronts && self.has_pages() {
            actions.push(InterruptAction::ReverseNow);
        }
        actions.push(InterruptAction::Finish);
        actions
    }

    fn click(&self) -> ScanClick {
        match (self.pass, self.scan_kind) {
            (Pass::Backs, _) => ScanClick::ReverseSides,
            (_, ScanKind::Insert(_)) => ScanClick::Insert,
            _ => ScanClick::Create,
        }
    }

    fn resume(&self) -> Resume {
        Resume {
            scan_kind: self.scan_kind,
            pages: self.remaining().filter(|left| *left > 0),
            carried: self.received,
            expected_total: self.expected,
            create_doc: self.doc,
            pages_in_create: self.pages_in_create,
            create_file_index: self.create_file_index,
            pages_per_file: self.pages_per_file,
            insert_at: self.insert_at,
            source: (self.received > 0 && self.source != Source::Auto).then_some(self.source),
            feeder: self.source == Source::Feeder,
        }
    }

    fn headline(&self) -> &'static str {
        match self.kind {
            InterruptKind::CoverOpen => tl!("A cover on the scanner is open."),
            InterruptKind::Jam => tl!("The scanner has a paper jam."),
            InterruptKind::PaperProblem => tl!("The scanner reported a paper problem (a jam or several sheets fed together)."),
            InterruptKind::FeederEmpty => tl!("The feeder ran out of paper."),
            InterruptKind::MultiFeed => tl!("The feeder took more than one sheet."),
            InterruptKind::Offline => tl!("The scanner is offline or needs attention."),
            InterruptKind::Attention => tl!("The scanner needs attention."),
            InterruptKind::Busy => tl!("The scanner is busy."),
            InterruptKind::BusyOrFeederEmpty => tl!("The scanner is busy, or the feeder has no paper."),
            InterruptKind::DeviceError => tl!("The scanner reported a lamp or device error."),
            InterruptKind::Other => tl!("The scan was interrupted."),
        }
    }

    /// "2 of 3 pages were scanned." (a fixed count) or "2 pages were scanned." (all pages).
    fn count_line(&self) -> String {
        let n = self.received.to_string();
        match (self.received, self.expected) {
            (0, _) => tl!("No pages were scanned.").to_string(),
            (_, Some(m)) => crate::i18n::fmt(tl!("{n} of {m} pages were scanned."), &[("n", &n), ("m", &m.to_string())]),
            (1, None) => tl!("1 page was scanned.").to_string(),
            (_, None) => crate::i18n::fmt(tl!("{n} pages were scanned."), &[("n", &n)]),
        }
    }

    /// Where the pages already scanned are.
    fn kept_line(&self) -> Option<&'static str> {
        if !self.has_pages() {
            return None;
        }
        Some(match self.pass {
            Pass::Direct => tl!("The pages already scanned were kept in the document."),
            Pass::Fronts => tl!("The fronts already scanned are kept for the reverse-side pass."),
            Pass::Backs => tl!("The reverse sides already scanned are kept."),
        })
    }

    /// The one instruction: kind-specific, or the engine's sentence when it named nothing known.
    fn instruction_text(&self) -> String {
        let retry = self.primary() == Some(InterruptAction::TryAgain);
        match instruction(self.kind, retry) {
            Some(text) => text.to_string(),
            None if self.pass == Pass::Backs && self.has_pages() => {
                format!("{} {}", self.message, tl!("Scan the missing reverse sides, then choose Finish."))
            }
            None => self.message.clone(),
        }
    }

    fn primary_label(&self) -> &'static str {
        match (self.primary(), self.pass) {
            (Some(InterruptAction::TryAgain), _) => tl!("Try again"),
            (_, Pass::Direct) => tl!("Continue scanning"),
            (_, Pass::Fronts) => tl!("Continue scanning fronts"),
            (_, Pass::Backs) => tl!("Continue scanning reverse sides"),
        }
    }

    fn reverse_now_label(&self) -> String {
        if self.received == 1 {
            tl!("Scan reverse side now (1 front)").to_string()
        } else {
            crate::i18n::fmt(tl!("Scan reverse sides now ({n} fronts)"), &[("n", &self.received.to_string())])
        }
    }
}

/// The instruction for a cause. `retry` is true when nothing was scanned, so the button reads
/// Try again. `None` for a cause the engine did not name: the dialog shows the engine's sentence.
fn instruction(kind: InterruptKind, retry: bool) -> Option<&'static str> {
    Some(match (kind, retry) {
        (InterruptKind::CoverOpen, false) => tl!("Close the feeder cover and check the paper, then choose Continue."),
        (InterruptKind::CoverOpen, true) => tl!("Close the feeder cover and check the paper, then choose Try again."),
        (InterruptKind::Jam, false) => tl!("Clear the jam and load the remaining sheets squarely, then choose Continue."),
        (InterruptKind::Jam, true) => tl!("Clear the jam and load the sheets squarely, then choose Try again."),
        (InterruptKind::FeederEmpty, false) => tl!("Load more sheets, then choose Continue; or choose Finish to keep what was scanned."),
        (InterruptKind::FeederEmpty, true) => tl!("Load sheets in the feeder, then choose Try again."),
        (InterruptKind::MultiFeed, false) => tl!("Separate the sheets and load the remaining ones, then choose Continue."),
        (InterruptKind::MultiFeed, true) => tl!("Separate the sheets and load them again, then choose Try again."),
        (InterruptKind::PaperProblem, false) => {
            tl!("Open the cover, clear any jam or sheets fed together, load the remaining sheets squarely, then choose Continue.")
        }
        (InterruptKind::PaperProblem, true) => {
            tl!("Open the cover, clear any jam or sheets fed together, load the sheets squarely, then choose Try again.")
        }
        (InterruptKind::Offline, false) => tl!("Check the scanner is on and connected, then choose Continue."),
        (InterruptKind::Offline, true) => tl!("Check the scanner is on and connected, then choose Try again."),
        (InterruptKind::Attention, false) => {
            tl!("Look at the scanner for a message or an open door or cover, fix what it shows, then choose Continue.")
        }
        (InterruptKind::Attention, true) => {
            tl!("Look at the scanner for a message or an open door or cover, fix what it shows, then choose Try again.")
        }
        (InterruptKind::Busy, false) => tl!("Wait for the scanner to finish, or close any other program using it, then choose Continue."),
        (InterruptKind::Busy, true) => tl!("Wait for the scanner to finish, or close any other program using it, then choose Try again."),
        (InterruptKind::BusyOrFeederEmpty, false) => {
            tl!("Check that the feeder has paper, or wait for the scanner to finish, then choose Continue.")
        }
        (InterruptKind::BusyOrFeederEmpty, true) => {
            tl!("Check that the feeder has paper, or wait for the scanner to finish, then choose Try again.")
        }
        (InterruptKind::DeviceError, false) => tl!("Switch the scanner off and on again, wait until it is ready, then choose Continue."),
        (InterruptKind::DeviceError, true) => tl!("Switch the scanner off and on again, wait until it is ready, then choose Try again."),
        (InterruptKind::Other, _) => return None,
    })
}

/// Reverse sides still to scan: one per front, less those already in hand (at least one).
fn reverse_pages(fronts: usize, backs: usize) -> u32 {
    u32::try_from(fronts.saturating_sub(backs)).unwrap_or(u32::MAX).max(1)
}

/// Where the `back_index`-th reverse side scanned goes, as a page index, when the `n_fronts` fronts
/// sit contiguously from `first` and every back arriving so far has been inserted. A flipped stack
/// (`reversed`, the normal case) delivers the back of the LAST front first, so each back lands right
/// after its front and nothing already placed moves it. In scan order, earlier backs shift it down.
/// `None` when there is no front left for it.
fn back_position(first: usize, n_fronts: usize, back_index: usize, reversed: bool) -> Option<usize> {
    if back_index >= n_fronts {
        return None;
    }
    let (front, placed_before) = if reversed { (n_fronts - 1 - back_index, 0) } else { (back_index, back_index) };
    Some(first.saturating_add(front).saturating_add(1).saturating_add(placed_before))
}

impl ScanJob {
    /// Follow the source the engine reports it is transferring from. Auto is a guess at start;
    /// when the feeder turns out empty the engine scans the bed instead, one page, so the job's
    /// source, the manual-duplex source and the page counts are corrected to match.
    fn adopt_source(&mut self, used: Source) {
        if used == Source::Auto || used == self.source {
            return;
        }
        let was_feeder = self.source == Source::Feeder;
        self.source = used;
        if self.duplex_source.is_some() {
            self.duplex_source = Some(used);
        }
        if was_feeder && used == Source::Glass {
            self.expected = Some(1);
            self.expected_total = Some(self.received.saturating_add(1));
        }
    }

    fn pass(&self) -> Pass {
        if matches!(self.kind, ScanKind::ReverseSides) {
            Pass::Backs
        } else if self.duplex_source.is_some() {
            Pass::Fronts
        } else {
            Pass::Direct
        }
    }

    /// What this job stopped with, for the dialog and for a Continue.
    fn interruption(&self, message: &str) -> Interrupted {
        Interrupted {
            kind: interrupt_kind(message),
            message: message.to_string(),
            received: self.received,
            expected: self.expected_total,
            scan_kind: self.kind,
            pass: self.pass(),
            doc: self.create_doc,
            pages_in_create: self.pages_in_create,
            create_file_index: self.create_file_index,
            pages_per_file: self.pages_per_file,
            insert_at: self.insert_cursor,
            source: self.source,
        }
    }
}

/// What the scanner dialog shows instead of its controls where there is no scanner backend
/// (`None` when scanning is available). Scanning is Windows/WIA only in this version; other
/// platforms build the same dialog and answer here, at run time, without listing devices.
fn unavailable_notice(supported: bool) -> Option<&'static str> {
    (!supported).then(|| tl!("Scanning needs Windows (WIA) in this version."))
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> ScanClick {
    if let Some(notice) = unavailable_notice(scan::supported()) {
        ui.label(egui::RichText::new(tl!("From scanner…")).font(theme::semibold(18.0)));
        ui.add_space(6.0);
        ui.label(egui::RichText::new(notice).color(t.text_muted));
        ui.add_space(10.0);
        let mut click = ScanClick::None;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::pill_button(ui, tl!("Close"), true).clicked() {
                click = ScanClick::Close;
            }
        });
        return click;
    }
    app.ensure_scanner_list();
    let busy = app.scan.job.is_some();
    let has_doc = app.active.is_some();
    if app.scan.draft.device.is_empty()
        && let Some(id) = default_device_id(&app.scan.devices.refs)
    {
        app.scan.draft.device = id;
    }
    app.ensure_scanner_details();
    // Source and size snapping only follow capabilities that were really read.
    let picked = app.scan.devices.selected_ref(&app.scan.draft.device).and_then(|item| app.scan.devices.ready(item)).cloned();
    if let Some(scanner) = picked.as_ref() {
        let glass = scanner.flatbed || scanner.glass.is_some();
        let feeder = scanner.feeder || scanner.feeder_choices.is_some();
        let before = app.scan.draft.source;
        if app.scan.draft.source == Source::Glass && !glass && feeder {
            app.scan.draft.set_source(Source::Feeder);
        } else if app.scan.draft.source == Source::Feeder && !feeder && glass {
            app.scan.draft.set_source(Source::Glass);
        } else if app.scan.draft.source == Source::Auto && !(glass && feeder) {
            app.scan.draft.set_source(if feeder { Source::Feeder } else { Source::Glass });
        }
        if app.scan.draft.source != before {
            app.scan.advanced = None;
        }
        let choices = source_choices(scanner, effective_source(&app.scan.draft, scanner));
        snap_choices(&mut app.scan.draft, scanner, &choices);
    }
    if busy {
        app.scan.advanced = None;
    }

    // Nested settings dialog: edits a copy; Done merges advanced knobs back.
    if app.scan.advanced.is_some() {
        advanced_modal(ui.ctx(), app, t, busy);
    }

    ui.label(egui::RichText::new(tl!("From scanner…")).font(theme::semibold(18.0)));
    ui.add_space(6.0);
    let mut click = ScanClick::None;
    if !busy && let Some(pending) = app.scan.duplex.as_ref() {
        // Persistent: the fronts are done (or stopped early) and the stack has to be turned over.
        let fronts = pending.fronts.to_string();
        let text = match pending.source {
            Source::Glass => {
                crate::i18n::fmt(tl!("Fronts scanned: {n}. Flip the sheet on the bed, then choose Scan reverse sides."), &[("n", &fronts)])
            }
            _ => crate::i18n::fmt(
                tl!("Fronts scanned: {n}. Flip the stack so the last sheet is on top, reload it, then choose Scan reverse sides."),
                &[("n", &fronts)],
            ),
        };
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(text).color(t.text));
        });
        ui.add_space(6.0);
    }
    egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
        device_row(ui, app, t, busy);
        if app.scan.devices.loading {
            ui.label(egui::RichText::new(tl!("Listing scanners…")).small().color(t.text_muted));
        }
        if let Some(error) = app.scan.devices.error.clone() {
            ui.label(egui::RichText::new(error).color(Color32::from_rgb(0xB0, 0x30, 0x30)));
        }
        let scanner = app.scan.devices.scanner_selected(&app.scan.draft.device);
        if app.scan.devices.checking() {
            ui.label(egui::RichText::new(tl!("Checking the scanner…")).small().color(t.text_muted));
        }
        if let Some(error) = app.scan.devices.details_error().map(str::to_string) {
            ui.label(egui::RichText::new(error).color(Color32::from_rgb(0xB0, 0x30, 0x30)));
            if ui.add_enabled(!busy, egui::Button::new(tl!("Try again"))).clicked() {
                app.retry_scanner_details();
            }
        }
        if let Some(scanner) = scanner.as_ref() {
            source_block(ui, app, t, scanner, busy);
            let effective = effective_source(&app.scan.draft, scanner);
            let choices = source_choices(scanner, effective);
            controls(ui, &mut app.scan.draft, t, &choices, scanner, effective, busy);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    preset_buttons(ui, &mut app.scan.draft, &choices);
                });
                if ui.add_enabled(!busy, egui::Button::new(tl!("Advanced…"))).clicked() {
                    let mut draft = app.scan.draft.clone();
                    draft.preview = None;
                    draft.selection = None;
                    draft.anchor = None;
                    app.scan.advanced = Some(AdvancedEdit { draft });
                }
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new(size_line(&app.scan.draft, scanner)).small().color(t.text_muted));
        } else if !app.scan.devices.loading && app.scan.devices.error.is_none() && app.scan.devices.refs.is_empty() {
            ui.label(tl!("No scanner is connected."));
        }
        ui.add_space(6.0);
        let uses_feeder = app
            .scan
            .devices
            .scanner_selected(&app.scan.draft.device)
            .map(|scanner| effective_source(&app.scan.draft, &scanner) == Source::Feeder)
            .unwrap_or(false);
        preview_block(ui, &mut app.scan.draft, t, busy, uses_feeder);
        if let Some(error) = app.scan.draft.error.clone() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(error).color(Color32::from_rgb(0xB0, 0x30, 0x30)));
        }
        if busy {
            ui.add_space(4.0);
            if let Some(job) = app.scan.job.as_ref() {
                let label = scan_status_label(job);
                let fraction = scan_bar_fraction(job.live.pages_done(), job.expected, job.live.page_percent());
                ui.label(label);
                match fraction {
                    // Buffer / page fill (or multi-page count) — never a made-up asymptotic %.
                    Some(f) => {
                        ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width()).animate(true));
                    }
                    None => {
                        scan_indeterminate_bar(ui, t);
                    }
                }
            } else {
                ui.label(tl!("Scanning…"));
            }
        }
    });
    ui.add_space(8.0);
    let selected_scanner = app.scan.devices.scanner_selected(&app.scan.draft.device);
    let uses_feeder = selected_scanner.as_ref().map(|scanner| effective_source(&app.scan.draft, scanner) == Source::Feeder).unwrap_or(false);
    // Fallback defaults carry no handling status, so a device that could not be read never blocks here.
    let handling_blocks = selected_scanner.as_ref().and_then(|scanner| scanner.handling_status.acquire_block_reason()).is_some();
    let checking = app.scan.devices.checking();
    let awaiting_flip = app.scan.duplex.is_some();
    if !awaiting_flip {
        ui.horizontal(|ui| {
            let mut multi = app.scan.draft.pages_per_file > 0;
            if ui.add_enabled(!busy, crate::widgets::checkbox(&mut multi, tl!("Multiple files"))).changed() {
                app.scan.draft.pages_per_file = if multi { 1 } else { 0 };
            }
            if multi {
                ui.label(tl!("Pages per file"));
                let mut n = app.scan.draft.pages_per_file.max(1);
                if ui.add_enabled(!busy, egui::DragValue::new(&mut n).range(1..=999)).changed() {
                    app.scan.draft.pages_per_file = n;
                }
            }
        });
        ui.add_space(4.0);
    }
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if awaiting_flip {
                if ui
                    .add_enabled_ui(!busy && !checking && !handling_blocks, |ui| widgets::pill_button(ui, tl!("Scan reverse sides"), true))
                    .inner
                    .clicked()
                {
                    click = ScanClick::ReverseSides;
                }
            } else {
                if ui.add_enabled_ui(!busy && !checking && has_doc, |ui| widgets::pill_button(ui, tl!("Insert scanned pages"), true)).inner.clicked()
                {
                    click = ScanClick::Insert;
                }
                if ui.add_enabled_ui(!busy && !checking && !handling_blocks, |ui| widgets::pill_button(ui, tl!("Create PDF"), true)).inner.clicked() {
                    click = ScanClick::Create;
                }
            }
            // Preview frames a region on the bed. The feeder would pull a sheet and still not
            // show where the next page lands, so the button stays off for that source.
            let mut preview = ui
                .add_enabled_ui(!busy && !checking && !uses_feeder && !awaiting_flip && !handling_blocks, |ui| {
                    widgets::pill_button(ui, tl!("Preview"), false)
                })
                .inner;
            if uses_feeder {
                preview = preview.on_disabled_hover_text(tl!("Preview uses the bed. Switch the source to Bed, or scan without a preview."));
            }
            if preview.clicked() {
                click = ScanClick::Preview;
            }
            let cancel = if busy {
                tl!("Stop")
            } else if awaiting_flip {
                tl!("Cancel reverse sides")
            } else {
                tl!("Cancel")
            };
            if widgets::pill_button(ui, cancel, false).clicked() {
                click = if busy {
                    ScanClick::Stop
                } else if awaiting_flip {
                    app.cancel_reverse_sides();
                    ScanClick::None
                } else {
                    ScanClick::Close
                };
            }
        });
    });
    click
}

/// Body of [`crate::Dialog::ScanInterrupted`]: what stopped, how far it got, what to do next.
/// Returns the chosen action. Closing the dialog counts as Finish (the caller maps that).
pub(crate) fn interrupted_body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> Option<InterruptAction> {
    let Some(state) = app.scan.interrupted.clone() else { return Some(InterruptAction::Finish) };
    ui.label(egui::RichText::new(tl!("Scan interrupted")).font(theme::semibold(18.0)));
    ui.add_space(8.0);
    ui.label(egui::RichText::new(state.headline()).font(theme::semibold(14.0)).color(Color32::from_rgb(0xB0, 0x30, 0x30)));
    ui.add_space(4.0);
    ui.label(state.count_line());
    if let Some(kept) = state.kept_line() {
        ui.label(kept);
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new(state.instruction_text()).color(t.text));
    if let Some(error) = app.scan.draft.error.clone() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(error).color(Color32::from_rgb(0xB0, 0x30, 0x30)));
    }
    ui.add_space(10.0);
    let mut chosen = None;
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            for action in state.actions() {
                let (label, primary) = match action {
                    InterruptAction::Continue | InterruptAction::TryAgain => (state.primary_label().to_string(), true),
                    InterruptAction::ReverseNow => (state.reverse_now_label(), false),
                    InterruptAction::Finish => (tl!("Finish").to_string(), false),
                };
                if widgets::pill_button(ui, &label, primary).clicked() {
                    chosen = Some(action);
                }
            }
        });
    });
    chosen
}

fn advanced_modal(ctx: &egui::Context, app: &mut PdfCraftApp, t: &Tokens, busy: bool) {
    let Some(scanner) = app.scan.devices.scanner_selected(&app.scan.draft.device) else {
        app.scan.advanced = None;
        return;
    };
    let mut close = false;
    let mut apply = false;
    egui::Modal::new(egui::Id::new("scan-advanced")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(egui::RichText::new(tl!("Scanner settings")).font(theme::semibold(16.0)));
        ui.label(
            egui::RichText::new(tl!("Settings this source offers. Leave a control alone to keep the driver’s value.")).small().color(t.text_muted),
        );
        ui.add_space(10.0);
        let effective = effective_source(&app.scan.draft, &scanner);
        let choices = source_choices(&scanner, effective);
        if let Some(edit) = app.scan.advanced.as_mut() {
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                more_options(ui, &mut edit.draft, t, &choices, &scanner, busy);
            });
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::pill_button(ui, tl!("Done"), true).clicked() {
                    apply = true;
                    close = true;
                }
                if widgets::pill_button(ui, tl!("Cancel"), false).clicked() {
                    close = true;
                }
            });
        });
    });
    if apply && let Some(edit) = app.scan.advanced.take() {
        apply_advanced(&edit.draft, &mut app.scan.draft);
    } else if close {
        app.scan.advanced = None;
    }
}

/// Copy knobs that belong in Advanced; leave device, source, dpi, color, pages, duplex, preview alone.
fn apply_advanced(from: &ScanDraft, to: &mut ScanDraft) {
    to.dpi_y = from.dpi_y;
    to.brightness = from.brightness;
    to.contrast = from.contrast;
    to.threshold = from.threshold;
    to.rotation = from.rotation;
    to.format = from.format.clone();
    to.compression = from.compression.clone();
    to.intent = from.intent;
    to.page_size = from.page_size;
    to.auto_deskew = from.auto_deskew;
    to.orientation = from.orientation;
    to.photometric = from.photometric;
    to.invert = from.invert;
    to.mirror = from.mirror;
    to.lamp = from.lamp;
}

/// Short tag for how a scanner is connected; `None` when that is not known.
pub(crate) fn connection_label(connection: Connection) -> Option<&'static str> {
    match connection {
        Connection::Usb => Some(tl!("USB")),
        Connection::Wsd => Some(tl!("Network")),
        Connection::Unknown => None,
    }
}

/// The name shown in the device list: the scanner's own name, then its connection tag when known.
pub(crate) fn device_label(name: &str, connection: Connection) -> String {
    match connection_label(connection) {
        Some(tag) => format!("{name}  · {tag}"),
        None => name.to_string(),
    }
}

/// `name` without a trailing ` [..]` suffix (the WIA network entry carries a device id there).
fn base_name(name: &str) -> &str {
    let trimmed = name.trim_end();
    if trimmed.ends_with(']')
        && let Some(open) = trimmed.rfind(" [")
    {
        return trimmed[..open].trim_end();
    }
    trimmed
}

fn same_base_name(a: &str, b: &str) -> bool {
    base_name(a).to_lowercase() == base_name(b).to_lowercase()
}

/// The sentence for `item` when another listed device has the same base name over a different
/// connection (a printer reached both by USB and over the network).
pub(crate) fn same_printer_note(refs: &[ScannerRef], item: &ScannerRef) -> Option<String> {
    if item.connection == Connection::Unknown {
        return None;
    }
    let other = refs.iter().find(|o| o.id != item.id && o.connection != item.connection && same_base_name(&o.name, &item.name))?;
    match other.connection {
        Connection::Usb => Some(tl!("The same printer is also available over USB.").to_string()),
        Connection::Wsd => Some(tl!("The same printer is also available over the network.").to_string()),
        Connection::Unknown => None,
    }
}

/// The device to select when none is remembered: the USB entry of a printer that is also listed
/// over the network (the vendor's driver is the fuller one), else the first listed device.
pub(crate) fn default_device_id(refs: &[ScannerRef]) -> Option<String> {
    let both = refs.iter().find(|item| {
        item.connection == Connection::Usb && refs.iter().any(|o| o.connection == Connection::Wsd && same_base_name(&o.name, &item.name))
    });
    both.or_else(|| refs.first()).map(|item| item.id.clone())
}

fn device_row(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, busy: bool) {
    let _ = t;
    ui.horizontal(|ui| {
        let devices: Vec<(String, String, Option<String>)> = app
            .scan
            .devices
            .refs
            .iter()
            .map(|item| (item.id.clone(), device_label(&item.name, item.connection), same_printer_note(&app.scan.devices.refs, item)))
            .collect();
        let name = devices.iter().find(|(id, _, _)| id == &app.scan.draft.device).map(|(_, name, _)| name.clone()).unwrap_or_else(|| "—".to_string());
        egui::ComboBox::from_id_salt("scan-device").selected_text(name).width(280.0).show_ui(ui, |ui| {
            for (id, scanner_name, note) in &devices {
                let mut response = ui.selectable_label(id == &app.scan.draft.device, scanner_name);
                if let Some(note) = note {
                    response = response.on_hover_text(note.as_str());
                }
                let picked = response.clicked();
                if picked && id != &app.scan.draft.device {
                    app.scan.draft.device.clone_from(id);
                    app.scan.draft.clear_preview();
                    app.scan.draft.snapped_device.clear();
                    app.scan.advanced = None;
                }
            }
        });
        if ui
            .add_enabled(!busy && !app.scan.devices.loading, egui::Button::new(tl!("Refresh")))
            .on_hover_text(tl!("Refresh lists scanners. It does not feed paper."))
            .clicked()
        {
            app.refresh_scanners();
        }
    });
}

fn source_block(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens, scanner: &Scanner, busy: bool) {
    let glass = scanner.flatbed || scanner.glass.is_some();
    let feeder = scanner.feeder || scanner.feeder_choices.is_some();
    let both = glass && feeder;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_enabled_ui(!busy && both, |ui| {
            if ui.add(crate::widgets::radio(app.scan.draft.source == Source::Auto, tl!("Auto"))).clicked() {
                app.scan.draft.set_source(Source::Auto);
                app.scan.advanced = None;
            }
        });
        ui.add_enabled_ui(!busy && glass, |ui| {
            if ui.add(crate::widgets::radio(app.scan.draft.source == Source::Glass, tl!("Bed"))).clicked() {
                app.scan.draft.set_source(Source::Glass);
                app.scan.advanced = None;
            }
        });
        ui.add_enabled_ui(!busy && feeder, |ui| {
            if ui.add(crate::widgets::radio(app.scan.draft.source == Source::Feeder, tl!("Feeder"))).clicked() {
                app.scan.draft.set_source(Source::Feeder);
                app.scan.advanced = None;
            }
        });
    });
    if app.scan.draft.source == Source::Auto && both {
        let line = if effective_source(&app.scan.draft, scanner) == Source::Feeder { tl!("Will use the feeder.") } else { tl!("Will use the bed.") };
        ui.label(egui::RichText::new(line).small().color(t.text_muted));
    }
    if scanner.feeder_ready && matches!(app.scan.draft.source, Source::Feeder | Source::Auto) {
        ui.label(
            egui::RichText::new(tl!("The driver reports a sheet waiting. The bit can stay on after the tray is empty.")).small().color(t.text_muted),
        );
    }
    let status = scanner.handling_status;
    if status.jam {
        ui.label(egui::RichText::new(tl!("The driver reports a paper jam.")).color(Color32::from_rgb(0xB0, 0x30, 0x30)));
    }
    if status.cover_open || status.path_cover_open {
        ui.label(egui::RichText::new(tl!("A cover on the scanner is open.")).color(Color32::from_rgb(0xB0, 0x30, 0x30)));
    }
}

fn controls(ui: &mut egui::Ui, draft: &mut ScanDraft, t: &Tokens, choices: &ImageChoices, scanner: &Scanner, effective: Source, busy: bool) {
    ui.add_space(6.0);
    egui::Grid::new("scan-controls").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        if !choices.dpi.is_empty() {
            ui.label(tl!("Resolution"));
            let dpi = draft.dpi;
            egui::ComboBox::from_id_salt("scan-dpi").selected_text(format!("{dpi} dpi")).width(180.0).show_ui(ui, |ui| {
                for value in &choices.dpi {
                    ui.selectable_value(&mut draft.dpi, *value, format!("{value} dpi"));
                }
            });
            ui.end_row();
        }
        if !choices.colors.is_empty() {
            ui.label(tl!("Color"));
            egui::ComboBox::from_id_salt("scan-color").selected_text(color_label(draft.color)).width(180.0).show_ui(ui, |ui| {
                for color in &choices.colors {
                    ui.selectable_value(&mut draft.color, *color, color_label(*color));
                }
            });
            ui.end_row();
        }
        if effective == Source::Feeder {
            ui.label(tl!("Pages"));
            let (values, all) = page_values(scanner.pages);
            let label = if draft.pages == 0 { tl!("All pages").to_string() } else { draft.pages.to_string() };
            egui::ComboBox::from_id_salt("scan-pages").selected_text(label).width(180.0).show_ui(ui, |ui| {
                for value in values {
                    ui.selectable_value(&mut draft.pages, value, value.to_string());
                }
                if all {
                    ui.selectable_value(&mut draft.pages, 0, tl!("All pages"));
                }
            });
            ui.end_row();
        }
        if scanner.duplex && effective == Source::Feeder {
            ui.label(tl!("Duplex"));
            ui.add_enabled(!busy, crate::widgets::checkbox(&mut draft.duplex, tl!("Both sides")));
            ui.end_row();
        } else if !scanner.duplex && matches!(effective, Source::Feeder | Source::Glass) {
            ui.label(tl!("Duplex"));
            ui.add_enabled(!busy, crate::widgets::checkbox(&mut draft.manual_duplex, tl!("Reverse sides")));
            ui.end_row();
        }
    });
    if draft.manual_duplex && !scanner.duplex {
        let note = if effective == Source::Glass {
            tl!("Scan the front, flip the sheet on the bed, then scan the reverse side.")
        } else {
            tl!("Scan the fronts, flip the stack (last sheet on top), then scan the reverse sides. Pages are interleaved front, back, …")
        };
        ui.label(egui::RichText::new(note).small().color(t.text_muted));
    } else if effective == Source::Feeder {
        let note = if draft.pages == 0 {
            tl!("All pages asks the driver for the whole stack.")
        } else {
            tl!("A page count stops the feeder after that many sheets. Cancel ends the transfer early; pages already received stay in the document.")
        };
        ui.label(egui::RichText::new(note).small().color(t.text_muted));
    }
}

fn more_options(ui: &mut egui::Ui, draft: &mut ScanDraft, t: &Tokens, choices: &ImageChoices, scanner: &Scanner, busy: bool) {
    let mut notes = Vec::new();
    if let (Some(x), Some(y)) = (scanner.optical_x, scanner.optical_y) {
        notes.push(crate::i18n::fmt(tl!("Optical resolution {x} × {y} dpi."), &[("x", &x.to_string()), ("y", &y.to_string())]));
    }
    match scanner.connection {
        pdfcraft_platform::scan::Connection::Usb => notes.push(tl!("Connected by USB.").to_string()),
        pdfcraft_platform::scan::Connection::Wsd => {
            notes.push(tl!("Connected over the network (Web Services). Options come from that driver, not the USB one.").to_string())
        }
        pdfcraft_platform::scan::Connection::Unknown => {}
    }
    if choices.formats.len() == 1 {
        notes.push(crate::i18n::fmt(tl!("Transfer format: {format}."), &[("format", choices.formats.first().map(String::as_str).unwrap_or("bmp"))]));
    }
    if !scanner.duplex {
        notes.push(tl!("Duplex is not offered.").to_string());
    }
    if !notes.is_empty() {
        ui.label(egui::RichText::new(notes.join(" ")).small().color(t.text_muted));
        ui.add_space(10.0);
    }

    let orientation_turns: Vec<Rotation> = choices.orientation.iter().filter_map(|value| Rotation::from_driver(*value)).collect();
    let orientation_matches_rotation = !choices.rotation.is_empty()
        && orientation_turns.len() == choices.orientation.len()
        && orientation_turns.iter().all(|turn| choices.rotation.contains(turn));

    egui::Grid::new("scan-more").num_columns(2).spacing([16.0, 14.0]).min_col_width(100.0).show(ui, |ui| {
        if !choices.dpi_y.is_empty() && choices.dpi_y != choices.dpi {
            ui.label(tl!("Vertical resolution"));
            let current = draft.dpi_y.unwrap_or(draft.dpi);
            egui::ComboBox::from_id_salt("scan-dpi-y").selected_text(format!("{current} dpi")).width(220.0).show_ui(ui, |ui| {
                for value in &choices.dpi_y {
                    ui.selectable_value(&mut draft.dpi_y, Some(*value), format!("{value} dpi"));
                }
            });
            ui.end_row();
        }
        if let Some(span) = choices.brightness {
            ui.label(tl!("Brightness"));
            span_slider(ui, span, &mut draft.brightness, busy);
            ui.end_row();
        }
        if let Some(span) = choices.contrast {
            ui.label(tl!("Contrast"));
            span_slider(ui, span, &mut draft.contrast, busy);
            ui.end_row();
        }
        if draft.color == Color::BlackAndWhite
            && let Some(span) = choices.threshold
        {
            ui.label(tl!("Threshold"));
            span_slider(ui, span, &mut draft.threshold, busy);
            ui.end_row();
        }
        if choices.formats.len() > 1 {
            ui.label(tl!("Format"));
            let current = draft.format.clone().unwrap_or_else(|| choices.formats.first().cloned().unwrap_or_default());
            egui::ComboBox::from_id_salt("scan-format").selected_text(&current).width(220.0).show_ui(ui, |ui| {
                for format in &choices.formats {
                    ui.selectable_value(&mut draft.format, Some(format.clone()), format);
                }
            });
            ui.end_row();
        }
        if choices.compression.len() > 1 {
            ui.label(tl!("Compression"));
            let current = draft.compression.clone().unwrap_or_else(|| choices.compression.first().cloned().unwrap_or_default());
            egui::ComboBox::from_id_salt("scan-compression").selected_text(&current).width(220.0).show_ui(ui, |ui| {
                for name in &choices.compression {
                    ui.selectable_value(&mut draft.compression, Some(name.clone()), name);
                }
            });
            ui.end_row();
        }
        if !choices.intents.is_empty() {
            ui.label(tl!("Intent"));
            let current = draft.intent.map(|intent| intent.name().to_string()).unwrap_or_else(|| tl!("Driver default").to_string());
            egui::ComboBox::from_id_salt("scan-intent").selected_text(current).width(220.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut draft.intent, None, tl!("Driver default"));
                for intent in &choices.intents {
                    ui.selectable_value(&mut draft.intent, Some(*intent), intent.name());
                }
            });
            ui.end_row();
        }
        if choices.page_sizes.len() > 1 {
            ui.label(tl!("Page size"));
            let current = draft.page_size.map(|size| size.name().to_string()).unwrap_or_default();
            egui::ComboBox::from_id_salt("scan-page-size").selected_text(current).width(220.0).show_ui(ui, |ui| {
                for size in &choices.page_sizes {
                    ui.selectable_value(&mut draft.page_size, Some(*size), size.name());
                }
            });
            ui.end_row();
        }
        if choices.auto_deskew {
            ui.label(tl!("Auto-deskew"));
            let mut on = draft.auto_deskew.unwrap_or(false);
            if ui.add_enabled(!busy, crate::widgets::checkbox(&mut on, tl!("Straighten the page"))).changed() {
                draft.auto_deskew = Some(on);
            }
            ui.end_row();
        }
        if choices.photometric.len() > 1 {
            ui.label(tl!("Photometric"));
            let current = draft.photometric.map(|value| value.to_string()).unwrap_or_default();
            egui::ComboBox::from_id_salt("scan-photometric").selected_text(current).width(220.0).show_ui(ui, |ui| {
                for value in &choices.photometric {
                    ui.selectable_value(&mut draft.photometric, Some(*value), value.to_string());
                }
            });
            ui.end_row();
        }
        if choices.invert {
            ui.label(tl!("Invert"));
            let mut on = draft.invert.unwrap_or(false);
            if ui.add_enabled(!busy, crate::widgets::checkbox(&mut on, tl!("Invert tones"))).changed() {
                draft.invert = Some(on);
            }
            ui.end_row();
        }
        if choices.mirror {
            ui.label(tl!("Mirror"));
            let mut on = draft.mirror.unwrap_or(false);
            if ui.add_enabled(!busy, crate::widgets::checkbox(&mut on, tl!("Mirror the page"))).changed() {
                draft.mirror = Some(on);
            }
            ui.end_row();
        }
        if choices.lamp {
            ui.label(tl!("Lamp"));
            let mut on = draft.lamp.unwrap_or(true);
            if ui.add_enabled(!busy, crate::widgets::checkbox(&mut on, tl!("Lamp on"))).changed() {
                draft.lamp = Some(on);
            }
            ui.end_row();
        }
    });

    // Wide control: keep it out of the two-column grid so the thumbs don't crush other rows.
    if !choices.rotation.is_empty() {
        ui.add_space(14.0);
        ui.label(tl!("Rotation"));
        ui.add_space(4.0);
        ui.add_enabled_ui(!busy, |ui| {
            rotation_examples(ui, t, &choices.rotation, draft.rotation, |turn| {
                draft.rotation = Some(turn);
                if orientation_matches_rotation && choices.orientation.contains(&turn.driver()) {
                    draft.orientation = Some(turn.driver());
                }
            });
        });
        ui.label(egui::RichText::new(tl!("The driver may accept a turn and leave the picture as it was.")).small().color(t.text_muted));
    } else if !choices.orientation.is_empty() {
        ui.add_space(14.0);
        ui.label(tl!("Orientation"));
        ui.add_space(4.0);
        if orientation_turns.len() == choices.orientation.len() {
            let selected = draft.orientation.and_then(Rotation::from_driver);
            ui.add_enabled_ui(!busy, |ui| {
                rotation_examples(ui, t, &orientation_turns, selected, |turn| draft.orientation = Some(turn.driver()));
            });
        } else {
            let current = draft.orientation.map(|value| value.to_string()).unwrap_or_default();
            egui::ComboBox::from_id_salt("scan-orientation").selected_text(current).width(220.0).show_ui(ui, |ui| {
                for value in &choices.orientation {
                    ui.selectable_value(&mut draft.orientation, Some(*value), value.to_string());
                }
            });
        }
    }
}

/// Slider with the span’s ends labelled so the control reads as adjustable, not a lone number.
fn span_slider(ui: &mut egui::Ui, span: ValueSpan, slot: &mut Option<i32>, busy: bool) {
    let value = slot.get_or_insert(if span.contains(0) { 0 } else { span.min });
    *value = (*value).clamp(span.min, span.max);
    let step = if span.step <= 0 { 1.0 } else { f64::from(span.step) };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(egui::RichText::new(span.min.to_string()).small().color(ui.visuals().weak_text_color()));
        ui.add_enabled(
            !busy,
            egui::Slider::new(value, span.min..=span.max).step_by(step).clamping(egui::SliderClamping::Always).min_decimals(0).max_decimals(0),
        );
        ui.label(egui::RichText::new(span.max.to_string()).small().color(ui.visuals().weak_text_color()));
    });
}

fn preview_block(ui: &mut egui::Ui, draft: &mut ScanDraft, t: &Tokens, busy: bool, feeder: bool) {
    if feeder {
        ui.label(
            egui::RichText::new(tl!("Preview is for the bed. Set a region in inches below, or leave it empty for the whole sheet."))
                .small()
                .color(t.text_muted),
        );
    } else {
        ui.label(
            egui::RichText::new(tl!("Drag on the preview to choose a region. An empty selection scans the whole area.")).small().color(t.text_muted),
        );
    }
    if let Some(preview) = draft.preview.clone() {
        paint_preview(ui, draft, t, &preview, busy);
    }
    ui.label(egui::RichText::new(tl!("Region in inches. Leave the width or the height at zero to scan the whole area.")).small().color(t.text_muted));
    ui.horizontal(|ui| {
        inch_field(ui, tl!("Left"), &mut draft.left_in, &mut draft.selection, busy);
        inch_field(ui, tl!("Top"), &mut draft.top_in, &mut draft.selection, busy);
        inch_field(ui, tl!("Width"), &mut draft.width_in, &mut draft.selection, busy);
        inch_field(ui, tl!("Height"), &mut draft.height_in, &mut draft.selection, busy);
        let has_region = draft.selection.is_some() || (draft.width_in > 0.0 && draft.height_in > 0.0);
        if ui.add_enabled(!busy && has_region, egui::Button::new(tl!("Clear region"))).clicked() {
            draft.clear_region();
        }
    });
}

fn inch_field(ui: &mut egui::Ui, label: &str, value: &mut f64, selection: &mut Option<(u32, u32, u32, u32)>, busy: bool) {
    ui.label(label);
    let response = ui.add_enabled(!busy, egui::DragValue::new(value).range(0.0..=30.0).speed(0.01).suffix(" in"));
    if response.changed() {
        *selection = None;
    }
}

fn paint_preview(ui: &mut egui::Ui, draft: &mut ScanDraft, t: &Tokens, preview: &PreviewImage, busy: bool) {
    let max = Vec2::new(360.0, 420.0);
    let native = Vec2::new(preview.width.max(1) as f32, preview.height.max(1) as f32);
    let scale = (max.x / native.x).min(max.y / native.y).min(1.0);
    let size = native * scale;
    let sense = if busy { Sense::hover() } else { Sense::drag() };
    let (response, painter) = ui.allocate_painter(size, sense);
    if preview.rgba.len() == preview.width as usize * preview.height as usize * 4 {
        let image = egui::ColorImage::from_rgba_unmultiplied([preview.width as usize, preview.height as usize], &preview.rgba);
        let texture = ui.ctx().load_texture("pdfcraft-scan-preview", image, egui::TextureOptions::LINEAR);
        painter.image(texture.id(), response.rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    }
    if !busy && let Some(pointer) = response.interact_pointer_pos() {
        let pixel = pixel_at(response.rect, preview.width, preview.height, pointer);
        if response.drag_started() {
            draft.anchor = Some(pixel);
        }
        if response.dragged()
            && let Some(anchor) = draft.anchor
        {
            draft.remember_selection(Some(pixel_rect(anchor, pixel, preview.width, preview.height)));
        }
    }
    if response.drag_stopped() {
        draft.anchor = None;
    }
    if let Some(sel) = overlay(draft, preview) {
        let rect = widget_rect(response.rect, preview.width, preview.height, sel);
        painter.rect_stroke(rect, egui::CornerRadius::ZERO, egui::Stroke::new(1.5, t.accent), egui::StrokeKind::Middle);
    }
}

/// Bed or feeder the next acquire will use. Auto follows [`Scanner::feeder_ready`].
fn effective_source(draft: &ScanDraft, scanner: &Scanner) -> Source {
    let has_feeder = scanner.feeder || scanner.feeder_choices.is_some();
    let has_glass = scanner.flatbed || scanner.glass.is_some();
    draft.source.resolve(scanner.feeder_ready, has_feeder, has_glass).unwrap_or(Source::Glass)
}

fn size_line(draft: &ScanDraft, scanner: &Scanner) -> String {
    let region = draft.region();
    let (width, height, full) = if let Some(region) = region {
        (region.width, region.height, false)
    } else {
        match effective_source(draft, scanner) {
            Source::Glass | Source::Auto => match scanner.bed {
                Some((w, h)) => (w, h, true),
                None => return tl!("Full area").to_string(),
            },
            Source::Feeder => match scanner.sheet {
                Some((w, h)) => (w, h, true),
                None => return tl!("Full area").to_string(),
            },
        }
    };
    let Some(px) = region_pixels(Region { left: 0, top: 0, width, height }, draft.dpi).ok() else {
        return tl!("Full area").to_string();
    };
    let Some(bytes) = estimated_bmp_bytes(px.width, px.height, draft.color.depth()) else {
        return tl!("Full area").to_string();
    };
    let size = usize::try_from(bytes).map(crate::panels::human_size).unwrap_or_else(|_| format!("{bytes} bytes"));
    let mut line = crate::i18n::fmt(
        tl!("{w} × {h} in, {pxw} × {pxh} pixels, {size}"),
        &[
            ("w", &format!("{:.2}", f64::from(width) / 1000.0)),
            ("h", &format!("{:.2}", f64::from(height) / 1000.0)),
            ("pxw", &px.width.to_string()),
            ("pxh", &px.height.to_string()),
            ("size", &size),
        ],
    );
    if full {
        line = format!("{} — {line}", tl!("Full area"));
    }
    if bytes >= 64 * 1024 * 1024 {
        line.push(' ');
        line.push_str(tl!("Large scan"));
    }
    line
}

fn overlay(draft: &ScanDraft, preview: &PreviewImage) -> Option<(u32, u32, u32, u32)> {
    if let Some(selection) = draft.selection {
        return Some(selection);
    }
    let region = draft.region()?;
    let px = region_pixels(region, preview.dpi).ok()?;
    Some((px.x, px.y, px.x.saturating_add(px.width).min(preview.width), px.y.saturating_add(px.height).min(preview.height)))
}

fn pixel_at(rect: Rect, width: u32, height: u32, pos: Pos2) -> (u32, u32) {
    if width == 0 || height == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return (0, 0);
    }
    let x = ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 0.999);
    let y = ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 0.999);
    let px = (x * width as f32) as u32;
    let py = (y * height as f32) as u32;
    (px.min(width - 1), py.min(height - 1))
}

fn pixel_rect(a: (u32, u32), b: (u32, u32), width: u32, height: u32) -> (u32, u32, u32, u32) {
    let (x0, x1) = (a.0.min(b.0), a.0.max(b.0).saturating_add(1).min(width));
    let (y0, y1) = (a.1.min(b.1), a.1.max(b.1).saturating_add(1).min(height));
    (x0, y0, x1, y1)
}

fn widget_rect(image: Rect, width: u32, height: u32, sel: (u32, u32, u32, u32)) -> Rect {
    let sx = image.width() / width.max(1) as f32;
    let sy = image.height() / height.max(1) as f32;
    Rect::from_min_max(image.min + Vec2::new(sel.0 as f32 * sx, sel.1 as f32 * sy), image.min + Vec2::new(sel.2 as f32 * sx, sel.3 as f32 * sy))
}

fn page_values(span: Option<ValueSpan>) -> (Vec<u32>, bool) {
    let Some(span) = span else {
        return (vec![1], false);
    };
    let step = span.step.max(1);
    let mut values = Vec::new();
    let mut n = span.min.max(1);
    while n <= span.max && values.len() < 64 {
        if span.contains(n)
            && let Ok(page) = u32::try_from(n)
        {
            values.push(page);
        }
        let Some(next) = n.checked_add(step) else { break };
        n = next;
    }
    if values.is_empty() {
        values.push(1);
    }
    (values, span.contains(0))
}

fn preferred_dpi(listed: &[u32]) -> Option<u32> {
    [300, 200].into_iter().find(|dpi| listed.contains(dpi)).or_else(|| listed.first().copied())
}

/// Prefer 600 dpi for photographs, then 400, then the usual document dpi.
fn photo_dpi(listed: &[u32]) -> Option<u32> {
    [600, 400, 300]
        .into_iter()
        .find(|dpi| listed.contains(dpi))
        .or_else(|| listed.iter().copied().filter(|dpi| *dpi <= 600).max())
        .or_else(|| listed.first().copied())
}

fn preset_buttons(ui: &mut egui::Ui, draft: &mut ScanDraft, choices: &ImageChoices) {
    let offer = |color: Color| choices.colors.is_empty() || choices.colors.contains(&color);
    if offer(Color::Gray) && widgets::pill_button(ui, tl!("Text"), false).clicked() {
        draft.apply_text_ocr(choices);
    }
    if offer(Color::Rgb) && widgets::pill_button(ui, tl!("Color document"), false).clicked() {
        draft.apply_color_document(choices);
    }
    if offer(Color::BlackAndWhite) && widgets::pill_button(ui, tl!("Black and white"), false).clicked() {
        draft.apply_black_and_white(choices);
    }
    if offer(Color::Rgb) && widgets::pill_button(ui, tl!("Photo"), false).clicked() {
        draft.apply_photo(choices);
    }
}

fn snap_choices(draft: &mut ScanDraft, scanner: &Scanner, choices: &ImageChoices) {
    let resolved = effective_source(draft, scanner);
    if draft.snapped_device == scanner.id && draft.snapped_source == draft.source && draft.snapped_resolved == resolved {
        return;
    }
    if draft.snapped_device == scanner.id
        && draft.source == Source::Auto
        && draft.snapped_resolved != Source::Auto
        && draft.snapped_resolved != resolved
    {
        draft.clear_preview();
    }
    draft.snapped_device = scanner.id.clone();
    draft.snapped_source = draft.source;
    draft.snapped_resolved = resolved;
    if !choices.dpi.is_empty() && !choices.dpi.contains(&draft.dpi) {
        draft.dpi = preferred_dpi(&choices.dpi).unwrap_or(draft.dpi);
    }
    if !choices.colors.is_empty() && !choices.colors.contains(&draft.color) {
        draft.color = if choices.colors.contains(&Color::Gray) { Color::Gray } else { choices.colors.first().copied().unwrap_or(Color::Gray) };
    }
    draft.brightness = choices.brightness.map(|span| if span.contains(0) { 0 } else { span.min });
    draft.contrast = choices.contrast.map(|span| if span.contains(0) { 0 } else { span.min });
    draft.threshold = choices.threshold.map(|span| if span.contains(128) { 128 } else { span.min });
    draft.rotation = if choices.rotation.contains(&Rotation::Portrait) { Some(Rotation::Portrait) } else { choices.rotation.first().copied() };
    if choices.formats.len() > 1 {
        if draft.format.as_ref().is_none_or(|format| !choices.formats.contains(format)) {
            draft.format = choices.formats.first().cloned();
        }
    } else {
        draft.format = None;
    }
    if choices.compression.len() > 1 {
        if draft.compression.as_ref().is_none_or(|name| !choices.compression.contains(name)) {
            draft.compression = choices.compression.first().cloned();
        }
    } else {
        draft.compression = None;
    }
    if !choices.page_sizes.is_empty() {
        if draft.page_size.is_none_or(|size| !choices.page_sizes.contains(&size)) {
            draft.page_size = choices.page_sizes.first().copied();
        }
    } else {
        draft.page_size = None;
    }
    if !choices.auto_deskew {
        draft.auto_deskew = None;
    }
    if draft.intent.is_some_and(|intent| !choices.intents.is_empty() && !choices.intents.contains(&intent)) {
        draft.intent = None;
    }
    if draft.orientation.is_some_and(|value| !choices.orientation.is_empty() && !choices.orientation.contains(&value)) {
        draft.orientation = choices.orientation.first().copied();
    }
    if choices.orientation.is_empty() {
        draft.orientation = None;
    }
    if draft.photometric.is_some_and(|value| !choices.photometric.is_empty() && !choices.photometric.contains(&value)) {
        draft.photometric = choices.photometric.first().copied();
    }
    if choices.photometric.is_empty() {
        draft.photometric = None;
    }
    if !choices.invert {
        draft.invert = None;
    }
    if !choices.mirror {
        draft.mirror = None;
    }
    if !choices.lamp {
        draft.lamp = None;
    }
    if draft.dpi_y.is_some_and(|dpi| !choices.dpi_y.is_empty() && !choices.dpi_y.contains(&dpi)) {
        draft.dpi_y = None;
    }
    if choices.dpi_y.is_empty() || choices.dpi_y == choices.dpi {
        draft.dpi_y = None;
    }
    if !scanner.duplex {
        draft.duplex = false;
    } else {
        draft.manual_duplex = false;
    }
    let (values, all) = page_values(scanner.pages);
    if draft.pages != 0 && !values.contains(&draft.pages) {
        draft.pages = values.last().copied().unwrap_or(1);
    } else if draft.pages == 0 && !all && draft.source == Source::Glass {
        // The bed is always one page; feeder `0` still means "until empty" in `scan::acquire`.
        // Only when the user chose the bed: under Auto the source can resolve to the bed just
        // because the feeder looked empty when the list loaded, and this write is sticky (the
        // Pages list has no "All pages" entry to get back to), so a later feeder scan would
        // ask for one sheet. `final_settings` already sends one page for any bed scan.
        draft.pages = 1;
    }
}

fn source_choices(scanner: &Scanner, source: Source) -> ImageChoices {
    match source {
        Source::Glass | Source::Auto => scanner.glass.clone(),
        Source::Feeder => scanner.feeder_choices.clone(),
    }
    .unwrap_or_default()
}

fn color_label(color: Color) -> String {
    match color {
        Color::Gray => tl!("Gray").to_string(),
        Color::Rgb => tl!("Color").to_string(),
        Color::BlackAndWhite => tl!("Black and white").to_string(),
    }
}

fn rotation_label(turn: Rotation) -> String {
    match turn {
        Rotation::Portrait => tl!("Portrait").to_string(),
        Rotation::Landscape => tl!("Landscape").to_string(),
        Rotation::UpsideDown => tl!("Upside down").to_string(),
        Rotation::Left => tl!("Turned left").to_string(),
    }
}

/// Selectable page sketches for the four turns the driver offers.
fn rotation_examples(ui: &mut egui::Ui, t: &Tokens, turns: &[Rotation], selected: Option<Rotation>, mut choose: impl FnMut(Rotation)) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for &turn in turns {
            let on = selected == Some(turn);
            let response = orientation_thumb(ui, t, turn, on);
            if response.clicked() {
                choose(turn);
            }
        }
    });
}

fn orientation_thumb(ui: &mut egui::Ui, t: &Tokens, turn: Rotation, selected: bool) -> egui::Response {
    let size = Vec2::new(52.0, 64.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let response = response.on_hover_text(rotation_label(turn));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    let fill = if selected { t.accent_soft } else { t.field };
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(if selected { 1.5 } else { 1.0 }, if selected { t.accent } else { t.border }),
        egui::StrokeKind::Inside,
    );
    let page = Rect::from_center_size(rect.center(), Vec2::new(26.0, 36.0));
    painter.rect_filled(page, 2.0, Color32::WHITE);
    painter.rect_stroke(page, 2.0, egui::Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
    // Header bar and two text lines in page space, then turned to match the driver code.
    let center = page.center();
    let header = [
        Pos2::new(page.min.x + 4.0, page.min.y + 5.0),
        Pos2::new(page.max.x - 4.0, page.min.y + 5.0),
        Pos2::new(page.max.x - 4.0, page.min.y + 11.0),
        Pos2::new(page.min.x + 4.0, page.min.y + 11.0),
    ]
    .map(|p| turn_point(p, center, turn));
    painter.add(egui::Shape::convex_polygon(header.to_vec(), t.text_muted.gamma_multiply(0.55), egui::Stroke::NONE));
    for (i, y) in [16.0_f32, 22.0, 28.0].into_iter().enumerate() {
        let inset = if i == 2 { 10.0 } else { 4.0 };
        let a = turn_point(Pos2::new(page.min.x + 4.0, page.min.y + y), center, turn);
        let b = turn_point(Pos2::new(page.max.x - inset, page.min.y + y), center, turn);
        painter.line_segment([a, b], egui::Stroke::new(1.5, t.text_muted.gamma_multiply(0.7)));
    }
    response
}

fn turn_point(point: Pos2, center: Pos2, turn: Rotation) -> Pos2 {
    let v = point - center;
    let (x, y) = match turn {
        Rotation::Portrait => (v.x, v.y),
        Rotation::Landscape => (-v.y, v.x),
        Rotation::UpsideDown => (-v.x, -v.y),
        Rotation::Left => (v.y, -v.x),
    };
    center + Vec2::new(x, y)
}

fn preview_settings(draft: &ScanDraft, scanner: &Scanner) -> ScanSettings {
    // Preview always uses the bed. Callers must refuse the feeder before reaching here.
    // Color follows the dialog so a Color choice is not previewed as gray; dpi stays low.
    let choices = source_choices(scanner, Source::Glass);
    let color = if choices.colors.is_empty() || choices.colors.contains(&draft.color) {
        draft.color
    } else if choices.colors.contains(&Color::Gray) {
        Color::Gray
    } else {
        choices.colors.first().copied().unwrap_or(Color::Gray)
    };
    let intent = if choices.intents.contains(&Intent::BestPreview) {
        Some(Intent::BestPreview)
    } else {
        match color {
            Color::Rgb if choices.intents.contains(&Intent::Color) => Some(Intent::Color),
            Color::Gray if choices.intents.contains(&Intent::Gray) => Some(Intent::Gray),
            Color::BlackAndWhite if choices.intents.contains(&Intent::Text) => Some(Intent::Text),
            _ => None,
        }
    };
    ScanSettings {
        device: draft.device.clone(),
        source: Source::Glass,
        dpi: preview_dpi(&choices.dpi),
        dpi_y: None,
        color,
        pages: 1,
        brightness: None,
        contrast: None,
        threshold: if color == Color::BlackAndWhite { draft.threshold } else { None },
        rotation: None,
        region: None,
        format: None,
        compression: None,
        intent,
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

fn final_settings(draft: &ScanDraft, scanner: &Scanner, cancel: CancelToken) -> ScanSettings {
    let effective = effective_source(draft, scanner);
    ScanSettings {
        device: draft.device.clone(),
        source: draft.source,
        dpi: draft.dpi,
        dpi_y: draft.dpi_y,
        color: draft.color,
        pages: if effective == Source::Glass { 1 } else { draft.pages },
        brightness: draft.brightness,
        contrast: draft.contrast,
        threshold: if draft.color == Color::BlackAndWhite { draft.threshold } else { None },
        rotation: draft.rotation,
        region: draft.region(),
        format: draft.format.clone(),
        compression: draft.compression.clone(),
        intent: draft.intent,
        page_size: draft.page_size,
        auto_deskew: draft.auto_deskew,
        orientation: draft.orientation,
        photometric: draft.photometric,
        invert: draft.invert,
        mirror: draft.mirror,
        lamp: draft.lamp,
        duplex: draft.duplex && scanner.duplex && effective == Source::Feeder,
        cancel: Some(cancel),
        progress: None,
    }
}

fn scan_status_label(job: &ScanJob) -> String {
    let done = job.live.pages_done();
    let page_pct = job.live.page_percent();
    let received = job.live.bytes_received();
    let bytes_expected = job.live.bytes_expected();
    // Unknown stack or a one-page job: the buffer fill is the story, not a made-up overall %.
    let page_focused = job.expected.map(|total| total <= 1).unwrap_or(true);
    let prefix = match job.kind {
        ScanKind::ReverseSides => tl!("Scanning reverse side {n}…"),
        _ if job.duplex_source.is_some() => tl!("Scanning front {n}…"),
        _ => tl!("Scanning page {n}…"),
    };
    let mut base = match job.expected {
        Some(total) if total > 0 && matches!(job.kind, ScanKind::ReverseSides) => {
            crate::i18n::fmt(tl!("Scanning reverse side {n} of {t}…"), &[("n", &done.max(1).to_string()), ("t", &total.to_string())])
        }
        Some(total) if total > 0 && job.duplex_source.is_some() => {
            crate::i18n::fmt(tl!("Scanning front {n} of {t}…"), &[("n", &done.max(1).to_string()), ("t", &total.to_string())])
        }
        Some(total) if total > 0 => crate::i18n::fmt(tl!("Scanning page {n} of {t}…"), &[("n", &done.max(1).to_string()), ("t", &total.to_string())]),
        _ if done > 0 => crate::i18n::fmt(prefix, &[("n", &done.to_string())]),
        _ => tl!("Scanning…").to_string(),
    };
    // Digits only — no new i18n key. Buffer size first when that is the progress we have.
    // WSD JFIF: expected may be unknown mid-stream — still show received bytes + soft %.
    if page_focused {
        if received > 0 || page_pct.is_some() {
            let r = crate::panels::human_size(usize::try_from(received).unwrap_or(usize::MAX));
            base = match (bytes_expected, page_pct) {
                (Some(expected), Some(p)) => {
                    let e = crate::panels::human_size(usize::try_from(expected).unwrap_or(usize::MAX));
                    format!("{} · {r} / {e} ({p}%)", base.trim_end_matches('…'))
                }
                (Some(expected), None) => {
                    let e = crate::panels::human_size(usize::try_from(expected).unwrap_or(usize::MAX));
                    format!("{} · {r} / {e}", base.trim_end_matches('…'))
                }
                (None, Some(p)) if received > 0 => {
                    format!("{} · {r} ({p}%)", base.trim_end_matches('…'))
                }
                (None, Some(p)) => format!("{} {}%", base.trim_end_matches('…'), p),
                (None, None) if received > 0 => format!("{} · {r}", base.trim_end_matches('…')),
                _ => base,
            };
        }
    } else {
        if let Some(p) = page_pct {
            base = format!("{} {}%", base.trim_end_matches('…'), p);
        }
        if received > 0 {
            let r = crate::panels::human_size(usize::try_from(received).unwrap_or(usize::MAX));
            base = match bytes_expected {
                Some(expected) => {
                    let e = crate::panels::human_size(usize::try_from(expected).unwrap_or(usize::MAX));
                    format!("{base} · {r} / {e}")
                }
                None => format!("{base} · {r}"),
            };
        }
    }
    base
}

struct CreateApplyState {
    create_doc: Option<DocId>,
    pages_in_create: u32,
    create_file_index: u32,
}

/// Name for Create PDF document `index` (`0` → Scan.pdf, `1` → Scan (2).pdf).
fn scan_create_name(index: u32) -> String {
    if index == 0 { "Scan.pdf".into() } else { format!("Scan ({}).pdf", index + 1) }
}

/// Pages that still fit in the current multi-file document before a new one must open.
fn pages_remaining(per: u32, already: u32, has_doc: bool) -> u32 {
    if !has_doc {
        return per.max(1);
    }
    per.saturating_sub(already)
}

/// Honest bar fill. Multi-page with a known count uses pages + in-page fill; a single page or an
/// unknown stack uses only the page buffer (`None` until the first band/status arrives).
fn scan_bar_fraction(done: usize, expected: Option<u32>, page_percent: Option<u8>) -> Option<f32> {
    let page = page_percent.map(|p| f32::from(p) / 100.0);
    match expected {
        Some(total) if total > 1 => Some((((done as f32) + page.unwrap_or(0.0)) / (total as f32)).clamp(0.0, 0.99)),
        _ => page.map(|p| p.clamp(0.0, 0.99)),
    }
}

/// Walking band while we wait for the first buffer/status update (no honest fraction yet).
fn scan_indeterminate_bar(ui: &mut egui::Ui, t: &Tokens) {
    let width = ui.available_width();
    let (track, _) = ui.allocate_exact_size(egui::vec2(width, 6.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(track, egui::CornerRadius::same(3), t.text_muted.gamma_multiply(0.25));
    let now = ui.ctx().input(|i| i.time);
    let band = (track.width() * 0.28).clamp(36.0, 96.0);
    let travel = track.width() + band;
    let x = track.left() - band + ((now * 140.0) % f64::from(travel.max(1.0))) as f32;
    let clip = painter.with_clip_rect(track);
    clip.rect_filled(
        egui::Rect::from_min_size(egui::pos2(x, track.top()), egui::vec2(band, track.height())),
        egui::CornerRadius::same(3),
        t.accent.gamma_multiply(0.85),
    );
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

/// BMP (1, 4, 8, 24, or 32 bits uncompressed; 4 and 8 bits also RLE-compressed) as top-down RGBA.
/// Row 0 is the top of the page. Any other layout (bit fields, JPEG/PNG payloads, 16 bits) gives
/// `None`, and so does a truncated file, so the caller falls back to its own decoder. The header
/// rules and the decoder are `pdfcraft_scan::bmp`, shared with the scan crate's page builder.
pub fn bmp_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    scan::bmp::to_rgba(bytes)
}

fn spawn_named(name: &str, work: impl FnOnce() + Send + 'static) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = name;
        work();
        true
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new().name(name.into()).spawn(work).is_ok()
    }
}

impl PdfCraftApp {
    /// List scanners when the dialog opens. Listing does not open any device, feed paper or scan.
    pub fn ensure_scanner_list(&mut self) {
        if self.scan.devices.loading || self.scan.devices.listed {
            return;
        }
        self.refresh_scanners();
    }

    pub fn refresh_scanners(&mut self) {
        let Some(generation) = self.scan.devices.begin_list() else { return };
        let slot = Arc::new(Mutex::new(ListBox { generation, result: None }));
        let progress = Arc::clone(&slot);
        let started = spawn_named("pdfcraft-scan-list", move || {
            let result = scan::scanner_list().map_err(|error| error.to_string());
            if let Ok(mut guard) = progress.lock() {
                guard.result = Some(result);
            }
        });
        if started {
            self.scan.devices.slot = Some(slot);
        } else {
            self.scan.devices.loading = false;
            self.scan.devices.listed = true;
            self.scan.devices.error = Some(tl!("The scanner list could not be refreshed.").to_string());
        }
    }

    /// Read the capabilities of the selected device only, once per selection.
    pub fn ensure_scanner_details(&mut self) {
        let Some(id) = self.scan.devices.selected_ref(&self.scan.draft.device).map(|item| item.id.clone()) else { return };
        if let Some(generation) = self.scan.devices.want_details(&id) {
            self.start_details_load(generation, id);
        }
    }

    /// Try the selected device again after its details failed.
    pub fn retry_scanner_details(&mut self) {
        if let Some(generation) = self.scan.devices.retry_details() {
            let id = self.scan.devices.details_id.clone();
            self.start_details_load(generation, id);
        }
    }

    fn start_details_load(&mut self, generation: u64, id: String) {
        let slot = Arc::new(Mutex::new(DetailsBox { generation, id: id.clone(), result: None }));
        let progress = Arc::clone(&slot);
        let started = spawn_named("pdfcraft-scan-details", move || {
            let result = scan::scanner_details(&id).map_err(|error| error.to_string());
            if let Ok(mut guard) = progress.lock() {
                guard.result = Some(result);
            }
        });
        if started {
            self.scan.devices.details_slot = Some(slot);
        } else {
            let id = self.scan.devices.details_id.clone();
            self.scan.devices.finish_details(generation, &id, Err(tl!("The scanner list could not be refreshed.").to_string()));
        }
    }

    pub fn start_scan(&mut self, click: ScanClick) {
        self.start_scan_with(click, None);
    }

    /// [`Self::start_scan`], or the next leg of an interrupted scan. True when a job started.
    fn start_scan_with(&mut self, click: ScanClick, resume: Option<Resume>) -> bool {
        if matches!(click, ScanClick::Stop) {
            if let Some(job) = &self.scan.job {
                job.cancel.request();
            }
            return false;
        }
        if self.scan.job.is_some() || matches!(click, ScanClick::None | ScanClick::Close) {
            return false;
        }
        let Some(scanner) = self.scan.devices.scanner_selected(&self.scan.draft.device) else {
            let message = if self.scan.devices.checking() { tl!("Checking the scanner…") } else { tl!("No scanner is connected.") };
            self.scan.draft.error = Some(message.to_string());
            return false;
        };
        if matches!(click, ScanClick::Preview) && effective_source(&self.scan.draft, &scanner) == Source::Feeder {
            self.scan.draft.error = Some(tl!("Preview uses the bed. Switch the source to Bed, or scan without a preview.").to_string());
            return false;
        }
        if matches!(click, ScanClick::ReverseSides) && self.scan.duplex.is_none() {
            self.scan.draft.error = Some(tl!("Scan the fronts before the reverse sides.").to_string());
            return false;
        }
        let cancel = CancelToken::new();
        let live = ProgressToken::new();
        let mut settings = match click {
            ScanClick::Preview => {
                let mut settings = preview_settings(&self.scan.draft, &scanner);
                settings.cancel = Some(cancel.clone());
                settings.progress = Some(live.clone());
                settings
            }
            ScanClick::Create | ScanClick::Insert | ScanClick::ReverseSides => {
                let mut settings = final_settings(&self.scan.draft, &scanner, cancel.clone());
                settings.progress = Some(live.clone());
                settings
            }
            ScanClick::None | ScanClick::Close | ScanClick::Stop => return false,
        };
        if let Some(r) = &resume {
            // Once pages came in, keep the bed or feeder they came from (Auto would pick the bed
            // when the tray is now empty) and ask only for the pages still missing.
            if settings.source == Source::Auto
                && let Some(source) = r.source
            {
                settings.source = source;
            }
            if r.feeder
                && !matches!(click, ScanClick::ReverseSides)
                && let Some(pages) = r.pages
            {
                settings.pages = pages;
            }
        }
        if matches!(click, ScanClick::ReverseSides)
            && let Some(pending) = &self.scan.duplex
        {
            let n = u32::try_from(pending.fronts).unwrap_or(0);
            if n == 0 {
                self.scan.draft.error = Some(tl!("Scan the fronts before the reverse sides.").to_string());
                return false;
            }
            // Same source as the fronts. Forcing the feeder here broke bed reverse-side scans.
            settings.source = pending.source;
            settings.pages = if pending.source == Source::Glass { 1 } else { reverse_pages(pending.fronts, pending.backs) };
            if pending.source == Source::Glass && n != 1 {
                self.scan.draft.error =
                    Some(tl!("The bed scans one page at a time. Scan one front, flip it, then scan its reverse side.").to_string());
                return false;
            }
        }
        if let Err(error) = settings.checked() {
            self.scan.draft.error = Some(error.to_string());
            return false;
        }
        if let Some(region) = settings.region {
            let max = match effective_source(&self.scan.draft, &scanner) {
                Source::Glass | Source::Auto => scanner.bed,
                Source::Feeder => scanner.sheet,
            };
            if let Err(error) = scan::fit_region(region, max).and_then(|fitted| region_pixels(fitted, settings.dpi)) {
                self.scan.draft.error = Some(error.to_string());
                return false;
            }
        }
        let kind = match click {
            ScanClick::Preview => ScanKind::Preview,
            ScanClick::Create => ScanKind::Create,
            ScanClick::Insert => match resume.as_ref().map(|r| r.scan_kind) {
                Some(ScanKind::Insert(id)) => {
                    if !self.views.iter().any(|view| view.id == id) {
                        self.scan.draft.error = Some(tl!("The document is no longer open.").to_string());
                        return false;
                    }
                    ScanKind::Insert(id)
                }
                _ => {
                    let Some((_, id)) = self.active_ids() else { return false };
                    ScanKind::Insert(id)
                }
            },
            ScanClick::ReverseSides => ScanKind::ReverseSides,
            ScanClick::None | ScanClick::Close | ScanClick::Stop => return false,
        };
        let duplex_source = match effective_source(&self.scan.draft, &scanner) {
            Source::Auto => Source::Glass, // acquire will resolve; bed is the empty-tray case
            source => source,
        };
        // Prefer the source acquire will actually use when Auto is chosen.
        let duplex_source = if self.scan.draft.source == Source::Auto {
            if scanner.feeder_ready && (scanner.feeder || scanner.feeder_choices.is_some()) { Source::Feeder } else { Source::Glass }
        } else {
            duplex_source
        };
        let duplex_first_pass = self.scan.draft.manual_duplex
            && !scanner.duplex
            && matches!(kind, ScanKind::Create | ScanKind::Insert(_))
            && matches!(duplex_source, Source::Feeder | Source::Glass);
        let expected = match kind {
            ScanKind::Preview => Some(1),
            ScanKind::ReverseSides => Some(settings.pages.max(1)),
            ScanKind::Create | ScanKind::Insert(_) if settings.pages > 0 => Some(settings.pages),
            ScanKind::Create | ScanKind::Insert(_) if duplex_first_pass && duplex_source == Source::Glass => Some(1),
            _ => None,
        };
        self.scan.draft.error = None;
        let hardware_duplex = settings.duplex;
        let slot = Arc::new(Mutex::new(JobSlot { result: None }));
        let progress = Arc::clone(&slot);
        // acquire lights the lamp or pulls a sheet. It never runs on this thread.
        let started = spawn_named("pdfcraft-scan", move || {
            let result = scan::acquire(&settings).map(|acquired| acquired.pages).map_err(|error| error.to_string());
            if let Ok(mut guard) = progress.lock() {
                guard.result = Some(result);
            }
        });
        if !started {
            self.scan.draft.error = Some(tl!("The scan could not be started.").to_string());
            return false;
        }
        // Every kind leaves the dialog so the document can show each page as it arrives; a manual
        // duplex reopens it for the flip step once the fronts are in.
        if matches!(kind, ScanKind::Create | ScanKind::Insert(_) | ScanKind::ReverseSides) {
            self.dialog = None;
        }
        // The backs find their fronts by position, so the fronts of a manual duplex stay in one document.
        let pages_per_file = match kind {
            ScanKind::Create if self.scan.draft.pages_per_file > 0 && !duplex_first_pass => Some(self.scan.draft.pages_per_file),
            _ => None,
        };
        let carried = resume.as_ref().map_or(0, |r| r.carried);
        let expected_total = match (kind, expected) {
            _ if hardware_duplex => None,
            (ScanKind::Preview, _) => None,
            (_, Some(leg)) => Some(carried.saturating_add(leg)),
            (_, None) => None,
        };
        let (create_doc, pages_in_create, create_file_index, pages_per_file) = match &resume {
            // A document the user closed since is not appended to: the next page opens a new one.
            Some(r) => match r.create_doc.filter(|id| self.views.iter().any(|view| view.id == *id)) {
                Some(id) => (Some(id), r.pages_in_create, r.create_file_index, r.pages_per_file.or(pages_per_file)),
                None => (None, 0, r.create_file_index, r.pages_per_file.or(pages_per_file)),
            },
            None => (None, 0, 0, pages_per_file),
        };
        // The next page goes after the last one this scan already added.
        let insert_cursor = match kind {
            ScanKind::Insert(id) => match resume.as_ref().and_then(|r| r.insert_at) {
                Some(at) => Some(at),
                None => self.insert_cursor_for(id),
            },
            _ => None,
        };
        if resume.is_some() {
            let target = match kind {
                ScanKind::Insert(id) => insert_cursor.map(|at| (id, at)),
                ScanKind::Create => create_doc.map(|id| (id, self.session.get(id).map_or(0, |doc| doc.info.pages.len()))),
                ScanKind::Preview | ScanKind::ReverseSides => None,
            };
            if let Some((id, at)) = target
                && let Some(view) = self.views.iter_mut().find(|view| view.id == id)
            {
                view.insert_at = Some(at);
            }
        } else {
            self.scan.last_create_files = 0;
            self.scan.interrupted = None;
            if duplex_first_pass {
                // A new first pass starts a new set of fronts.
                self.scan.duplex = None;
            }
        }
        self.scan.job = Some(ScanJob {
            kind,
            create_doc,
            pages_in_create,
            create_file_index,
            pages_per_file,
            expected,
            duplex_source: if duplex_first_pass { Some(duplex_source) } else { None },
            received: carried,
            expected_total,
            insert_cursor,
            source: duplex_source,
            live,
            progress: slot,
            cancel,
        });
        self.poll_scan();
        true
    }

    pub(crate) fn poll_scan(&mut self) {
        self.poll_scanner_list();
        self.poll_scanner_details();
        let Some(job) = &self.scan.job else { return };
        let kind = job.kind;
        let live = job.live.clone();
        if let Some(used) = live.source()
            && let Some(job) = self.scan.job.as_mut()
        {
            job.adopt_source(used);
        }
        let Some(job) = &self.scan.job else { return };
        let expected = job.expected;
        let progress = Arc::clone(&job.progress);
        let Ok(mut slot) = progress.lock() else {
            self.scan.job = None;
            self.progress_notice = None;
            return;
        };
        let finished = slot.result.take();
        drop(slot);

        // Preview waits for the full transfer; other kinds apply each finished page now.
        if !matches!(kind, ScanKind::Preview) {
            let pages = live.take_pages();
            if !pages.is_empty() {
                self.apply_scanned_pages(kind, pages);
            }
        }

        let Some(result) = finished else {
            let done = live.pages_done();
            let page_percent = live.page_percent();
            let label = self.scan.job.as_ref().map(scan_status_label).unwrap_or_else(|| tl!("Scanning…").to_string());
            let cancelling = self.scan.job.as_ref().is_some_and(|job| job.cancel.is_requested());
            let show_notice =
                matches!(kind, ScanKind::Create | ScanKind::Insert(_) | ScanKind::ReverseSides) || self.dialog != Some(crate::Dialog::Scanner);
            if show_notice {
                self.progress_notice = Some(crate::widgets::ProgressNotice {
                    label: if cancelling { tl!("Cancelling…").to_string() } else { label },
                    fraction: scan_bar_fraction(done, expected, page_percent),
                    cancellable: !cancelling,
                });
            }
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            return;
        };

        self.progress_notice = None;
        match (kind, result) {
            (ScanKind::Preview, Ok(pages)) => {
                self.scan.job = None;
                self.take_preview(pages);
            }
            (ScanKind::Create | ScanKind::Insert(_), Ok(_)) => {
                let rest = live.take_pages();
                if !rest.is_empty() {
                    self.apply_scanned_pages(kind, rest);
                }
                let streamed = self.scan.job.as_ref().is_some_and(|job| job.create_doc.is_some() || job.received > 0) || live.pages_done() > 0;
                if matches!(kind, ScanKind::Create) {
                    self.scan.last_create_files = self.scan.job.as_ref().map(|job| job.create_file_index).unwrap_or(0).max(1);
                }
                let fronts_done = self.scan.job.as_ref().is_some_and(|job| job.duplex_source.is_some()) && self.scan.duplex.is_some();
                self.scan.job = None;
                if fronts_done {
                    // The fronts are in the document; the flip instruction stays on screen in the dialog.
                    self.dialog = Some(crate::Dialog::Scanner);
                } else if streamed {
                    self.finish_scan_job(kind);
                }
            }
            (ScanKind::ReverseSides, Ok(_)) => {
                let rest = live.take_pages();
                if !rest.is_empty() {
                    self.apply_scanned_pages(kind, rest);
                }
                self.scan.job = None;
                self.finish_manual_duplex();
            }
            (ScanKind::Preview, Err(error)) => {
                self.scan.job = None;
                self.scan.draft.error = Some(error.clone());
                if self.dialog != Some(crate::Dialog::Scanner) {
                    self.notify(error);
                }
            }
            (_, Err(error)) => self.scan_interrupted(kind, error, live.take_pages()),
        }
    }

    /// A Create/Insert/reverse-side job ended with an error. Whatever pages arrived are kept in
    /// the document (the fronts and backs of a manual duplex included), and the Scan interrupted
    /// dialog says what stopped and what to do. A scan the user stopped is not an interruption.
    fn scan_interrupted(&mut self, kind: ScanKind, error: String, leftover: Vec<Page>) {
        let cancelled = self.scan.job.as_ref().is_some_and(|job| job.cancel.is_requested());
        if !leftover.is_empty() {
            self.apply_scanned_pages(kind, leftover);
        }
        let Some(job) = self.scan.job.take() else {
            self.scan.draft.error = Some(error);
            return;
        };
        let state = job.interruption(&error);
        if cancelled {
            // The user pressed Stop: keep the pages, say nothing more than before.
            if state.pass == Pass::Direct {
                self.scan.draft.error = Some(error.clone());
                if self.dialog != Some(crate::Dialog::Scanner) {
                    self.notify(error);
                }
            } else {
                if self.scan.duplex.is_none() {
                    self.scan.draft.error = Some(error);
                }
                self.dialog = Some(crate::Dialog::Scanner);
            }
            return;
        }
        self.scan.draft.error = None;
        self.scan.interrupted = Some(state);
        self.dialog = Some(crate::Dialog::ScanInterrupted);
    }

    /// Where the next page of an Insert goes: the gap chosen, else after the selection.
    fn insert_cursor_for(&self, id: DocId) -> Option<usize> {
        let view = self.views.iter().find(|view| view.id == id)?;
        let count = self.session.get(id).map_or(0, |doc| doc.info.pages.len());
        let after = view.target_pages().last().map_or(0, |page| page.saturating_add(1));
        Some(view.insert_at.unwrap_or(after).min(count))
    }

    /// The user's choice in the Scan interrupted dialog.
    pub(crate) fn resolve_interrupt(&mut self, action: InterruptAction) {
        match action {
            InterruptAction::Continue | InterruptAction::TryAgain => self.resume_interrupted(),
            InterruptAction::ReverseNow => {
                self.scan.interrupted = None;
                // The fronts are held in `duplex`: the Scanner dialog shows the flip step.
                self.dialog = self.scan.duplex.is_some().then_some(crate::Dialog::Scanner);
            }
            InterruptAction::Finish => self.finish_interrupted(),
        }
    }

    /// Continue (or Try again): the next acquire carries on with the same settings and the same
    /// document, for the pages still missing.
    fn resume_interrupted(&mut self) {
        let Some(state) = self.scan.interrupted.take() else { return };
        if self.dialog == Some(crate::Dialog::ScanInterrupted) {
            self.dialog = None;
        }
        self.scan.draft.error = None;
        if self.start_scan_with(state.click(), Some(state.resume())) {
            return;
        }
        // It could not start (no scanner, document closed): stay in the dialog with the reason.
        self.scan.interrupted = Some(state);
        self.dialog = Some(crate::Dialog::ScanInterrupted);
    }

    /// Finish: keep everything scanned and close. Every page scanned is already in the document
    /// (fronts, and the reverse sides so far beside their fronts); fronts without a reverse side
    /// stay single pages.
    fn finish_interrupted(&mut self) {
        let Some(state) = self.scan.interrupted.take() else {
            if self.dialog == Some(crate::Dialog::ScanInterrupted) {
                self.dialog = None;
            }
            return;
        };
        if self.dialog == Some(crate::Dialog::ScanInterrupted) {
            self.dialog = None;
        }
        self.scan.draft.error = None;
        let kind = match state.pass {
            Pass::Direct => state.scan_kind,
            Pass::Fronts | Pass::Backs => match self.scan.duplex.take() {
                Some(pending) => pending.kind,
                None => return,
            },
        };
        if state.has_pages() {
            if matches!(kind, ScanKind::Create) {
                let files = if state.pass == Pass::Direct { state.create_file_index } else { self.scan.last_create_files };
                self.scan.last_create_files = files.max(1);
            }
            self.finish_scan_job(kind);
        }
    }

    /// The user gave up on the reverse sides: the document keeps the fronts as they are.
    pub(crate) fn cancel_reverse_sides(&mut self) {
        let Some(pending) = self.scan.duplex.take() else { return };
        self.scan.draft.error = None;
        if matches!(pending.kind, ScanKind::Create) {
            self.scan.last_create_files = self.scan.last_create_files.max(1);
        }
        self.finish_scan_job(pending.kind);
    }

    /// Insert scanned pages as they arrive. The first front of a Create opens the document.
    fn apply_scanned_pages(&mut self, kind: ScanKind, pages: Vec<Page>) {
        if matches!(kind, ScanKind::ReverseSides) {
            self.apply_back_pages(pages);
            return;
        }
        let count = pages.len();
        let mut first_hint = 0;
        let mut fronts_pass = false;
        if let Some(job) = &mut self.scan.job {
            first_hint = job.insert_cursor.unwrap_or(0);
            fronts_pass = job.duplex_source.is_some();
            job.received = job.received.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
            job.insert_cursor = job.insert_cursor.map(|at| at.saturating_add(count));
        }
        match kind {
            ScanKind::Create => {
                let (create_doc, pages_in, file_index, per) = self
                    .scan
                    .job
                    .as_ref()
                    .map(|job| (job.create_doc, job.pages_in_create, job.create_file_index, job.pages_per_file))
                    .unwrap_or((None, 0, 0, None));
                let per = if fronts_pass {
                    None
                } else {
                    per.or_else(|| {
                        let n = self.scan.draft.pages_per_file;
                        (n > 0).then_some(n)
                    })
                };
                if fronts_pass
                    && let Some(pending) = self.scan.duplex
                    && Some(pending.doc) == create_doc
                {
                    self.place_next_insert(pending.doc, pending.first.saturating_add(pending.fronts));
                }
                let state = self.apply_create_pages(pages, create_doc, pages_in, file_index, per);
                if let Some(job) = &mut self.scan.job {
                    job.create_doc = state.create_doc;
                    job.pages_in_create = state.pages_in_create;
                    job.create_file_index = state.create_file_index;
                }
                self.scan.last_create_files = state.create_file_index.max(self.scan.last_create_files);
                if fronts_pass {
                    self.note_fronts(kind, state.create_doc, 0, count);
                }
            }
            ScanKind::Insert(id) => {
                if fronts_pass {
                    let at = match self.scan.duplex {
                        Some(pending) if pending.doc == id => pending.first.saturating_add(pending.fronts),
                        _ => first_hint,
                    };
                    self.place_next_insert(id, at);
                }
                self.insert_scanned_pdf(id, &pages);
                if fronts_pass {
                    self.note_fronts(kind, Some(id), first_hint, count);
                }
            }
            ScanKind::Preview | ScanKind::ReverseSides => {}
        }
    }

    /// The next insert into `id` goes to page index `at`, whatever is selected.
    fn place_next_insert(&mut self, id: DocId, at: usize) {
        let count = self.session.get(id).map_or(0, |doc| doc.info.pages.len());
        if let Some(view) = self.views.iter_mut().find(|view| view.id == id) {
            view.insert_at = Some(at.min(count));
        }
    }

    /// Remember where the fronts of this manual duplex are: `count` more in `doc`, the first of the
    /// whole set at `first` (Insert) or page 0 (Create).
    fn note_fronts(&mut self, kind: ScanKind, doc: Option<DocId>, first: usize, count: usize) {
        let Some(doc) = doc.filter(|id| self.views.iter().any(|view| view.id == *id)) else { return };
        let first = if matches!(kind, ScanKind::Create) { 0 } else { first };
        let source = self.scan.job.as_ref().and_then(|job| job.duplex_source).unwrap_or(Source::Glass);
        match &mut self.scan.duplex {
            Some(pending) if pending.doc == doc => pending.fronts = pending.fronts.saturating_add(count),
            _ => self.scan.duplex = Some(DuplexAwait { kind, source, doc, first, fronts: count, backs: 0 }),
        }
    }

    /// Reverse sides arrive one by one: each goes right after its front (see [`back_position`]).
    fn apply_back_pages(&mut self, pages: Vec<Page>) {
        for page in pages {
            let Some(pending) = self.scan.duplex else { return };
            if !self.views.iter().any(|view| view.id == pending.doc) {
                let message = tl!("The document is no longer open.").to_string();
                self.scan.draft.error = Some(message.clone());
                self.notify(message);
                return;
            }
            // The stack was flipped as a whole, so the first back belongs to the last front.
            let Some(at) = back_position(pending.first, pending.fronts, pending.backs, true) else { return };
            self.place_next_insert(pending.doc, at);
            self.insert_scanned_pdf(pending.doc, std::slice::from_ref(&page));
            if let Some(held) = &mut self.scan.duplex {
                held.backs = held.backs.saturating_add(1);
            }
            if let Some(job) = &mut self.scan.job {
                job.received = job.received.saturating_add(1);
            }
        }
    }

    /// Apply Create pages, optionally splitting into a new document every `per` pages.
    fn apply_create_pages(
        &mut self,
        pages: Vec<Page>,
        mut create_doc: Option<DocId>,
        mut pages_in_create: u32,
        mut create_file_index: u32,
        per: Option<u32>,
    ) -> CreateApplyState {
        if pages.is_empty() {
            return CreateApplyState { create_doc, pages_in_create, create_file_index };
        }
        let Some(per) = per.filter(|n| *n > 0) else {
            if let Some(id) = create_doc {
                self.insert_scanned_pdf(id, &pages);
                pages_in_create = pages_in_create.saturating_add(pages.len() as u32);
            } else {
                let Some((first, rest)) = pages.split_first() else {
                    return CreateApplyState { create_doc, pages_in_create, create_file_index };
                };
                self.open_scanned_pdf_named(&scan_create_name(create_file_index), std::slice::from_ref(first));
                create_file_index = create_file_index.saturating_add(1);
                create_doc = self.active_ids().map(|(_, id)| id);
                pages_in_create = 1;
                if let Some(id) = create_doc
                    && !rest.is_empty()
                {
                    self.insert_scanned_pdf(id, rest);
                    pages_in_create = pages_in_create.saturating_add(rest.len() as u32);
                }
            }
            return CreateApplyState { create_doc, pages_in_create, create_file_index };
        };

        let mut rest = pages;
        while !rest.is_empty() {
            let room = pages_remaining(per, pages_in_create, create_doc.is_some());
            if room == 0 {
                create_doc = None;
                pages_in_create = 0;
                continue;
            }
            let take = (room as usize).min(rest.len());
            let chunk: Vec<Page> = rest.drain(..take).collect();
            if let Some(id) = create_doc {
                self.insert_scanned_pdf(id, &chunk);
                pages_in_create = pages_in_create.saturating_add(chunk.len() as u32);
            } else {
                let Some((first, more)) = chunk.split_first() else { break };
                self.open_scanned_pdf_named(&scan_create_name(create_file_index), std::slice::from_ref(first));
                create_file_index = create_file_index.saturating_add(1);
                create_doc = self.active_ids().map(|(_, id)| id);
                pages_in_create = 1;
                if let Some(id) = create_doc
                    && !more.is_empty()
                {
                    self.insert_scanned_pdf(id, more);
                    pages_in_create = pages_in_create.saturating_add(more.len() as u32);
                }
            }
            if pages_in_create >= per {
                create_doc = None;
                pages_in_create = 0;
            }
        }
        CreateApplyState { create_doc, pages_in_create, create_file_index }
    }

    /// The reverse-side pass ended: done when every front has its back, otherwise offer the rest.
    fn finish_manual_duplex(&mut self) {
        let Some(pending) = self.scan.duplex else {
            self.scan.draft.error = Some(tl!("Scan the fronts before the reverse sides.").to_string());
            return;
        };
        if pending.backs < pending.fronts {
            let message = format!(
                "{} ({} {}, {} {})",
                tl!("Manual duplex needs one reverse side for each front."),
                pending.fronts,
                tl!("front(s)"),
                pending.backs,
                tl!("reverse side(s)"),
            );
            // Short: the backs are in place; offer Continue for the missing ones, or Finish.
            let state = Interrupted {
                kind: InterruptKind::Other,
                message,
                received: u32::try_from(pending.backs).unwrap_or(u32::MAX),
                expected: u32::try_from(pending.fronts).ok(),
                scan_kind: ScanKind::ReverseSides,
                pass: Pass::Backs,
                doc: None,
                pages_in_create: 0,
                create_file_index: 0,
                pages_per_file: None,
                insert_at: None,
                source: pending.source,
            };
            self.scan.draft.error = None;
            self.scan.interrupted = Some(state);
            self.dialog = Some(crate::Dialog::ScanInterrupted);
            return;
        }
        self.scan.duplex = None;
        self.notify(tl!("Reverse sides interleaved.").to_string());
        if matches!(pending.kind, ScanKind::Create) {
            self.scan.last_create_files = self.scan.last_create_files.max(1);
        }
        self.finish_scan_job(pending.kind);
    }

    /// After Create/Insert finishes: close the scanner dialog and leave Create a PDF / Scan & OCR
    /// so the document is the focus. Does not open Organize — rotation stays a choice, as with
    /// Enhance camera image's optional next steps.
    fn finish_scan_job(&mut self, kind: ScanKind) {
        if self.dialog == Some(crate::Dialog::Scanner) {
            self.dialog = None;
        }
        if matches!(self.left, crate::LeftPanel::Tool("create" | "scan")) {
            self.left = crate::LeftPanel::AllTools;
        }
        match kind {
            ScanKind::Create => {
                let n = self.scan.last_create_files.max(1);
                if n > 1 {
                    self.notify_fmt("Created {n} scanned files", &[("n", &n.to_string())]);
                } else {
                    self.notify_tr("Created Scan.pdf");
                }
            }
            ScanKind::Insert(_) => self.notify_tr("Inserted scanned pages"),
            ScanKind::Preview | ScanKind::ReverseSides => {}
        }
    }

    fn poll_scanner_list(&mut self) {
        let Some(slot) = self.scan.devices.slot.clone() else { return };
        let Ok(mut guard) = slot.lock() else { return };
        let Some(result) = guard.result.take() else {
            drop(guard);
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            return;
        };
        let generation = guard.generation;
        drop(guard);
        if let Some(chosen) = self.scan.devices.finish_list(generation, result, &self.scan.draft.device)
            && chosen != self.scan.draft.device
        {
            self.scan.draft.device = chosen;
            self.scan.draft.clear_preview();
            self.scan.draft.snapped_device.clear();
        }
    }

    fn poll_scanner_details(&mut self) {
        let Some(slot) = self.scan.devices.details_slot.clone() else { return };
        let Ok(mut guard) = slot.lock() else { return };
        let Some(result) = guard.result.take() else {
            drop(guard);
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
            return;
        };
        let (generation, id) = (guard.generation, guard.id.clone());
        drop(guard);
        self.scan.devices.finish_details(generation, &id, result);
    }

    fn take_preview(&mut self, pages: Vec<Page>) {
        let Some(page) = pages.into_iter().next() else {
            self.scan.draft.error = Some(tl!("The preview could not be shown.").to_string());
            return;
        };
        match bmp_rgba(&page.bytes) {
            Some((width, height, rgba)) => {
                self.scan.draft.selection = None;
                self.scan.draft.anchor = None;
                self.scan.draft.preview = Some(PreviewImage { width, height, dpi: page.dpi.max(1), rgba });
                self.scan.draft.error = None;
            }
            None => self.scan.draft.error = Some(tl!("The preview could not be shown.").to_string()),
        }
    }

    fn scanned_pdf(&self, pages: &[Page]) -> Result<Vec<u8>, String> {
        let Some(first) = pages.first() else {
            return Err(tl!("The scan produced no pages.").to_string());
        };
        let images =
            pages.iter().enumerate().map(|(index, page)| (format!("scan-{}.{}", index + 1, page.format), page.bytes.clone())).collect::<Vec<_>>();
        let pdf = self
            .session
            .create_from_images_with_resolution(&images, pdfcraft_engine::ImageResolution::Dpi(f64::from(first.dpi)))
            .map_err(|error| error.to_string())?;
        Ok(pdf.as_ref().clone())
    }

    fn open_scanned_pdf_named(&mut self, name: &str, pages: &[Page]) {
        match self.scanned_pdf(pages) {
            Ok(bytes) => {
                if let Err(error) = self.open_bytes(name, None, bytes) {
                    self.scan.draft.error = Some(error);
                } else if self.dialog == Some(crate::Dialog::Scanner) {
                    // Dialog can close as pages stream in; Create/Scan tools stay until the job ends.
                    self.dialog = None;
                }
            }
            Err(error) => self.scan.draft.error = Some(error),
        }
    }

    fn insert_scanned_pdf(&mut self, id: DocId, pages: &[Page]) {
        let Some(index) = self.views.iter().position(|view| view.id == id) else {
            let message = tl!("The document is no longer open.").to_string();
            self.scan.draft.error = Some(message.clone());
            self.notify(message);
            return;
        };
        let bytes = match self.scanned_pdf(pages) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.scan.draft.error = Some(error);
                return;
            }
        };
        self.active = Some(index);
        self.insert_pages_from("Scan.pdf", bytes);
        if self.dialog == Some(crate::Dialog::Scanner) {
            self.dialog = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PreviewImage, ScanDraft, bmp_rgba};

    /// Runs on every platform: the gate decides from a plain bool, so both answers are checked.
    #[test]
    fn the_dialog_shows_a_notice_only_where_scanning_is_unsupported() {
        assert_eq!(super::unavailable_notice(true), None);
        assert_eq!(super::unavailable_notice(false), Some("Scanning needs Windows (WIA) in this version."));
    }
    use egui::Pos2;
    use pdfcraft_platform::scan::Source;

    #[test]
    fn the_dialog_defaults_to_auto_300_dpi_gray() {
        let draft = ScanDraft::default();
        assert_eq!(draft.source, Source::Auto);
        assert_eq!(draft.dpi, 300);
        assert_eq!(draft.color, pdfcraft_platform::scan::Color::Gray);
        assert_eq!(draft.pages, 0, "feeder/auto defaults to the whole stack");
    }

    #[test]
    fn snap_choices_keeps_feeder_all_pages_when_wia_omits_zero_in_range() {
        use super::snap_choices;
        use pdfcraft_platform::scan::{Color, Connection, ImageChoices, Scanner, ValueSpan};

        let feeder_choices = ImageChoices { dpi: vec![100, 200, 300], colors: vec![Color::Gray], ..ImageChoices::default() };
        let scanner = Scanner {
            id: "wsd-1".into(),
            name: "Network scanner (WSD)".into(),
            feeder: true,
            flatbed: true,
            duplex: false,
            feeder_ready: true,
            handling_status: Default::default(),
            feeder_capacity: Some(30),
            connection: Connection::Wsd,
            port: Some("http://192.0.2.10/wsd".into()),
            optical_x: None,
            optical_y: None,
            pages: Some(ValueSpan { min: 1, max: 30, step: 1 }),
            bed: Some((8500, 11000)),
            sheet: None,
            glass: Some(ImageChoices::default()),
            feeder_choices: Some(feeder_choices.clone()),
        };
        let mut draft = ScanDraft { device: scanner.id.clone(), source: Source::Feeder, pages: 0, ..ScanDraft::default() };
        draft.snapped_device.clear();
        snap_choices(&mut draft, &scanner, &feeder_choices);
        assert_eq!(draft.pages, 0, "All pages on the feeder must stay 0 when the driver range is 1..30 (scan::acquire whole-stack)");
    }

    #[test]
    fn snap_choices_clamps_glass_all_pages_to_one_without_driver_zero() {
        use super::snap_choices;
        use pdfcraft_platform::scan::{Color, Connection, ImageChoices, Scanner, ValueSpan};

        let glass = ImageChoices { dpi: vec![100, 200, 300], colors: vec![Color::Gray], ..ImageChoices::default() };
        let scanner = Scanner {
            id: "wsd-1".into(),
            name: "Network scanner (WSD)".into(),
            feeder: true,
            flatbed: true,
            duplex: false,
            feeder_ready: false,
            handling_status: Default::default(),
            feeder_capacity: None,
            connection: Connection::Wsd,
            port: None,
            optical_x: None,
            optical_y: None,
            pages: Some(ValueSpan { min: 1, max: 1, step: 1 }),
            bed: Some((8500, 11000)),
            sheet: None,
            glass: Some(glass.clone()),
            feeder_choices: Some(ImageChoices::default()),
        };
        let mut draft = ScanDraft { device: scanner.id.clone(), source: Source::Glass, pages: 0, ..ScanDraft::default() };
        draft.snapped_device.clear();
        snap_choices(&mut draft, &scanner, &glass);
        assert_eq!(draft.pages, 1, "the bed is always one page");
    }

    /// Regression probe for the "only the first sheet from the feeder" report: with Source::Auto,
    /// an empty/not-ready feeder makes the draft resolve to the bed, `snap_choices` clamps
    /// "All pages" (0) to 1, and that clamp is sticky when the feeder later becomes ready.
    #[test]
    fn auto_source_does_not_keep_one_page_after_the_feeder_becomes_ready() {
        use super::{effective_source, final_settings, snap_choices};
        use pdfcraft_platform::scan::{CancelToken, Connection, ImageChoices, Scanner, ValueSpan};

        let choices = ImageChoices { dpi: vec![300], ..ImageChoices::default() };
        let mut scanner = Scanner {
            id: "wsd-1".into(),
            name: "Network scanner (WSD)".into(),
            feeder: true,
            flatbed: true,
            duplex: false,
            feeder_ready: false,
            handling_status: Default::default(),
            feeder_capacity: None,
            connection: Connection::Wsd,
            port: None,
            optical_x: None,
            optical_y: None,
            pages: Some(ValueSpan { min: 1, max: 30, step: 1 }),
            bed: None,
            sheet: None,
            glass: Some(choices.clone()),
            feeder_choices: Some(choices.clone()),
        };
        let mut draft = ScanDraft { device: scanner.id.clone(), ..ScanDraft::default() };
        assert_eq!((draft.source, draft.pages), (Source::Auto, 0));
        snap_choices(&mut draft, &scanner, &choices);
        assert_eq!(effective_source(&draft, &scanner), Source::Glass);
        // Paper is loaded and the scanner list refreshes.
        scanner.feeder_ready = true;
        snap_choices(&mut draft, &scanner, &choices);
        assert_eq!(effective_source(&draft, &scanner), Source::Feeder);
        let settings = final_settings(&draft, &scanner, CancelToken::new());
        assert_eq!(settings.pages, 0, "whole feeder expected; got {} page(s) for source Auto -> feeder", settings.pages);
    }

    #[test]
    fn final_settings_passes_feeder_all_pages_to_acquire() {
        use super::final_settings;
        use pdfcraft_platform::scan::{CancelToken, Connection, ImageChoices, Scanner, ValueSpan};

        let scanner = Scanner {
            id: "wsd-1".into(),
            name: "Network scanner".into(),
            feeder: true,
            flatbed: true,
            duplex: false,
            feeder_ready: true,
            handling_status: Default::default(),
            feeder_capacity: Some(5),
            connection: Connection::Wsd,
            port: None,
            optical_x: None,
            optical_y: None,
            pages: Some(ValueSpan { min: 1, max: 30, step: 1 }),
            bed: None,
            sheet: None,
            glass: Some(ImageChoices::default()),
            feeder_choices: Some(ImageChoices::default()),
        };
        let draft = ScanDraft { device: scanner.id.clone(), source: Source::Feeder, pages: 0, ..ScanDraft::default() };
        let settings = final_settings(&draft, &scanner, CancelToken::new());
        assert_eq!(settings.pages, 0);
        assert_eq!(settings.source, Source::Feeder);
    }

    #[test]
    fn text_ocr_picks_300_dpi_gray_and_clears_the_region() {
        use pdfcraft_platform::scan::{Color, ImageChoices, Rotation};

        let mut draft = ScanDraft {
            dpi: 1200,
            color: Color::Rgb,
            brightness: Some(20),
            contrast: Some(-10),
            selection: Some((0, 0, 10, 10)),
            width_in: 2.0,
            height_in: 1.0,
            ..ScanDraft::default()
        };
        let choices = ImageChoices {
            dpi: vec![100, 200, 300, 600],
            colors: vec![Color::Gray, Color::Rgb],
            brightness: Some(pdfcraft_platform::scan::ValueSpan { min: -50, max: 50, step: 1 }),
            contrast: Some(pdfcraft_platform::scan::ValueSpan { min: -50, max: 50, step: 1 }),
            threshold: None,
            rotation: vec![Rotation::Portrait, Rotation::Landscape],
            intents: vec![pdfcraft_platform::scan::Intent::Text],
            ..ImageChoices::default()
        };
        draft.apply_text_ocr(&choices);
        assert_eq!(draft.dpi, 300);
        assert_eq!(draft.color, Color::Gray);
        assert_eq!(draft.intent, Some(pdfcraft_platform::scan::Intent::Text));
        assert_eq!(draft.brightness, Some(0));
        assert_eq!(draft.contrast, Some(0));
        assert_eq!(draft.rotation, Some(Rotation::Portrait));
        assert!(draft.selection.is_none());
        assert!(draft.region().is_none());
    }

    #[test]
    fn color_document_photo_and_black_and_white_presets() {
        use pdfcraft_platform::scan::{Color, ImageChoices, Intent, Rotation, ValueSpan};

        let choices = ImageChoices {
            dpi: vec![100, 200, 300, 600, 1200],
            colors: vec![Color::BlackAndWhite, Color::Gray, Color::Rgb],
            brightness: Some(ValueSpan { min: -50, max: 50, step: 1 }),
            contrast: Some(ValueSpan { min: -50, max: 50, step: 1 }),
            threshold: Some(ValueSpan { min: 0, max: 255, step: 1 }),
            rotation: vec![Rotation::Portrait],
            intents: vec![Intent::Color, Intent::Gray, Intent::Text, Intent::MaximizeQuality],
            ..ImageChoices::default()
        };
        let mut draft = ScanDraft { dpi: 100, color: Color::Gray, ..ScanDraft::default() };
        draft.apply_color_document(&choices);
        assert_eq!(draft.dpi, 300);
        assert_eq!(draft.color, Color::Rgb);
        assert_eq!(draft.intent, Some(Intent::Color));

        draft.apply_photo(&choices);
        assert_eq!(draft.dpi, 600);
        assert_eq!(draft.color, Color::Rgb);
        assert_eq!(draft.intent, Some(Intent::MaximizeQuality));

        draft.apply_black_and_white(&choices);
        assert_eq!(draft.dpi, 300);
        assert_eq!(draft.color, Color::BlackAndWhite);
        assert_eq!(draft.intent, Some(Intent::Text));
        assert_eq!(draft.threshold, Some(128));
    }

    #[test]
    fn advanced_settings_do_not_overwrite_the_main_dpi_and_source() {
        use super::apply_advanced;
        let mut main = ScanDraft { source: Source::Feeder, dpi: 300, color: pdfcraft_platform::scan::Color::Gray, pages: 2, ..ScanDraft::default() };
        let advanced = ScanDraft {
            source: Source::Glass,
            dpi: 600,
            color: pdfcraft_platform::scan::Color::Rgb,
            pages: 1,
            brightness: Some(10),
            format: Some("jpeg".into()),
            invert: Some(true),
            ..ScanDraft::default()
        };
        apply_advanced(&advanced, &mut main);
        assert_eq!(main.source, Source::Feeder);
        assert_eq!(main.dpi, 300);
        assert_eq!(main.color, pdfcraft_platform::scan::Color::Gray);
        assert_eq!(main.pages, 2);
        assert_eq!(main.brightness, Some(10));
        assert_eq!(main.format.as_deref(), Some("jpeg"));
        assert_eq!(main.invert, Some(true));
    }

    #[test]
    fn turn_point_rotates_around_the_page_centre() {
        let c = Pos2::new(10.0, 10.0);
        let top = Pos2::new(10.0, 0.0);
        assert_eq!(super::turn_point(top, c, pdfcraft_platform::scan::Rotation::Portrait), top);
        assert_eq!(super::turn_point(top, c, pdfcraft_platform::scan::Rotation::Landscape), Pos2::new(20.0, 10.0));
        assert_eq!(super::turn_point(top, c, pdfcraft_platform::scan::Rotation::UpsideDown), Pos2::new(10.0, 20.0));
        assert_eq!(super::turn_point(top, c, pdfcraft_platform::scan::Rotation::Left), Pos2::new(0.0, 10.0));
    }

    #[test]
    fn preview_follows_the_chosen_color() {
        use pdfcraft_platform::scan::{Color, ImageChoices, Intent, Scanner};

        let scanner = Scanner {
            id: "test".into(),
            name: "test".into(),
            feeder: false,
            flatbed: true,
            duplex: false,
            feeder_ready: false,
            handling_status: Default::default(),
            feeder_capacity: None,
            connection: pdfcraft_platform::scan::Connection::Usb,
            port: Some(r"\\.\Usbscan0".into()),
            optical_x: Some(1200),
            optical_y: Some(1200),
            pages: None,
            bed: Some((8500, 11000)),
            sheet: None,
            glass: Some(ImageChoices {
                dpi: vec![100, 150, 200, 300],
                colors: vec![Color::BlackAndWhite, Color::Gray, Color::Rgb],
                intents: vec![Intent::Color, Intent::Gray, Intent::Text, Intent::BestPreview],
                ..ImageChoices::default()
            }),
            feeder_choices: None,
        };
        let mut draft = ScanDraft { color: Color::Rgb, ..ScanDraft::default() };
        let settings = super::preview_settings(&draft, &scanner);
        assert_eq!(settings.color, Color::Rgb);
        assert_eq!(settings.source, pdfcraft_platform::scan::Source::Glass);
        assert_eq!(settings.dpi, 100);
        draft.color = Color::BlackAndWhite;
        let bw = super::preview_settings(&draft, &scanner);
        assert_eq!(bw.color, Color::BlackAndWhite);
    }

    #[test]
    fn scan_bar_uses_page_buffer_when_total_is_unknown_or_one() {
        assert_eq!(super::scan_bar_fraction(0, None, None), None);
        assert_eq!(super::scan_bar_fraction(3, None, None), None, "finished pages alone are not a made-up %");
        assert_eq!(super::scan_bar_fraction(0, None, Some(42)), Some(0.42));
        assert_eq!(super::scan_bar_fraction(0, Some(1), Some(75)), Some(0.75));
        assert!((super::scan_bar_fraction(1, Some(4), Some(50)).unwrap() - 0.375).abs() < 1e-4);
    }

    #[test]
    fn switching_source_clears_the_preview_and_the_region() {
        let mut draft = ScanDraft::default();
        assert_eq!(draft.source, Source::Auto);
        draft.preview = Some(PreviewImage { width: 200, height: 100, dpi: 100, rgba: Vec::new() });
        draft.selection = Some((0, 0, 200, 100));
        assert!(draft.region().is_some_and(|region| region.left == 0 && region.top == 0 && region.width >= 2000 && region.height >= 1000));
        draft.set_source(Source::Feeder);
        assert!(draft.preview.is_none());
        assert!(draft.selection.is_none());
        assert!(draft.region().is_none());
        assert_eq!(draft.source, Source::Feeder);
        draft.left_in = 1.0;
        draft.top_in = 0.0;
        draft.width_in = 2.0;
        draft.height_in = 1.0;
        assert!(draft.region().is_some());
        draft.set_source(Source::Glass);
        assert!(draft.region().is_none());
    }

    #[test]
    fn a_one_pixel_bmp_decodes_with_the_top_at_row_zero() {
        let mut bytes = vec![0u8; 58];
        bytes[0] = b'B';
        bytes[1] = b'M';
        bytes[10] = 54;
        bytes[14] = 40;
        bytes[18] = 1;
        bytes[22] = 1;
        bytes[26] = 1;
        bytes[28] = 24;
        bytes[54] = 0;
        bytes[55] = 0;
        bytes[56] = 255;
        assert!(bmp_rgba(&bytes).is_some_and(|(width, height, rgba)| width == 1 && height == 1 && rgba == [255, 0, 0, 255]));
    }

    /// A BMP with a 4-colour palette (black, red, green, blue) at indices 0..4.
    fn palette_bmp(bits: u16, compression: u32, width: i32, height: i32, body: &[u8]) -> Vec<u8> {
        let colours = 1usize << bits;
        let pixel_at = 54 + colours * 4;
        let mut bytes = vec![0u8; pixel_at];
        bytes[0] = b'B';
        bytes[1] = b'M';
        bytes[10..14].copy_from_slice(&u32::try_from(pixel_at).unwrap_or(0).to_le_bytes());
        bytes[14] = 40;
        bytes[18..22].copy_from_slice(&width.to_le_bytes());
        bytes[22..26].copy_from_slice(&height.to_le_bytes());
        bytes[26] = 1;
        bytes[28..30].copy_from_slice(&bits.to_le_bytes());
        bytes[30..34].copy_from_slice(&compression.to_le_bytes());
        // BGRA palette entries.
        for (i, bgr) in [[0u8, 0, 0], [0, 0, 255], [0, 255, 0], [255, 0, 0]].iter().enumerate() {
            bytes[54 + i * 4..54 + i * 4 + 3].copy_from_slice(bgr);
        }
        bytes.extend_from_slice(body);
        bytes
    }

    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    fn pixels(rgba: &[u8]) -> Vec<[u8; 4]> {
        rgba.chunks(4).map(|px| [px[0], px[1], px[2], px[3]]).collect()
    }

    #[test]
    fn a_four_bit_palette_bmp_decodes_both_nibbles_and_row_order() {
        // 3x2, bottom-up: the first stored row is the bottom one. Rows pad to 4 bytes.
        let bottom = [0x12, 0x30, 0, 0]; // indices 1,2,3
        let top = [0x01, 0x20, 0, 0]; // indices 0,1,2
        let body = [bottom, top].concat();
        let (width, height, rgba) = bmp_rgba(&palette_bmp(4, 0, 3, 2, &body)).expect("4-bit");
        assert_eq!((width, height), (3, 2));
        assert_eq!(pixels(&rgba), vec![BLACK, RED, GREEN, RED, GREEN, BLUE]);
        // The same data top-down (negative height) keeps the stored order.
        let (_, _, down) = bmp_rgba(&palette_bmp(4, 0, 3, -2, &body)).expect("top-down");
        assert_eq!(pixels(&down), vec![RED, GREEN, BLUE, BLACK, RED, GREEN]);
    }

    #[test]
    fn an_eight_bit_palette_bmp_still_decodes() {
        let body = [1, 2, 3, 0, 0, 1, 0, 0];
        let (_, _, rgba) = bmp_rgba(&palette_bmp(8, 0, 3, 2, &body)).expect("8-bit");
        assert_eq!(pixels(&rgba), vec![BLACK, RED, BLACK, RED, GREEN, BLUE]);
    }

    #[test]
    fn rle8_bitmaps_decode_runs_absolute_runs_and_deltas() {
        // 4x2 bottom-up. Bottom row: a run of four index-1 pixels. Top row: an absolute run of
        // three (padded to 4 bytes), then one encoded pixel.
        let body = [4, 1, 0, 0, 0, 3, 2, 3, 1, 0, 1, 2, 0, 0, 0, 1];
        let (_, _, rgba) = bmp_rgba(&palette_bmp(8, 1, 4, 2, &body)).expect("rle8");
        assert_eq!(pixels(&rgba), vec![GREEN, BLUE, RED, GREEN, RED, RED, RED, RED]);
        // A delta skips one pixel right and leaves it at index 0: 3x1.
        let skip = [0, 2, 1, 0, 1, 3, 0, 1];
        let (_, _, rgba) = bmp_rgba(&palette_bmp(8, 1, 3, 1, &skip)).expect("delta");
        assert_eq!(pixels(&rgba), vec![BLACK, BLUE, BLACK]);
    }

    #[test]
    fn rle4_bitmaps_decode_nibble_runs() {
        // 4x1: an encoded run of four pixels alternating indices 1 and 2.
        let (_, _, rgba) = bmp_rgba(&palette_bmp(4, 2, 4, 1, &[4, 0x12, 0, 1])).expect("rle4");
        assert_eq!(pixels(&rgba), vec![RED, GREEN, RED, GREEN]);
        // An absolute run of three nibbles (two bytes, already even): indices 3,2,1.
        let (_, _, rgba) = bmp_rgba(&palette_bmp(4, 2, 3, 1, &[0, 3, 0x32, 0x10, 0, 1])).expect("rle4 absolute");
        assert_eq!(pixels(&rgba), vec![BLUE, GREEN, RED]);
    }

    #[test]
    fn unsupported_or_broken_bmps_fall_back_without_panicking() {
        // Bit fields, JPEG and PNG payloads, 16 bits, and RLE on the wrong depth.
        for (bits, compression) in [(32u16, 3u32), (24, 4), (24, 5), (16, 0), (4, 1), (8, 2), (1, 1)] {
            let mut bytes = palette_bmp(4, 0, 2, 2, &[0; 8]);
            bytes[28..30].copy_from_slice(&bits.to_le_bytes());
            bytes[30..34].copy_from_slice(&compression.to_le_bytes());
            assert!(bmp_rgba(&bytes).is_none(), "{bits} bits, compression {compression}");
        }
        // A truncated RLE stream, a truncated absolute run, and a truncated plain body.
        assert!(bmp_rgba(&palette_bmp(8, 1, 4, 1, &[4])).is_none());
        assert!(bmp_rgba(&palette_bmp(8, 1, 4, 1, &[0, 4, 1, 2])).is_none());
        assert!(bmp_rgba(&palette_bmp(4, 0, 8, 2, &[0x11])).is_none());
        // A run past the right edge is clipped, not an error.
        let (_, _, rgba) = bmp_rgba(&palette_bmp(8, 1, 2, 1, &[5, 1, 0, 1])).expect("clipped");
        assert_eq!(pixels(&rgba), vec![RED, RED]);
        // Junk and a short buffer.
        assert!(bmp_rgba(b"BM").is_none());
        assert!(bmp_rgba(&[]).is_none());
    }

    #[test]
    fn hostile_bmp_headers_neither_allocate_absurdly_nor_panic() {
        let started = std::time::Instant::now();
        // 100000 x 100000 pixels claimed by a file of a few hundred bytes, every depth, both row orders, RLE too.
        for (bits, compression) in [(4u16, 0u32), (8, 0), (4, 2), (8, 1)] {
            for height in [100_000, -100_000] {
                assert!(bmp_rgba(&palette_bmp(bits, compression, 100_000, height, &[0; 32])).is_none(), "{bits} bits {height}");
            }
        }
        let mut huge = palette_bmp(8, 0, 2, 1, &[1, 2, 0, 0]);
        huge[18..22].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(bmp_rgba(&huge).is_none());
        // A palette count of four billion is clamped to what 8 bits can hold; the picture still decodes.
        let mut counted = palette_bmp(8, 0, 2, 1, &[1, 2, 0, 0]);
        counted[46..50].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(bmp_rgba(&counted).map(|(_, _, rgba)| pixels(&rgba)), Some(vec![RED, GREEN]));
        // bfOffBits of 0 means "after the palette"; one inside the headers is refused.
        let mut zero = palette_bmp(8, 0, 2, 1, &[1, 2, 0, 0]);
        zero[10..14].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(bmp_rgba(&zero).map(|(_, _, rgba)| pixels(&rgba)), Some(vec![RED, GREEN]));
        zero[10..14].copy_from_slice(&20u32.to_le_bytes());
        assert!(bmp_rgba(&zero).is_none());
        // An index past the stored table paints black instead of dropping the preview.
        let past = palette_bmp(8, 0, 2, 1, &[200, 1, 0, 0]);
        assert_eq!(bmp_rgba(&past).map(|(_, _, rgba)| pixels(&rgba)), Some(vec![BLACK, RED]));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    fn tiny_bmp_page() -> pdfcraft_platform::scan::Page {
        // 2x2 24-bit BMP (rows padded to 8 bytes).
        let mut bytes = vec![0u8; 54 + 16];
        bytes[0] = b'B';
        bytes[1] = b'M';
        bytes[2] = 70;
        bytes[10] = 54;
        bytes[14] = 40;
        bytes[18] = 2;
        bytes[22] = 2;
        bytes[26] = 1;
        bytes[28] = 24;
        bytes[34] = 16;
        pdfcraft_platform::scan::Page { bytes, format: "bmp", width: 2, height: 2, dpi: 300, thresholded: false }
    }

    /// GUI bookkeeping: pages that stream in over several poll batches all land in the one
    /// document opened by the first batch.
    #[test]
    fn create_keeps_every_page_across_poll_batches() {
        for batches in [vec![3usize], vec![1, 1, 1], vec![1, 2], vec![2, 1], vec![1, 1, 1, 1, 1]] {
            let mut app = crate::PdfCraftApp::new();
            let (mut doc, mut in_doc, mut index) = (None, 0u32, 0u32);
            for n in &batches {
                let pages: Vec<_> = (0..*n).map(|_| tiny_bmp_page()).collect();
                let state = app.apply_create_pages(pages, doc, in_doc, index, None);
                (doc, in_doc, index) = (state.create_doc, state.pages_in_create, state.create_file_index);
            }
            let total: usize = batches.iter().sum();
            assert_eq!(app.views.len(), 1, "batches {batches:?} opened extra documents");
            let id = doc.expect("create doc");
            let count = app.session.get(id).map(|d| d.info.pages.len());
            assert_eq!(count, Some(total), "batches {batches:?}");
            assert_eq!(in_doc as usize, total);
        }
    }

    #[test]
    fn create_file_names_and_remaining_capacity() {
        assert_eq!(super::scan_create_name(0), "Scan.pdf");
        assert_eq!(super::scan_create_name(1), "Scan (2).pdf");
        assert_eq!(super::scan_create_name(2), "Scan (3).pdf");
        assert_eq!(super::pages_remaining(3, 0, false), 3);
        assert_eq!(super::pages_remaining(3, 2, true), 1);
        assert_eq!(super::pages_remaining(3, 3, true), 0);
        assert_eq!(super::pages_remaining(1, 0, true), 1);
    }

    #[test]
    fn job_follows_the_source_the_engine_used() {
        let mut job = super::ScanJob { duplex_source: Some(Source::Feeder), ..test_job(ScanKind::Create, 0, None) };
        job.adopt_source(Source::Auto);
        assert_eq!((job.source, job.duplex_source), (Source::Feeder, Some(Source::Feeder)));
        job.adopt_source(Source::Feeder);
        assert_eq!(job.expected, None);
        // Auto picked the feeder, the engine fell back to the bed: one page, bed source everywhere.
        job.adopt_source(Source::Glass);
        assert_eq!((job.source, job.duplex_source), (Source::Glass, Some(Source::Glass)));
        assert_eq!((job.expected, job.expected_total), (Some(1), Some(1)));
        // A single direct job keeps no duplex source.
        let mut direct = test_job(ScanKind::Create, 2, Some(5));
        direct.adopt_source(Source::Glass);
        assert_eq!((direct.source, direct.duplex_source, direct.expected_total), (Source::Glass, None, Some(3)));
    }

    #[test]
    fn multiple_files_defaults_off() {
        assert_eq!(ScanDraft::default().pages_per_file, 0);
    }

    // ---- Scan interrupted: pure state machine, no scanner ----

    use super::{InterruptAction as A, InterruptKind as K, Pass, ScanKind};
    use pdfcraft_engine::DocId;

    fn test_job(kind: ScanKind, received: u32, expected_total: Option<u32>) -> super::ScanJob {
        super::ScanJob {
            kind,
            create_doc: None,
            pages_in_create: 0,
            create_file_index: 0,
            pages_per_file: None,
            expected: expected_total,
            duplex_source: None,
            received,
            expected_total,
            insert_cursor: None,
            source: Source::Feeder,
            live: pdfcraft_platform::scan::ProgressToken::new(),
            progress: std::sync::Arc::new(std::sync::Mutex::new(super::JobSlot { result: None })),
            cancel: pdfcraft_platform::scan::CancelToken::new(),
        }
    }

    fn finish_with(app: &mut crate::PdfCraftApp, job: super::ScanJob, error: &str) {
        if let Ok(mut slot) = job.progress.lock() {
            slot.result = Some(Err(error.to_string()));
        }
        app.scan.job = Some(job);
        app.poll_scan();
    }

    #[test]
    fn create_interruption_keeps_the_document_and_asks_for_the_rest() {
        let mut job = test_job(ScanKind::Create, 2, Some(5));
        job.create_doc = Some(DocId(7));
        job.pages_in_create = 2;
        job.create_file_index = 1;
        job.pages_per_file = Some(4);
        let state = job.interruption("The scanner has a paper jam.");
        assert_eq!(state.kind, K::Jam);
        assert_eq!(state.remaining(), Some(3));
        assert_eq!(state.actions(), vec![A::Continue, A::Finish]);
        assert_eq!(state.click(), super::ScanClick::Create);
        let resume = state.resume();
        assert_eq!((resume.pages, resume.carried, resume.expected_total), (Some(3), 2, Some(5)));
        assert_eq!((resume.create_doc, resume.pages_in_create, resume.create_file_index, resume.pages_per_file), (Some(DocId(7)), 2, 1, Some(4)));
        assert_eq!(resume.source, Some(Source::Feeder), "pages came in, so the feeder stays the source");
        assert_eq!(state.count_line(), "2 of 5 pages were scanned.");
        assert_eq!(state.primary_label(), "Continue scanning");
    }

    #[test]
    fn insert_interruption_keeps_the_position() {
        let mut job = test_job(ScanKind::Insert(DocId(3)), 2, Some(4));
        job.insert_cursor = Some(6);
        let state = job.interruption("A cover on the scanner is open.");
        assert_eq!(state.kind, K::CoverOpen);
        assert_eq!(state.click(), super::ScanClick::Insert);
        let resume = state.resume();
        assert_eq!((resume.scan_kind, resume.insert_at, resume.pages), (ScanKind::Insert(DocId(3)), Some(6), Some(2)));
    }

    #[test]
    fn all_pages_interruption_has_no_expected_count() {
        let state = test_job(ScanKind::Create, 2, None).interruption("The feeder has no paper.");
        assert_eq!(state.kind, K::FeederEmpty);
        assert_eq!((state.expected, state.remaining()), (None, None));
        assert_eq!(state.actions(), vec![A::Continue, A::Finish]);
        assert_eq!(state.resume().pages, None, "the next leg asks for all the pages again");
        assert_eq!(state.count_line(), "2 pages were scanned.");
        assert_eq!(test_job(ScanKind::Create, 1, None).interruption("x").count_line(), "1 page was scanned.");
    }

    #[test]
    fn nothing_left_to_scan_offers_only_finish() {
        let state = test_job(ScanKind::Create, 3, Some(3)).interruption("The scanner is offline.");
        assert!(!state.can_continue());
        assert_eq!(state.actions(), vec![A::Finish]);
    }

    #[test]
    fn preflight_refusal_offers_try_again() {
        let mut app = crate::PdfCraftApp::new();
        finish_with(&mut app, test_job(ScanKind::Create, 0, Some(3)), "A cover on the scanner is open.");
        let state = app.scan.interrupted.clone().expect("interrupted");
        assert_eq!(app.dialog, Some(crate::Dialog::ScanInterrupted));
        assert_eq!(state.actions(), vec![A::TryAgain, A::Finish]);
        assert_eq!(state.primary_label(), "Try again");
        assert_eq!(state.instruction_text(), "Close the feeder cover and check the paper, then choose Try again.");
        assert_eq!(state.count_line(), "No pages were scanned.");
        assert!(state.kept_line().is_none());
        assert_eq!(state.resume().pages, Some(3), "start again with the original count");
        assert_eq!(state.resume().source, None, "nothing came in, so Auto stays Auto");
    }

    #[test]
    fn each_kind_selects_its_instruction() {
        let say = |kind, retry| super::instruction(kind, retry);
        assert!(say(K::CoverOpen, false).is_some_and(|text| text.starts_with("Close the feeder cover") && text.ends_with("choose Continue.")));
        assert!(say(K::Jam, false).is_some_and(|text| text.starts_with("Clear the jam") && text.contains("squarely")));
        assert!(say(K::FeederEmpty, false).is_some_and(|text| text.starts_with("Load more sheets") && text.contains("Finish")));
        assert!(say(K::MultiFeed, false).is_some_and(|text| text.starts_with("Separate the sheets")));
        assert!(say(K::PaperProblem, false).is_some_and(|text| text.contains("any jam or sheets fed together") && !text.contains("Clear the jam")));
        assert!(say(K::Offline, false).is_some_and(|text| text.starts_with("Check the scanner is on")));
        assert!(say(K::Attention, false).is_some_and(|text| text.starts_with("Look at the scanner")));
        assert!(say(K::Busy, false).is_some_and(|text| text.starts_with("Wait for the scanner")));
        assert!(say(K::DeviceError, false).is_some_and(|text| text.starts_with("Switch the scanner off and on")));
        assert_eq!(say(K::Other, false), None);
        assert_eq!(say(K::Other, true), None);
        assert!(say(K::BusyOrFeederEmpty, false).is_some_and(|text| text.starts_with("Check that the feeder has paper")));
        let all = [
            K::CoverOpen,
            K::Jam,
            K::PaperProblem,
            K::FeederEmpty,
            K::MultiFeed,
            K::Offline,
            K::Attention,
            K::Busy,
            K::BusyOrFeederEmpty,
            K::DeviceError,
        ];
        for retry in [false, true] {
            let texts: Vec<_> = all.iter().filter_map(|kind| say(*kind, retry)).collect();
            assert_eq!(texts.len(), all.len());
            let distinct: std::collections::BTreeSet<_> = texts.iter().collect();
            assert_eq!(distinct.len(), all.len(), "every cause has its own instruction");
            assert!(texts.iter().all(|text| text.contains(if retry { "Try again" } else { "Continue" })));
        }
        // Other shows the engine's own sentence.
        let other = test_job(ScanKind::Create, 1, None).interruption("The scan stopped before all pages arrived.");
        assert_eq!(other.instruction_text(), "The scan stopped before all pages arrived.");
        // The engine's sentences reach the right kind end to end.
        for (message, kind) in [
            ("Only 1 of 3 pages were scanned: the feeder reported no more paper.", K::FeederEmpty),
            ("The feeder took more than one sheet.", K::MultiFeed),
            ("The scanner has a paper problem.", K::PaperProblem),
            ("The scanner needs attention. Check it, then try again.", K::Attention),
            ("The scanner lamp is off.", K::DeviceError),
            ("The scanner is busy. Close any other program that is using it, or wait for the current scan to finish, then try again.", K::Busy),
            ("The scanner is busy, or the feeder has no paper.", K::BusyOrFeederEmpty),
        ] {
            assert_eq!(test_job(ScanKind::Create, 1, Some(3)).interruption(message).kind, kind, "{message}");
        }
    }

    /// A 1-pixel-high BMP `w` pixels wide; at 72 dpi the page is `w` points wide, so `w` tags a page.
    fn tagged(w: u32) -> pdfcraft_platform::scan::Page {
        let row = (w * 3).div_ceil(4) * 4;
        let mut bytes = vec![0u8; 54 + row as usize];
        bytes[0] = b'B';
        bytes[1] = b'M';
        bytes[10] = 54;
        bytes[14] = 40;
        bytes[18..22].copy_from_slice(&w.to_le_bytes());
        bytes[22] = 1;
        bytes[26] = 1;
        bytes[28] = 24;
        bytes[34..38].copy_from_slice(&row.to_le_bytes());
        pdfcraft_platform::scan::Page { bytes, format: "bmp", width: w, height: 1, dpi: 72, thresholded: false }
    }

    fn widths(app: &crate::PdfCraftApp, id: DocId) -> Vec<u32> {
        app.session.get(id).map(|doc| doc.info.pages.iter().map(|page| page.width.round() as u32).collect()).unwrap_or_default()
    }

    fn fronts_job(kind: ScanKind, received: u32) -> super::ScanJob {
        let mut job = test_job(kind, received, None);
        job.duplex_source = Some(Source::Feeder);
        job
    }

    fn fail_current(app: &mut crate::PdfCraftApp, error: &str) {
        if let Some(job) = &app.scan.job
            && let Ok(mut slot) = job.progress.lock()
        {
            slot.result = Some(Err(error.to_string()));
        }
        app.poll_scan();
    }

    fn succeed_current(app: &mut crate::PdfCraftApp) {
        if let Some(job) = &app.scan.job
            && let Ok(mut slot) = job.progress.lock()
        {
            slot.result = Some(Ok(Vec::new()));
        }
        app.poll_scan();
    }

    /// Scan three fronts (10 11 12) into a new document the way a live Create does; returns its id.
    fn create_three_fronts(app: &mut crate::PdfCraftApp) -> DocId {
        app.scan.draft.manual_duplex = true;
        app.scan.job = Some(fronts_job(ScanKind::Create, 0));
        app.apply_scanned_pages(ScanKind::Create, vec![tagged(10)]);
        app.apply_scanned_pages(ScanKind::Create, vec![tagged(11), tagged(12)]);
        app.scan.duplex.expect("fronts are noted").doc
    }

    /// The reverse-side pass over `backs` (in scan order), one page per poll like a live scan.
    fn scan_backs(app: &mut crate::PdfCraftApp, received: u32, backs: &[u32]) {
        app.scan.job = Some(test_job(ScanKind::ReverseSides, received, None));
        for back in backs {
            app.apply_scanned_pages(ScanKind::ReverseSides, vec![tagged(*back)]);
        }
    }

    /// Merge by the same rule the live path uses, on plain numbers.
    fn merged(mut doc: Vec<u32>, first: usize, fronts: usize, backs: &[u32], reversed: bool) -> Vec<u32> {
        for (index, back) in backs.iter().enumerate() {
            let at = super::back_position(first, fronts, index, reversed).expect("a front is left");
            doc.insert(at, *back);
        }
        doc
    }

    #[test]
    fn back_position_follows_the_flipped_stack_or_scan_order() {
        // Fronts 1 2 3 4; the stack was flipped as a whole: the backs of 4, 3, 2, 1 arrive.
        assert_eq!(merged(vec![1, 2, 3, 4], 0, 4, &[14, 13, 12, 11], true), vec![1, 11, 2, 12, 3, 13, 4, 14]);
        // Not flipped: the backs arrive in front order and each shifts the rest down.
        assert_eq!(merged(vec![1, 2, 3, 4], 0, 4, &[11, 12, 13, 14], false), vec![1, 11, 2, 12, 3, 13, 4, 14]);
        // Fewer backs than fronts: they belong to the LAST fronts.
        assert_eq!(merged(vec![1, 2, 3, 4], 0, 4, &[14, 13], true), vec![1, 2, 3, 13, 4, 14]);
        assert_eq!(merged(vec![1, 2, 3, 4], 0, 4, &[11, 12], false), vec![1, 11, 2, 12, 3, 4]);
        assert_eq!(merged(vec![1, 2], 0, 2, &[], true), vec![1, 2]);
        // Fronts that start part-way into the document.
        assert_eq!(merged(vec![7, 8, 1, 2, 9], 2, 2, &[12, 11], true), vec![7, 8, 1, 11, 2, 12, 9]);
        assert_eq!(super::back_position(0, 2, 0, true), Some(2));
        assert_eq!(super::back_position(0, 2, 1, true), Some(1));
        assert_eq!(super::back_position(0, 2, 2, true), None, "no front is left for a third back");
        assert_eq!(super::back_position(0, 0, 0, true), None);
        assert_eq!(super::reverse_pages(0, 0), 1, "never asks the scanner for zero pages");
        assert_eq!(super::reverse_pages(3, 1), 2);
    }

    #[test]
    fn fronts_enter_the_document_as_they_arrive() {
        let mut app = crate::PdfCraftApp::new();
        app.scan.draft.manual_duplex = true;
        app.scan.job = Some(fronts_job(ScanKind::Create, 0));
        app.apply_scanned_pages(ScanKind::Create, vec![tagged(10)]);
        let held = app.scan.duplex.expect("the first front is noted");
        assert_eq!((held.first, held.fronts, held.backs, held.source), (0, 1, 0, Source::Feeder));
        assert_eq!(widths(&app, held.doc), vec![10], "the document already shows the first front");
        app.apply_scanned_pages(ScanKind::Create, vec![tagged(11), tagged(12)]);
        assert_eq!(widths(&app, held.doc), vec![10, 11, 12]);
        assert_eq!(app.scan.duplex.map(|pending| pending.fronts), Some(3));
        assert_eq!(app.scan.job.as_ref().map(|job| job.received), Some(3));
    }

    #[test]
    fn the_first_pass_ending_opens_the_flip_step_and_keeps_the_fronts() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        app.dialog = None;
        succeed_current(&mut app);
        assert!(app.scan.job.is_none());
        assert_eq!(app.dialog, Some(crate::Dialog::Scanner));
        assert_eq!(app.scan.duplex.map(|pending| pending.fronts), Some(3));
        assert_eq!(widths(&app, id), vec![10, 11, 12]);
        // The user never scans the backs: the fronts stay, nothing is added or lost.
        app.cancel_reverse_sides();
        assert!(app.scan.duplex.is_none());
        assert_eq!(app.dialog, None);
        assert_eq!(widths(&app, id), vec![10, 11, 12]);
        assert!(app.scan.last_create_files >= 1);
    }

    #[test]
    fn reverse_sides_land_after_their_fronts_as_they_arrive() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        app.scan.job = Some(test_job(ScanKind::ReverseSides, 0, Some(3)));
        app.apply_scanned_pages(ScanKind::ReverseSides, vec![tagged(112)]);
        assert_eq!(widths(&app, id), vec![10, 11, 12, 112], "the first back belongs to the last front");
        app.apply_scanned_pages(ScanKind::ReverseSides, vec![tagged(111), tagged(110)]);
        assert_eq!(widths(&app, id), vec![10, 110, 11, 111, 12, 112]);
        assert_eq!(app.scan.duplex.map(|pending| pending.backs), Some(3));
        succeed_current(&mut app);
        assert!(app.scan.duplex.is_none() && app.scan.interrupted.is_none());
        assert_eq!(app.dialog, None);
        assert_eq!(widths(&app, id), vec![10, 110, 11, 111, 12, 112]);
    }

    #[test]
    fn an_extra_reverse_side_is_not_inserted() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        scan_backs(&mut app, 0, &[112, 111, 110, 999]);
        assert_eq!(widths(&app, id), vec![10, 110, 11, 111, 12, 112]);
    }

    #[test]
    fn a_fronts_interruption_keeps_the_inserted_fronts() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        fail_current(&mut app, "The scanner has a paper jam.");
        let state = app.scan.interrupted.clone().expect("interrupted");
        assert_eq!((state.pass, state.received), (Pass::Fronts, 3));
        assert_eq!(state.actions(), vec![A::Continue, A::ReverseNow, A::Finish]);
        assert_eq!(app.dialog, Some(crate::Dialog::ScanInterrupted));
        assert_eq!(widths(&app, id), vec![10, 11, 12]);
        let resume = state.resume();
        assert_eq!((resume.carried, resume.create_doc), (3, Some(id)));
        // A resumed first pass carries on after the last front, wherever the selection moved to.
        app.scan.interrupted = None;
        app.scan.job = Some({
            let mut job = fronts_job(ScanKind::Create, 3);
            job.create_doc = Some(id);
            job.pages_in_create = 3;
            job
        });
        app.views.iter_mut().for_each(|view| view.select_pages(&[0]));
        app.apply_scanned_pages(ScanKind::Create, vec![tagged(13)]);
        assert_eq!(widths(&app, id), vec![10, 11, 12, 13]);
        assert_eq!(app.scan.duplex.map(|pending| (pending.first, pending.fronts)), Some((0, 4)));
    }

    #[test]
    fn finish_after_a_fronts_interruption_keeps_the_fronts_as_pages() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        fail_current(&mut app, "The feeder has no paper.");
        app.resolve_interrupt(A::Finish);
        assert!(app.scan.duplex.is_none() && app.scan.interrupted.is_none());
        assert_eq!(app.dialog, None);
        assert_eq!(widths(&app, id), vec![10, 11, 12]);
    }

    #[test]
    fn reverse_now_after_a_fronts_interruption_opens_the_flip_step() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        fail_current(&mut app, "The feeder has no paper.");
        app.resolve_interrupt(A::ReverseNow);
        assert_eq!(app.dialog, Some(crate::Dialog::Scanner));
        assert!(app.scan.interrupted.is_none());
        assert_eq!(app.scan.duplex.map(|pending| pending.fronts), Some(3));
        assert_eq!(widths(&app, id), vec![10, 11, 12]);
    }

    #[test]
    fn a_reverse_pass_interruption_keeps_the_backs_and_resumes_at_the_right_front() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        scan_backs(&mut app, 0, &[112]);
        if let Some(job) = &mut app.scan.job {
            job.expected_total = Some(3);
        }
        fail_current(&mut app, "The scanner has a paper jam.");
        let state = app.scan.interrupted.clone().expect("interrupted");
        assert_eq!((state.pass, state.received, state.remaining()), (Pass::Backs, 1, Some(2)));
        assert_eq!(state.actions(), vec![A::Continue, A::Finish]);
        assert_eq!(state.click(), super::ScanClick::ReverseSides);
        assert_eq!(state.resume().pages, Some(2));
        assert_eq!(app.scan.duplex.map(|pending| pending.backs), Some(1));
        assert_eq!(widths(&app, id), vec![10, 11, 12, 112]);
        // The next leg continues with the back of the second-to-last front.
        app.scan.interrupted = None;
        scan_backs(&mut app, 1, &[111, 110]);
        assert_eq!(widths(&app, id), vec![10, 110, 11, 111, 12, 112]);
        succeed_current(&mut app);
        assert!(app.scan.duplex.is_none());
    }

    #[test]
    fn finish_after_too_few_reverse_sides_pairs_the_last_fronts() {
        let mut app = crate::PdfCraftApp::new();
        let id = create_three_fronts(&mut app);
        scan_backs(&mut app, 0, &[112]);
        succeed_current(&mut app);
        let state = app.scan.interrupted.clone().expect("interrupted");
        assert_eq!((state.pass, state.received, state.remaining()), (Pass::Backs, 1, Some(2)));
        assert_eq!(state.kind, K::Other);
        assert!(state.instruction_text().contains("Manual duplex needs one reverse side for each front."));
        assert!(state.instruction_text().ends_with("Scan the missing reverse sides, then choose Finish."));
        assert_eq!(app.dialog, Some(crate::Dialog::ScanInterrupted));
        app.resolve_interrupt(A::Finish);
        assert!(app.scan.duplex.is_none());
        assert_eq!(app.dialog, None);
        assert_eq!(widths(&app, id), vec![10, 11, 12, 112]);
    }

    #[test]
    fn insert_puts_fronts_at_the_chosen_gap_and_backs_after_them() {
        let mut app = crate::PdfCraftApp::new();
        app.scan.draft.manual_duplex = true;
        let state = app.apply_create_pages(vec![tagged(1), tagged(2), tagged(3), tagged(4)], None, 0, 0, None);
        let id = state.create_doc.expect("document");
        assert_eq!(widths(&app, id), vec![1, 2, 3, 4]);
        if let Some(view) = app.views.iter_mut().find(|view| view.id == id) {
            view.insert_at = Some(2);
        }
        let cursor = app.insert_cursor_for(id);
        assert_eq!(cursor, Some(2));
        let mut job = fronts_job(ScanKind::Insert(id), 0);
        job.insert_cursor = cursor;
        app.scan.job = Some(job);
        app.apply_scanned_pages(ScanKind::Insert(id), vec![tagged(20)]);
        assert_eq!(widths(&app, id), vec![1, 2, 20, 3, 4]);
        app.apply_scanned_pages(ScanKind::Insert(id), vec![tagged(21), tagged(22)]);
        assert_eq!(widths(&app, id), vec![1, 2, 20, 21, 22, 3, 4]);
        let held = app.scan.duplex.expect("fronts noted");
        assert_eq!((held.doc, held.first, held.fronts, held.kind), (id, 2, 3, ScanKind::Insert(id)));
        succeed_current(&mut app);
        assert_eq!(app.dialog, Some(crate::Dialog::Scanner));
        scan_backs(&mut app, 0, &[122, 121, 120]);
        assert_eq!(widths(&app, id), vec![1, 2, 20, 120, 21, 121, 22, 122, 3, 4]);
        succeed_current(&mut app);
        assert!(app.scan.duplex.is_none());
        assert_eq!(widths(&app, id), vec![1, 2, 20, 120, 21, 121, 22, 122, 3, 4]);
    }

    #[test]
    fn a_resumed_insert_pass_continues_at_the_cursor() {
        let mut app = crate::PdfCraftApp::new();
        let state = app.apply_create_pages(vec![tagged(1), tagged(2)], None, 0, 0, None);
        let id = state.create_doc.expect("document");
        let mut job = fronts_job(ScanKind::Insert(id), 0);
        job.insert_cursor = Some(1);
        app.scan.job = Some(job);
        app.apply_scanned_pages(ScanKind::Insert(id), vec![tagged(20), tagged(21)]);
        fail_current(&mut app, "The scanner has a paper jam.");
        let state = app.scan.interrupted.clone().expect("interrupted");
        assert_eq!((state.pass, state.received, state.insert_at), (Pass::Fronts, 2, Some(3)));
        assert_eq!(widths(&app, id), vec![1, 20, 21, 2]);
        // Resume: the job starts from the interrupted cursor.
        let mut job = fronts_job(ScanKind::Insert(id), 2);
        job.insert_cursor = state.resume().insert_at;
        app.scan.job = Some(job);
        app.apply_scanned_pages(ScanKind::Insert(id), vec![tagged(22)]);
        assert_eq!(widths(&app, id), vec![1, 20, 21, 22, 2]);
        assert_eq!(app.scan.duplex.map(|pending| (pending.first, pending.fronts)), Some((1, 3)));
    }

    #[test]
    fn stopping_a_scan_is_not_an_interruption() {
        let mut app = crate::PdfCraftApp::new();
        let job = test_job(ScanKind::Create, 0, Some(3));
        job.cancel.request();
        finish_with(&mut app, job, "the scan was cancelled");
        assert!(app.scan.interrupted.is_none());
        assert_ne!(app.dialog, Some(crate::Dialog::ScanInterrupted));
        assert_eq!(app.scan.draft.error.as_deref(), Some("the scan was cancelled"));
    }

    #[test]
    fn a_continue_that_cannot_start_returns_to_the_dialog() {
        let mut app = crate::PdfCraftApp::new();
        finish_with(&mut app, test_job(ScanKind::Create, 1, Some(3)), "The scanner has a paper jam.");
        app.resolve_interrupt(A::Continue);
        // No scanner is listed in a test app, so the next acquire cannot start.
        assert!(app.scan.job.is_none());
        assert!(app.scan.interrupted.is_some());
        assert_eq!(app.dialog, Some(crate::Dialog::ScanInterrupted));
        assert!(app.scan.draft.error.is_some());
        app.resolve_interrupt(A::Finish);
        assert!(app.scan.interrupted.is_none());
        assert_eq!(app.dialog, None);
    }
}

#[cfg(test)]
mod device_state_tests {
    use super::{DetailsState, DeviceList};
    use pdfcraft_platform::scan::{Connection, HandlingStatus, Scanner, ScannerRef};

    const BUSY: &str = "The scanner is busy. Close any other program that is using it, then try again.";

    fn item(id: &str) -> ScannerRef {
        ScannerRef::new(id.into(), format!("Scanner {id}"), None)
    }

    fn details(id: &str) -> Scanner {
        Scanner {
            id: id.into(),
            name: format!("Scanner {id}"),
            feeder: true,
            flatbed: true,
            duplex: true,
            feeder_ready: false,
            handling_status: HandlingStatus::default(),
            feeder_capacity: None,
            connection: Connection::Unknown,
            port: None,
            optical_x: Some(600),
            optical_y: Some(600),
            pages: None,
            bed: None,
            sheet: None,
            glass: None,
            feeder_choices: None,
        }
    }

    /// Run a list load with an injected loader; returns the selection the dialog would take.
    fn list(devices: &mut DeviceList, current: &str, loader: impl FnOnce() -> Result<Vec<ScannerRef>, String>) -> Option<String> {
        let generation = devices.begin_list()?;
        devices.finish_list(generation, loader(), current)
    }

    /// Select `id` and run its details load with an injected loader.
    fn select(devices: &mut DeviceList, id: &str, loader: impl FnOnce(&str) -> Result<Scanner, String>) -> bool {
        match devices.want_details(id) {
            Some(generation) => devices.finish_details(generation, id, loader(id)),
            None => false,
        }
    }

    #[test]
    fn list_ok_shows_every_device_and_selects_the_first() {
        let mut devices = DeviceList::default();
        let chosen = list(&mut devices, "", || Ok(vec![item("a"), item("b")]));
        assert_eq!(chosen.as_deref(), Some("a"));
        assert_eq!(devices.refs.len(), 2);
        assert!(!devices.loading && devices.error.is_none());
        assert_eq!(devices.selected_ref("b").map(|r| r.id.as_str()), Some("b"));
        assert_eq!(devices.selected_ref("gone").map(|r| r.id.as_str()), Some("a"));
    }

    #[test]
    fn last_used_device_stays_selected_when_listed() {
        let mut devices = DeviceList::default();
        assert_eq!(list(&mut devices, "b", || Ok(vec![item("a"), item("b")])).as_deref(), Some("b"));
    }

    #[test]
    fn list_empty_and_list_error() {
        let mut devices = DeviceList::default();
        assert_eq!(list(&mut devices, "a", || Ok(Vec::new())).as_deref(), Some(""));
        assert!(devices.refs.is_empty() && devices.error.is_none() && devices.listed);
        assert!(devices.want_details("").is_none(), "no device, nothing to load");
        let mut devices = DeviceList::default();
        assert_eq!(list(&mut devices, "", || Err("scanning needs Windows and a WIA driver".into())), None);
        assert_eq!(devices.error.as_deref(), Some("scanning needs Windows and a WIA driver"));
        assert!(!devices.loading && devices.listed);
    }

    #[test]
    fn only_one_list_load_runs_at_a_time() {
        let mut devices = DeviceList::default();
        let first = devices.begin_list();
        assert!(first.is_some() && devices.begin_list().is_none());
    }

    #[test]
    fn details_ok_gives_the_real_capabilities() {
        let mut devices = DeviceList::default();
        list(&mut devices, "", || Ok(vec![item("a")]));
        assert!(select(&mut devices, "a", |id| Ok(details(id))));
        let picked = devices.selected_ref("a").cloned().unwrap_or_else(|| item("a"));
        assert_eq!(devices.scanner_for(&picked).map(|s| s.duplex), Some(true));
        assert!(!devices.checking());
    }

    #[test]
    fn details_loading_has_no_scanner_yet() {
        let mut devices = DeviceList::default();
        list(&mut devices, "", || Ok(vec![item("a")]));
        assert!(devices.want_details("a").is_some());
        assert!(devices.checking());
        assert!(devices.scanner_selected("a").is_none());
        assert!(devices.want_details("a").is_none(), "already loading: no second load");
    }

    #[test]
    fn busy_device_stays_listed_selected_and_scannable_with_defaults() {
        let mut devices = DeviceList::default();
        list(&mut devices, "", || Ok(vec![item("a"), item("b")]));
        assert!(select(&mut devices, "a", |_| Err(BUSY.to_string())));
        assert_eq!(devices.refs.len(), 2, "a busy device is not dropped");
        assert_eq!(devices.details_error(), Some(BUSY));
        let fallback = devices.scanner_selected("a");
        assert!(fallback.as_ref().is_some_and(|s| s.id == "a" && s.flatbed && s.feeder && !s.duplex));
        assert!(fallback.is_some_and(|s| s.handling_status.acquire_block_reason().is_none()));
    }

    #[test]
    fn stale_details_result_is_ignored() {
        let mut devices = DeviceList::default();
        list(&mut devices, "", || Ok(vec![item("a"), item("b")]));
        let slow = devices.want_details("a");
        let fast = devices.want_details("b");
        assert!(slow.is_some() && fast.is_some());
        // The result for A arrives after B was selected.
        assert!(!devices.finish_details(slow.unwrap_or(0), "a", Ok(details("a"))));
        assert!(devices.checking(), "still waiting for B");
        // Same id but an older generation is stale too.
        assert!(!devices.finish_details(slow.unwrap_or(0), "b", Ok(details("b"))));
        assert!(devices.finish_details(fast.unwrap_or(0), "b", Ok(details("b"))));
        assert!(matches!(devices.details, DetailsState::Ready(ref s) if s.id == "b"));
    }

    #[test]
    fn reselecting_a_device_loads_it_again() {
        let mut devices = DeviceList::default();
        list(&mut devices, "", || Ok(vec![item("a"), item("b")]));
        assert!(select(&mut devices, "a", |id| Ok(details(id))));
        assert!(select(&mut devices, "b", |id| Ok(details(id))));
        assert!(devices.want_details("b").is_none(), "unchanged selection does not reload");
        assert!(devices.want_details("a").is_some(), "switching back reads A again");
    }

    #[test]
    fn retry_reloads_a_failed_device_only() {
        let mut devices = DeviceList::default();
        list(&mut devices, "", || Ok(vec![item("a")]));
        assert!(devices.retry_details().is_none(), "nothing failed yet");
        assert!(select(&mut devices, "a", |_| Err(BUSY.to_string())));
        let generation = devices.retry_details();
        assert!(generation.is_some() && devices.checking());
        assert!(devices.finish_details(generation.unwrap_or(0), "a", Ok(details("a"))));
        assert!(devices.details_error().is_none());
        assert!(devices.retry_details().is_none(), "a ready device is not retried");
    }

    #[test]
    fn refresh_forgets_details_so_the_selection_is_checked_again() {
        let mut devices = DeviceList::default();
        list(&mut devices, "", || Ok(vec![item("a")]));
        assert!(select(&mut devices, "a", |_| Err(BUSY.to_string())));
        list(&mut devices, "a", || Ok(vec![item("a")]));
        assert!(devices.want_details("a").is_some());
    }

    #[test]
    fn stale_list_result_is_ignored() {
        let mut devices = DeviceList::default();
        let generation = devices.begin_list().unwrap_or(0);
        assert_eq!(devices.finish_list(generation + 1, Ok(vec![item("a")]), ""), None);
        assert!(devices.refs.is_empty() && devices.loading);
    }
}

#[cfg(test)]
mod connection_tag_tests {
    use super::{base_name, connection_label, default_device_id, device_label, same_printer_note};
    use pdfcraft_platform::scan::{Connection, ScannerRef};

    fn dev(id: &str, name: &str, connection: Connection) -> ScannerRef {
        let mut item = ScannerRef::new(id.into(), name.into(), None);
        item.connection = connection;
        item
    }

    fn pair() -> Vec<ScannerRef> {
        vec![dev("w", "Example MFC-100 [001122334455]", Connection::Wsd), dev("u", "Example MFC-100", Connection::Usb)]
    }

    #[test]
    fn labels_by_connection() {
        assert_eq!(connection_label(Connection::Usb), Some("USB"));
        assert_eq!(connection_label(Connection::Wsd), Some("Network"));
        assert_eq!(connection_label(Connection::Unknown), None);
        assert_eq!(device_label("Scan X", Connection::Usb), "Scan X  · USB");
        assert_eq!(device_label("Scan X", Connection::Wsd), "Scan X  · Network");
        assert_eq!(device_label("Scan X", Connection::Unknown), "Scan X");
    }

    #[test]
    fn base_name_strips_bracket_suffix() {
        assert_eq!(base_name("Example [3c2a]"), "Example");
        assert_eq!(base_name("Example"), "Example");
        assert_eq!(base_name("[x]"), "[x]");
        assert_eq!(base_name("A [b] c"), "A [b] c");
    }

    #[test]
    fn same_printer_is_flagged_both_ways() {
        let refs = pair();
        assert_eq!(same_printer_note(&refs, &refs[0]).as_deref(), Some("The same printer is also available over USB."));
        assert_eq!(same_printer_note(&refs, &refs[1]).as_deref(), Some("The same printer is also available over the network."));
        let other = vec![dev("a", "Example MFC", Connection::Usb), dev("b", "Epson", Connection::Wsd), dev("c", "EXAMPLE mfc", Connection::Usb)];
        assert_eq!(same_printer_note(&other, &other[0]), None, "different connection required");
        assert_eq!(same_printer_note(&other, &other[1]), None);
        let unknown = vec![dev("a", "Example", Connection::Unknown), dev("b", "example [1]", Connection::Wsd)];
        assert_eq!(same_printer_note(&unknown, &unknown[0]), None);
        assert_eq!(same_printer_note(&unknown, &unknown[1]), None);
    }

    #[test]
    fn default_prefers_usb_of_a_pair() {
        assert_eq!(default_device_id(&pair()).as_deref(), Some("u"));
        let lone = vec![dev("w", "Example [1]", Connection::Wsd), dev("u", "Other", Connection::Usb)];
        assert_eq!(default_device_id(&lone).as_deref(), Some("w"));
        assert_eq!(default_device_id(&[]), None);
    }

    #[test]
    fn remembered_selection_is_unchanged() {
        let mut devices = super::DeviceList::default();
        let generation = devices.begin_list();
        let chosen = generation.and_then(|g| devices.finish_list(g, Ok(pair()), "w"));
        assert_eq!(chosen.as_deref(), Some("w"));
        let generation = devices.begin_list();
        let chosen = generation.and_then(|g| devices.finish_list(g, Ok(pair()), ""));
        assert_eq!(chosen.as_deref(), Some("u"));
        let generation = devices.begin_list();
        let chosen = generation.and_then(|g| devices.finish_list(g, Ok(pair()), "gone"));
        assert_eq!(chosen.as_deref(), Some("w"));
    }
}
