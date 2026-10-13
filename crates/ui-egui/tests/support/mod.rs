//! Completion-aware waits for the real background-open path.

use egui_kittest::Harness;
use pdfcraft_ui_egui::PdfCraftApp;

pub fn wait_for(h: &mut Harness<'static, PdfCraftApp>, ready: impl Fn(&PdfCraftApp) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready(h.state()) {
        assert!(std::time::Instant::now() < deadline, "document opening did not finish: {:?}", h.state().toast);
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    // The model can finish during the frame that first lays out a new modal. Let its
    // geometry settle before a following test clicks its AccessKit bounds.
    h.run_steps(2);
}
