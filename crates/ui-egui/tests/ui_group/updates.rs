//! User-initiated release checks, with deterministic stand-in sources and actual package names.

use std::sync::Arc;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;
use pdfcraft_ui_egui::updates::{Architecture, Asset, InstallContext, InstallKind, Platform, Release, UpdateSource, is_newer};

fn release(version: &str) -> Release {
    let bare = version.trim_start_matches('v');
    let name = format!("pdfcraft-{bare}-windows-x64.msi");
    Release {
        version: version.into(),
        url: format!("https://github.com/storytold/pdfcraft/releases/tag/{version}"),
        notes: "Improved PDF rendering.\n<html>This remains plain text.</html>".into(),
        draft: false,
        prerelease: false,
        assets: vec![Asset {
            url: format!("https://github.com/storytold/pdfcraft/releases/download/{version}/{name}"),
            name,
            size: 70_696_960,
            digest: None,
        }],
    }
}

fn source(answer: Result<&str, &str>) -> UpdateSource {
    let answer = answer.map(release).map_err(str::to_string);
    Arc::new(move || answer.clone())
}

fn context(kind: InstallKind) -> InstallContext {
    InstallContext { platform: Platform::Windows, architecture: Architecture::X64, kind }
}

fn harness(answer: Result<&str, &str>) -> Harness<'static, PdfCraftApp> {
    Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.update_context = context(InstallKind::WindowsMsi);
        app.update_source = Some(source(answer));
        app
    })
}

fn settle(h: &mut Harness<'static, PdfCraftApp>) {
    for _ in 0..200 {
        h.run_steps(2);
        if h.query_by_label_contains("Checking for a newer version").is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Modal placement and ScrollArea geometry may need additional layout passes.
    h.run_steps(6);
    h.run();
}

#[test]
fn versions_compare_by_number() {
    for (new, old) in [("v0.2.0", "0.1.1"), ("v0.1.10", "0.1.9"), ("1.2.3-rc.10", "1.2.3-rc.2"), ("1.2.3", "1.2.3-rc.10")] {
        assert!(is_newer(new, old), "{new} > {old}");
    }
    for (new, old) in [
        ("v0.1.1", "0.1.1"),
        ("v0.1.0", "0.1.1"),
        ("v0.1.1-beta.2", "0.1.1"),
        ("1.2.3+new", "1.2.3+old"),
        ("1.2.3-rc.2", "1.2.3-rc.10"),
        ("nightly", "0.1.1"),
        ("1", "0.9.9"),
        ("v1.2.3.4", "0.1.1"),
        ("v99999999999999999999.0.0", "0.1.1"),
        ("1.0.0", "bad"),
    ] {
        assert!(!is_newer(new, old), "{new} must not be offered over {old}");
    }
}

#[test]
fn a_newer_release_is_offered_for_download() {
    let mut h = harness(Ok("v99.0.0"));
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label_contains("PdfCraft 99.0.0 is available");
    h.get_by_label_contains("Installed version:");
    h.get_by_label_contains("Available version: 99.0.0");
    h.get_by_label_contains("Package: pdfcraft-99.0.0-windows-x64.msi");
    h.get_by_label_contains("Download size: 67.4 MiB");
    h.get_by_label_contains("<html>This remains plain text.</html>");
    h.get_by_label("Download package…");
    h.get_by_label("Remind Me Later").click();
    h.run_steps(3);
    assert!(h.query_by_label_contains("is available").is_none());
}

#[test]
fn an_up_to_date_or_failed_check_says_so() {
    for version in [concat!("v", env!("CARGO_PKG_VERSION")), "v0.0.1"] {
        let mut h = harness(Ok(version));
        h.state_mut().execute("help.check_updates");
        settle(&mut h);
        h.get_by_label_contains("is up to date");
        assert!(h.query_by_label("Download package…").is_none());
    }
    for error in ["couldn't reach GitHub", "release tag is not a valid SemVer version"] {
        let mut h = harness(Err(error));
        h.state_mut().execute("help.check_updates");
        settle(&mut h);
        h.get_by_label_contains(&format!("Couldn't check for updates: {error}"));
        h.get_by_label("Retry");
    }
}

#[test]
fn unknown_missing_and_ambiguous_installations_have_safe_fallbacks() {
    let mut h = harness(Ok("v99.0.0"));
    h.state_mut().update_context.kind = InstallKind::Unknown;
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label_contains("Installation type is unknown");
    h.get_by_label("View release page…");
    assert!(h.query_by_label("Download package…").is_none());

    for ambiguous in [false, true] {
        let mut r = release("v99.0.0");
        if ambiguous {
            r.assets.push(r.assets[0].clone());
        } else {
            r.assets.clear();
        }
        let mut h = harness(Ok("v99.0.0"));
        h.state_mut().update_source = Some(Arc::new(move || Ok(r.clone())));
        h.state_mut().execute("help.check_updates");
        settle(&mut h);
        h.get_by_label_contains(if ambiguous { "ambiguous packages" } else { "No compatible package" });
        h.get_by_label("View release page…");
        assert!(h.query_by_label("Download package…").is_none());
    }
}

#[test]
fn managed_installations_do_not_attempt_github() {
    for kind in [InstallKind::Deb, InstallKind::Rpm, InstallKind::Flatpak] {
        let mut h = harness(Err("must not be called"));
        h.state_mut().update_context = InstallContext { platform: Platform::Linux, architecture: Architecture::X64, kind };
        h.state_mut().update_source = Some(Arc::new(|| panic!("managed installation attempted network")));
        h.state_mut().execute("help.check_updates");
        settle(&mut h);
        h.get_by_label_contains("software manager");
        assert!(h.query_by_label("Download package…").is_none());
        assert!(h.query_by_label_contains("Couldn't check").is_none());
    }
}

#[test]
fn stable_channel_does_not_offer_drafts_or_prereleases() {
    for draft in [false, true] {
        let mut r = release("v99.0.0");
        r.draft = draft;
        r.prerelease = !draft;
        let mut h = harness(Ok("v99.0.0"));
        h.state_mut().update_source = Some(Arc::new(move || Ok(r.clone())));
        h.state_mut().execute("help.check_updates");
        settle(&mut h);
        h.get_by_label_contains("not a published stable update");
        assert!(h.query_by_label("Download package…").is_none());
    }
}

#[test]
fn long_notes_keep_actions_visible_in_light_and_dark_themes() {
    for theme in ["light", "dark"] {
        let mut r = release("v99.0.0");
        r.notes = "Release improvement with a long explanatory line.\n".repeat(1000);
        let mut h = harness(Ok("v99.0.0"));
        h.state_mut().set_option("theme", theme).unwrap();
        h.state_mut().update_source = Some(Arc::new(move || Ok(r.clone())));
        h.state_mut().execute("help.check_updates");
        settle(&mut h);
        let button = h.get_by_label("Download package…");
        assert!(button.rect().max.y < 800.0, "{theme}: actions fit in viewport");
        h.get_by_label("Remind Me Later").click();
        h.run_steps(3);
        assert!(h.query_by_label("Download package…").is_none());
    }
}

#[test]
fn nothing_is_asked_until_the_user_checks() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(move |_cc| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.restore(r#"{"check_updates_at_start": true}"#);
        app.update_source = Some(Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(release("v99.0.0"))
        }));
        app
    });
    settle(&mut h);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(h.query_by_label_contains("is available").is_none());
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    h.get_by_label_contains("PdfCraft 99.0.0 is available");
}

