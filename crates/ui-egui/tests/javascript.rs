//! Acrobat JavaScript in the shell (egui_kittest): the console, Document JavaScripts, a button
//! script and Preferences ▸ JavaScript.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};

/// A form with a text field and a push button whose Mouse Up script fills it in.
fn form() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R 5 0 R] /DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 6 0 R >> >> >> >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>",
        "<< /Type /Page /Parent 2 0 R /Annots [4 0 R 5 0 R] >>",
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (greeting) /Rect [20 250 280 270] /P 3 0 R /F 4 >>",
        "<< /Type /Annot /Subtype /Widget /FT /Btn /Ff 65536 /T (hello) /Rect [20 200 120 230] /P 3 0 R /F 4 /A << /S /JavaScript /JS (getField\\('greeting'\\).value = 'Hello from JavaScript'; app.alert\\('Filled in'\\);) >> >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("form.pdf", None, form()).unwrap();
        app
    });
    h.run_steps(4);
    h
}

fn value(h: &Harness<'static, PdfCraftApp>, name: &str) -> Vec<String> {
    let app = h.state();
    let id = app.active_ids().unwrap().1;
    app.session.get(id).unwrap().form.iter().find(|f| f.name == name).unwrap().value.clone()
}

#[test]
fn console_runs_scripts_against_the_document() {
    let mut h = harness();
    assert!(h.state_mut().execute("tools.js_console"));
    h.run_steps(2);
    h.get_by_label("JavaScript Console");
    h.state_mut().js_console.input = "getField('greeting').value = 'typed'; console.println(numFields); 6 * 7".into();
    h.get_by_label("Run").click();
    h.run_steps(2);
    assert_eq!(value(&h, "greeting"), ["typed"]);
    let log = h.state().js_console.log.clone();
    assert!(log.contains(&"2".to_string()) && log.contains(&"42".to_string()), "{log:?}");
    h.state_mut().js_console.input = "throw new Error('boom')".into();
    h.run_steps(4);
    h.get_by_label("Run").click();
    h.run_steps(2);
    assert!(h.state().js_console.log.last().unwrap().contains("boom"));
    h.run_steps(4);
    h.get_by_label("Clear").click();
    h.run_steps(2);
    assert!(h.state().js_console.log.is_empty(), "{:?} {:?}", h.state().js_console.log, h.state().dialog);
}

#[test]
fn button_scripts_run_and_alert() {
    let mut h = harness();
    let id = h.state().active_ids().unwrap().1;
    h.state_mut().run_button_script(id, "hello", "getField('greeting').value = 'Hello from JavaScript'; app.alert('Filled in');");
    h.run_steps(2);
    assert_eq!(value(&h, "greeting"), ["Hello from JavaScript"]);
    assert_eq!(h.state().toast.clone().unwrap().0, "Filled in");
}

#[test]
fn form_submission_shows_destination_and_cancel_sends_nothing() {
    use pdfcraft_engine::{Edit, FieldValue};

    let mut h = harness();
    let id = h.state().active_ids().unwrap().1;
    assert!(h.state_mut().apply_edit(Edit::SetFieldValue { name: "greeting".into(), value: FieldValue::Text("Ada & Co".into()) }));
    h.state_mut().request_form_submission(id, "https://forms.example/submit");
    h.run_steps(2);
    assert_eq!(h.state().pending_submission.as_ref().unwrap().body.as_slice(), b"greeting=Ada+%26+Co");
    assert!(h.query_all_by_label("Submit a form").count() >= 1);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().pending_submission.is_none());
}

#[test]
fn submit_button_opens_the_review_prompt() {
    use pdfcraft_engine::{Edit, FieldAction, FieldProps, FieldTrigger, FieldValue};

    let mut h = harness();
    assert!(h.state_mut().apply_edit(Edit::SetFieldValue { name: "greeting".into(), value: FieldValue::Text("Ada".into()) }));
    assert!(h.state_mut().apply_edit(Edit::SetFieldProps {
        name: "hello".into(),
        props: Box::new(FieldProps {
            actions: Some(vec![(FieldTrigger::MouseUp, FieldAction::Submit("https://forms.example/submit".into()))]),
            ..Default::default()
        }),
    }));
    h.run_steps(3);
    let p = {
        let app = h.state();
        let doc = app.session.get(app.views[0].id).unwrap();
        let field = doc.form.iter().find(|f| f.name == "hello").unwrap();
        pdfcraft_ui_egui::forms_ui::field_screen_rect(&app.views[0], &doc.info, field, 0).unwrap().center()
    };
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
    let pending = h.state().pending_submission.as_ref().expect("button click opens a prompt");
    assert_eq!(pending.url, "https://forms.example/submit");
    assert_eq!(pending.body.as_slice(), b"greeting=Ada");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().pending_submission.is_none());
}

