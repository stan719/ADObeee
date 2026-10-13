//! Bidirectional fields with a logical buffer. egui's editor provides undo, clipboard,
//! accessibility and IME; a separate visual layout supplies painting and hit testing.
//! Reordering the buffer itself would corrupt copying, insertion and saved PDF text.
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, FontId, Id, Pos2, Rect, Stroke, Ui, pos2, vec2};

use crate::bidi;

pub(crate) struct Options {
    pub id: Id,
    pub font: FontId,
    pub width: f32,
    pub rows: usize,
    pub color: Color32,
    pub frame: egui::Frame,
    pub margin: egui::Margin,
    pub hint: String,
}

impl Options {
    pub fn new(ui: &Ui, salt: &str) -> Self {
        Self {
            id: ui.make_persistent_id(salt),
            font: egui::TextStyle::Body.resolve(ui.style()),
            width: ui.available_width(),
            rows: 3,
            color: ui.visuals().text_color(),
            frame: egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .stroke(Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color))
                .corner_radius(ui.visuals().widgets.inactive.corner_radius),
            margin: egui::Margin::symmetric(4, 2),
            hint: String::new(),
        }
    }
}

#[derive(Clone)]
struct Cell {
    index: usize,
    before: Pos2,
    after: Pos2,
}
#[derive(Clone)]
struct Piece {
    text: String,
    pos: Pos2,
}
#[derive(Clone)]
struct Line {
    start: usize,
    end: usize,
    y: f32,
    rtl: bool,
    cells: std::ops::Range<usize>,
}
#[derive(Clone)]
struct Layout {
    cells: Vec<Cell>,
    pieces: Vec<Piece>,
    lines: Vec<Line>,
    height: f32,
    row_height: f32,
    width: f32,
    carets: Vec<Option<Pos2>>,
}

impl Layout {
    fn caret(&self, index: usize) -> Pos2 {
        self.carets.get(index).copied().flatten().unwrap_or(Pos2::ZERO)
    }
    fn nearest(&self, p: Pos2) -> usize {
        let row = self.lines.partition_point(|line| line.y + self.row_height * 0.5 < p.y);
        let line = [row.checked_sub(1), Some(row)]
            .into_iter()
            .flatten()
            .filter_map(|i| self.lines.get(i))
            .min_by(|a, b| (a.y + self.row_height * 0.5 - p.y).abs().total_cmp(&(b.y + self.row_height * 0.5 - p.y).abs()));
        let Some(line) = line else { return 0 };
        if line.start == line.end {
            return line.start;
        }
        self.cells
            .get(line.cells.clone())
            .unwrap_or_default()
            .iter()
            .flat_map(|c| [(c.index, c.before), (c.index.saturating_add(1), c.after)])
            .min_by(|a, b| {
                let distance = |v: Pos2| (v.y + self.row_height * 0.5 - p.y).abs() * self.width.max(1000.0) + (v.x - p.x).abs();
                distance(a.1).total_cmp(&distance(b.1))
            })
            .map_or(0, |c| c.0)
    }
    fn navigate(&self, index: usize, key: egui::Key) -> usize {
        let p = self.caret(index);
        match key {
            egui::Key::ArrowUp | egui::Key::ArrowDown => {
                self.nearest(p + vec2(0.0, self.row_height * if key == egui::Key::ArrowUp { -0.5 } else { 1.5 }))
            }
            egui::Key::Home | egui::Key::End => {
                self.nearest(pos2(if key == egui::Key::Home { 0.0 } else { self.width }, p.y + self.row_height * 0.5))
            }
            _ => {
                let left = key == egui::Key::ArrowLeft;
                let row = self.lines.partition_point(|line| line.y < p.y - 0.5);
                self.lines
                    .get(row)
                    .and_then(|line| self.cells.get(line.cells.clone()))
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|c| [(c.index, c.before), (c.index.saturating_add(1), c.after)])
                    .filter(|(i, v)| *i != index && (v.y - p.y).abs() < 0.5 && if left { v.x < p.x - 0.01 } else { v.x > p.x + 0.01 })
                    .min_by(|a, b| (a.1.x - p.x).abs().total_cmp(&(b.1.x - p.x).abs()))
                    .map_or_else(
                        || {
                            let Some(line) = self.lines.get(row) else { return index };
                            let next = if left == line.rtl { row.checked_add(1) } else { row.checked_sub(1) };
                            next.and_then(|r| self.lines.get(r))
                                .map_or(index, |l| self.nearest(pos2(if left { self.width } else { 0.0 }, l.y + self.row_height * 0.5)))
                        },
                        |c| c.0,
                    )
            }
        }
    }
}

