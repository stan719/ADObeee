//! File ▸ Open OFD (early alpha), #1036: pick an OFD file and open it as a new, unsaved PDF.
//!
//! The picked file goes through the normal open path (`open_path` / the browser inbox), which
//! hands OFD bytes to [`PdfCraftApp::open_ofd`]; dropped OFD files take the same route.

#[cfg(not(target_arch = "wasm32"))]
use crate::pickers;
use crate::{PdfCraftApp, i18n};

impl PdfCraftApp {
    /// The File ▸ Open OFD (early alpha)… picker.
    pub(crate) fn open_ofd_dialog(&mut self) {
        self.startup_superseded = true;
        #[cfg(not(target_arch = "wasm32"))]
        self.pick(pickers::PickFor::Open, rfd::AsyncFileDialog::new().add_filter(tl!("OFD documents"), &["ofd"]), false);
        // Browsers pick files asynchronously; the bytes arrive through `inbox`.
        #[cfg(target_arch = "wasm32")]
        {
            let inbox = self.inbox.clone();
            let ctx = self.ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Some(h) = rfd::AsyncFileDialog::new().add_filter(tl!("OFD documents"), &["ofd"]).pick_file().await {
                    let bytes = h.read().await;
                    if let Ok(mut q) = inbox.lock() {
                        q.push((h.file_name(), bytes));
                    }
                    if let Some(ctx) = ctx {
                        ctx.request_repaint();
                    }
                }
            });
        }
    }

    /// Open `bytes` as a new PDF tab if they are an OFD file; `None` when they are not.
    pub(crate) fn open_if_ofd(&mut self, name: &str, bytes: &[u8]) -> Option<Result<(), String>> {
        if pdfcraft_engine::source_kind(name, bytes) != Some(pdfcraft_engine::SourceKind::Ofd) {
            return None;
        }
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        Some(self.open_ofd(&format!("{stem}.pdf"), name, bytes))
    }

    /// Convert OFD bytes into a new tab and say, in one notice, that this is early alpha and
    /// what was left out (unsupported objects, seals and signatures).
    fn open_ofd(&mut self, pdf_name: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
        let (pdf, report) = self.session.convert_ofd(name, bytes).map_err(|e| e.to_string())?;
        self.open_created_bytes(pdf_name, Ok(pdf))?;
        let mut notice = if report.skipped == 0 {
            tl!("OFD (early alpha): converted to PDF. Compare it with the original.").to_string()
        } else {
            i18n::fmt(tl!("OFD (early alpha): converted to PDF. Objects not shown: {n}."), &[("n", &report.skipped.to_string())])
        };
        if report.signed {
            notice.push(' ');
            notice.push_str(tl!("Signatures and seals are not carried over or checked."));
        }
        self.notify(notice);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored (uncompressed) ZIP: just enough for a synthetic OFD package.
    fn zip(files: &[(&str, &str)]) -> Vec<u8> {
        let crc = |bytes: &[u8]| {
            let mut c = 0xFFFF_FFFFu32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 == 1 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
                }
            }
            !c
        };
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data) in files {
            let offset = out.len() as u32;
            let header = |central: bool| {
                let mut h = Vec::new();
                h.extend(if central { 0x0201_4b50u32 } else { 0x0403_4b50 }.to_le_bytes());
                if central {
                    h.extend(20u16.to_le_bytes());
                }
                h.extend([20, 0, 0, 0x08, 0, 0, 0, 0, 0, 0]);
                h.extend(crc(data.as_bytes()).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((data.len() as u32).to_le_bytes());
                h.extend((name.len() as u16).to_le_bytes());
                h.extend([0, 0]);
                if central {
                    h.extend([0u8; 10]);
                    h.extend(offset.to_le_bytes());
                }
                h.extend(name.as_bytes());
                h
            };
            out.extend(header(false));
            out.extend(data.as_bytes());
            central.extend(header(true));
        }
        let cd = out.len() as u32;
        out.extend(&central);
        out.extend([0x50, 0x4b, 5, 6, 0, 0, 0, 0]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(cd.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    fn ofd(body: &str, signed: bool) -> Vec<u8> {
        let ns = r#"xmlns:ofd="http://www.ofdspec.org/2016""#;
        let signs = if signed { "<ofd:Signatures>Doc_0/Signs/Signatures.xml</ofd:Signatures>" } else { "" };
        let root = format!(
            r#"<ofd:OFD {ns} Version="1.0" DocType="OFD"><ofd:DocBody><ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot>{signs}</ofd:DocBody></ofd:OFD>"#
        );
        let document = format!(
            r#"<ofd:Document {ns}><ofd:CommonData><ofd:PageArea><ofd:PhysicalBox>0 0 100 50</ofd:PhysicalBox></ofd:PageArea></ofd:CommonData><ofd:Pages><ofd:Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/></ofd:Pages></ofd:Document>"#
        );
        let page = format!(r#"<ofd:Page {ns}><ofd:Content><ofd:Layer ID="2">{body}</ofd:Layer></ofd:Content></ofd:Page>"#);
        zip(&[("OFD.xml", &root), ("Doc_0/Document.xml", &document), ("Doc_0/Pages/Page_0/Content.xml", &page)])
    }

    fn app() -> PdfCraftApp {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app
    }

    #[test]
    fn an_ofd_file_opens_as_a_new_pdf_tab_with_an_early_alpha_notice() {
        let mut app = app();
        let path = r#"<ofd:PathObject ID="3" Boundary="10 10 20 20"><ofd:AbbreviatedData>M 0 0 L 20 20</ofd:AbbreviatedData></ofd:PathObject>"#;
        app.open_bytes("invoice.ofd", None, ofd(path, false)).unwrap();
        assert_eq!(app.views.len(), 1);
        assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("OFD (early alpha): converted to PDF. Compare it with the original."));
    }

    #[test]
    fn skipped_objects_and_seals_are_reported_in_one_notice() {
        let mut app = app();
        let composite = r#"<ofd:CompositeObject ID="3" Boundary="0 0 10 10" ResourceID="9"/>"#;
        app.open_bytes("invoice.ofd", None, ofd(composite, true)).unwrap();
        let toast = app.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
        assert_eq!(toast, "OFD (early alpha): converted to PDF. Objects not shown: 1. Signatures and seals are not carried over or checked.");
    }

    /// The live app reads and prepares files on the open worker; an OFD file must come back
    /// from it as a conversion, not as a PDF that fails to parse.
    #[test]
    #[allow(clippy::expect_used)]
    fn an_ofd_file_opened_in_the_running_app_converts_after_background_preparation() {
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_eframe(|_| app());
        harness.run_steps(2);
        let (gate, started, release, finished) = crate::open_worker::prepare_gate();
        harness.state_mut().open_prepare_gate = Some(gate);
        let path = std::env::temp_dir().join(format!("pdfcraft-ofd-worker-{}.ofd", std::process::id()));
        let shape = r#"<ofd:PathObject ID="3" Boundary="10 10 20 20"><ofd:AbbreviatedData>M 0 0 L 20 20</ofd:AbbreviatedData></ofd:PathObject>"#;
        std::fs::write(&path, ofd(shape, false)).expect("write the synthetic OFD file");
        harness.state_mut().open_path(&path.to_string_lossy());
        let timeout = std::time::Duration::from_secs(5);
        started.recv_timeout(timeout).expect("the OFD file reached the open worker");
        release.send(()).expect("release the open worker");
        finished.recv_timeout(timeout).expect("the open worker finished");
        harness.run_steps(2);
        let _ = std::fs::remove_file(&path);
        let app = harness.state();
        assert_eq!(app.views.len(), 1, "the OFD file opens as one new PDF tab");
        assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("OFD (early alpha): converted to PDF. Compare it with the original."));
    }

    #[test]
    fn other_files_are_not_taken_for_ofd() {
        let mut app = app();
        assert!(app.open_if_ofd("a.pdf", b"%PDF-1.7").is_none());
        assert!(app.open_if_ofd("a.zip", &zip(&[("word/document.xml", "<w/>")])).is_none());
        // A broken OFD is an error the user sees, not a panic.
        let mut broken = ofd("", false);
        broken.truncate(broken.len() - 30);
        assert!(app.open_bytes("bad.ofd", None, broken).is_err());
        assert!(app.views.is_empty());
    }
}
