//! First-use address selection and saved language-specific preferences in the real shell.
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::{
    Dialog, PdfCraftApp,
    i18n::{Address, Lang, tr, tr_ctx},
};

fn harness(code: &str, saved: Option<String>) -> Harness<'static, PdfCraftApp> {
    let code = code.to_owned();
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(move |_| {
        let mut app = PdfCraftApp::new();
        if let Some(saved) = saved {
            app.restore(&saved);
        }
        app.set_option("language", &code).unwrap();
        app.request_address_choice();
        app
    });
    h.run_steps(4);
    h
}

#[test]
fn first_use_choices_persist_and_preferences_can_change_them() {
    for code in ["de", "fr", "es", "it"] {
        let lang = Lang::from_code(code).unwrap();
        let title = tr_ctx(lang, "address", "Choose a form of address");
        for (choice, label, other, other_label) in
            [(Address::Formal, "Formal", Address::Informal, "Informal"), (Address::Informal, "Informal", Address::Formal, "Formal")]
        {
            let mut h = harness(code, None);
            h.get_by_label(title);
            h.get_by_label(tr_ctx(lang, "address", label)).click();
            h.run_steps(3);
            assert!(h.query_by_label(title).is_none());
            assert_eq!(h.state().address.get(code), Some(&choice));
            let saved = h.state().persist();
            let mut restored = harness(code, Some(saved));
            assert!(restored.query_by_label(title).is_none());
            assert_eq!(restored.state().ui_language(), lang.with_address(choice));
            restored.state_mut().dialog = Some(Dialog::Preferences);
            restored.run_steps(3);
            restored.get_by_label(tr_ctx(lang, "address", other_label)).click();
            restored.run_steps(3);
            assert_eq!(restored.state().ui_language(), lang.with_address(other));
        }
    }
}

#[test]
fn choices_are_per_language_and_unsupported_languages_have_no_prompt() {
    let mut h = harness("en", None);
    assert!(h.state().address.is_empty());
    h.state_mut().set_option("language", "de").unwrap();
    h.run_steps(3);
    h.get_by_label("Informell (du)").click();
    h.run_steps(3);
    h.state_mut().set_option("language", "fr").unwrap();
    h.run_steps(3);
    h.get_by_label("Formel (vous)").click();
    h.run_steps(3);
    h.state_mut().set_option("language", "de").unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("Anrede wählen").is_none());
    assert_eq!(h.state().address.get("de"), Some(&Address::Informal));
    assert_eq!(h.state().address.get("fr"), Some(&Address::Formal));
    h.state_mut().set_option("language", "en").unwrap();
    assert!(h.state_mut().set_option("address", "formal").is_err());
}

#[test]
fn older_settings_keep_the_previous_register_and_bad_values_are_ignored() {
    let mut old = harness("de", Some(r#"{"language":"de"}"#.to_owned()));
    assert!(old.query_by_label("Anrede wählen").is_none());
    assert_eq!(old.state().address.get("de"), Some(&Address::Informal));
    assert_eq!(old.state().address.get("fr"), Some(&Address::Formal));
    old.state_mut().set_option("address", "formal").unwrap();
    assert!(old.state_mut().set_option("address", "invalid").is_err());
    assert_eq!(old.state().address.get("de"), Some(&Address::Formal));
    let invalid = harness("de", Some(r#"{"address":{"de":"invalid","xx":"formal"}}"#.to_owned()));
    invalid.get_by_label("Anrede wählen");
    assert!(invalid.state().address.is_empty());
}

#[test]
fn closing_without_a_choice_and_escape_both_save_formal_address() {
    for code in ["de", "fr", "es", "it"] {
        let lang = Lang::from_code(code).unwrap();
        let title = tr_ctx(lang, "address", "Choose a form of address");
        let h = harness(code, None);
        h.get_by_label(title);
        // eframe's save hook uses persist when the window closes.
        let restored = harness(code, Some(h.state().persist()));
        assert_eq!(restored.state().address.get(code), Some(&Address::Formal));
        assert!(restored.query_by_label(title).is_none());
        let mut escaped = harness(code, None);
        escaped.key_press(egui::Key::Escape);
        escaped.run_steps(3);
        assert_eq!(escaped.state().address.get(code), Some(&Address::Formal));
        assert!(escaped.query_by_label(title).is_none());
    }
}

#[test]
fn asking_again_does_not_change_document_state() {
    let mut h = harness("de", None);
    h.get_by_label("Formell (Sie)").click();
    h.run_steps(3);
    let original = h.state().persist();
    h.state_mut().set_option("address", "ask").unwrap();
    h.run_steps(3);
    h.get_by_label("Anrede wählen");
    h.get_by_label("Formell (Sie)").click();
    h.run_steps(3);
    assert_eq!(h.state().persist(), original);
    assert_eq!(tr(h.state().ui_language(), "Merged PRs"), "Gemergte PRs");
}

#[test]
fn address_and_window_preferences_round_trip_together() {
    let mut app = PdfCraftApp::new();
    app.set_option("language", "de").unwrap();
    app.set_option("address", "informal").unwrap();
    app.system_title_bar = true;
    let saved = app.persist();
    let settings: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(settings["address"]["de"], "informal");
    assert_eq!(settings["system_title_bar"], true);
    assert_eq!(settings["language"], "de");
    let mut restored = PdfCraftApp::new();
    restored.restore(&saved);
    assert!(restored.system_title_bar);
    assert_eq!(restored.address.get("de"), Some(&Address::Informal));
    assert_eq!(restored.ui_language(), Lang::from_code("de").unwrap().with_address(Address::Informal));
}
