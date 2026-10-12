//! Logical editing with a separate, shaped bidirectional view.
//!
//! Never put a reordered string into TextEdit's mutable buffer: its clipboard,
//! history and scalar offsets then describe the wrong document. Here all edits
//! use logical UTF-8 ranges. Only paint pieces are reordered. Selection can paint
//! several fragments while still representing one contiguous logical range.

use std::{ops::Range, sync::Arc};

use egui::{Color32, Event, Id, Key, Pos2, Rect, Response, Sense, Vec2, text::LayoutJob};
use unicode_bidi::{BidiInfo, Level};
use unicode_segmentation::UnicodeSegmentation;

const MAX_BYTES: usize = 4_000_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Caret {
    byte: usize,
    after: bool,
}

#[derive(Clone, Debug)]
struct Cell {
    range: Range<usize>,
    rect: Rect,
}

#[derive(Clone, Debug)]
struct Stop {
    caret: Caret,
    rect: Rect,
    row: usize,
}

#[derive(Clone)]
struct Piece {
    galley: Arc<egui::Galley>,
    pos: Pos2,
    row: usize,
}

/// Text pieces, grapheme cells and caret stops refer to the same logical job.
#[derive(Clone, Default)]
pub(crate) struct Layout {
    pieces: Vec<Piece>,
    cells: Vec<Cell>,
    stops: Vec<Stop>,
    pub(crate) size: Vec2,
}

impl Layout {
    fn stop(&self, caret: Caret) -> Option<&Stop> {
        self.stops.iter().find(|s| s.caret == caret).or_else(|| self.stops.iter().find(|s| s.caret.byte == caret.byte))
    }

    fn hit(&self, pos: Pos2) -> Caret {
        self.stops.iter().min_by(|a, b| distance(a.rect, pos).total_cmp(&distance(b.rect, pos))).map_or(Caret::default(), |s| s.caret)
    }

    fn horizontal(&self, caret: Caret, right: bool) -> Caret {
        let at = self.stops.iter().position(|s| s.caret == caret).or_else(|| self.stops.iter().position(|s| s.caret.byte == caret.byte));
        let Some(at) = at else { return caret };
        let next = if right { at.saturating_add(1) } else { at.saturating_sub(1) };
        self.stops.get(next).map_or(caret, |s| s.caret)
    }

    fn vertical(&self, caret: Caret, down: bool) -> Caret {
        let Some(stop) = self.stop(caret) else { return caret };
        let row = if down { stop.row.saturating_add(1) } else { stop.row.saturating_sub(1) };
        self.stops
            .iter()
            .filter(|s| s.row == row)
            .min_by(|a, b| (a.rect.left() - stop.rect.left()).abs().total_cmp(&(b.rect.left() - stop.rect.left()).abs()))
            .map_or(caret, |s| s.caret)
    }

    fn edge(&self, caret: Caret, right: bool) -> Caret {
        let Some(stop) = self.stop(caret) else { return caret };
        let row: Vec<_> = self.stops.iter().filter(|s| s.row == stop.row).collect();
        if right { row.last() } else { row.first() }.map_or(caret, |s| s.caret)
    }

    /// Source rows may have their own baseline and horizontal anchor. Transform
    /// paint, selection and caret together, rather than just moving the glyphs.
    pub(crate) fn place_rows(&mut self, mut position: impl FnMut(usize, f32, f32, f32) -> Vec2) {
        let mut offsets = Vec::new();
        for row in 0..self.stops.iter().map(|s| s.row).max().unwrap_or(0).saturating_add(1) {
            let bounds = self.stops.iter().filter(|s| s.row == row).fold(Rect::NOTHING, |r, s| r.union(s.rect));
            let baseline = self
                .pieces
                .iter()
                .find(|p| p.row == row)
                .and_then(|p| p.galley.rows.first().and_then(|r| r.glyphs.first()).map(|g| p.pos.y + g.pos.y - bounds.top()))
                .unwrap_or(bounds.height());
            offsets.push(position(row, bounds.width(), bounds.height(), baseline) - bounds.min.to_vec2());
        }
        for piece in &mut self.pieces {
            piece.pos += offsets.get(piece.row).copied().unwrap_or_default();
        }
        // Cells are assigned a row by their original y before any transformation.
        for cell in &mut self.cells {
            let row = self.stops.iter().find(|s| s.rect.top() == cell.rect.top()).map_or(0, |s| s.row);
            cell.rect = cell.rect.translate(offsets.get(row).copied().unwrap_or_default());
        }
        for stop in &mut self.stops {
            stop.rect = stop.rect.translate(offsets.get(stop.row).copied().unwrap_or_default());
        }
        let bounds = self.stops.iter().fold(Rect::NOTHING, |r, s| r.union(s.rect));
        self.size = egui::vec2(bounds.right().max(1.0), bounds.bottom().max(1.0));
    }
}

fn distance(rect: Rect, pos: Pos2) -> f32 {
    let dy = if pos.y < rect.top() {
        rect.top() - pos.y
    } else if pos.y > rect.bottom() {
        pos.y - rect.bottom()
    } else {
        0.0
    };
    dy * 10_000.0 + (pos.x - rect.left()).abs()
}

fn format_at(job: &LayoutJob, at: usize) -> egui::TextFormat {
    job.sections
        .iter()
        .find(|s| s.byte_range.contains(&at.into()))
        .or_else(|| job.sections.last())
        .map_or_else(Default::default, |s| s.format.clone())
}

