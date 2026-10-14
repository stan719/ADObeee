//! Protect Using Password in the real shell (egui_kittest).

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{Dialog, PdfCraftApp};

const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
        app
    });
    h.run_steps(4);
    h
}

fn type_into(h: &mut Harness<'static, PdfCraftApp>, label: &str, text: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
    h.get_by_label(label).type_text(text);
    h.run_steps(2);
}

#[test]
fn protecting_for_viewing_requires_matching_passwords_and_saves_encrypted() {
    let mut h = open_dialog();
    assert_eq!(h.state().dialog, Some(Dialog::Protect));
    h.get_by_role_and_label(Role::RadioButton, "Open the document");
    type_into(&mut h, "Password", "Open-Sesame-42");
    type_into(&mut h, "Confirm password", "Open-Sesame-4");
    // Leaving the confirmation with a mismatch reports it, and Apply keeps the dialog open.
    h.get_by_label("Apply protection").click();
    h.run_steps(3);
    h.get_by_label("Passwords do not match.");
    assert_eq!(h.state().dialog, Some(Dialog::Protect));
    type_into(&mut h, "Confirm password", "2");
    h.get_by_label("Apply protection").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    let id = h.state().views[0].id;
    let doc = h.state().session.get(id).unwrap();
    assert!(doc.info.encrypted && doc.dirty);
    assert!(doc.security_summary().unwrap().pending);
    // What Save writes needs the password.
    let bytes = h.state().session.save_bytes(id).unwrap();
    let mut s = pdfcraft_engine::Session::new();
    assert!(s.open("p.pdf", None, bytes.clone(), None).is_err());
    assert!(s.open("p.pdf", None, bytes, Some("Open-Sesame-42")).is_ok());
    // Remove security is now offered, and undo also takes the protection back.
    assert!(h.state_mut().execute("protect.remove"));
    h.state_mut().undo();
    h.state_mut().undo();
    assert!(!h.state().session.get(id).unwrap().info.encrypted);
}

#[test]
fn a_password_made_only_of_ignored_characters_is_named_under_its_field() {
    for purpose in ["Open the document", "Restrict editing"] {
        let mut h = open_dialog();
        h.get_by_role_and_label(Role::RadioButton, purpose).click();
        h.run_steps(2);
        let id = h.state().views[0].id;
        let before = h.state().session.get(id).unwrap().bytes.clone();
        type_into(&mut h, "Password", "\u{ad}");
        type_into(&mut h, "Confirm password", "\u{ad}");
        h.get_by_label("Apply protection").click();
        h.run_steps(3);
        h.get_by_label("Add at least one letter, number, or punctuation mark to your password.");
        assert_eq!(h.state().dialog, Some(Dialog::Protect));
        assert_eq!(h.state().protect_draft.password, "\u{ad}");
        assert_eq!(h.state().protect_draft.confirm, "\u{ad}");
        let doc = h.state().session.get(id).unwrap();
        assert_eq!(doc.bytes, before);
        assert!(!doc.dirty && !doc.info.encrypted);
        // Correct the existing input without losing it or reopening the dialog.
        type_into(&mut h, "Password", "x");
        type_into(&mut h, "Confirm password", "x");
        h.get_by_label("Apply protection").click();
        h.run_steps(3);
        assert_eq!(h.state().dialog, None);
        assert!(h.state().session.get(id).unwrap().info.encrypted);
    }
}

#[test]
fn editing_restrictions_via_advanced_options() {
    let mut h = open_dialog();
    h.get_by_role_and_label(Role::RadioButton, "Restrict editing").click();
    h.run_steps(2);
    h.get_by_label("Advanced settings").click();
    h.run_steps(8);
    type_into(&mut h, "Password", "boss");
    type_into(&mut h, "Confirm password", "boss");
    // The permissions sit below the fold of the scrolling form.
    h.get_by_label("Enable copying of text, images, and other content").scroll_to_me();
    h.run_steps(6);
    h.get_by_label("Enable copying of text, images, and other content").click();
    h.run_steps(2);
    assert!(h.state().protect_draft.protection.copy, "the checkbox took the click");
    h.get_by_label("Apply protection").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None, "the dialog applied");
    let id = h.state().views[0].id;
    let sec = h.state().session.get(id).unwrap().security_summary().unwrap();
    assert!(sec.permissions.copy() && sec.permissions.print() && !sec.permissions.modify() && !sec.permissions.annotate());
    // Remove security is disabled for nobody here: this session set it.
    assert!(h.state().session.get(id).unwrap().allows_security_change());
}

