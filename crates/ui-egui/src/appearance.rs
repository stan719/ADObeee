//! Preferences › Appearance: the appearance mode (follow the system, light or dark) above a light
//! and a dark theme card, each with a small picture of PdfCraft in that theme.

use egui::{Rect, RichText, pos2, vec2};

use crate::PdfCraftApp;
use crate::theme::ThemeContext as _;
use crate::theme::{self, ThemeKind, ThemePreference, Tokens};
use crate::widgets;

fn mode_label(preference: ThemePreference) -> &'static str {
    match preference {
        ThemePreference::System => "Use system setting",
        ThemePreference::Light => "Light",
        ThemePreference::Dark => "Dark",
        other => other.label(),
    }
}

fn mode_name(app: &PdfCraftApp, preference: ThemePreference) -> String {
    match preference {
        ThemePreference::Custom(id) => app.theme_library.name(id).map(theme::display_name).unwrap_or_else(|| tl!("Custom theme").into()),
        _ => tl!(mode_label(preference)).into(),
    }
}

/// A miniature PdfCraft window in the palette's colours: title bar and tabs, the tool bar, the page
/// thumbnails, a page on the pasteboard and the right-hand panel.
fn preview(ui: &mut egui::Ui, p: &Tokens, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 118.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, p.chrome);
    let title = Rect::from_min_size(rect.min, vec2(rect.width(), 12.0));
    painter.rect_filled(title, 0.0, p.titlebar);
    // The active tab and the theme button.
    painter.rect_filled(Rect::from_min_size(title.min + vec2(6.0, 3.0), vec2(36.0, 9.0)), 2.0, p.panel);
    painter.rect_filled(Rect::from_min_size(title.min + vec2(9.0, 6.0), vec2(20.0, 2.0)), 1.0, p.text);
    painter.circle_filled(pos2(title.right() - 7.0, title.center().y), 2.5, p.icon);
    // The tool bar, with its first tool active.
    let tools = Rect::from_min_size(pos2(rect.left(), title.bottom()), vec2(rect.width(), 12.0));
    painter.rect_filled(tools, 0.0, p.panel);
    for i in 0..8 {
        let c = if i == 0 { p.accent } else { p.icon };
        painter.rect_filled(Rect::from_min_size(tools.min + vec2(8.0 + i as f32 * 9.0, 3.5), vec2(5.0, 5.0)), 1.0, c);
    }
    let body = Rect::from_min_max(pos2(rect.left(), tools.bottom() + 1.0), rect.max);
    // Page thumbnails on the left, the second one selected.
    let rail = Rect::from_min_size(body.min, vec2((body.width() * 0.16).round(), body.height()));
    painter.rect_filled(rail, 0.0, p.panel);
    for i in 0..3 {
        let thumb = Rect::from_min_size(rail.min + vec2(rail.width() * 0.25, 6.0 + i as f32 * 26.0), vec2(rail.width() * 0.5, 20.0));
        if i == 1 {
            painter.rect_filled(thumb.expand(2.0), 2.0, p.accent_soft);
        }
        painter.rect_filled(thumb, 1.0, egui::Color32::WHITE);
        painter.rect_stroke(thumb, 1.0, egui::Stroke::new(0.5, p.border), egui::StrokeKind::Outside);
    }
    // The right-hand panel: a heading and a few rows.
    let side = Rect::from_min_max(pos2(body.right() - (body.width() * 0.22).round(), body.top()), body.max);
    painter.rect_filled(side, 0.0, p.panel);
    painter.rect_filled(Rect::from_min_size(side.min + vec2(5.0, 5.0), vec2(side.width() * 0.5, 2.5)), 1.0, p.text);
    for i in 0..4 {
        let y = side.top() + 14.0 + i as f32 * 8.0;
        painter.rect_filled(Rect::from_min_size(pos2(side.left() + 5.0, y), vec2(side.width() * 0.7, 2.0)), 1.0, p.text_muted);
    }
    // The page, white in both themes, with a heading and lines of text.
    let paste = Rect::from_min_max(pos2(rail.right(), body.top()), pos2(side.left(), body.bottom()));
    painter.rect_filled(paste, 0.0, p.pasteboard);
    let page_h = paste.height() - 10.0;
    let page = Rect::from_center_size(paste.center(), vec2(page_h * 0.77, page_h));
    painter.rect_filled(page.translate(vec2(1.0, 1.0)), 1.0, p.page_shadow);
    painter.rect_filled(page, 1.0, egui::Color32::WHITE);
    let ink = egui::Color32::from_gray(0x60);
    painter.rect_filled(Rect::from_min_size(page.min + vec2(6.0, 7.0), vec2(page.width() * 0.5, 3.0)), 1.0, egui::Color32::from_gray(0x30));
    for i in 0..8 {
        let w = if i % 4 == 3 { 0.45 } else { 0.8 };
        painter.rect_filled(Rect::from_min_size(page.min + vec2(6.0, 15.0 + i as f32 * 6.0), vec2(page.width() * w - 6.0, 1.5)), 0.5, ink);
    }
}

