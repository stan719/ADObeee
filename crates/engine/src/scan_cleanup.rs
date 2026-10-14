//! Reviewable removal of isolated, long, thin dark lines in an opaque scanned bitmap.
//! Only selected component pixels change; page geometry, text and OCR are not rasterized.
//! Printed rules and scan creases can look identical, so detection never applies an edit.

use std::{collections::HashSet, sync::Arc};

use crate::{DocId, Document, Edit, ImageEdit, export::ExportSource};

const MAX_PIXELS: usize = 16_000_000;
const MAX_COMPONENT: usize = 250_000;
const MAX_CANDIDATES: usize = 128;

/// A possible scan line, in bitmap pixels (top-left origin, exclusive right/bottom).
#[derive(Clone, Debug)]
pub struct Candidate {
    pub rect: [u32; 4],
    pub pixels: usize,
    mask: Vec<u32>,
    vertical: bool,
}

/// Immutable input that may be moved to a worker. Built only after permission checks.
pub struct Source {
    doc: DocId,
    generation: u64,
    page: usize,
    image: usize,
    pdf: ExportSource,
}

/// A review proposal tied to one document generation. Indices are zero-based.
pub struct Preview {
    source: Source,
    width: u32,
    height: u32,
    candidates: Vec<Candidate>,
    rgba: Vec<u8>,
}

impl Document {
    /// Prepare a single bitmap for scan cleanup, retaining unsaved edits. Form artwork is
    /// deliberately unsupported: replacing a whole form would lose its other contents.
    pub fn scan_cleanup_source(&self, page: usize, image: usize) -> Result<Source, String> {
        self.check_copying().map_err(|e| e.to_string())?;
        if !self.allows_modification() {
            return Err("the document's permissions don't allow modification".into());
        }
        let info = self.page_images(page).into_iter().nth(image).ok_or("no such page image")?;
        if info.is_form {
            return Err("scan cleanup requires a bitmap, not grouped Form artwork".into());
        }
        Ok(Source { doc: self.id, generation: self.edit_generation(), page, image, pdf: self.export_source() })
    }

    /// Reject a proposal after any edit, undo/redo, or switch to another document.
    pub fn check_scan_cleanup(&self, preview: &Preview) -> Result<(), String> {
        self.check_copying().map_err(|e| e.to_string())?;
        if !self.allows_modification() {
            return Err("the document's permissions don't allow modification".into());
        }
        if self.id != preview.source.doc || self.edit_generation() != preview.source.generation {
            return Err("the document changed; detect scan lines again".into());
        }
        Ok(())
    }
}

impl Source {
    /// Decode and detect without rendering the page. `keep_going` is polled during traversal.
    pub fn detect(self, mut keep_going: impl FnMut() -> bool) -> Result<Preview, String> {
        crate::guard(|| {
            if !keep_going() {
                return Err("scan cleanup cancelled".into());
            }
            let cos =
                pdfcraft_cos::Document::open_with_password(self.pdf.bytes.clone(), self.pdf.config.password.as_deref()).map_err(|e| e.to_string())?;
            let img =
                pdfcraft_edit::page_images(&cos, self.page).map_err(|e| e.to_string())?.into_iter().nth(self.image).ok_or("no such page image")?;
            if img.is_form {
                return Err("scan cleanup requires a bitmap".into());
            }
            let object = img.object.ok_or("the image has no object")?;
            // Check dimensions before extracting/decompressing the image into a file.
            let raw = cos.get(object);
            let pdfcraft_cos::Object::Stream(stream) = &*raw else { return Err("the image is not a stream".into()) };
            let w = u32::try_from(stream.dict.int(b"Width").unwrap_or(0)).map_err(|_| "invalid image width")?;
            let h = u32::try_from(stream.dict.int(b"Height").unwrap_or(0)).map_err(|_| "invalid image height")?;
            dimensions(w, h)?;
            let (_, bytes) = pdfcraft_create::image_file(&cos, object)?;
            let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some((MAX_PIXELS * 4) as u64);
            limits.max_image_width = Some(w);
            limits.max_image_height = Some(h);
            reader.limits(limits);
            let rgba = reader.decode().map_err(|e| e.to_string())?.into_rgba8();
            if rgba.dimensions() != (w, h) {
                return Err("image dimensions do not match the PDF".into());
            }
            let rgba = rgba.into_raw();
            let candidates = detect(&rgba, w, h, keep_going)?;
            Ok(Preview { source: self, width: w, height: h, candidates, rgba })
        })?
    }
}