fn slice_job(job: &LayoutJob, range: Range<usize>) -> LayoutJob {
    let mut out = LayoutJob::default();
    for section in &job.sections {
        let from = usize::from(section.byte_range.start).max(range.start);
        let to = usize::from(section.byte_range.end).min(range.end);
        if from < to {
            out.append(job.text.get(from..to).unwrap_or_default(), 0.0, section.format.clone());
        }
    }
    out.wrap.max_width = f32::INFINITY;
    out.keep_trailing_whitespace = true;
    out
}

fn measure(ui: &egui::Ui, job: &LayoutJob, range: Range<usize>) -> f32 {
    ui.fonts_mut(|f| f.layout_job(slice_job(job, range))).size().x
}

/// Wrap in logical order; bidi resolution then runs separately on every wrapped
/// line with the original paragraph level. Spaces and empty paragraphs survive.
fn lines(ui: &egui::Ui, job: &LayoutJob, range: Range<usize>) -> Vec<Range<usize>> {
    let width = job.wrap.max_width.max(1.0);
    if !width.is_finite() || range.is_empty() {
        return vec![range];
    }
    let mut result = Vec::new();
    let mut start = range.start;
    let mut used = 0.0;
    for (local, word) in job.text.get(range.clone()).unwrap_or_default().split_word_bound_indices() {
        let at = range.start.saturating_add(local);
        let end = at.saturating_add(word.len());
        let advance = measure(ui, job, at..end);
        if used > 0.0 && used + advance > width {
            result.push(start..at);
            start = at;
            used = 0.0;
        }
        if advance <= width {
            used += advance;
        } else {
            for (local, grapheme) in word.grapheme_indices(true) {
                let at = at.saturating_add(local);
                let end = at.saturating_add(grapheme.len());
                let advance = measure(ui, job, at..end);
                if used > 0.0 && used + advance > width {
                    result.push(start..at);
                    start = at;
                    used = 0.0;
                }
                used += advance;
            }
        }
    }
    result.push(start..range.end);
    result
}

/// Build the view from logical text and logical style ranges. Unicode controls
/// steer direction but are never removed from the document's editing buffer.
pub(crate) fn layout(ui: &egui::Ui, job: LayoutJob) -> Layout {
    let key = ui.id().with("bidi-layout-cache");
    let fonts = ui.fonts(|f| f.definitions().font_data.values().cloned().collect::<Vec<_>>());
    let pixels = ui.ctx().pixels_per_point();
    if let Some(cache) = ui.ctx().data(|d| d.get_temp::<LayoutCache>(key))
        && cache.job == job
        && cache.pixels == pixels
        && cache.fonts.len() == fonts.len()
        && cache.fonts.iter().zip(&fonts).all(|(a, b)| Arc::ptr_eq(a, b))
    {
        return cache.layout;
    }
    let layout = build_layout(ui, &job);
    ui.ctx().data_mut(|d| d.insert_temp(key, LayoutCache { job, pixels, fonts, layout: layout.clone() }));
    layout
}

#[derive(Clone)]
struct LayoutCache {
    job: LayoutJob,
    pixels: f32,
    fonts: Vec<Arc<egui::FontData>>,
    layout: Layout,
}