/// One theme card: its title, Active when it is what shows now, a preview of the chosen theme
/// and the chosen theme name. Selection uses the shared appearance picker above.
fn card(ui: &mut egui::Ui, t: &Tokens, title: &str, active: bool, shown: &Tokens, label: &str) {
    egui::Frame::new()
        .fill(t.card)
        .stroke(egui::Stroke::new(1.0, if active { t.accent } else { t.border }))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).font(theme::semibold(12.5)).color(t.text));
                if active {
                    ui.label(RichText::new(tl!("Active")).color(t.accent_text).small());
                }
            });
            ui.add_space(4.0);
            preview(ui, shown, ui.available_width());
            ui.add_space(4.0);
            let _ = ui.add(widgets::radio(true, label));
        });
}

/// The Appearance section of Preferences.
pub fn preferences_section(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) {
    ui.label(RichText::new(tl!("Appearance")).font(theme::semibold(13.0)));
    ui.horizontal(|ui| {
        ui.label(tl!("Appearance mode"));
        egui::ComboBox::from_id_salt("appearance-mode").selected_text(mode_name(app, app.theme_preference)).show_ui(ui, |ui| {
            let custom = app.theme_choices();
            let length = custom.len().saturating_add(ThemePreference::ALL.len());
            egui::ScrollArea::vertical().max_height(300.0).show_rows(ui, ui.spacing().interact_size.y, length, |ui, rows| {
                for row in rows {
                    let preference = if let Some(preference) = ThemePreference::ALL.get(row) {
                        *preference
                    } else if let Some(choice) = custom.get(row.saturating_sub(ThemePreference::ALL.len())) {
                        ThemePreference::Custom(choice.id)
                    } else {
                        continue;
                    };
                    ui.push_id(preference.id(), |ui| {
                        if ui.selectable_label(app.theme_preference == preference, mode_name(app, preference)).clicked() {
                            app.set_theme_preference(preference);
                            ui.close();
                        }
                    });
                }
            });
        });
    });
    ui.add_space(4.0);
    let light_active = app.theme == ThemeKind::Light;
    let active_palette = Tokens::get(ui.ctx());
    let active_label = if app.theme_preference == ThemePreference::System {
        tl!(if light_active { "Light gray" } else { "Dark gray" }).to_owned()
    } else if matches!(app.theme_preference, ThemePreference::Custom(_)) {
        mode_name(app, app.theme_preference)
    } else {
        tl!(app.theme_preference.label()).to_owned()
    };
    ui.columns(2, |cols| {
        let [light, dark] = cols else { return };
        let light_palette = if light_active { active_palette } else { Tokens::for_kind(ThemeKind::Light) };
        let dark_palette = if light_active { Tokens::for_kind(ThemeKind::Dark) } else { active_palette };
        card(light, t, tl!("Light theme"), light_active, &light_palette, if light_active { &active_label } else { tl!("Light gray") });
        card(dark, t, tl!("Dark theme"), !light_active, &dark_palette, if light_active { tl!("Dark gray") } else { &active_label });
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// The mode is chosen in Preferences and both theme cards show, the one on screen marked
    /// Active.
    #[test]
    fn preferences_choose_the_appearance_mode_and_show_both_themes() {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        let mut fonts = false;
        let mut h = Harness::builder().with_size(egui::vec2(640.0, 400.0)).build_ui_state(
            move |ui, app: &mut PdfCraftApp| {
                // The app's fonts (the semibold headings) take effect from the next frame.
                if !fonts {
                    ui.ctx().set_fonts(theme::font_definitions());
                    fonts = true;
                    return;
                }
                let t = Tokens::get(ui.ctx());
                preferences_section(ui, app, &t);
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Light theme").is_some() && h.query_by_label("Dark theme").is_some());
        assert!(h.query_by_label("Light gray").is_some() && h.query_by_label("Dark gray").is_some());
        assert_eq!(h.query_all_by_label("Active").count(), 1);
        if let Ok(path) = std::env::var("PDFCRAFT_APPEARANCE_SHOT") {
            h.render().unwrap().save(path).unwrap();
        }

        let combo = egui::accesskit::Role::ComboBox;
        h.get_by_role(combo).click();
        h.run();
        h.get_by_label("Dark").click();
        h.run();
        assert_eq!(h.state().theme_preference, ThemePreference::Dark);
        h.get_by_role(combo).click();
        h.run();
        h.get_by_label("Use system setting").click();
        h.run();
        assert_eq!(h.state().theme_preference, ThemePreference::System);

        for (id, label) in [
            ("catppuccin", "Atelier"),
            ("solarized", "Editorial"),
            ("forest", "Heritage"),
            ("ocean", "Executive"),
            ("sunset", "Sienna"),
            ("paper-ink", "Paper & Ink"),
        ] {
            h.get_by_role(combo).click();
            h.run();
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().theme_preference.id(), id);
            assert_eq!(h.query_all_by_label("Active").count(), 1);
        }
        let custom = h
            .state_mut()
            .manage_theme(
                "theme_create",
                &serde_json::json!({
                    "id":"00000000-0000-4000-8000-000000000001", "name":"Custom navy", "base_theme_id":"ocean",
                    "palette":{"text":"#fff4df"}
                }),
            )
            .unwrap();
        h.get_by_role(combo).click();
        h.run();
        h.get_by_label("Custom navy").click();
        h.run();
        assert_eq!(h.state().theme_preference.id().as_ref(), custom["id"].as_str().unwrap());
        assert_eq!(h.state().theme, ThemeKind::Dark);
        assert_eq!(h.state_mut().manage_theme("theme_inspect", &serde_json::json!({"id":custom["id"]})).unwrap()["palette"]["text"], "#fff4df");
        assert_eq!(h.query_all_by_label("Active").count(), 1);
    }
}
