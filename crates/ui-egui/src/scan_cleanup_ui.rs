//! Detect and review scan lines off the desktop UI thread. No deletion before Apply.
use crate::{PdfCraftApp, widgets};
use pdfcraft_engine::{DocId, Edit, scan_cleanup::Preview};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    doc: DocId,
    generation: u64,
    page: usize,
    image: usize,
}

enum Output {
    Detected(Result<Arc<Preview>, String>),
    Prepared(Arc<Preview>, Result<Box<Edit>, String>),
}
struct Job {
    key: Key,
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Output>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(crate) struct Draft {
    key: Option<Key>,
    image: usize,
    images: Vec<usize>,
    job: Option<Job>,
    preview: Option<Arc<Preview>>,
    texture: Option<egui::TextureHandle>,
    selected: Vec<bool>,
    error: Option<String>,
}

impl Draft {
    pub(crate) fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        // A closed dialog is polled too: release a finished worker's image promptly,
        // while retaining an active worker until it stops (no abandoned worker pileup).
        if self.job.as_ref().is_some_and(|job| !matches!(job.receiver.try_recv(), Err(mpsc::TryRecvError::Empty))) {
            self.job = None;
        }
        self.key = None;
        self.preview = None;
        self.texture = None;
        self.selected.clear();
        self.error = None;
    }

    fn start(&mut self, key: Key, ctx: &egui::Context, inline: bool, work: impl FnOnce(Arc<AtomicBool>) -> Output + Send + 'static) {
        if self.job.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.job = Some(Job { key, cancel: cancel.clone(), receiver });
        let ctx = ctx.clone();
        let run = move || {
            let _ = sender.send(work(cancel));
            ctx.request_repaint();
        };
        #[cfg(not(target_arch = "wasm32"))]
        if !inline {
            if let Err(e) = std::thread::Builder::new().name("scan-cleanup".into()).spawn(run) {
                self.job = None;
                self.error = Some(e.to_string());
            }
            return;
        }
        let _ = inline;
        run();
    }
}

fn texture(ctx: &egui::Context, preview: &Preview) -> egui::TextureHandle {
    // A bounded preview upload. The source bitmap stays at its original resolution for edits.
    let step = preview.width().max(preview.height()).div_ceil(900).max(1) as usize;
    let (w, h) = (preview.width() as usize, preview.height() as usize);
    let mut pixels = Vec::with_capacity(w.div_ceil(step) * h.div_ceil(step) * 4);
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            if let Some(p) = preview.rgba().get((y * w + x) * 4..(y * w + x) * 4 + 4) {
                pixels.extend_from_slice(p);
            }
        }
    }
    ctx.load_texture(
        "scan-cleanup",
        egui::ColorImage::from_rgba_unmultiplied([w.div_ceil(step), h.div_ceil(step)], &pixels),
        egui::TextureOptions::LINEAR,
    )
}

pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp) -> bool {
    ui.heading(tl!("Enhance scanned file"));
    ui.label(tl!(
        "Review detected lines before removing them. Printed rules can look like scan defects; text hidden by a crease cannot be restored."
    ));
    let Some((view, id)) = app.active_ids() else { return true };
    let Some(doc) = app.session.get(id) else { return true };
    let draft = &mut app.scan_cleanup_draft;
    let mut key = Key { doc: id, generation: doc.edit_generation(), page: app.views[view].current, image: draft.image };
    if draft.key != Some(key) {
        draft.cancel();
        draft.images = doc.page_images(key.page).iter().enumerate().filter_map(|(i, img)| (!img.is_form).then_some(i)).collect();
        if !draft.images.contains(&draft.image) {
            draft.image = draft.images.first().copied().unwrap_or(0);
        }
        key.image = draft.image;
        draft.key = Some(key);
    }
    let mut prepared = None;
    if let Some(job) = &draft.job {
        let output = match job.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Output::Detected(Err(tl!("Scan cleanup stopped unexpectedly.").into()))),
        };
        if let Some(output) = output {
            if job.key == key && !job.cancel.load(Ordering::Relaxed) {
                match output {
                    Output::Detected(Ok(preview)) => {
                        draft.texture = Some(texture(ui.ctx(), &preview));
                        draft.selected = vec![false; preview.candidates().len()];
                        draft.preview = Some(preview);
                        draft.error = None;
                    }
                    Output::Detected(Err(error)) | Output::Prepared(_, Err(error)) => draft.error = Some(error),
                    Output::Prepared(preview, Ok(edit)) => prepared = Some((preview, edit)),
                }
            }
            draft.job = None;
        }
    }
    ui.horizontal(|ui| {
        ui.label(tl!("Image"));
        egui::ComboBox::from_id_salt("scan-image").selected_text((draft.image + 1).to_string()).show_ui(ui, |ui| {
            for &image in &draft.images {
                ui.selectable_value(&mut draft.image, image, (image + 1).to_string());
            }
        });
        if ui
            .add_enabled_ui(!draft.images.is_empty() && draft.job.is_none() && draft.image == key.image, |ui| {
                widgets::pill_button(ui, tl!("Detect scan lines"), false)
            })
            .inner
            .clicked()
        {
            match doc.scan_cleanup_source(key.page, key.image) {
                Ok(source) => draft.start(key, ui.ctx(), app.run_inline, move |cancel| {
                    Output::Detected(source.detect(|| !cancel.load(Ordering::Relaxed)).map(Arc::new))
                }),
                Err(error) => draft.error = Some(error),
            }
        }
    });
    if draft.images.is_empty() {
        ui.label(tl!("This page has no editable bitmap."));
    }
    if draft.image != key.image {
        draft.cancel();
        return false;
    }
    if draft.job.is_some() {
        ui.spinner();
    }
    if let Some(error) = &draft.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
    }
    if let (Some(preview), Some(texture)) = (&draft.preview, &draft.texture) {
        ui.label(tl!("Preview"));
        let size = texture.size_vec2();
        let scale = (ui.available_width() / size.x).min(270.0 / size.y).min(1.0);
        let response = ui.add(egui::Image::new((texture.id(), size * scale)));
        for (i, candidate) in preview.candidates().iter().enumerate() {
            let [x0, y0, x1, y1] = candidate.rect;
            let pos =
                |x, y| response.rect.min + egui::vec2(x as f32 / preview.width() as f32, y as f32 / preview.height() as f32) * response.rect.size();
            let color = if draft.selected.get(i) == Some(&true) { egui::Color32::RED } else { egui::Color32::from_rgb(40, 110, 230) };
            let rect = egui::Rect::from_min_max(pos(x0, y0), pos(x1, y1)).expand(2.0);
            ui.painter().rect_stroke(rect, 0.0, (1.5, color), egui::StrokeKind::Outside);
            ui.painter().text(rect.right_top(), egui::Align2::LEFT_TOP, format!(" #{}", i + 1), egui::FontId::proportional(12.0), color);
        }
        ui.label(tl!("Only checked lines will be removed. Other image pixels and OCR text stay unchanged."));
        if preview.candidates().is_empty() {
            ui.label(tl!("No isolated scan lines found."));
        }
        egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
            for (i, selected) in draft.selected.iter_mut().enumerate() {
                ui.add(widgets::checkbox(selected, format!("#{}", i + 1)));
            }
        });
        if ui
            .add_enabled_ui(draft.job.is_none() && draft.selected.iter().any(|&s| s), |ui| widgets::pill_button(ui, tl!("Apply"), true))
            .inner
            .clicked()
        {
            let selected = draft.selected.iter().enumerate().filter_map(|(i, &s)| s.then_some(i)).collect::<Vec<_>>();
            let preview = preview.clone();
            draft.start(key, ui.ctx(), app.run_inline, move |_| {
                let edit = preview.edit(&selected).map(Box::new);
                Output::Prepared(preview, edit)
            });
        }
    }
    if let Some((preview, edit)) = prepared {
        match doc.check_scan_cleanup(&preview) {
            Ok(()) => return app.apply_edit(*edit),
            Err(error) => draft.error = Some(error),
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    #[test]
    fn scan_cleanup_ui_requires_selection_applies_and_undoes_with_a_worker() {
        for inline in [true, false] {
            let mut rgba = vec![255; 200 * 300 * 4];
            for y in 10..290 {
                for x in 10..12 {
                    rgba[(y * 200 + x) * 4..(y * 200 + x) * 4 + 3].fill(0);
                }
            }
            let png = pdfcraft_engine::export::encode_png(200, 300, &rgba).unwrap();
            let pdf = pdfcraft_engine::Session::new().create_from_images(&[("scan.png".into(), png)]).unwrap();
            let mut h = Harness::builder().with_size(egui::vec2(1200.0, 1000.0)).build_eframe(|_| {
                let mut app = PdfCraftApp::new();
                app.run_inline = inline;
                app.set_option("language", "en").unwrap();
                app.open_bytes("scan.pdf", None, pdf.as_ref().clone()).unwrap();
                app.run_command("ocr.enhance");
                app
            });
            let wait = |h: &mut Harness<'_, PdfCraftApp>| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    h.run_steps(2);
                    if h.state().scan_cleanup_draft.job.is_none() {
                        break;
                    }
                    assert!(std::time::Instant::now() < deadline, "scan worker timed out");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                h.run_steps(2);
            };
            h.run_steps(3);
            h.get_by_label("Detect scan lines").click();
            wait(&mut h);
            assert_eq!(h.state().scan_cleanup_draft.selected, vec![false]);
            let id = h.state().views[0].id;
            assert!(h.state().session.get(id).unwrap().can_undo().is_none());
            h.get_by_label("#1").click();
            h.run_steps(2);
            h.get_by_label("Apply").click();
            wait(&mut h);
            assert!(h.state().dialog.is_none());
            assert_eq!(h.state().session.get(id).unwrap().can_undo(), Some("Clean scan lines"));
            assert!(h.state().session.get(id).unwrap().scan_cleanup_source(0, 0).unwrap().detect(|| true).unwrap().candidates().is_empty());
            h.state_mut().session.undo(id).unwrap();
            assert_eq!(h.state().session.get(id).unwrap().page_image_rgba(0, 0).unwrap().2, rgba);
        }
    }

    #[test]
    fn scan_cleanup_cancel_marks_the_worker_and_drops_its_proposal() {
        let mut draft = Draft::default();
        let key = Key { doc: DocId(1), generation: 0, page: 0, image: 0 };
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        draft.key = Some(key);
        draft.job = Some(Job { key, receiver, cancel: cancel.clone() });
        draft.selected = vec![true];
        draft.cancel();
        assert!(cancel.load(Ordering::Relaxed));
        assert!(draft.key.is_none() && draft.selected.is_empty());
        assert!(draft.job.is_some(), "wait for the old worker before allowing another");
        sender.send(Output::Detected(Err("cancelled".into()))).ok().unwrap();
        draft.cancel();
        assert!(draft.job.is_none(), "release completed results even while the dialog is closed");
    }
}
