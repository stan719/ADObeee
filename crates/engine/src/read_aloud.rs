//! Read Out Loud text extraction and reading sequence (M12).
//!
//! Prepares document content in reading order for platform text-to-speech:
//! paragraphs, headings, image alternate text, and interactive form fields.

use crate::Document;

/// The role of a chunk in the spoken sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadAloudKind {
    Paragraph,
    Heading(u8),
    ImageAlt,
    FormField,
}

/// One chunk of spoken text, with its location on the page for visual highlighting.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadAloudChunk {
    /// 0-based page index.
    pub page: usize,
    /// The spoken text.
    pub text: String,
    /// The kind of chunk.
    pub kind: ReadAloudKind,
    /// [x0, y0, x1, y1] in PDF user space (y up), if known.
    pub rect: Option<[f64; 4]>,
}

impl Document {
    /// Extract text chunks on `page` (0-based) for Read Out Loud.
    pub fn read_aloud_page(&self, page: usize) -> Vec<ReadAloudChunk> {
        let mut chunks = Vec::new();
        if page >= self.info.pages.len() {
            return chunks;
        }

        // 1. Text blocks in reading order (if editor is available)
        if let Some(cos) = self.editor.as_ref().map(|e| &e.cos) {
            if let Ok(blocks) = pdfcraft_edit::reading_blocks(cos, page) {
                let avg_size = if blocks.is_empty() {
                    12.0
                } else {
                    let sum: f64 = blocks.iter().map(|b| b.size).sum();
                    sum / blocks.len() as f64
                };

                for b in blocks {
                    let text = b.text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let is_heading = b.size >= avg_size * 1.3 && text.len() < 120;
                    let kind =
                        if is_heading { ReadAloudKind::Heading(if b.size >= avg_size * 1.6 { 1 } else { 2 }) } else { ReadAloudKind::Paragraph };

                    chunks.push(ReadAloudChunk { page, text: text.to_string(), kind, rect: Some(b.rect) });
                }
            }

            // 2. Figures with alternate text on this page
            for fig in self.figures() {
                if fig.page == Some(page) {
                    if let Some(alt) = &fig.alt {
                        let alt = alt.trim();
                        if !alt.is_empty() {
                            let text = format!("Graphic: {alt}");
                            let rect = fig.bbox.map(|b| [b[0] as f64, b[1] as f64, b[2] as f64, b[3] as f64]);
                            chunks.push(ReadAloudChunk { page, text, kind: ReadAloudKind::ImageAlt, rect });
                        }
                    }
                }
            }
        }

        // 3. Fallback: extract page text through the rendering pool if no chunks were found yet
        if chunks.is_empty() {
            let mut renderer = pdfcraft_render::PageRenderer::new(self.display.clone(), self.config.clone());
            let res =
                renderer.render(pdfcraft_render::RenderRequest { page, kind: pdfcraft_render::RequestKind::Text, scale: 1.0, ..Default::default() });
            if let Some(t) = res.text {
                let plain = t.plain_text();
                for para in plain.split("\n\n") {
                    let trimmed = para.trim();
                    if !trimmed.is_empty() {
                        chunks.push(ReadAloudChunk { page, text: trimmed.replace('\n', " "), kind: ReadAloudKind::Paragraph, rect: None });
                    }
                }
            }
        }

        // 4. Form fields on this page
        for field in self.form.iter() {
            for widget in &field.widgets {
                if widget.page == Some(page) {
                    let desc = field.tooltip.as_deref().unwrap_or(&field.name);
                    let val = field.display_value();
                    let text = if val.is_empty() { format!("Form field: {desc}") } else { format!("Form field: {desc}, {val}") };
                    let rect = [widget.rect[0] as f64, widget.rect[1] as f64, widget.rect[2] as f64, widget.rect[3] as f64];
                    chunks.push(ReadAloudChunk { page, text, kind: ReadAloudKind::FormField, rect: Some(rect) });
                }
            }
        }

        chunks
    }

    /// Continuous plain text on `page` (0-based) formatted for speech synthesis.
    pub fn read_aloud_text(&self, page: usize) -> String {
        let chunks = self.read_aloud_page(page);
        if chunks.is_empty() {
            return format!("Page {} has no readable text.", page + 1);
        }
        let mut out = String::new();
        for (i, chunk) in chunks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&chunk.text);
            if !chunk.text.ends_with('.') && !chunk.text.ends_with('!') && !chunk.text.ends_with('?') {
                out.push('.');
            }
        }
        out
    }

    /// Extract reading chunks from `from_page` to the end of the document.
    pub fn read_aloud_document(&self, from_page: usize) -> Vec<ReadAloudChunk> {
        let total = self.info.pages.len();
        let mut all = Vec::new();
        for p in from_page..total {
            all.extend(self.read_aloud_page(p));
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_aloud_chunk_types_and_methods() {
        let chunk =
            ReadAloudChunk { page: 0, text: "Welcome to PdfCraft".into(), kind: ReadAloudKind::Heading(1), rect: Some([10.0, 20.0, 200.0, 50.0]) };
        assert_eq!(chunk.page, 0);
        assert_eq!(chunk.kind, ReadAloudKind::Heading(1));
        assert!(chunk.text.contains("PdfCraft"));
    }
}
