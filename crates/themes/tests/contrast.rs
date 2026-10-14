use ecolor::Color32;
use pdfcraft_themes::{ContrastDiagnostic, ThemeKind, ThemeLibrary, ThemePreference, contrast_diagnostics, contrast_ratio, primary_button_fills};

fn palette(choice: ThemePreference) -> pdfcraft_themes::Palette {
    choice.palette(&ThemeLibrary::default(), None, ThemeKind::Light)
}

fn has_role(diagnostics: &[ContrastDiagnostic], role: &str) -> bool {
    diagnostics.iter().any(|d| d.role == role)
}

#[test]
fn actual_primary_button_fills_meet_normal_text_contrast_for_every_preset_and_interpolation() {
    for choice in ThemePreference::ALL {
        let p = palette(choice);
        let fills = primary_button_fills(&p);
        let resting_ratio = contrast_ratio(fills.foreground, fills.rest);
        assert!(resting_ratio >= 4.5, "{} rest: {resting_ratio:.4}:1", choice.id());
        for hover in [0.0, 0.2, 0.5, 0.8, 1.0] {
            for down in [0.0, 0.2, 0.5, 0.8, 1.0] {
                let actual_fill = fills.fill(hover, down);
                let ratio = contrast_ratio(fills.foreground, actual_fill);
                assert!(ratio >= 4.5, "{} hover={hover} down={down}: {ratio:.4}:1", choice.id());
            }
        }
        if choice != ThemePreference::Dark {
            assert_eq!(fills.rest, p.accent, "only the known low-contrast legacy Dark fill is derived");
        }
    }
}

#[test]
fn editorial_pressed_button_is_adjusted_only_as_derived_interaction_treatment() {
    let p = palette(ThemePreference::Solarized);
    let fills = primary_button_fills(&p);
    let original = p.accent.lerp_to_gamma(Color32::BLACK, 0.22);
    assert!(contrast_ratio(p.on_accent, original) < 4.5);
    assert!(contrast_ratio(fills.foreground, fills.pressed) >= 4.5);
    assert_eq!(p.accent, palette(ThemePreference::Solarized).accent);
}

#[test]
fn legacy_dark_button_gets_a_derived_resting_fill_without_mutating_its_palette() {
    let dark = palette(ThemePreference::Dark);
    let fills = primary_button_fills(&dark);
    assert_ne!(fills.rest, dark.accent);
    let resting_ratio = contrast_ratio(fills.foreground, fills.rest);
    assert!(resting_ratio >= 4.5, "legacy Dark button contrast: {resting_ratio:.4}:1");
    assert_eq!(dark.accent, palette(ThemePreference::Dark).accent, "stored palette values stay unchanged");
    let copy = dark;
    assert_eq!(primary_button_fills(&copy), fills, "an exact built-in copy renders identically");
}

#[test]
fn custom_low_contrast_accent_is_not_adjusted() {
    let mut custom = palette(ThemePreference::Ocean);
    custom.accent = custom.on_accent;
    let fills = primary_button_fills(&custom);
    assert_eq!(fills.rest, custom.accent);
    assert_eq!(fills.hover, custom.accent_text);
    assert_eq!(fills.pressed, custom.accent.lerp_to_gamma(Color32::BLACK, 0.22));
    assert!(contrast_diagnostics(&custom).iter().any(|d| d.role == "primary_button.normal"));
}

#[test]
fn ocean_card_example_reports_machine_readable_text_card_ratio() {
    let mut p = palette(ThemePreference::Ocean);
    p.card = p.text;
    let findings = contrast_diagnostics(&p);
    let finding = findings.iter().find(|d| d.role == "text.card");
    assert_eq!(finding.map(|d| d.foreground_token.as_str()), Some("text"));
    assert_eq!(finding.and_then(|d| d.background_token.as_deref()), Some("card"));
    assert_eq!(finding.and_then(|d| d.ratio), Some(1.0));
    assert_eq!(finding.map(|d| d.threshold), Some(4.5));
    assert_eq!(finding.map(|d| d.kind.as_str()), Some("contrast"));
}

#[test]
fn diagnostics_cover_fields_controls_selection_badges_feedback_and_primary_states() {
    let mut p = palette(ThemePreference::Ocean);
    p.text_faint = p.field;
    p.text_muted = p.panel;
    p.text = p.hover;
    p.pressed = p.text;
    p.selection_text = p.selected;
    p.on_accent = p.badge_new;
    p.success = p.panel;
    p.warning = p.panel;
    p.error = p.panel;
    p.info = p.panel;
    let findings = contrast_diagnostics(&p);
    for role in [
        "field.placeholder",
        "disabled.text",
        "secondary_button.hover",
        "secondary_button.pressed",
        "selected_item.text",
        "badge.text",
        "feedback.success",
        "feedback.warning",
        "feedback.error",
        "feedback.info",
    ] {
        assert!(has_role(&findings, role), "missing diagnostic for {role}");
    }
    for finding in findings.iter().filter(|d| d.kind == "contrast") {
        assert!(finding.ratio.is_some());
        assert!(!finding.foreground_token.is_empty());
        assert!(finding.background_token.is_some());
        assert_eq!(finding.threshold, 4.5);
        assert!(finding.message.contains("below 4.5:1"));
    }
}

#[test]
fn translucent_token_warning_remains_advisory_and_compositing_aware() {
    let mut p = palette(ThemePreference::Ocean);
    p.accent_soft = Color32::from_rgba_unmultiplied(10, 20, 30, 128);
    let findings = contrast_diagnostics(&p);
    let finding = findings.iter().find(|d| d.role == "token.accent_soft");
    assert_eq!(finding.map(|d| d.kind.as_str()), Some("translucent"));
    assert_eq!(finding.and_then(|d| d.ratio), None);
    assert!(finding.is_some_and(|d| d.message.contains("depends on underlying surfaces")));
}
