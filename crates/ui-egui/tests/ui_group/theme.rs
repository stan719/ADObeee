//! Theme preferences stay in sync across both menus, OS changes and saved settings.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;
use pdfcraft_ui_egui::theme::{ThemeContext as _, ThemeKind, ThemePreference, Tokens};

fn harness() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app
    });
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(4);
    h
}

fn assert_theme(h: &Harness<'static, PdfCraftApp>, preference: ThemePreference, kind: ThemeKind) {
    assert_eq!(h.state().theme_preference, preference);
    assert_eq!(h.state().theme, kind);
    assert_eq!(Tokens::get(&h.ctx).kind, kind);
    assert_eq!(h.ctx.global_style().visuals.dark_mode, kind == ThemeKind::Dark);
    assert_eq!(h.ctx.options(|o| o.theme_preference), egui::ThemePreference::System);
    for branch in [egui::Theme::Light, egui::Theme::Dark] {
        let style = h.ctx.style_of(branch);
        assert_eq!(style.visuals.dark_mode, kind == ThemeKind::Dark);
        assert_eq!(style.visuals.panel_fill, Tokens::for_kind(kind).panel);
    }
}

/// A click on the title bar's theme button cycles Light, Dark, System.
fn click_toolbar(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    h.get_by_label(&format!("Display theme: {label}")).click();
    h.run_steps(3);
}

fn open_view_menu(h: &mut Harness<'static, PdfCraftApp>) {
    h.get_by_label("Menu").click();
    h.run_steps(2);
    h.get_by_label("View ⏵").hover();
    h.run_steps(3);
    assert!(h.query_by_label("Switch light / dark theme").is_none());
    h.get_by_label("Display theme ⏵").hover();
    h.run_steps(3);
    for choice in ["Use system setting", "Light gray", "Dark gray", "Atelier", "Editorial", "Heritage", "Executive", "Sienna", "Paper & Ink"] {
        h.get_by_label(choice);
    }
}

#[test]
fn toolbar_and_view_menu_share_three_state_preferences() {
    let mut h = harness();
    // Explicit light wins even on a dark desktop, including egui's popup style.
    assert_theme(&h, ThemePreference::Light, ThemeKind::Light);
    click_toolbar(&mut h, "Light gray");
    assert_theme(&h, ThemePreference::Dark, ThemeKind::Dark);
    click_toolbar(&mut h, "Dark gray");
    assert_theme(&h, ThemePreference::System, ThemeKind::Dark);

    open_view_menu(&mut h);
    h.get_by_label("Light gray").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::Light, ThemeKind::Light);
    click_toolbar(&mut h, "Light gray");
    assert_theme(&h, ThemePreference::Dark, ThemeKind::Dark);

    open_view_menu(&mut h);
    h.get_by_label("Use system setting").click();
    h.run_steps(3);
    assert_theme(&h, ThemePreference::System, ThemeKind::Dark);
    h.input_mut().system_theme = Some(egui::Theme::Light);
    h.run_steps(3);
    assert_theme(&h, ThemePreference::System, ThemeKind::Light);
    click_toolbar(&mut h, "Use system setting");
    click_toolbar(&mut h, "Light gray");
    // A manual choice exits system mode and survives subsequent OS changes.
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(2);
    h.input_mut().system_theme = Some(egui::Theme::Light);
    h.run_steps(2);
    assert_theme(&h, ThemePreference::Dark, ThemeKind::Dark);
}