fn build_layout(ui: &egui::Ui, job: &LayoutJob) -> Layout {
    let info = BidiInfo::new(&job.text, None);
    let mut out = Layout::default();
    let mut row = 0usize;
    let mut top = 0.0;
    // split_inclusive doesn't yield a final empty paragraph, so append it here.
    let mut paragraphs: Vec<_> = info.paragraphs.iter().map(|p| (p.range.clone(), p.level)).collect();
    if job.text.is_empty() || job.text.ends_with('\n') {
        paragraphs.push((job.text.len()..job.text.len(), Level::ltr()));
    }
    for (paragraph, level) in paragraphs {
        let content = job.text.get(paragraph.clone()).unwrap_or_default().trim_end_matches(['\n', '\r']);
        let content_range = paragraph.start..paragraph.start.saturating_add(content.len());
        let content_end = content_range.end;
        for line in lines(ui, job, content_range) {
            let mut x = 0.0;
            let height = ui.fonts_mut(|f| f.row_height(&format_at(job, line.start).font_id)).max(1.0);
            let mut row_height = height;
            let row_first_cell = out.cells.len();
            let row_first_piece = out.pieces.len();
            let row_first_stop = out.stops.len();
            if let Some(para) = info.paragraphs.iter().find(|p| p.range.start == paragraph.start)
                && !line.is_empty()
            {
                let (levels, runs) = info.visual_runs(para, line.clone());
                for run in runs {
                    let rtl = levels.get(run.start).is_some_and(Level::is_rtl);
                    // A shaping piece has a single script face and style. Font
                    // boundaries already break contextual shaping in the PDF path.
                    let mut pieces: Vec<Range<usize>> = Vec::new();
                    let mut from = run.start;
                    let mut previous = None;
                    for (local, c) in job.text.get(run.clone()).unwrap_or_default().char_indices() {
                        let at = run.start.saturating_add(local);
                        let key = (crate::bidi::is_rtl_script(c) || matches!(c, '\u{200c}' | '\u{200d}'), format_at(job, at));
                        if previous.as_ref().is_some_and(|p| *p != key) {
                            pieces.push(from..at);
                            from = at;
                        }
                        previous = Some(key);
                    }
                    pieces.push(from..run.end);
                    if rtl {
                        pieces.reverse();
                    }
                    for range in pieces {
                        let logical = job.text.get(range.clone()).unwrap_or_default();
                        let script_rtl = logical.chars().any(crate::bidi::is_rtl_script);
                        let mut displayed = String::new();
                        if script_rtl != rtl {
                            displayed.extend(logical.chars().rev().map(|c| if rtl { crate::bidi::mirrored(c) } else { c }));
                        } else {
                            displayed.extend(logical.chars().map(|c| if rtl { crate::bidi::mirrored(c) } else { c }));
                        }
                        displayed.retain(|c| !crate::bidi::is_bidi_control(c));
                        let format = format_at(job, range.start);
                        let galley = ui.fonts_mut(|f| {
                            let mut piece = LayoutJob::default();
                            piece.append(&displayed, 0.0, format.clone());
                            piece.keep_trailing_whitespace = true;
                            f.layout_job(piece)
                        });
                        let advance = galley.size().x.max(0.0);
                        row_height = row_height.max(galley.size().y);
                        let clusters = clusters(ui, logical, &format.font_id, rtl);
                        let total: f64 = clusters.iter().map(|(_, a)| *a).sum();
                        let mut pen = x;
                        for (cluster, advance_em) in clusters {
                            let width = if total > 0.0 { advance * (advance_em / total) as f32 } else { 0.0 };
                            let cluster_text = logical.get(cluster.clone()).unwrap_or_default();
                            let graphemes: Vec<_> = cluster_text.grapheme_indices(true).collect();
                            let count = graphemes.len().max(1) as f32;
                            for (i, (local, grapheme)) in graphemes.iter().enumerate() {
                                let from = range.start.saturating_add(cluster.start).saturating_add(*local);
                                let to = from.saturating_add(grapheme.len());
                                let order = if rtl { graphemes.len().saturating_sub(i).saturating_sub(1) } else { i };
                                let left = pen + width * order as f32 / count;
                                let right = left + width / count;
                                out.cells.push(Cell {
                                    range: from..to,
                                    rect: Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, top + row_height)),
                                });
                                let (before, after) = if rtl { (right, left) } else { (left, right) };
                                out.stops.push(Stop { caret: Caret { byte: from, after: false }, rect: caret_rect(before, top, row_height), row });
                                out.stops.push(Stop { caret: Caret { byte: to, after: true }, rect: caret_rect(after, top, row_height), row });
                            }
                            pen += width;
                        }
                        out.pieces.push(Piece { galley, pos: egui::pos2(x, top), row });
                        x += advance;
                    }
                }
            }
            if out.stops.len() == row_first_stop {
                out.stops.push(Stop { caret: Caret { byte: line.start, after: false }, rect: caret_rect(0.0, top, row_height), row });
            }
            let width = if job.wrap.max_width.is_finite() { job.wrap.max_width.max(x) } else { x };
            if job.justify && line.end < content_end && width > x {
                let gaps: Vec<_> = out
                    .pieces
                    .iter()
                    .skip(row_first_piece)
                    .filter(|p| {
                        !p.galley.job.text.is_empty()
                            && p.galley.job.text.chars().all(char::is_whitespace)
                            && p.pos.x > 0.0
                            && p.pos.x + p.galley.size().x < x
                            && p.galley.size().x > 0.0
                    })
                    .map(|p| p.pos.x..p.pos.x + p.galley.size().x)
                    .collect();
                if !gaps.is_empty() {
                    let extra = (width - x) / gaps.len() as f32;
                    let expand = |at: f32| {
                        at + gaps
                            .iter()
                            .map(|g| {
                                if at >= g.end {
                                    extra
                                } else if at > g.start {
                                    extra * (at - g.start) / (g.end - g.start)
                                } else {
                                    0.0
                                }
                            })
                            .sum::<f32>()
                    };
                    for piece in out.pieces.iter_mut().skip(row_first_piece) {
                        piece.pos.x = expand(piece.pos.x);
                    }
                    for cell in out.cells.iter_mut().skip(row_first_cell) {
                        cell.rect.min.x = expand(cell.rect.min.x);
                        cell.rect.max.x = expand(cell.rect.max.x);
                    }
                    for stop in out.stops.iter_mut().skip(row_first_stop) {
                        stop.rect.min.x = expand(stop.rect.min.x);
                        stop.rect.max.x = stop.rect.min.x;
                    }
                    x = width;
                }
            }
            let shift = match job.halign {
                egui::Align::Center => (width - x) * 0.5,
                egui::Align::Max => width - x,
                _ => 0.0,
            };
            // Mixed-font pieces share a baseline rather than their top edge.
            let baseline = out
                .pieces
                .iter()
                .skip(row_first_piece)
                .filter_map(|p| p.galley.rows.first().and_then(|r| r.glyphs.first()).map(|g| g.pos.y))
                .fold(0.0, f32::max);
            for piece in out.pieces.iter_mut().skip(row_first_piece) {
                piece.pos.x += shift;
                let ascent = piece.galley.rows.first().and_then(|r| r.glyphs.first()).map_or(baseline, |g| g.pos.y);
                piece.pos.y += baseline - ascent;
                row_height = row_height.max(piece.galley.size().y + baseline - ascent);
            }
            for cell in out.cells.iter_mut().skip(row_first_cell) {
                cell.rect = cell.rect.translate(egui::vec2(shift, 0.0));
                cell.rect.max.y = top + row_height;
            }
            for stop in out.stops.iter_mut().skip(row_first_stop) {
                stop.rect = stop.rect.translate(egui::vec2(shift, 0.0));
                stop.rect.max.y = top + row_height;
            }
            // Newline selection/typing uses the same paragraph-end boundary.
            if line.end == paragraph.end.saturating_sub(1) && job.text.get(line.end..paragraph.end) == Some("\n") {
                let edge = if level.is_rtl() { shift } else { shift + x };
                out.stops.push(Stop { caret: Caret { byte: line.end, after: true }, rect: caret_rect(edge, top, row_height), row });
            }
            out.size.x = out.size.x.max(width);
            top += row_height;
            row = row.saturating_add(1);
        }
    }
    out.size.y = top.max(1.0);
    out.stops.sort_by(|a, b| a.row.cmp(&b.row).then_with(|| a.rect.left().total_cmp(&b.rect.left())));
    // Equal-offset affinity at the same pixel is not an extra arrow-key stop.
    out.stops.dedup_by(|a, b| a.row == b.row && a.caret.byte == b.caret.byte && (a.rect.left() - b.rect.left()).abs() < 0.1);
    out
}