fn open_dialog() -> Harness<'static, PdfCraftApp> {
    let mut h = harness();
    assert!(h.state_mut().execute("protect.password"));
    h.run_steps(3);
    h
}

#[test]
fn a_fresh_dialog_is_collapsed_and_shows_no_errors() {
    let h = open_dialog();
    h.get_by_label("Protect PDF with a password");
    h.get_by_label("Protection takes effect when you save the PDF.");
    assert!(!h.state().protect_draft.advanced);
    assert!(h.query_by_label("Compatibility").is_none(), "Advanced settings starts collapsed");
    for text in ["Enter a password.", "Confirm your password.", "Passwords do not match."] {
        assert!(h.query_by_label(text).is_none(), "{text}");
    }
}

#[test]
fn applying_an_empty_form_names_each_missing_field_and_focuses_the_first() {
    let mut h = open_dialog();
    h.get_by_label("Apply protection").click();
    h.run_steps(3);
    h.get_by_label("Enter a password.");
    h.get_by_label("Confirm your password.");
    assert_eq!(h.state().dialog, Some(Dialog::Protect), "nothing was applied");
    assert!(h.get_by_role_and_label(Role::PasswordInput, "Password").is_focused());
}

#[test]
fn reveal_buttons_unmask_one_field_and_keep_the_text() {
    let mut h = open_dialog();
    type_into(&mut h, "Password", "dummy-secret");
    h.get_by_label("Show password").click();
    h.run_steps(2);
    h.get_by_label("Hide password");
    assert!(h.state().protect_draft.password_visible && !h.state().protect_draft.confirm_visible);
    assert_eq!(h.state().protect_draft.password, "dummy-secret");
    assert!(h.get_by_role_and_label(Role::TextInput, "Password").is_focused(), "the caret stays in the field");
    h.get_by_label("Show confirm password");
}

#[test]
fn switching_the_mode_and_the_disclosure_keeps_typed_values() {
    let mut h = open_dialog();
    type_into(&mut h, "Password", "dummy-secret");
    type_into(&mut h, "Confirm password", "dummy-secret");
    h.get_by_label("Advanced settings").click();
    h.run_steps(8);
    h.get_by_label("Compatibility");
    assert!(h.query_by_label("Printing allowed").is_none(), "permissions belong to Restrict editing");
    h.get_by_role_and_label(Role::RadioButton, "Restrict editing").click();
    h.run_steps(8);
    assert_eq!(h.state().protect_draft.mode, pdfcraft_ui_egui::protect::ProtectionMode::RestrictEditing);
    h.get_by_label("Printing allowed").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Printing allowed");
    h.get_by_label("Changes allowed");
    h.get_by_label("Advanced settings").click();
    h.run_steps(8);
    assert!(h.query_by_label("Compatibility").is_none());
    let d = &h.state().protect_draft;
    assert_eq!((d.password.as_str(), d.confirm.as_str()), ("dummy-secret", "dummy-secret"));
}

#[test]
fn a_click_beside_the_dialog_keeps_a_typed_password_but_not_an_empty_form() {
    let mut h = open_dialog();
    type_into(&mut h, "Password", "dummy-secret");
    h.get_by_label("Protect PDF with a password");
    h.hover_at(egui::pos2(5.0, 400.0));
    h.run_steps(1);
    h.drag_at(egui::pos2(5.0, 400.0));
    h.run_steps(1);
    h.drop_at(egui::pos2(5.0, 400.0));
    h.run_steps(3);
    assert_eq!(h.state().dialog, Some(Dialog::Protect), "typed text isn't discarded by a stray click");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    assert!(!h.state().protect_draft.has_input(), "Cancel clears the secrets");
}

#[test]
fn the_dialog_fits_narrow_and_short_windows() {
    for (theme, size) in ["light", "dark"].into_iter().flat_map(|theme| {
        [egui::vec2(1400.0, 900.0), egui::vec2(560.0, 700.0), egui::vec2(420.0, 600.0), egui::vec2(900.0, 420.0)]
            .into_iter()
            .map(move |size| (theme, size))
    }) {
        let mut h = Harness::builder().with_size(size).build_eframe(move |_cc| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.set_option("theme", theme).unwrap();
            app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
            app.protect_draft.advanced = true;
            app.protect_draft.mode = pdfcraft_ui_egui::protect::ProtectionMode::RestrictEditing;
            app
        });
        h.run_steps(3);
        h.state_mut().execute("protect.password");
        h.state_mut().protect_draft.advanced = true;
        h.run_steps(4);
        let card: egui::Rect = h.ctx.data(|d| d.get_temp(egui::Id::new("dialog").with("card"))).expect("dialog card");
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        assert!(screen.contains_rect(card), "{size:?}: {card:?} outside the window");
        assert!(card.width() <= 620.0 + 1.0, "{size:?}: {card:?}");
        for label in ["Apply protection", "Cancel", "Protection takes effect when you save the PDF."] {
            let r = h.get_by_label(label).rect();
            assert!(card.contains_rect(r), "{size:?}: {label} {r:?} outside {card:?}");
        }
    }
}