#[test]
fn submit_pdf_button_reviews_entire_current_document() {
    use pdfcraft_engine::{Edit, FieldAction, FieldProps, FieldTrigger, FieldValue};
    use pdfcraft_ui_egui::SubmissionFormat;

    let mut h = harness();
    assert!(h.state_mut().apply_edit(Edit::SetFieldValue { name: "greeting".into(), value: FieldValue::Text("Ada".into()) }));
    assert!(h.state_mut().apply_edit(Edit::SetFieldProps {
        name: "hello".into(),
        props: Box::new(FieldProps {
            actions: Some(vec![(FieldTrigger::MouseUp, FieldAction::SubmitPdf("https://forms.example/upload".into()))]),
            ..Default::default()
        }),
    }));
    h.run_steps(3);
    let p = {
        let app = h.state();
        let doc = app.session.get(app.views[0].id).unwrap();
        let field = doc.form.iter().find(|f| f.name == "hello").unwrap();
        pdfcraft_ui_egui::forms_ui::field_screen_rect(&app.views[0], &doc.info, field, 0).unwrap().center()
    };
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
    let pending = h.state().pending_submission.as_ref().expect("PDF button opens a prompt");
    assert_eq!(pending.format, SubmissionFormat::Pdf);
    assert_eq!(pending.url, "https://forms.example/upload");
    assert!(pending.body.starts_with(b"%PDF-"));
    assert!(h.query_all_by_label("Submit PDF document").count() >= 1);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().pending_submission.is_none());
}

#[test]
fn submit_xfdf_button_reviews_current_form_data() {
    use pdfcraft_engine::{Edit, FieldAction, FieldProps, FieldTrigger, FieldValue};
    use pdfcraft_ui_egui::SubmissionFormat;

    let mut h = harness();
    assert!(h.state_mut().apply_edit(Edit::SetFieldValue { name: "greeting".into(), value: FieldValue::Text("Ada & Co".into()) }));
    assert!(h.state_mut().apply_edit(Edit::SetFieldProps {
        name: "hello".into(),
        props: Box::new(FieldProps {
            actions: Some(vec![(FieldTrigger::MouseUp, FieldAction::SubmitXfdf("https://forms.example/xfdf".into()))]),
            ..Default::default()
        }),
    }));
    h.run_steps(3);
    let p = {
        let app = h.state();
        let doc = app.session.get(app.views[0].id).unwrap();
        let field = doc.form.iter().find(|f| f.name == "hello").unwrap();
        pdfcraft_ui_egui::forms_ui::field_screen_rect(&app.views[0], &doc.info, field, 0).unwrap().center()
    };
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
    let pending = h.state().pending_submission.as_ref().expect("XFDF button opens a prompt");
    assert_eq!(pending.format, SubmissionFormat::Xfdf);
    assert_eq!(pending.url, "https://forms.example/xfdf");
    assert!(String::from_utf8_lossy(&pending.body).contains("Ada &amp; Co"));
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().pending_submission.is_none());
}

#[test]
fn submit_get_button_reviews_query_transport_before_sending() {
    use pdfcraft_engine::{Edit, FieldAction, FieldProps, FieldTrigger, FieldValue};
    use pdfcraft_ui_egui::SubmissionFormat;

    let mut h = harness();
    assert!(h.state_mut().apply_edit(Edit::SetFieldValue { name: "greeting".into(), value: FieldValue::Text("Ada & Co".into()) }));
    assert!(h.state_mut().apply_edit(Edit::SetFieldProps {
        name: "hello".into(),
        props: Box::new(FieldProps {
            actions: Some(vec![(FieldTrigger::MouseUp, FieldAction::SubmitGet("https://forms.example/submit?ticket=7".into()))]),
            ..Default::default()
        }),
    }));
    h.run_steps(3);
    let p = {
        let app = h.state();
        let doc = app.session.get(app.views[0].id).unwrap();
        let field = doc.form.iter().find(|f| f.name == "hello").unwrap();
        pdfcraft_ui_egui::forms_ui::field_screen_rect(&app.views[0], &doc.info, field, 0).unwrap().center()
    };
    h.hover_at(p);
    h.run_steps(1);
    h.drag_at(p);
    h.run_steps(1);
    h.drop_at(p);
    h.run_steps(3);
    let pending = h.state().pending_submission.as_ref().expect("GET button opens a prompt");
    assert_eq!(pending.format, SubmissionFormat::HtmlGet);
    assert_eq!(pending.url, "https://forms.example/submit?ticket=7");
    assert_eq!(pending.body.as_slice(), b"greeting=Ada+%26+Co");
    assert!(h.query_all_by_label("GET").count() >= 1);
    assert!(h.query_all_by_label("Only send if you trust this document and destination. GET puts form values in the URL.").count() >= 1);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().pending_submission.is_none());
}