fn dimensions(w: u32, h: u32) -> Result<usize, String> {
    let n = u64::from(w) * u64::from(h);
    if w == 0 || h == 0 || n > MAX_PIXELS as u64 {
        return Err(format!("scan cleanup supports images up to {MAX_PIXELS} pixels"));
    }
    Ok(n as usize)
}

fn dark(px: &[u8]) -> bool {
    px.iter().take(3).copied().max().unwrap_or(255) < 200
}

/// Connected components exclude short text strokes and connected table grids. A narrow
/// isolated printed rule can still pass, which is why every candidate requires review.
fn detect(rgba: &[u8], width: u32, height: u32, mut keep_going: impl FnMut() -> bool) -> Result<Vec<Candidate>, String> {
    let n = dimensions(width, height)?;
    if rgba.len() != n * 4 || rgba.as_chunks::<4>().0.iter().any(|p| p.get(3) != Some(&255)) {
        return Err("scan cleanup requires an opaque RGBA bitmap".into());
    }
    let (w, h) = (width as usize, height as usize);
    let mut unseen: Vec<bool> = rgba.as_chunks::<4>().0.iter().map(|p| dark(p)).collect();
    let mut stack = Vec::<u32>::new();
    let mut result = Vec::new();
    let mut visited = 0usize;
    for start in 0..n {
        if start.is_multiple_of(4096) && !keep_going() {
            return Err("scan cleanup cancelled".into());
        }
        if unseen.get(start) != Some(&true) {
            continue;
        }
        if let Some(v) = unseen.get_mut(start) {
            *v = false;
        }
        stack.push(start as u32);
        let mut mask = Vec::new();
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        let mut count = 0;
        while let Some(i) = stack.pop().map(|i| i as usize) {
            visited += 1;
            if visited.is_multiple_of(4096) && !keep_going() {
                return Err("scan cleanup cancelled".into());
            }
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            count += 1;
            if mask.len() < MAX_COMPONENT {
                mask.push(i as u32);
            }
            for ny in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                    let next = ny * w + nx;
                    if let Some(v) = unseen.get_mut(next)
                        && *v
                    {
                        *v = false;
                        stack.push(next as u32);
                    }
                }
            }
        }
        let (dx, dy) = (x1 - x0, y1 - y0);
        let vertical = dy >= dx;
        let (length, thickness, major, minor) = if vertical { (dy, dx, h, w) } else { (dx, dy, w, h) };
        if count > MAX_COMPONENT || length < 60.max(major / 5) || thickness > 3.max(minor / 100) || length < thickness * 20 {
            continue;
        }
        let candidate = Candidate { rect: [x0 as u32, y0 as u32, x1 as u32, y1 as u32], pixels: count, mask, vertical };
        // Both adjacent sides, when present, must be paper. A line touching text or a dark
        // block is not safe to propose. Image-edge lines may have just one paper side.
        if candidate.mask.iter().all(|&i| paper_sides(rgba, w, h, &candidate, i as usize).is_some()) {
            if result.len() == MAX_CANDIDATES {
                return Err("too many possible scan lines to review".into());
            }
            result.push(candidate);
        }
    }
    Ok(result)
}