#[test]
fn collapsing_advanced_settings_shrinks_the_dialog_back() {
    let mut h = open_dialog();
    h.run_steps(12);
    let card = |h: &Harness<'static, PdfCraftApp>| -> egui::Rect { h.ctx.data(|d| d.get_temp(egui::Id::new("dialog").with("card"))).unwrap() };
    let collapsed = card(&h).height();
    h.get_by_label("Advanced settings").click();
    h.run_steps(12);
    assert!(card(&h).height() > collapsed + 100.0, "expanding makes room for the settings");
    h.get_by_label("Advanced settings").click();
    h.run_steps(12);
    assert!((card(&h).height() - collapsed).abs() < 1.0, "collapsed again: {} vs {collapsed}", card(&h).height());
    // The buttons sit directly under the notice, not centred in leftover height.
    let notice = h.get_by_label("Protection takes effect when you save the PDF.").rect();
    let apply = h.get_by_label("Apply protection").rect();
    assert!(apply.top() - notice.bottom() < 40.0, "gap {} between the notice and the buttons", apply.top() - notice.bottom());
}

fn protect_in_the_session(h: &mut Harness<'static, PdfCraftApp>) {
    type_into(h, "Password", "dummy-secret");
    type_into(h, "Confirm password", "dummy-secret");
    h.get_by_label("Apply protection").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
}

#[test]
fn an_unprotected_document_has_nothing_to_remove() {
    let h = open_dialog();
    assert!(h.query_by_label("Remove security").is_none());
    h.get_by_label("Apply protection");
}

#[test]
fn the_dialog_that_set_the_password_can_take_it_off() {
    let mut h = open_dialog();
    protect_in_the_session(&mut h);
    let id = h.state().views[0].id;
    assert!(h.state().session.get(id).unwrap().info.encrypted);
    // Reopen: the same footer now offers Remove security beside Cancel and Apply protection.
    assert!(h.state_mut().execute("protect.password"));
    h.run_steps(4);
    let remove = h.get_by_label("Remove security").rect();
    let cancel = h.get_by_label("Cancel").rect();
    let apply = h.get_by_label("Apply protection").rect();
    assert!(remove.right() < cancel.left() && cancel.right() < apply.left(), "{remove:?} {cancel:?} {apply:?}");
    assert!((remove.center().y - apply.center().y).abs() < 1.0, "one row: {remove:?} {apply:?}");
    h.get_by_label("Remove security").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None, "the dialog closes once security is taken off");
    assert!(!h.state().session.get(id).unwrap().info.encrypted);
    // Like Apply, it is written by the next save, and Undo takes it back.
    assert!(h.state().session.get(id).unwrap().dirty);
    h.state_mut().undo();
    assert!(h.state().session.get(id).unwrap().info.encrypted, "undo restores the protection");
}

#[test]
fn removing_security_leaves_typed_text_alone_until_it_closes_and_clears_it() {
    let mut h = open_dialog();
    protect_in_the_session(&mut h);
    assert!(h.state_mut().execute("protect.password"));
    h.run_steps(4);
    type_into(&mut h, "Password", "dummy-secret");
    h.get_by_label("Remove security").click();
    h.run_steps(3);
    assert_eq!(h.state().dialog, None);
    assert!(!h.state().protect_draft.has_input(), "the typed secret is gone with the dialog");
}

#[test]
fn remove_security_gets_its_own_row_in_a_narrow_window() {
    let mut h = Harness::builder().with_size(egui::vec2(420.0, 700.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("doc.pdf", None, FIXTURE.to_vec()).unwrap();
        app
    });
    h.run_steps(4);
    h.state_mut().execute("protect.password");
    h.run_steps(3);
    protect_in_the_session(&mut h);
    h.state_mut().execute("protect.password");
    h.run_steps(5);
    let card: egui::Rect = h.ctx.data(|d| d.get_temp(egui::Id::new("dialog").with("card"))).unwrap();
    let remove = h.get_by_label("Remove security").rect();
    let apply = h.get_by_label("Apply protection").rect();
    assert!(card.contains_rect(remove) && card.contains_rect(apply), "{remove:?} {apply:?} in {card:?}");
    assert!(remove.bottom() <= apply.top(), "stacked above the other two: {remove:?} {apply:?}");
}