#[test]
fn keyboard_focus_and_escape_keep_the_existing_modal_behavior() {
    let mut h = harness(Ok("v99.0.0"));
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label("Remind Me Later").focus();
    h.run_steps(2);
    h.key_press(egui::Key::Space);
    h.run_steps(3);
    assert!(h.query_by_label("Download package…").is_none());
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_label("Download package…").is_none());
}

#[test]
fn retry_checks_again_and_package_links_do_not_claim_installation() {
    let mut h = harness(Err("offline"));
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.state_mut().update_source = Some(source(Ok("v99.0.0")));
    h.get_by_label("Retry").click();
    settle(&mut h);
    h.get_by_label("Download package…");
    h.get_by_label_contains("Installation is manual");
    assert!(h.query_by_label("Download and Install").is_none());
}

/// Optional visual QA writes outside the repository; no baselines or assets are committed.
#[test]
fn update_dialog_visual_preview() {
    let Ok(directory) = std::env::var("PDFCRAFT_UPDATE_PREVIEW_DIR") else { return };
    std::fs::create_dir_all(&directory).unwrap();
    for theme in ["light", "dark"] {
        let mut h = harness(Ok("v99.0.0"));
        h.state_mut().set_option("theme", theme).unwrap();
        h.state_mut().execute("help.check_updates");
        settle(&mut h);
        h.render().unwrap().save(std::path::Path::new(&directory).join(format!("updates-{theme}.png"))).unwrap();
    }
}

#[test]
fn release_details_use_the_selected_language() {
    use pdfcraft_ui_egui::i18n::{Lang, tr};
    let language = Lang::from_code("de").unwrap();
    let mut h = harness(Ok("v99.0.0"));
    h.state_mut().set_option("language", "de").unwrap();
    h.state_mut().execute("help.check_updates");
    settle(&mut h);
    h.get_by_label(tr(language, "Download package…"));
    h.get_by_label(tr(language, "Remind Me Later"));
    h.get_by_label(tr(language, "Release notes"));
    assert!(h.query_by_label("Download package…").is_none());
    h.get_by_label(tr(language, "Remind Me Later")).click();
    h.run_steps(3);
    assert!(h.query_by_label(tr(language, "Download package…")).is_none());
}