fn caret_rect(x: f32, y: f32, height: f32) -> Rect {
    Rect::from_min_size(egui::pos2(x, y), egui::vec2(0.0, height))
}

/// Use the face chosen by the same egui font stack to obtain logical clusters.
/// Ligatures are divided at grapheme boundaries if the font has no caret table;
/// marks share their base's cell. Missing/subset fonts use bounded grapheme cells.
fn clusters(ui: &egui::Ui, text: &str, font: &egui::FontId, rtl: bool) -> Vec<(Range<usize>, f64)> {
    let shaped = ui.fonts(|f| {
        let definitions = f.definitions();
        definitions.families.get(&font.family).into_iter().flatten().find_map(|name| {
            let data = definitions.font_data.get(name)?;
            pdfcraft_fonts::source_shaping::shape(&data.font, text).ok()
        })
    });
    if let Some(shaped) = shaped {
        let mut out: Vec<(Range<usize>, f64)> = Vec::new();
        for glyph in shaped.glyphs {
            if let Some((range, advance)) = out.last_mut().filter(|(r, _)| *r == glyph.range) {
                let _ = range;
                *advance += glyph.advance.max(0.0);
            } else {
                out.push((glyph.range, glyph.advance.max(0.0)));
            }
        }
        if out.iter().any(|(_, advance)| *advance > 0.0) {
            return out;
        }
    }
    let mut out: Vec<_> = text.grapheme_indices(true).map(|(at, g)| (at..at.saturating_add(g.len()), 1.0)).collect();
    if rtl {
        out.reverse();
    }
    out
}

#[derive(Clone)]
struct Composition {
    range: Range<usize>,
    text: String,
    active_range: Option<Range<usize>>,
}

#[derive(Clone, Default)]
struct State {
    caret: Caret,
    anchor: usize,
    edit: egui::text_edit::TextEditState,
    last: String,
    composition: Option<Composition>,
}

fn scalar(text: &str, byte: usize) -> usize {
    text.get(..byte).map_or(0, |s| s.chars().count())
}
fn byte(text: &str, scalar: usize) -> usize {
    text.char_indices().nth(scalar).map_or(text.len(), |(at, _)| at)
}
fn boundary(text: &str, at: usize) -> usize {
    text.grapheme_indices(true).map(|(b, _)| b).chain(std::iter::once(text.len())).take_while(|b| *b <= at).last().unwrap_or(0)
}

impl State {
    fn selected(&self) -> Range<usize> {
        self.anchor.min(self.caret.byte)..self.anchor.max(self.caret.byte)
    }
    fn snapshot(&self, text: &str) -> (egui::text::CCursorRange, String) {
        (
            egui::text::CCursorRange::two(
                egui::text::CCursor::new(scalar(text, self.anchor)),
                egui::text::CCursor::new(scalar(text, self.caret.byte)),
            ),
            text.to_owned(),
        )
    }
    fn restore(&mut self, text: &mut String, snapshot: (egui::text::CCursorRange, String)) {
        *text = snapshot.1;
        self.anchor = byte(text, snapshot.0.secondary.index.into());
        self.caret = Caret { byte: byte(text, snapshot.0.primary.index.into()), after: true };
        self.composition = None;
    }
    fn replace(&mut self, text: &mut String, range: Range<usize>, inserted: &str) {
        if text.get(range.clone()).is_none() || text.len().saturating_sub(range.len()).saturating_add(inserted.len()) > MAX_BYTES {
            return;
        }
        self.edit.set_undoer({
            let mut undo = self.edit.undoer();
            undo.add_undo(&self.snapshot(text));
            undo
        });
        text.replace_range(range.clone(), inserted);
        self.caret = Caret { byte: range.start.saturating_add(inserted.len()), after: true };
        self.anchor = self.caret.byte;
        self.composition = None;
    }
    fn move_to(&mut self, caret: Caret, shift: bool) {
        self.caret = caret;
        if !shift {
            self.anchor = caret.byte;
        }
    }
    fn delete(&mut self, text: &mut String, backwards: bool) {
        let mut range = self.selected();
        if range.is_empty() {
            if backwards {
                range.start = text.grapheme_indices(true).map(|(b, _)| b).take_while(|b| *b < self.caret.byte).last().unwrap_or(0);
            } else {
                range.end = text.grapheme_indices(true).map(|(b, _)| b).find(|b| *b > self.caret.byte).unwrap_or(text.len());
            }
        }
        self.replace(text, range, "");
    }
}