#[test]
fn saved_preferences_restore_with_legacy_light_dark_settings() {
    for (value, preference) in [("light", ThemePreference::Light), ("dark", ThemePreference::Dark), ("system", ThemePreference::System)] {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.set_option("theme", value).unwrap();
        let saved = app.persist();
        let mut restored = PdfCraftApp::new();
        restored.set_option("language", "en").unwrap();
        restored.restore(&saved);
        assert_eq!(restored.theme_preference, preference);
    }
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.restore(r#"{"theme":"Dark"}"#);
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    assert_eq!(app.theme, ThemeKind::Dark);
    app.restore(r#"{"theme":"Light"}"#);
    assert_eq!(app.theme_preference, ThemePreference::Light);
    assert_eq!(app.theme, ThemeKind::Light);
    app.restore(r#"{"theme":"System"}"#);
    app.restore(r#"{"theme":"Purple"}"#);
    assert_eq!(app.theme_preference, ThemePreference::System);
}

#[test]
fn commands_and_options_apply_before_the_first_frame() {
    let mut app = PdfCraftApp::new();
    app.set_option("language", "en").unwrap();
    app.run_command("view.theme.dark");
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    assert_eq!(app.theme, ThemeKind::Dark);
    app.run_command("view.theme.system");
    assert_eq!(app.theme_preference, ThemePreference::System);
    assert_eq!(app.theme, ThemeKind::Light);
    // The existing palette/control toggle also goes through the preference setter.
    app.run_command("view.theme");
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    assert_eq!(app.theme, ThemeKind::Dark);
    app.run_command("view.theme.light");
    app.set_option("theme", "dark").unwrap();
    assert!(app.set_option("theme", "purple").unwrap_err().contains("light, dark, or system"));
    assert_eq!(app.theme_preference, ThemePreference::Dark);
    assert_eq!(app.theme, ThemeKind::Dark);
}

#[test]
fn all_presets_switch_live_persist_and_preserve_unsaved_documents() {
    let mut h = harness();
    // Repeated saves stamp ModDate. Freeze time so byte equality proves document preservation.
    h.state_mut().session = pdfcraft_engine::Session::default().with_clock(|| 1_700_000_000);
    assert!(h.state_mut().execute("create.blank"));
    h.run_steps(3);
    let id = h.state().views[0].id;
    h.state_mut().apply_edit(pdfcraft_engine::Edit::RotatePages { pages: vec![0], degrees: 90 });
    h.run_steps(4); // Let fit-to-page respond to rotation before recording view state.
    let bytes = h.state().session.save_bytes(id).unwrap();
    let view_count = h.state().views.len();
    let zoom = h.state().views[0].zoom;
    for preference in ThemePreference::ALL {
        h.state_mut().set_option("theme", &preference.id()).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().theme_preference, preference);
        let expected = preference.palette(&h.state().theme_library, h.ctx.system_theme().map(pdfcraft_ui_egui::theme::system_kind), h.state().theme);
        assert_eq!(Tokens::get(&h.ctx), expected);
        assert_eq!(h.ctx.global_style().visuals.panel_fill, expected.panel);
        for branch in [egui::Theme::Light, egui::Theme::Dark] {
            assert_eq!(h.ctx.style_of(branch).visuals.panel_fill, expected.panel);
        }
        assert_eq!(h.state().views.len(), view_count);
        assert_eq!(h.state().views[0].id, id);
        assert_eq!(h.state().views[0].zoom, zoom);
        assert!(h.state().session.get(id).unwrap().dirty);
        assert_eq!(h.state().session.save_bytes(id).unwrap(), bytes);
        let mut restored = PdfCraftApp::new();
        restored.restore(&h.state().persist());
        assert_eq!(restored.theme_preference, preference);
    }
}

#[test]
fn new_presets_are_available_from_the_existing_menu_and_commands() {
    let mut h = harness();
    for preference in ThemePreference::ALL.into_iter().skip(3) {
        open_view_menu(&mut h);
        h.get_by_label(preference.label()).click();
        h.run_steps(3);
        assert_eq!(h.state().theme_preference, preference);
        h.state_mut().run_command("view.theme.light");
        h.state_mut().run_command(&format!("view.theme.{}", preference.id()));
        h.run_steps(2);
        assert_eq!(h.state().theme_preference, preference);
        h.input_mut().system_theme = Some(egui::Theme::Light);
        h.run_steps(2);
        h.input_mut().system_theme = Some(egui::Theme::Dark);
        h.run_steps(2);
        assert_eq!(h.state().theme_preference, preference);
    }
}

/// Set PDFCRAFT_THEME_SHOTS to inspect the actual app without committing images.
#[test]
fn palette_visual_review() {
    let Ok(dir) = std::env::var("PDFCRAFT_THEME_SHOTS") else { return };
    std::fs::create_dir_all(&dir).unwrap();
    for preference in ThemePreference::ALL.into_iter().skip(1) {
        let mut h = harness();
        h.state_mut().set_theme_preference(preference);
        h.run_steps(4);
        h.render().unwrap().save(format!("{dir}/{}-home.png", preference.id())).unwrap();
        assert!(h.state_mut().execute("create.blank"));
        h.run_steps(8);
        let shot = h.render().unwrap();
        assert!(shot.pixels().filter(|p| p.0 == [255, 255, 255, 255]).count() > 10_000, "PDF paper remains white");
        shot.save(format!("{dir}/{}-document.png", preference.id())).unwrap();
        h.get_by_label(&format!("Display theme: {}", preference.label())).click_secondary();
        h.run_steps(3);
        h.render().unwrap().save(format!("{dir}/{}-menu.png", preference.id())).unwrap();
        h.get_by_label(preference.label()).click();
        h.run_steps(3);
        h.state_mut().run_command("app.preferences");
        h.run_steps(4);
        h.render().unwrap().save(format!("{dir}/{}-dialog.png", preference.id())).unwrap();
    }
    let mut h = harness();
    let created = h
        .state_mut()
        .manage_theme(
            "theme_create",
            &serde_json::json!({
                "name":"Navy and cream", "base_theme_id":"ocean", "palette":{"text":"#fff4df","accent":"#efbd88"}
            }),
        )
        .unwrap();
    h.state_mut().set_option("theme", created["id"].as_str().unwrap()).unwrap();
    h.run_steps(4);
    h.render().unwrap().save(format!("{dir}/custom-home.png")).unwrap();
    h.get_by_label("Display theme: Custom theme").click_secondary();
    h.run_steps(3);
    h.render().unwrap().save(format!("{dir}/custom-menu.png")).unwrap();
    h.get_by_label("Navy and cream").click();
    h.run_steps(3);
    assert_eq!(h.state().theme_preference.id(), created["id"].as_str().unwrap());
}

#[test]
fn custom_palette_restores_and_unknown_library_data_is_not_lost() {
    use pdfcraft_ui_egui::theme::{CustomThemeId, ThemeDefinition};
    let mut h = harness();
    let id = CustomThemeId::from_bytes([7; 16]);
    let mut custom = ThemeDefinition::duplicate_builtin(ThemePreference::Forest, id, "My forest".into()).unwrap();
    custom.palette.0.insert("panel".into(), serde_json::json!("#112233"));
    h.state_mut().theme_library.insert(custom).unwrap();
    h.state_mut().set_option("theme", &format!("custom:{id}")).unwrap();
    h.run_steps(3);
    assert_eq!(Tokens::get(&h.ctx).panel, egui::Color32::from_rgb(17, 34, 51));
    let saved = h.state().persist();
    let mut restored = PdfCraftApp::new();
    restored.restore(&saved);
    assert_eq!(restored.theme_preference, ThemePreference::Custom(id));
    assert_eq!(restored.theme_library, h.state().theme_library);
    // A legacy file with no library does not replace a valid library with null.
    restored.restore(r#"{"theme":"Light"}"#);
    assert_eq!(restored.theme_library, h.state().theme_library);
    restored.restore(r#"{"theme":"unknown-future","theme_library":{"schema_version":99,"themes":[{"future":42}],"extension":true}}"#);
    assert_eq!(restored.theme_preference, ThemePreference::Light);
    let saved: serde_json::Value = serde_json::from_str(&restored.persist()).unwrap();
    assert_eq!(saved["theme_library"]["schema_version"], 99);
    assert_eq!(saved["theme_library"]["themes"][0]["future"], 42);
    assert_eq!(saved["theme_library"]["extension"], true);
}

#[test]
fn legacy_copies_are_identical_until_a_color_is_edited() {
    use pdfcraft_ui_egui::theme::{self, CustomThemeId, ThemeDefinition};
    for preference in [ThemePreference::Light, ThemePreference::Dark] {
        let palette = preference.palette(&Default::default(), None, ThemeKind::Light);
        let mut copy = ThemeDefinition::duplicate_builtin(preference, CustomThemeId::from_bytes([3; 16]), "Copy".into()).unwrap();
        assert_eq!(copy.resolve(), palette);
        let original_ctx = egui::Context::default();
        let copy_ctx = egui::Context::default();
        theme::apply_palette(&original_ctx, palette);
        theme::apply_palette(&copy_ctx, copy.resolve());
        assert_eq!(original_ctx.global_style().visuals, copy_ctx.global_style().visuals);
        copy.palette.0.insert("error".into(), serde_json::json!("#ab1234"));
        let edited = copy.resolve();
        assert_eq!(edited.compatible(egui::Color32::RED, theme::ColorToken::Error), egui::Color32::from_rgb(171, 18, 52));
        theme::apply_palette(&copy_ctx, edited);
        assert_eq!(copy_ctx.global_style().visuals.error_fg_color, edited.error);
    }
}

#[test]
fn native_store_cli_edits_and_gui_activation_share_latest_library() {
    use pdfcraft_ui_egui::theme::store::ThemeStore;
    let dir = std::env::temp_dir().join(format!("pdfcraft-gui-themes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("themes.json");
    let mut app = PdfCraftApp::new();
    app.restore(r#"{"theme":"System"}"#);
    app.enable_theme_store(&path).unwrap();
    let store = ThemeStore::new(&path);
    let id = "custom:00000000-0000-0000-0000-000000000001";
    store
        .execute("theme_create", &serde_json::json!({"id":"00000000-0000-0000-0000-000000000001", "name":"CLI Executive", "base_theme_id":"ocean"}))
        .unwrap();
    store.execute("theme_activate", &serde_json::json!({"id":id})).unwrap();
    // A GUI action re-reads the current store instead of saving its startup snapshot.
    app.set_option("theme", "dark").unwrap();
    assert_eq!(app.theme_library.definitions().count(), 1);
    assert_eq!(store.load().unwrap().theme, ThemePreference::Dark);
    app.set_option("theme", id).unwrap();
    assert_eq!(app.theme_preference.id(), id);
    // App settings remain a compatibility mirror, never authoritative over themes.json.
    let mirror = app.persist();
    store.execute("theme_delete", &serde_json::json!({"id":id})).unwrap();
    let mut restarted = PdfCraftApp::new();
    restarted.restore(&mirror);
    restarted.enable_theme_store(&path).unwrap();
    assert_eq!(restarted.theme_preference, ThemePreference::Ocean);
    assert_eq!(restarted.theme_library.definitions().count(), 0);
    std::fs::write(&path, "{malformed").unwrap();
    assert!(restarted.set_option("theme", "light").is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{malformed");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn restored_system_tracks_native_input_without_rewriting_saved_preference() {
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(|_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.restore(r#"{"theme":"System"}"#);
        app
    });
    for (input, expected) in [
        (Some(egui::Theme::Dark), ThemeKind::Dark),
        (Some(egui::Theme::Light), ThemeKind::Light),
        (None, ThemeKind::Light),
        (Some(egui::Theme::Dark), ThemeKind::Dark),
    ] {
        h.input_mut().system_theme = input;
        h.run_steps(3);
        assert_theme(&h, ThemePreference::System, expected);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&h.state().persist()).unwrap()["theme"], "System");
    }
    for (manual, expected) in [(ThemePreference::Light, ThemeKind::Light), (ThemePreference::Dark, ThemeKind::Dark)] {
        h.state_mut().set_theme_preference(manual);
        for input in [Some(egui::Theme::Dark), Some(egui::Theme::Light), None] {
            h.input_mut().system_theme = input;
            h.run_steps(3);
            assert_theme(&h, manual, expected);
        }
    }
}

#[test]
fn unknown_system_uses_light_after_a_manual_dark_choice() {
    let mut app = PdfCraftApp::new();
    app.set_theme_preference(ThemePreference::Dark);
    app.set_theme_preference(ThemePreference::System);
    assert_eq!(app.theme, ThemeKind::Light);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&app.persist()).unwrap()["theme"], "System");
}

#[test]
fn native_appearance_remains_inherited_for_every_saved_choice() {
    #[derive(Clone)]
    struct NativeCommands(std::sync::Arc<std::sync::Mutex<Vec<egui::SystemTheme>>>);
    impl egui::Plugin for NativeCommands {
        fn debug_name(&self) -> &'static str {
            "theme-command-proof"
        }
        fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
            for command in output.viewport_output.values().flat_map(|v| &v.commands) {
                if let egui::ViewportCommand::SetTheme(theme) = command {
                    self.0.lock().unwrap().push(*theme);
                }
            }
        }
    }
    for preference in ThemePreference::ALL {
        let commands = NativeCommands(Default::default());
        let observed = commands.clone();
        let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_eframe(move |cc| {
            cc.egui_ctx.add_plugin(commands);
            let mut app = PdfCraftApp::new();
            app.set_option("language", "en").unwrap();
            app.set_theme_preference(preference);
            app
        });
        h.input_mut().system_theme = Some(egui::Theme::Light);
        h.run_steps(3);
        h.input_mut().system_theme = Some(egui::Theme::Dark);
        h.run_steps(3);
        // Observe the actual app's startup and later viewport output. This proves
        // native window intent, not operating-system ThemeChanged delivery.
        let commands = observed.0.lock().unwrap();
        assert!(!commands.is_empty());
        assert!(commands.iter().all(|theme| *theme == egui::SystemTheme::SystemDefault), "{commands:?}");
        assert_eq!(h.ctx.options(|o| o.theme_preference), egui::ThemePreference::System);
    }
}