fn paper_sides(rgba: &[u8], w: usize, h: usize, c: &Candidate, i: usize) -> Option<([u8; 3], [u8; 3])> {
    let [x0, y0, x1, y1] = c.rect.map(|v| v as usize);
    let at = |x, y| -> Option<[u8; 3]> {
        let p = rgba.get((y * w + x) * 4..(y * w + x) * 4 + 3)?;
        if p.iter().any(|&v| v < 220) {
            return None;
        }
        Some([*p.first()?, *p.get(1)?, *p.get(2)?])
    };
    let (a, b) = if c.vertical {
        (if x0 > 0 { Some(at(x0 - 1, i / w)?) } else { None }, if x1 < w { Some(at(x1, i / w)?) } else { None })
    } else {
        (if y0 > 0 { Some(at(i % w, y0 - 1)?) } else { None }, if y1 < h { Some(at(i % w, y1)?) } else { None })
    };
    Some((a.or(b)?, b.or(a)?))
}

impl Preview {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }
    pub fn generation(&self) -> u64 {
        self.source.generation
    }

    /// Original pixels for a preview. Treat these as document data, never public fixtures.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Remove exactly the selected components, filling from the adjacent paper colour.
    pub fn cleaned_rgba(&self, selected: &[usize]) -> Result<Vec<u8>, String> {
        if selected.is_empty() {
            return Err("select at least one scan line".into());
        }
        let mut seen = HashSet::new();
        for &i in selected {
            if self.candidates.get(i).is_none() || !seen.insert(i) {
                return Err("invalid or duplicate scan line selection".into());
            }
        }
        let mut out = self.rgba.clone();
        for &index in selected {
            let c = self.candidates.get(index).ok_or("no such scan line")?;
            for &i in &c.mask {
                let i = i as usize;
                let (a, b) =
                    paper_sides(&self.rgba, self.width as usize, self.height as usize, c, i).ok_or("scan line no longer has a paper background")?;
                let [x0, y0, x1, y1] = c.rect;
                let (pos, start, end) =
                    if c.vertical { ((i % self.width as usize) as u32, x0, x1) } else { ((i / self.width as usize) as u32, y0, y1) };
                let t = (pos - start + 1) as f32 / (end - start + 1) as f32;
                if let Some(pixel) = out.get_mut(i * 4..i * 4 + 3) {
                    for ((channel, a), b) in pixel.iter_mut().zip(a).zip(b) {
                        *channel = (f32::from(a) * (1.0 - t) + f32::from(b) * t).round() as u8;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Build one undoable image replacement. Call `Document::check_scan_cleanup` immediately
    /// before applying; the preview may have been prepared in a background worker.
    pub fn edit(&self, selected: &[usize]) -> Result<Edit, String> {
        crate::guard(|| {
            let pixels = self.cleaned_rgba(selected)?;
            let bytes = crate::export::encode_png(self.width, self.height, &pixels)?;
            Ok(Edit::Batch {
                label: "Clean scan lines".into(),
                edits: vec![Edit::EditPageImage {
                    page: self.source.page,
                    index: self.source.image,
                    change: ImageEdit::Replace { name: "clean-scan.png".into(), bytes: Arc::new(bytes) },
                }],
            })
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut pixels = vec![255; 200 * 300 * 4];
        for y in 8..292 {
            for x in 10..12 {
                pixels[(y * 200 + x) * 4..(y * 200 + x) * 4 + 3].fill(30);
            }
        }
        // Text-like strokes and a connected table must not become candidates.
        for y in 40..55 {
            for x in 40..45 {
                pixels[(y * 200 + x) * 4..(y * 200 + x) * 4 + 3].fill(0);
            }
        }
        for y in 80..240 {
            for x in 60..180 {
                if y == 80 || y == 239 || x == 60 || x == 179 {
                    pixels[(y * 200 + x) * 4..(y * 200 + x) * 4 + 3].fill(0);
                }
            }
        }
        pixels
    }

    #[test]
    fn scan_cleanup_detects_isolated_lines_keeps_text_tables_and_cancels() {
        let rgba = fixture();
        let candidates = detect(&rgba, 200, 300, || true).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rect, [10, 8, 12, 292]);
        assert!(detect(&rgba, 200, 300, || false).unwrap_err().contains("cancelled"));
        assert!(detect(&[], u32::MAX, u32::MAX, || true).is_err());
        assert!(detect(&rgba[..100], 200, 300, || true).is_err());
        let mut transparent = rgba.clone();
        transparent[3] = 0;
        assert!(detect(&transparent, 200, 300, || true).is_err());
        let mut touching = rgba;
        for x in 12..45 {
            touching[(50 * 200 + x) * 4..(50 * 200 + x) * 4 + 3].fill(0);
        }
        assert!(detect(&touching, 200, 300, || true).unwrap().is_empty());
    }

    #[test]
    fn scan_cleanup_replaces_only_selected_pixels_and_undo_restores_image() {
        let rgba = fixture();
        let png = crate::export::encode_png(200, 300, &rgba).unwrap();
        let mut session = crate::Session::new();
        let pdf = session.create_from_images(&[("scan.png".into(), png)]).unwrap();
        let id = session.open_new("scan", pdf.clone()).unwrap();
        session
            .apply(
                id,
                Edit::AddOcrText {
                    page: 0,
                    words: vec![pdfcraft_ocr::PlacedWord { text: "OCR survives".into(), origin: [40.0, 30.0], across: [90.0, 0.0], up: [0.0, 16.0] }],
                },
            )
            .unwrap();
        let text = crate::export::Exporter::new(session.get(id).unwrap()).text(0).unwrap();
        assert!(text.contains("OCR survives"));
        let preview = session.get(id).unwrap().scan_cleanup_source(0, 0).unwrap().detect(|| true).unwrap();
        assert!(preview.cleaned_rgba(&[]).is_err());
        assert!(preview.cleaned_rgba(&[0, 0]).is_err());
        assert!(preview.cleaned_rgba(&[usize::MAX]).is_err());
        let clean = preview.cleaned_rgba(&[0]).unwrap();
        for (i, (before, after)) in rgba.as_chunks::<4>().0.iter().zip(clean.as_chunks::<4>().0).enumerate() {
            if (8..292).contains(&(i / 200)) && (10..12).contains(&(i % 200)) {
                assert_eq!(after, &[255; 4]);
            } else {
                assert_eq!(before, after);
            }
        }
        session.get(id).unwrap().check_scan_cleanup(&preview).unwrap();
        session.apply(id, preview.edit(&[0]).unwrap()).unwrap();
        assert_eq!(crate::export::Exporter::new(session.get(id).unwrap()).text(0).unwrap(), text);
        assert!(session.get(id).unwrap().check_scan_cleanup(&preview).is_err());
        assert!(session.get(id).unwrap().scan_cleanup_source(0, 0).unwrap().detect(|| true).unwrap().candidates.is_empty());
        session.undo(id).unwrap();
        assert_eq!(session.get(id).unwrap().page_image_rgba(0, 0).unwrap().2, rgba);
        assert!(session.get(id).unwrap().check_scan_cleanup(&preview).is_err());
        let other = session.open_new("other", pdf).unwrap();
        assert!(session.get(other).unwrap().check_scan_cleanup(&preview).is_err());
        assert!(session.get(id).unwrap().scan_cleanup_source(1, 0).is_err());
    }

    #[test]
    fn scan_cleanup_detects_horizontal_edges_and_keeps_the_paper_tint() {
        let mut rgba = vec![255; 300 * 200 * 4];
        for p in rgba.as_chunks_mut::<4>().0 {
            p[..3].copy_from_slice(&[240, 245, 250]);
        }
        for x in 10..290 {
            rgba[x * 4..x * 4 + 3].fill(50);
        }
        let candidates = detect(&rgba, 300, 200, || true).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rect, [10, 0, 290, 1]);
        assert_eq!(paper_sides(&rgba, 300, 200, &candidates[0], 25).unwrap(), ([240, 245, 250], [240, 245, 250]));
        let mut calls = 0;
        assert!(
            detect(&[0, 0, 0, 255].repeat(500 * 500), 500, 500, || {
                calls += 1;
                calls < 3
            })
            .is_err()
        );
    }
}