fn layout(ui: &Ui, text: &str, font: &FontId, width: f32) -> Layout {
    let row_height = ui.fonts_mut(|f| f.row_height(font)).max(1.0);
    let mut result = Layout {
        cells: Vec::new(),
        pieces: Vec::new(),
        lines: Vec::new(),
        height: row_height,
        row_height,
        width,
        carets: vec![None; text.chars().count().saturating_add(1)],
    };
    let mut bytes: Vec<_> = text.char_indices().map(|(at, _)| at).collect();
    bytes.push(text.len());
    let character = |at| bytes.binary_search(&at).unwrap_or_else(|i| i);
    let lines =
        bidi::mapped_lines(text, |s| ui.painter().layout_no_wrap(bidi::visual_auto(s).into_owned(), font.clone(), Color32::WHITE).size().x <= width);
    for (row, line) in lines.into_iter().enumerate() {
        let cells_start = result.cells.len();
        let y = row as f32 * row_height;
        let mut pieces = Vec::new();
        let mut x = 0.0;
        let mut start = 0;
        while start < line.chars.len() {
            let script = |c: &bidi::MappedChar| bidi::is_rtl_script(c.ch) || bidi::is_joiner(c.ch);
            let rtl_script = line.chars.get(start).is_some_and(script);
            let end = line
                .chars
                .get(start..)
                .unwrap_or_default()
                .iter()
                .position(|c| script(c) != rtl_script)
                .map_or(line.chars.len(), |i| start.saturating_add(i));
            let chars = line.chars.get(start..end).unwrap_or_default();
            let shown: String = chars.iter().map(|c| c.ch).collect();
            let galley = ui.painter().layout_no_wrap(shown.clone(), font.clone(), Color32::WHITE);
            let advance = galley.size().x;
            // Shaping supplies cluster intervals, including ligatures and combining marks.
            // The approved Arabic face is also the proportional UI fallback for this field.
            let shaped = rtl_script
                .then(|| pdfcraft_fonts::document_arabic_font().and_then(|f| pdfcraft_fonts::source_shaping::shape(f.bytes, &shown).ok()))
                .flatten();
            // Index clusters once: scanning every glyph for every character makes long
            // Arabic input quadratic, including a grapheme with many combining marks.
            let mut cluster_bounds = std::collections::BTreeMap::new();
            let mut local_boundaries: Vec<_> = shown.char_indices().map(|(at, _)| at).collect();
            local_boundaries.push(shown.len());
            if let Some(run) = &shaped {
                for glyph in &run.glyphs {
                    let first = local_boundaries.binary_search(&glyph.cluster.start).unwrap_or_else(|i| i);
                    let last = local_boundaries.binary_search(&glyph.cluster.end).unwrap_or_else(|i| i);
                    let bounds =
                        cluster_bounds.entry(glyph.cluster.start).or_insert((glyph.cluster.end, first, last, glyph.pen, glyph.pen + glyph.advance));
                    bounds.3 = bounds.3.min(glyph.pen);
                    bounds.4 = bounds.4.max(glyph.pen + glyph.advance);
                }
            }
            let mut local_bytes = 0usize;
            for (i, c) in chars.iter().enumerate() {
                let (mut left, mut right) =
                    (advance * i as f32 / chars.len().max(1) as f32, advance * i.saturating_add(1) as f32 / chars.len().max(1) as f32);
                if let Some(run) = &shaped {
                    if let Some((_, &(end, first, last, lo, hi))) = cluster_bounds.range(..=local_bytes).next_back()
                        && local_bytes < end
                        && run.advance > 0.0
                    {
                        let count = last.saturating_sub(first).max(1) as f64;
                        let at = i.saturating_sub(first) as f64;
                        let a = if c.rtl { 1.0 - (at + 1.0) / count } else { at / count };
                        let b = if c.rtl { 1.0 - at / count } else { (at + 1.0) / count };
                        left = ((lo + (hi - lo) * a) / run.advance) as f32 * advance;
                        right = ((lo + (hi - lo) * b) / run.advance) as f32 * advance;
                    }
                } else if let Some(g) = galley.rows.first().and_then(|r| r.glyphs.get(i)) {
                    left = g.pos.x;
                    right = left + g.advance_width;
                }
                if left.is_finite() && right.is_finite() {
                    let (before, after) = if c.rtl { (right, left) } else { (left, right) };
                    result.cells.push(Cell { index: character(c.byte), before: pos2(x + before, y), after: pos2(x + after, y) });
                }
                local_bytes = local_bytes.saturating_add(c.ch.len_utf8());
            }
            pieces.push(Piece { text: shown, pos: pos2(x, y) });
            x += advance;
            start = end;
        }
        let offset = if line.rtl { (width - x).max(0.0) } else { 0.0 };
        for cell in result.cells.get_mut(cells_start..).unwrap_or_default() {
            cell.before.x += offset;
            cell.after.x += offset;
        }
        for mut piece in pieces {
            piece.pos.x += offset;
            result.pieces.push(piece);
        }
        result.lines.push(Line {
            start: character(line.range.start),
            end: character(line.range.end),
            y,
            rtl: line.rtl || (line.range.is_empty() && crate::i18n::current().rtl()),
            cells: cells_start..result.cells.len(),
        });
        result.height = y + row_height;
    }
    // Logical lookup is constant-time. Row-local hit testing/navigation never scans
    // unrelated rows; the text byte limit bounds all of these index allocations.
    for cell in &result.cells {
        if let Some(slot) = result.carets.get_mut(cell.index) {
            slot.get_or_insert(cell.before);
        }
    }
    for line in result.lines.iter().filter(|line| line.start == line.end) {
        if let Some(slot) = result.carets.get_mut(line.start) {
            slot.get_or_insert(pos2(if line.rtl { width } else { 0.0 }, line.y));
        }
    }
    for cell in &result.cells {
        if let Some(slot) = result.carets.get_mut(cell.index.saturating_add(1)) {
            slot.get_or_insert(cell.after);
        }
    }
    result
}