#[test]
fn fdf_submission_reviews_comments_option() {
    use pdfcraft_engine::{Edit, FieldValue, form_submit::SubmitOptions};
    use pdfcraft_ui_egui::SubmissionFormat;

    let mut h = harness();
    let id = h.state().active_ids().unwrap().1;
    assert!(h.state_mut().apply_edit(Edit::SetFieldValue { name: "greeting".into(), value: FieldValue::Text("Ada".into()) }));
    h.state_mut().request_selected_form_submission(id, "https://forms.example/fdf", SubmissionFormat::Fdf, &SubmitOptions::default(), true);
    h.run_steps(2);
    let pending = h.state().pending_submission.as_ref().unwrap();
    assert_eq!(pending.format, SubmissionFormat::Fdf);
    assert!(pending.include_annotations);
    assert!(pending.body.starts_with(b"%FDF-"));
    assert!(h.query_all_by_label("Comments to send").count() >= 1);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().pending_submission.is_none());
}

#[test]
fn approved_form_submission_posts_in_the_background() {
    use pdfcraft_engine::{Edit, FieldValue};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/submit", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if bytes.windows(b"greeting=Ada".len()).any(|w| w == b"greeting=Ada") {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                break;
            }
        }
    });
    let mut h = harness();
    let id = h.state().active_ids().unwrap().1;
    assert!(h.state_mut().apply_edit(Edit::SetFieldValue { name: "greeting".into(), value: FieldValue::Text("Ada".into()) }));
    h.state_mut().request_form_submission(id, &url);
    h.state_mut().resolve_pending_submission(true);
    server.join().unwrap();
    for _ in 0..30 {
        h.run_steps(1);
        if h.state().toast.as_ref().is_some_and(|t| t.0.contains("Form sent")) {
            break;
        }
    }
    assert!(h.state().toast.as_ref().is_some_and(|t| t.0.contains("Form sent")), "{:?}", h.state().toast);
}

#[test]
fn document_scripts_and_preferences() {
    let mut h = harness();
    assert!(h.state_mut().execute("tools.document_js"));
    h.run_steps(2);
    h.get_by_label("Document JavaScripts");
    h.state_mut().doc_js.name = "init".into();
    h.state_mut().doc_js.script = "function hi() { return 'hi'; }".into();
    h.get_by_label("Save").click();
    h.run_steps(2);
    let id = h.state().active_ids().unwrap().1;
    assert_eq!(h.state().session.get(id).unwrap().document_scripts()[0].0, "init");
    h.state_mut().dialog = Some(Dialog::Preferences);
    h.run_steps(2);
    // Below the fold in a 900-point window: the preferences scroll to it.
    h.get_by_label("Enable Acrobat JavaScript").scroll_to_me();
    h.run_steps(4);
    h.get_by_label("Enable Acrobat JavaScript").click();
    h.run_steps(2);
    assert!(!h.state().session.javascript());
    let saved = h.state().persist();
    let mut fresh = PdfCraftApp::new();
    fresh.set_option("language", "en").unwrap();
    fresh.restore(&saved);
    assert!(!fresh.session.javascript(), "the preference is remembered");
}

#[test]
fn merge_data_files_into_a_spreadsheet() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-merge-ui-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("report.csv");
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.save_override = Some(out.to_string_lossy().into_owned());
    app.merge_data_files(vec![("form.pdf".into(), form())]);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "greeting\n\n");
    assert!(app.toast.clone().unwrap().0.starts_with("Merged 1 file"));
    app.merge_data_files(vec![("x.fdf".into(), b"junk".to_vec())]);
    assert!(app.toast.clone().unwrap().0.starts_with("x.fdf:"));
}

#[test]
fn prepare_a_form_detects_fields_on_a_paper_form() {
    let paper = pdfcraft_engine::Session::new().create_from_text("t", "Name: ____________________\n\nPhone: ____________________").unwrap().to_vec();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("paper.pdf", None, paper.clone()).unwrap();
        app
    });
    h.run_steps(4);
    assert!(h.state_mut().execute("form.prepare"));
    h.run_steps(3);
    let app = h.state();
    let id = app.active_ids().unwrap().1;
    let names: Vec<String> = app.session.get(id).unwrap().form.iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, ["Name", "Phone"]);
    assert_eq!(app.toast.clone().unwrap().0, "Detected 2 form fields");
}
