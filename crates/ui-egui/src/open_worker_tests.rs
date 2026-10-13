use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui_kittest::Harness;
use serde_json::{Value, json};

use crate::{PdfCraftApp, control::ControlClient, open_worker};

fn drain(app: &mut PdfCraftApp) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !app.pending_opens.is_empty() && app.password_prompt.is_none() {
        app.poll_pending_opens();
        assert!(std::time::Instant::now() < deadline, "opening did not complete");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[allow(clippy::unwrap_used)]
fn test_path(name: &str, bytes: &[u8]) -> String {
    let path = std::env::temp_dir().join(format!("pdfcraft-worker-{}-{name}.pdf", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn queued_open_runs_the_requested_tool_on_its_own_document() {
    let path = test_path("tool", &fixture(2));
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.open_path_then(&path, "page.insert_blank");
    drain(&mut app);
    assert_eq!(app.views.len(), 1);
    assert_eq!(app.session.get(app.views[0].id).unwrap().info.pages.len(), 3);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn queued_session_restores_each_page_and_the_saved_active_tab() {
    let a = test_path("session-a", &fixture(5));
    let b = test_path("session-b", &fixture(4));
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.reopen_last_session = true;
    app.last_session = crate::last_session::LastSession {
        files: vec![crate::last_session::SessionFile { path: a.clone(), page: 3 }, crate::last_session::SessionFile { path: b.clone(), page: 2 }],
        active: Some(a.clone()),
    };
    app.reopen_last_files(&[]);
    drain(&mut app);
    assert_eq!(app.views.iter().map(|v| v.current).collect::<Vec<_>>(), [3, 2]);
    assert_eq!(app.active, Some(0));
    for path in [a, b] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn startup_view_options_do_not_leak_to_a_later_open_request() {
    let startup = test_path("options-startup", &fixture(4));
    let later = test_path("options-later", &fixture(4));
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.open_path(&startup);
    app.set_startup_options(vec![("page".into(), "3".into())]);
    app.open_path(&later);
    drain(&mut app);
    assert_eq!(app.views.iter().map(|v| v.current).collect::<Vec<_>>(), [2, 0]);
    for path in [startup, later] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn startup_view_options_survive_password_retries_on_their_original_request() {
    let startup = test_path("options-secret", &encrypted());
    let later = test_path("options-after-secret", &fixture(3));
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.open_path(&startup);
    app.set_startup_options(vec![("page".into(), "2".into())]);
    app.open_path(&later);
    drain(&mut app);
    app.submit_password(Some("wrong".into()));
    drain(&mut app);
    app.submit_password(Some("pw".into()));
    drain(&mut app);
    assert_eq!(app.views.iter().map(|v| v.current).collect::<Vec<_>>(), [1, 0]);
    for path in [startup, later] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn failed_or_cancelled_startup_does_not_apply_view_options_to_another_file() {
    for (name, bytes) in [("failed", b"invalid PDF".to_vec()), ("cancelled", encrypted())] {
        let startup = test_path(&format!("options-{name}"), &bytes);
        let later = test_path(&format!("options-after-{name}"), &fixture(3));
        let mut app = PdfCraftApp::new();
        app.enable_background_open(&egui::Context::default());
        app.open_path(&startup);
        app.set_startup_options(vec![("page".into(), "2".into())]);
        app.open_path(&later);
        drain(&mut app);
        if name == "cancelled" {
            assert!(app.password_prompt.is_some());
            app.submit_password(None);
            drain(&mut app);
        }
        assert_eq!(app.views.len(), 1);
        assert_eq!(app.views[0].current, 0);
        assert!(app.startup_options.is_empty());
        for path in [startup, later] {
            std::fs::remove_file(path).unwrap();
        }
    }
}

#[test]
fn queued_password_results_do_not_replace_an_unanswered_prompt() {
    let mut app = PdfCraftApp::new();
    for (token, name) in [(1, "first.pdf"), (2, "second.pdf")] {
        let (tx, receiver) = std::sync::mpsc::channel();
        tx.send(open_worker::ResultMessage {
            token,
            kind: open_worker::ResultKind::Prepared { bytes: Arc::new(fixture(1)), result: Box::new(Err(pdfcraft_render::OpenError::NeedsPassword)) },
        })
        .unwrap();
        app.pending_opens.push(crate::PendingOpen {
            token,
            receiver,
            context: app.session.open_context(),
            password: None,
            name: name.into(),
            path: None,
            stamp: None,
            activate: true,
            after_open: None,
            recovered: None,
            then: None,
        });
    }
    app.poll_pending_opens();
    assert_eq!(app.password_prompt.as_ref().unwrap().name, "first.pdf");
    app.poll_pending_opens();
    assert_eq!(app.password_prompt.as_ref().unwrap().name, "first.pdf");
    app.submit_password(None);
    app.poll_pending_opens();
    assert_eq!(app.password_prompt.as_ref().unwrap().name, "second.pdf");
}

#[allow(clippy::unwrap_used)]
fn encrypted() -> Vec<u8> {
    let mut doc = pdfcraft_cos::Document::open(Arc::new(fixture(2))).unwrap();
    doc.set_encryption(&pdfcraft_cos::NewEncryption {
        algorithm: pdfcraft_cos::Algorithm::Aes256,
        user_password: "pw",
        owner_password: "owner",
        permissions: -1,
        encrypt_metadata: true,
        seed: [7; 32],
    })
    .unwrap();
    pdfcraft_cos::write_full(&doc, &Default::default()).unwrap()
}

#[test]
fn startup_opens_before_the_first_frame_without_blocking_and_keeps_options() {
    let path = test_path("startup", &fixture(5));
    let mut app = PdfCraftApp::new();
    let (gate, started, release, finished) = open_worker::prepare_gate();
    app.open_prepare_gate = Some(gate);
    app.enable_background_open(&egui::Context::default());
    app.open_path(&path);
    assert!(app.views.is_empty(), "startup must not prepare synchronously before the first frame");
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    app.set_startup_options(vec![("page".into(), "4".into()), ("zoom".into(), "150".into())]);
    assert!(app.toast.is_none(), "view options wait for their document");
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    drain(&mut app);
    assert_eq!(app.views[0].current, 3);
    assert_eq!(app.views[0].zoom, 1.5);
    assert!(app.startup_options.is_empty());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn password_retry_keeps_its_tool_and_does_not_lose_the_next_file() {
    let secret = test_path("secret", &encrypted());
    let plain = test_path("after-secret", &fixture(1));
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.open_path_then(&secret, "page.insert_blank");
    app.open_path(&plain);
    drain(&mut app);
    assert_eq!(app.password_prompt.as_ref().unwrap().path.as_deref(), Some(secret.as_str()));
    app.submit_password(Some("wrong".into()));
    drain(&mut app);
    assert!(app.password_prompt.as_ref().unwrap().error.is_some());
    assert!(app.views.is_empty(), "a retry retains its place ahead of later documents");
    app.submit_password(Some("pw".into()));
    drain(&mut app);
    assert_eq!(app.views.len(), 2);
    assert_eq!(app.session.get(app.views[0].id).unwrap().info.pages.len(), 3);
    assert_eq!(app.session.get(app.views[1].id).unwrap().info.pages.len(), 1);
    for path in [secret, plain] {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn startup_bytes_wait_while_a_local_password_retry_is_preparing() {
    for password in ["wrong", "pw"] {
        let path = test_path(&format!("startup-retry-{password}"), &encrypted());
        let (mut h, _) = harness();
        let (gate, started, release, finished) = open_worker::prepare_gate();
        h.state_mut().open_prepare_gate = Some(gate);
        h.state_mut().open_path_then(&path, "page.insert_blank");
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
        h.step();
        assert!(h.state().password_prompt.is_some());

        h.state_mut().submit_password(Some(password.into()));
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        h.state().startup_inbox.lock().unwrap().push(("late-encrypted.pdf".into(), encrypted()));
        h.step();
        let queued_while_held = h.state().startup_inbox.lock().unwrap().len();
        let prompt_while_held = h.state().password_prompt.as_ref().map(|p| p.name.clone());
        let tabs_while_held = h.state().views.len();
        // Release before asserting so even a failed regression cannot strand a worker.
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(5)).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(queued_while_held, 1, "startup bytes must remain queued during {password:?} preparation");
        assert_eq!(prompt_while_held, None, "a late file must not occupy the local retry's prompt");
        assert_eq!(tabs_while_held, 1);

        h.step();
        if password == "wrong" {
            assert!(h.state().password_prompt.as_ref().unwrap().error.is_some());
            assert_eq!(h.state().startup_inbox.lock().unwrap().len(), 1);
            assert_eq!(h.state().views.len(), 1);
            h.state_mut().submit_password(None);
            h.step();
            assert_eq!(h.state().views.len(), 1, "cancelling must not run the deferred tool on the existing tab");
        } else {
            assert_eq!(h.state().views.len(), 2);
            let app = h.state();
            assert_eq!(app.session.get(app.views[1].id).unwrap().info.pages.len(), 3, "the tool runs once on the unlocked PDF");
        }
        assert_eq!(h.state().password_prompt.as_ref().unwrap().name, "late-encrypted.pdf");
        assert_eq!(h.state().session.get(h.state().views[0].id).unwrap().info.pages.len(), 3, "the existing tab is untouched");
    }
}

#[test]
fn reopening_a_closed_tab_consumes_history_only_after_success() {
    let path = test_path("reopen", &fixture(1));
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.closed_tabs.push(path.clone());
    app.tab_action(crate::tab_context::Action::Reopen);
    app.tab_action(crate::tab_context::Action::Reopen);
    assert_eq!(app.pending_opens.len(), 1);
    assert_eq!(app.closed_tabs.len(), 1);
    drain(&mut app);
    assert_eq!(app.views.len(), 1);
    assert!(app.closed_tabs.is_empty());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn conversion_runs_the_requested_tool_and_failures_leave_existing_tabs_alone() {
    let text = test_path("convert", b"Hello from a synthetic text document");
    let named_text = format!("{text}.txt");
    std::fs::rename(&text, &named_text).unwrap();
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.open_path_then(&named_text, "page.insert_blank");
    drain(&mut app);
    assert_eq!(app.views.len(), 1);
    assert_eq!(app.session.get(app.views[0].id).unwrap().info.pages.len(), 2);
    std::fs::remove_file(&named_text).unwrap();
    app.open_path_then(&named_text, "page.insert_blank");
    drain(&mut app);
    assert_eq!(app.views.len(), 1);
    assert_eq!(app.session.get(app.views[0].id).unwrap().info.pages.len(), 2);
}

fn wait_until(h: &mut Harness<'static, PdfCraftApp>, complete: impl Fn(&PdfCraftApp) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !complete(h.state()) {
        assert!(std::time::Instant::now() < deadline, "asynchronous opening did not reach the expected state");
        h.step();
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn picker_tool_runs_once_after_plain_or_encrypted_open_and_never_on_cancel() {
    for (name, bytes, cancel) in [("plain", fixture(2), false), ("encrypted", encrypted(), false), ("cancel", encrypted(), true)] {
        let path = test_path(&format!("picker-tool-{name}"), &bytes);
        let (mut h, _) = harness();
        h.state_mut().active = None;
        h.state_mut().pick_override = Some(vec![path.clone()]);
        h.state_mut().open_then_run(pdfcraft_engine::commands::command("page.insert_blank").unwrap());
        wait_until(&mut h, |app| app.password_prompt.is_some() || app.views.len() == 2);
        if name != "plain" {
            h.state_mut().submit_password(Some("wrong".into()));
            wait_until(&mut h, |app| app.password_prompt.is_some());
            assert!(h.state().password_prompt.as_ref().unwrap().error.is_some());
            assert_eq!(h.state().views.len(), 1);
            h.state_mut().submit_password((!cancel).then(|| "pw".into()));
            if !cancel {
                wait_until(&mut h, |app| app.views.len() == 2);
            }
        }
        h.run_steps(3);
        assert_eq!(h.state().views.len(), if cancel { 1 } else { 2 });
        assert!(h.state().pending_opens.is_empty());
        for view in &h.state().views {
            assert_eq!(h.state().session.get(view.id).unwrap().info.pages.len(), 3, "the old tab is unchanged; the new tab gets one blank page");
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn background_text_and_image_conversion_only_run_the_tool_after_success() {
    let mut image = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut image, 2, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&[120; 12]).unwrap();
    }
    for (name, bytes, succeeds) in [
        ("valid.txt", b"Synthetic conversion test".to_vec(), true),
        ("valid.png", image, true),
        ("invalid.txt", "Unsupported plain-text glyph: \u{4e2d}".as_bytes().to_vec(), false),
        ("invalid.png", b"invalid PNG".to_vec(), false),
    ] {
        let path = std::env::temp_dir().join(format!("pdfcraft-convert-worker-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let (mut h, _) = harness();
        h.state_mut().open_path_then(&path.to_string_lossy(), "page.insert_blank");
        wait_until(&mut h, |app| app.pending_opens.is_empty());
        h.run_steps(3);
        let app = h.state();
        assert_eq!(app.views.len(), if succeeds { 2 } else { 1 }, "{name}");
        assert_eq!(app.session.get(app.views[0].id).unwrap().info.pages.len(), 3, "existing tab: {name}");
        if succeeds {
            let converted = app.session.get(app.views[1].id).unwrap();
            assert_eq!(converted.info.pages.len(), 2, "one conversion page plus exactly one inserted page: {name}");
            assert!(converted.dirty && converted.path.is_none());
        } else {
            assert!(app.toast.is_some(), "failed conversion is reported: {name}");
        }
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn opening_queue_accepts_a_whole_saved_session_and_is_bounded() {
    let path = test_path("bound", &fixture(1));
    let mut app = PdfCraftApp::new();
    let (gate, started, release, finished) = open_worker::prepare_gate();
    app.open_prepare_gate = Some(gate);
    app.enable_background_open(&egui::Context::default());
    for _ in 0..crate::last_session::MAX_FILES {
        app.open_path(&path);
    }
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(app.pending_opens.len(), crate::last_session::MAX_FILES);
    app.open_path(&path);
    assert_eq!(app.pending_opens.len(), crate::last_session::MAX_FILES);
    assert!(app.toast.as_ref().unwrap().0.contains("too many"));
    drop(app);
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pending_restore_is_preserved_if_the_app_quits_before_opening_finishes() {
    let path = test_path("persist-pending", &fixture(4));
    let mut app = PdfCraftApp::new();
    let (gate, started, release, finished) = open_worker::prepare_gate();
    app.open_prepare_gate = Some(gate);
    app.enable_background_open(&egui::Context::default());
    app.open_path_with(&path, true, Some(crate::OpenDestination::SessionPage(2)), None);
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    let saved = app.open_session();
    assert_eq!(saved.files, [crate::last_session::SessionFile { path: path.clone(), page: 2 }]);
    assert_eq!(saved.active.as_deref(), Some(path.as_str()));
    drop(app);
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pending_open_reprepares_after_security_preferences_change() {
    let path = test_path("policy", &fixture(2));
    let mut app = PdfCraftApp::new();
    let (gate, started, release, finished) = open_worker::prepare_gate();
    app.open_prepare_gate = Some(gate);
    app.enable_background_open(&egui::Context::default());
    app.open_path(&path);
    app.set_startup_options(vec![("page".into(), "2".into())]);
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    app.session.set_javascript(false);
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    app.poll_pending_opens();
    assert!(app.views.is_empty(), "do not commit preparation made under stale security settings");
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    app.session.set_trusted_certificates(Vec::new());
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    app.poll_pending_opens();
    assert!(app.views.is_empty(), "certificate trust changes also require fresh preparation");
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    drain(&mut app);
    assert_eq!(app.views.len(), 1);
    assert_eq!(app.views[0].current, 1);
    assert!(app.startup_open.is_none());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn failed_background_recovery_remains_available_and_success_keeps_its_origin() {
    let dir = std::env::temp_dir().join(format!("pdfcraft-worker-recovery-{}", std::process::id()));
    let store = crate::RecoveryStore::new(&dir);
    let meta = crate::RecoveryMeta {
        key: "snapshot".into(),
        name: "recovered.pdf".into(),
        path: Some("original.pdf".into()),
        saved_at: 1,
        encrypted: false,
    };
    store.write(&meta, b"invalid PDF").unwrap();
    let mut app = PdfCraftApp::new();
    app.enable_background_open(&egui::Context::default());
    app.enable_recovery(store.clone());
    // The Recover button dismisses this modal before completions are consumed.
    app.dialog = None;
    app.recover(std::slice::from_ref(&meta.key));
    assert!(app.recoverable.is_empty(), "accepted work is not offered twice");
    drain(&mut app);
    assert!(app.views.is_empty());
    assert_eq!(app.recoverable.as_slice(), std::slice::from_ref(&meta), "a failed open can be retried");
    store.write(&meta, &fixture(2)).unwrap();
    app.recover(std::slice::from_ref(&meta.key));
    drain(&mut app);
    assert!(app.recoverable.is_empty());
    let doc = app.session.get(app.views[0].id).unwrap();
    assert_eq!(doc.path, meta.path);
    assert!(doc.dirty);
    assert_eq!(app.recovery_keys.get(&doc.id), Some(&meta.key));
    store.remove(&meta.key);
    std::fs::remove_dir(dir).unwrap();
}

fn fixture(pages: usize) -> Vec<u8> {
    let mut objects = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string()];
    let kids = (0..pages).map(|page| format!("{} 0 R", 4 + 2 * page)).collect::<Vec<_>>().join(" ");
    objects.push(format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 200 300] >>"));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for page in 0..pages {
        objects.push(format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>", 5 + 2 * page));
        let body = format!("BT /F1 24 Tf 20 150 Td (Page {}) Tj ET", page + 1);
        objects.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()));
    }
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    bytes
}

#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
fn harness() -> (Harness<'static, PdfCraftApp>, ControlClient) {
    let slot: Arc<Mutex<Option<ControlClient>>> = Arc::default();
    let saved = slot.clone();
    let mut harness = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_eframe(move |creation| {
        let mut app = PdfCraftApp::new();
        app.enable_background_open(&creation.egui_ctx);
        app.set_option("language", "en").expect("language preference");
        *saved.lock().expect("control-client slot") = Some(app.attach_control(&creation.egui_ctx));
        app.open_bytes("existing.pdf", None, fixture(3)).expect("existing document");
        app
    });
    harness.run_steps(3);
    (harness, slot.lock().expect("control-client slot").take().expect("control client"))
}

#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
fn request(harness: &mut Harness<'static, PdfCraftApp>, client: &ControlClient, method: &str, params: Value) -> Value {
    let reply = client.send(method, params);
    for _ in 0..5 {
        harness.step();
        if let Ok(result) = reply.try_recv() {
            return result.expect("control request succeeded");
        }
    }
    panic!("{method} was not answered during the held prepare phase");
}

#[test]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
fn open_prepare_keeps_control_and_existing_tabs_live_until_atomic_commit() {
    let (mut harness, client) = harness();
    let (gate, started, release, finished) = open_worker::prepare_gate();
    harness.state_mut().open_prepare_gate = Some(gate);

    let path = std::env::temp_dir().join(format!("pdfcraft-open-worker-{}.pdf", std::process::id()));
    std::fs::write(&path, fixture(2)).expect("write test PDF");
    let open_reply = request(&mut harness, &client, "ui.open", json!({ "path": path.to_string_lossy() }));
    assert_eq!(open_reply["pending"], true);
    started.recv_timeout(Duration::from_secs(5)).expect("worker reached prepare barrier");

    let state = request(&mut harness, &client, "ui.state", json!({}));
    assert_eq!(state["documents"].as_array().map(Vec::len), Some(1));
    assert_eq!(state["documents"][0]["name"], "existing.pdf");
    assert_eq!(state["pending_opens"].as_array().map(Vec::len), Some(1));
    request(&mut harness, &client, "ui.command", json!({ "id": "page.insert_blank" }));
    assert_eq!(harness.state().session.get(harness.state().views[0].id).expect("existing tab").info.pages.len(), 4);
    assert_eq!(harness.state().views.len(), 1, "prepared state is not visible in the session");

    release.send(()).expect("release prepare barrier");
    finished.recv_timeout(Duration::from_secs(5)).expect("worker finished preparation");
    harness.step();
    let app = harness.state();
    assert_eq!(app.views.len(), 2, "one prepared document commits exactly once");
    assert_eq!(app.session.get(app.views[1].id).expect("new tab").info.pages.len(), 2);
    assert_eq!(app.active, Some(1));
    let _ = std::fs::remove_file(path);
}