fn shadow(ui: &Ui, text: &dyn egui::TextBuffer, height: f32, width: f32, visible_height: f32) -> Arc<egui::Galley> {
    // One LTR scalar per logical scalar prevents egui's RTL continuation-glyph bug from
    // changing cursor indices. The invisible galley is solely its logical editing model.
    let logical = text.as_str();
    let dummy: String = logical.chars().map(|c| if c == '\n' { c } else { '.' }).collect();
    let format = TextFormat { font_id: FontId::monospace(1.0), color: Color32::TRANSPARENT, line_height: Some(height), ..Default::default() };
    let mut job = LayoutJob::default();
    job.append(&dummy, 0.0, format);
    let mut galley = (*ui.fonts_mut(|f| f.layout_job(job))).clone();
    galley.rect.max.x = width.max(1.0);
    galley.rect.max.y = galley.rect.max.y.max(visible_height);
    let job = Arc::make_mut(&mut galley.job);
    job.text = logical.to_owned();
    for section in &mut job.sections {
        section.byte_range = 0.into()..logical.len().into();
    }
    let mut chars = logical.chars().filter(|c| *c != '\n');
    for row in &mut galley.rows {
        for glyph in &mut Arc::make_mut(&mut row.row).glyphs {
            if let Some(c) = chars.next() {
                glyph.chr = c;
            }
        }
    }
    Arc::new(galley)
}