pub(crate) struct Output {
    pub(crate) response: Response,
    pub(crate) selection: Option<Range<usize>>,
}

pub(crate) struct Options {
    pub(crate) id: Id,
    pub(crate) width: f32,
    pub(crate) margin: f32,
    pub(crate) fill: Color32,
    pub(crate) color: Color32,
    pub(crate) hint: String,
}

pub(crate) fn reset(ctx: &egui::Context, id: Id) {
    ctx.data_mut(|d| d.remove::<State>(id.with("logical-bidi")));
}

pub(crate) fn active(ctx: &egui::Context, id: Id, text: &str) -> bool {
    crate::bidi::has_rtl(text) || ctx.data(|d| d.get_temp::<State>(id.with("logical-bidi")).is_some())
}

/// Public egui APIs are sufficient for input, focus, clipboard and IME. Latin
/// fields retain TextEdit; once a field contains RTL it uses this logical editor
/// until the draft closes, including when the user deletes its last character.
pub(crate) fn show(ui: &mut egui::Ui, text: &mut String, options: Options, mut make_layout: impl FnMut(&egui::Ui, &str, f32) -> Layout) -> Output {
    let key = options.id.with("logical-bidi");
    let before = text.clone();
    let mut state = ui.ctx().data_mut(|d| d.get_temp::<State>(key)).unwrap_or_else(|| {
        let edit = egui::text_edit::TextEditState::load(ui.ctx(), options.id).unwrap_or_default();
        let cursor = edit.cursor.char_range();
        let caret = cursor.map_or(text.len(), |c| byte(text, c.primary.index.into()));
        let anchor = cursor.map_or(caret, |c| byte(text, c.secondary.index.into()));
        State {
            caret: Caret { byte: boundary(text, caret), after: true },
            anchor: boundary(text, anchor),
            edit,
            last: text.clone(),
            ..Default::default()
        }
    });
    state.caret.byte = boundary(text, state.caret.byte.min(text.len()));
    state.anchor = boundary(text, state.anchor.min(text.len()));
    if state.last != *text {
        state.composition = None;
    }
    let mut layout = make_layout(ui, text, options.width);
    let (rect, _) = ui.allocate_exact_size(layout.size.max(egui::vec2(options.width, 20.0)) + Vec2::splat(options.margin * 2.0), Sense::hover());
    let mut response = ui.interact(rect, options.id, Sense::click_and_drag());
    let origin = rect.min + Vec2::splat(options.margin);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    if response.clicked() || response.drag_started() {
        response.request_focus();
        state.composition = None;
        let pressed_at = if response.drag_started() { ui.input(|i| i.pointer.press_origin()) } else { response.interact_pointer_pos() };
        if let Some(pos) = pressed_at {
            let caret = layout.hit((pos - origin).to_pos2());
            state.move_to(caret, ui.input(|i| i.modifiers.shift));
            if response.double_clicked() {
                let range = text.split_word_bound_indices().find(|(at, word)| *at <= caret.byte && at.saturating_add(word.len()) > caret.byte);
                if let Some((at, word)) = range {
                    state.anchor = at;
                    state.caret.byte = at.saturating_add(word.len());
                }
            }
        }
    }
    if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
    {
        state.caret = layout.hit((pos - origin).to_pos2());
    }
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(options.id, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() })
        });
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::Copy if !state.selected().is_empty() => ui.ctx().copy_text(text.get(state.selected()).unwrap_or_default().to_owned()),
                Event::Cut if !state.selected().is_empty() => {
                    ui.ctx().copy_text(text.get(state.selected()).unwrap_or_default().to_owned());
                    state.replace(text, state.selected(), "");
                }
                Event::Paste(inserted) | Event::Text(inserted) if state.composition.is_none() => state.replace(text, state.selected(), &inserted),
                Event::Key { key, pressed: true, modifiers, .. } => {
                    if key == Key::Escape {
                        state.composition = None;
                    }
                    if modifiers.command && key == Key::A {
                        state.anchor = 0;
                        state.caret = Caret { byte: text.len(), after: true };
                    } else if modifiers.command && matches!(key, Key::Z | Key::Y) {
                        let snapshot = state.snapshot(text);
                        let mut undo = state.edit.undoer();
                        let restored = if key == Key::Y || modifiers.shift { undo.redo(&snapshot).cloned() } else { undo.undo(&snapshot).cloned() };
                        if let Some(restored) = restored {
                            state.restore(text, restored);
                        }
                        state.edit.set_undoer(undo);
                    } else if matches!(key, Key::Backspace | Key::Delete) {
                        state.delete(text, key == Key::Backspace);
                    } else if key == Key::Enter && !modifiers.command {
                        state.replace(text, state.selected(), "\n");
                    } else if matches!(key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Home | Key::End) {
                        state.composition = None;
                        let caret = if modifiers.command && matches!(key, Key::ArrowLeft | Key::ArrowRight) {
                            layout.edge(state.caret, key == Key::ArrowRight)
                        } else {
                            match key {
                                Key::ArrowLeft => layout.horizontal(state.caret, false),
                                Key::ArrowRight => layout.horizontal(state.caret, true),
                                Key::ArrowUp => layout.vertical(state.caret, false),
                                Key::ArrowDown => layout.vertical(state.caret, true),
                                Key::Home => layout.edge(state.caret, false),
                                Key::End => layout.edge(state.caret, true),
                                _ => state.caret,
                            }
                        };
                        state.move_to(caret, modifiers.shift);
                    }
                    if matches!(
                        key,
                        Key::A
                            | Key::Z
                            | Key::Y
                            | Key::Backspace
                            | Key::Delete
                            | Key::ArrowLeft
                            | Key::ArrowRight
                            | Key::ArrowUp
                            | Key::ArrowDown
                            | Key::Home
                            | Key::End
                    ) || (key == Key::Enter && !modifiers.command)
                    {
                        ui.input_mut(|i| {
                            i.consume_key(modifiers, key);
                        });
                    }
                }
                Event::Ime(egui::ImeEvent::Preedit { text: preedit, active_range_chars }) => {
                    if preedit.is_empty() {
                        state.composition = None;
                    } else if preedit.len() <= MAX_BYTES {
                        let range = state.composition.as_ref().map_or_else(|| state.selected(), |c| c.range.clone());
                        state.composition = Some(Composition { range, text: preedit, active_range: active_range_chars });
                    }
                }
                Event::Ime(egui::ImeEvent::Commit(committed)) => {
                    let range = state.composition.take().map_or_else(|| state.selected(), |c| c.range);
                    if !committed.is_empty() {
                        state.replace(text, range, &committed);
                    }
                }
                Event::Ime(egui::ImeEvent::DeleteSurrounding { before_chars, after_chars }) => {
                    let at = scalar(text, state.caret.byte);
                    let from = boundary(text, byte(text, at.saturating_sub(before_chars)));
                    let raw_end = byte(text, at.saturating_add(after_chars));
                    let end =
                        text.grapheme_indices(true).map(|(b, _)| b).chain(std::iter::once(text.len())).find(|b| *b >= raw_end).unwrap_or(text.len());
                    state.replace(text, from..end, "");
                }
                _ => {}
            }
            if before != *text {
                layout = make_layout(ui, text, options.width);
            }
        }
    } else {
        state.composition = None;
    }
    let selected = state.selected();
    let mut paint_caret = state.caret;
    if let Some(composition) = &state.composition {
        let mut working = text.clone();
        if working.get(composition.range.clone()).is_some() && working.len().saturating_add(composition.text.len()) <= MAX_BYTES {
            working.replace_range(composition.range.clone(), &composition.text);
            let active_end = composition.active_range.as_ref().map_or(composition.text.len(), |r| byte(&composition.text, r.end));
            paint_caret = Caret { byte: composition.range.start.saturating_add(active_end), after: true };
            layout = make_layout(ui, &working, options.width);
        }
    }
    let painter = ui.painter().with_clip_rect(ui.clip_rect());
    painter.rect_filled(rect, egui::CornerRadius::ZERO, options.fill);
    for cell in &layout.cells {
        if cell.range.start < selected.end && selected.start < cell.range.end {
            painter.rect_filled(cell.rect.translate(origin.to_vec2()), egui::CornerRadius::ZERO, ui.visuals().selection.bg_fill);
        }
        if let Some(composition) = &state.composition {
            let range = composition.range.start..composition.range.start.saturating_add(composition.text.len());
            if cell.range.start < range.end && range.start < cell.range.end {
                let rect = cell.rect.translate(origin.to_vec2());
                painter.line_segment([rect.left_bottom(), rect.right_bottom()], ui.visuals().text_cursor.stroke);
            }
        }
    }
    for piece in &layout.pieces {
        painter.galley(origin + piece.pos.to_vec2(), piece.galley.clone(), options.color);
    }
    if text.is_empty() && state.composition.is_none() && !options.hint.is_empty() {
        painter.text(origin, egui::Align2::LEFT_TOP, &options.hint, egui::FontId::proportional(12.0), ui.visuals().weak_text_color());
    }
    if response.has_focus()
        && let Some(stop) = layout.stop(paint_caret)
    {
        let cursor = stop.rect.translate(origin.to_vec2());
        painter.line_segment([cursor.left_top(), cursor.left_bottom()], ui.visuals().text_cursor.stroke);
        ui.output_mut(|o| {
            o.ime =
                Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Normal, rect, cursor_rect: cursor, should_interrupt_composition: false });
        });
    }
    if before != *text {
        response.mark_changed();
        ui.ctx().request_repaint();
    }
    response.widget_info(|| egui::WidgetInfo::text_edit(ui.is_enabled(), &before, text.as_str(), &options.hint));
    state.edit.cursor.set_char_range(Some(state.snapshot(text).0));
    state.edit.clone().store(ui.ctx(), options.id);
    state.last = text.clone();
    ui.ctx().data_mut(|d| d.insert_temp(key, state));
    Output { response, selection: (!selected.is_empty()).then_some(selected) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(text: &str, width: f32) -> LayoutJob {
        let mut job = LayoutJob::default();
        job.append(text, 0.0, egui::TextFormat { font_id: egui::FontId::proportional(18.0), ..Default::default() });
        job.wrap.max_width = width;
        job
    }

    struct Editor {
        ctx: egui::Context,
        text: String,
        id: Id,
        view: Layout,
        rect: Rect,
    }
    impl Editor {
        fn new(text: &str) -> Self {
            let ctx = egui::Context::default();
            ctx.set_fonts(crate::theme::font_definitions());
            let mut editor = Self { ctx, text: text.into(), id: Id::new("bidi-regression"), view: Layout::default(), rect: Rect::NOTHING };
            editor.frame(Vec::new()).drop_without_applying_deltas();
            editor.ctx.memory_mut(|m| m.request_focus(editor.id));
            editor
        }
        fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
            self.ctx.run_ui(
                egui::RawInput { events, screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
                |ui| {
                    let output = show(
                        ui,
                        &mut self.text,
                        Options { id: self.id, width: 400.0, margin: 0.0, fill: Color32::WHITE, color: Color32::BLACK, hint: String::new() },
                        |ui, text, width| {
                            let view = layout(ui, job(text, width));
                            self.view = view.clone();
                            view
                        },
                    );
                    self.rect = output.response.rect;
                },
            )
        }
        fn select(&mut self, range: Range<usize>) {
            self.ctx.data_mut(|d| {
                let mut state = d.get_temp::<State>(self.id.with("logical-bidi")).unwrap();
                state.anchor = range.start;
                state.caret = Caret { byte: range.end, after: true };
                d.insert_temp(self.id.with("logical-bidi"), state);
            });
        }
    }

    fn key(key: Key, command: bool, shift: bool) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers { command, shift, ..Default::default() } }
    }

    #[test]
    fn selected_logical_text_is_copied_cut_and_restored_exactly() {
        let mut editor = Editor::new("مرحبا بالعالم");
        editor.select(0..editor.text.len());
        let output = editor.frame(vec![Event::Copy]);
        assert!(output.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == "مرحبا بالعالم")));
        output.drop_without_applying_deltas();
        editor.frame(vec![Event::Cut]).drop_without_applying_deltas();
        assert_eq!(editor.text, "");
        editor.frame(vec![key(Key::Z, true, false)]).drop_without_applying_deltas();
        assert_eq!(editor.text, "مرحبا بالعالم");
        editor.frame(vec![key(Key::Z, true, true)]).drop_without_applying_deltas();
        assert_eq!(editor.text, "");
    }

    #[test]
    fn typing_and_replacement_history_never_store_visual_order() {
        let mut editor = Editor::new("اب جد");
        editor.frame(vec![key(Key::A, true, false), Event::Text("س ص".into())]).drop_without_applying_deltas();
        assert_eq!(editor.text, "س ص");
        editor.frame(vec![key(Key::Z, true, false)]).drop_without_applying_deltas();
        assert_eq!(editor.text, "اب جد");
        editor.frame(vec![key(Key::Z, true, true)]).drop_without_applying_deltas();
        assert_eq!(editor.text, "س ص");
    }

    #[test]
    fn insertion_at_a_mapped_word_boundary_changes_that_word() {
        let mut editor = Editor::new("اب جد");
        editor.select(0..0);
        editor.frame(vec![Event::Text("س".into())]).drop_without_applying_deltas();
        assert_eq!(editor.text, "ساب جد");
        editor.select(3..3); // Inside an Arabic UTF-8 scalar: safely snaps down.
        editor.frame(vec![Event::Text("م".into())]).drop_without_applying_deltas();
        assert_eq!(editor.text, "سماب جد");
    }

    #[test]
    fn deletion_keeps_unselected_letters_and_whole_marked_graphemes() {
        let mut editor = Editor::new("اب جد هز");
        editor.select(5..7); // Only ج; د and ه are outside this range.
        editor.frame(vec![key(Key::Delete, false, false)]).drop_without_applying_deltas();
        assert_eq!(editor.text, "اب د هز");
        let mut editor = Editor::new("سَلَام");
        editor.select("سَ".len().."سَ".len());
        editor.frame(vec![key(Key::Backspace, false, false)]).drop_without_applying_deltas();
        assert_eq!(editor.text, "لَام");
    }

    #[test]
    fn composition_stays_out_of_document_until_a_single_commit() {
        let mut editor = Editor::new("سلام ");
        editor.frame(vec![Event::Ime(egui::ImeEvent::Preedit { text: "ع".into(), active_range_chars: Some(0..1) })]).drop_without_applying_deltas();
        assert_eq!(editor.text, "سلام ");
        editor
            .frame(vec![Event::Ime(egui::ImeEvent::Preedit { text: "عليكم".into(), active_range_chars: Some(0..5) })])
            .drop_without_applying_deltas();
        assert_eq!(editor.text, "سلام ");
        editor.frame(vec![Event::Ime(egui::ImeEvent::Commit("عليكم".into()))]).drop_without_applying_deltas();
        assert_eq!(editor.text, "سلام عليكم");
        editor.frame(vec![key(Key::Z, true, false)]).drop_without_applying_deltas();
        assert_eq!(editor.text, "سلام ");
    }

    #[test]
    fn shaped_word_order_and_pointer_positions_share_logical_boundaries() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let text = "مرحبا بالعالم";
            let view = layout(ui, job(text, 400.0));
            let first_word = view.cells.iter().filter(|c| c.range.start < "مرحبا".len()).map(|c| c.rect.left()).fold(f32::INFINITY, f32::min);
            let second_word = view.cells.iter().filter(|c| c.range.start >= "مرحبا ".len()).map(|c| c.rect.right()).fold(0.0, f32::max);
            assert!(second_word <= first_word, "second word must be left of first");
            let first = view.stop(Caret { byte: 0, after: false }).unwrap();
            assert_eq!(view.hit(first.rect.center()).byte, 0);
            let final_stop = view.stop(Caret { byte: text.len(), after: true }).unwrap();
            assert_eq!(view.hit(final_stop.rect.center()).byte, text.len());
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn style_ranges_follow_words_after_reordering() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut job = LayoutJob::default();
            job.append("مرحبا ", 0.0, egui::TextFormat { color: Color32::RED, ..Default::default() });
            job.append("بالعالم", 0.0, egui::TextFormat { color: Color32::BLUE, ..Default::default() });
            let view = layout(ui, job);
            let second = view.pieces.iter().find(|p| p.galley.job.text == "بالعالم").unwrap();
            let first = view.pieces.iter().find(|p| p.galley.job.text == "مرحبا").unwrap();
            assert_eq!(second.galley.job.sections[0].format.color, Color32::BLUE);
            assert_eq!(first.galley.job.sections[0].format.color, Color32::RED);
            assert!(second.pos.x < first.pos.x);
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn pointer_insertion_and_drag_selection_use_the_painted_stops() {
        let mut editor = Editor::new("مرحبا بالعالم");
        let position = |editor: &Editor, caret| editor.rect.min + editor.view.stop(caret).unwrap().rect.center().to_vec2();
        let start = position(&editor, Caret { byte: 0, after: false });
        let pointer = |pos, pressed| Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        editor.frame(vec![Event::PointerMoved(start), pointer(start, true)]).drop_without_applying_deltas();
        editor.frame(vec![pointer(start, false)]).drop_without_applying_deltas();
        editor.frame(vec![Event::Text("س".into())]).drop_without_applying_deltas();
        assert_eq!(editor.text, "سمرحبا بالعالم");
        let start = position(&editor, Caret { byte: 0, after: false });
        let end = position(&editor, Caret { byte: editor.text.len(), after: true });
        editor.frame(vec![Event::PointerMoved(start), pointer(start, true)]).drop_without_applying_deltas();
        editor.frame(vec![Event::PointerMoved(end)]).drop_without_applying_deltas();
        editor.frame(vec![pointer(end, false)]).drop_without_applying_deltas();
        let output = editor.frame(vec![Event::Copy]);
        assert!(output.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == "سمرحبا بالعالم")));
        output.drop_without_applying_deltas();
    }

    #[test]
    fn lam_alef_has_two_editing_stops_and_marks_stay_with_their_base() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let view = layout(ui, job("لَا", 400.0));
            assert!(view.stops.iter().any(|s| s.caret.byte == "لَ".len()));
            assert!(!view.stops.iter().any(|s| s.caret.byte == "ل".len()));
            assert!(view.stops.iter().all(|s| "لَا".is_char_boundary(s.caret.byte)));
            assert!(view.stops.iter().all(|s| s.rect.is_finite()));
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn relayout_tracks_text_styles_width_and_pixel_scale_without_stale_stops() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let wide = layout(ui, job("مرحبا بالعالم مرحبا بالعالم", 400.0));
            let narrow = layout(ui, job("مرحبا بالعالم مرحبا بالعالم", 50.0));
            assert!(narrow.size.y > wide.size.y);
            let changed = layout(ui, job("لا", 50.0));
            assert!(changed.stops.iter().all(|s| s.caret.byte <= "لا".len()));
            let mut styled = job("لا", 50.0);
            styled.sections[0].format.font_id.size = 36.0;
            assert!(layout(ui, styled).size.y > changed.size.y);
        })
        .drop_without_applying_deltas();
        ctx.set_pixels_per_point(2.0);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let changed = layout(ui, job("لا", 50.0));
            assert!(changed.stops.iter().all(|s| s.rect.is_finite()));
        })
        .drop_without_applying_deltas();
    }

    #[test]
    #[ignore = "local timing probe; no hardware-dependent performance assertion"]
    fn bidi_layout_performance_probe() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::theme::font_definitions());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            for scalars in [1000usize, 10000] {
                let text: String = "مرحبا PDF 123 ".repeat(scalars.div_ceil(14)).chars().take(scalars).collect();
                let begin = std::time::Instant::now();
                let first = layout(ui, job(&text, 500.0));
                let changed = begin.elapsed();
                let begin = std::time::Instant::now();
                let cached = layout(ui, job(&text, 500.0));
                let reused = begin.elapsed();
                assert_eq!(first.stops.len(), cached.stops.len());
                assert!(Arc::ptr_eq(&first.pieces[0].galley, &cached.pieces[0].galley));
                eprintln!("{} scalars: changed {:?}, cached {:?}", text.chars().count(), changed, reused);
            }
        })
        .drop_without_applying_deltas();
    }
}