/// Native editing with bidirectional display. Large buffers use the bounded native layout;
/// no content is truncated or rewritten to make it fit the interface.
pub(crate) fn show(ui: &mut Ui, text: &mut String, options: Options) -> egui::text_edit::TextEditOutput {
    let Options { id, font, width, rows, color, mut frame, margin, hint } = options;
    // egui uses the custom frame's margin; TextEdit::margin only supplies a default
    // frame when none was provided. Keep geometry and painting on this one source.
    frame.inner_margin = margin;
    let width = width.min(ui.available_width()).max(1.0);
    if text.len() > 65_536 || (!bidi::has_rtl(text) && !crate::i18n::current().rtl()) {
        return egui::TextEdit::multiline(text)
            .id(id)
            .font(font)
            .desired_width(width)
            .desired_rows(rows)
            .frame(frame)
            .text_color(color)
            .hint_text(hint)
            .show(ui);
    }
    // Geometry is cached separately from galleys, whose atlas can change between frames.
    let content_width = (width - f32::from(margin.left) - f32::from(margin.right) - frame.stroke.width * 2.0).max(1.0);
    let key = (text.clone(), font.clone(), content_width.to_bits(), ui.ctx().pixels_per_point().to_bits());
    let cached = ui.ctx().data_mut(|d| d.get_temp::<((String, FontId, u32, u32), Layout)>(id.with("bidi-layout")));
    let mut display = cached.filter(|(k, _)| k == &key).map_or_else(|| layout(ui, text, &font, content_width), |(_, l)| l);
    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
    let before = state.cursor.char_range();
    if ui.memory(|m| m.has_focus(id))
        && let Some(mut range) = before
    {
        let events = ui.input(|i| i.events.clone());
        for event in events {
            if let egui::Event::Key { key, pressed: true, modifiers, .. } = event
                && matches!(
                    key,
                    egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::Home | egui::Key::End
                )
                && !modifiers.alt
            {
                // Ctrl+arrows retain the native logical word navigation; unmodified arrows
                // follow screen order. Home/End with Ctrl go to the document boundaries.
                if modifiers.command && !matches!(key, egui::Key::Home | egui::Key::End) {
                    continue;
                }
                ui.input_mut(|i| {
                    i.consume_key(modifiers, key);
                });
                let next = if modifiers.command {
                    if key == egui::Key::Home { 0 } else { text.chars().count() }
                } else {
                    display.navigate(range.primary.index.into(), key)
                };
                range.primary = CCursor::new(next);
                if !modifiers.shift {
                    range.secondary = range.primary;
                }
            }
        }
        state.cursor.set_char_range(Some(range));
        state.store(ui.ctx(), id);
    }
    let height = display.row_height;
    let visible_height = display.height;
    let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, width: f32| shadow(ui, text, height, width, visible_height);
    let selection_fill = ui.visuals().selection.bg_fill;
    let focus_stroke = ui.visuals().selection.stroke;
    let caret = ui.visuals().text_cursor.stroke;
    let mut output = ui
        .scope(|ui| {
            ui.visuals_mut().selection.bg_fill = Color32::TRANSPARENT;
            ui.visuals_mut().selection.stroke.color = Color32::TRANSPARENT;
            ui.visuals_mut().text_cursor.stroke.color = Color32::TRANSPARENT;
            ui.visuals_mut().weak_text_color = Some(Color32::TRANSPARENT);
            egui::TextEdit::multiline(text)
                .id(id)
                .font(font.clone())
                .desired_width(width)
                .desired_rows(rows)
                .min_size(vec2(width, display.height))
                .clip_text(true)
                .frame(frame)
                .text_color(Color32::TRANSPARENT)
                .hint_text(hint.clone())
                .layouter(&mut layouter)
                .show(ui)
        })
        .inner;
    // Native selection-change metadata omits the hint. Keep fields discoverable by the
    // same placeholder through the control channel and assistive technology while typing.
    if !hint.is_empty() {
        ui.ctx().accesskit_node_builder(id, |node| node.set_placeholder(hint.clone()));
    }
    if output.response.changed() {
        display = layout(ui, text, &font, content_width);
    }
    let origin = output.text_clip_rect.min;
    if frame.stroke.width > 0.0 && output.response.has_focus() {
        ui.painter().rect_stroke(output.response.rect, frame.corner_radius, focus_stroke, egui::StrokeKind::Inside);
    }
    let shift = ui.input(|i| i.modifiers.shift);
    if (output.response.clicked() || output.response.dragged())
        && let Some(p) = output.response.interact_pointer_pos()
    {
        let index = display.nearest((p - origin).to_pos2());
        let anchor_id = id.with("bidi-drag-anchor");
        let anchor = if output.response.drag_started() {
            let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
            display.nearest((press - origin).to_pos2())
        } else if output.response.dragged() {
            ui.ctx().data_mut(|d| d.get_temp::<usize>(anchor_id)).unwrap_or_else(|| {
                let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(p);
                display.nearest((press - origin).to_pos2())
            })
        } else if shift {
            before.map_or(index, |r| r.secondary.index.into())
        } else {
            index
        };
        let mut range = CCursorRange::two(CCursor::new(anchor), CCursor::new(index));
        if output.response.double_clicked() {
            let chars: Vec<_> = text.chars().collect();
            let mut a = index.min(chars.len());
            let mut b = a;
            while a > 0 && chars.get(a.saturating_sub(1)).is_some_and(|c| !c.is_whitespace()) {
                a = a.saturating_sub(1);
            }
            while chars.get(b).is_some_and(|c| !c.is_whitespace()) {
                b = b.saturating_add(1);
            }
            range = CCursorRange::two(CCursor::new(a), CCursor::new(b));
        }
        ui.ctx().data_mut(|d| d.insert_temp(anchor_id, anchor));
        output.state.cursor.set_char_range(Some(range));
        output.cursor_range = Some(range);
        output.state.clone().store(ui.ctx(), id);
    }
    let painter = ui.painter().with_clip_rect(output.text_clip_rect);
    if output.response.has_focus()
        && let Some(range) = output.cursor_range
    {
        let selected = range.as_sorted_char_range();
        let a: usize = selected.start.into();
        let b: usize = selected.end.into();
        for cell in display.cells.iter().filter(|c| (a..b).contains(&c.index)) {
            let rect = Rect::from_min_size(
                origin + vec2(cell.before.x.min(cell.after.x), cell.before.y),
                vec2((cell.after.x - cell.before.x).abs(), display.row_height),
            );
            painter.rect_filled(rect, 0, selection_fill);
        }
        let p = origin + display.caret(range.primary.index.into()).to_vec2();
        painter.line_segment([p, p + vec2(0.0, display.row_height)], Stroke::new(caret.width.max(1.0), caret.color));
        ui.ctx().output_mut(|o| {
            if let Some(ime) = &mut o.ime {
                ime.cursor_rect = Rect::from_min_size(p, vec2(1.0, display.row_height));
            }
        });
    }
    for piece in &display.pieces {
        let galley = painter.layout_no_wrap(piece.text.clone(), font.clone(), color);
        painter.galley(origin + piece.pos.to_vec2(), galley, color);
    }
    if text.is_empty() && !hint.is_empty() {
        let galley = painter.layout_no_wrap(hint, font, ui.visuals().weak_text_color());
        let offset = if crate::i18n::current().rtl() { (content_width - galley.size().x).max(0.0) } else { 0.0 };
        painter.galley(origin + vec2(offset, 0.0), galley, ui.visuals().weak_text_color());
    }
    ui.ctx().data_mut(|d| d.insert_temp(id.with("bidi-layout"), ((text.clone(), key.1, key.2, key.3), display)));
    output
}
