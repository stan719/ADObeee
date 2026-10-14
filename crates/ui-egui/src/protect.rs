//! Protect PDF with a password: what the password is for, the password and its confirmation, and
//! the Advanced settings behind a disclosure (compatibility, what to encrypt and, when the password
//! restricts editing, what stays allowed). Execution plan M8.1.
//!
//! Nothing here talks to the engine except [`ProtectDraft::edit`]; the dialog host applies it.

use egui::{Align, Color32, CornerRadius, Layout, Rect, RichText, Sense, Stroke, StrokeKind, vec2};
use pdfcraft_engine::{Algorithm, Changes, Edit, Printing, Protection};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons, widgets};

/// Dialog width, outer padding and the margin kept to the window edge.
const DIALOG_WIDTH: f32 = 620.0;
const GUTTER: f32 = 24.0;
const INPUT_H: f32 = 44.0;
const BUTTON_H: f32 = 40.0;
const EYE_W: f32 = 40.0;
const CARD_GAP: f32 = 12.0;
const SECTION_GAP: f32 = 24.0;
const FIELD_GAP: f32 = 16.0;
/// Space between a field's border and its text.
const FIELD_PAD: f32 = 14.0;
const LABEL_GAP: f32 = 8.0;
/// Room the title and the footer take; the form scrolls in what is left.
const CHROME_H: f32 = 190.0;

const PASSWORD_ID: &str = "protect-password";
const CONFIRM_ID: &str = "protect-confirm";

/// What the password is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProtectionMode {
    /// The password is needed to open the document.
    #[default]
    OpenDocument,
    /// The document opens freely; the password is needed to change what is allowed.
    RestrictEditing,
}

/// The dialog's state while it is open. The passwords live only here until Apply.
#[derive(Clone, Default, PartialEq)]
pub struct ProtectDraft {
    pub mode: ProtectionMode,
    pub password: String,
    pub confirm: String,
    pub password_visible: bool,
    pub confirm_visible: bool,
    pub advanced: bool,
    /// The confirmation field has been left once, so a mismatch may be reported.
    pub confirm_touched: bool,
    /// Apply was pressed, so every missing field is reported.
    pub submit_attempted: bool,
    pub protection: Protection,
}

/// Never print a password, even in a failed assertion.
impl std::fmt::Debug for ProtectDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectDraft")
            .field("mode", &self.mode)
            .field("password", &"<hidden>")
            .field("confirm", &"<hidden>")
            .field("advanced", &self.advanced)
            .finish_non_exhaustive()
    }
}

/// What is wrong with each field. The wording stays English here (tested); only its display translates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Problems {
    pub password: Option<&'static str>,
    pub confirm: Option<&'static str>,
}

impl ProtectDraft {
    /// Everything that stops Apply, whether or not the dialog shows it yet.
    pub fn problems(&self) -> Problems {
        let password = if self.password.is_empty() {
            Some("Enter a password.")
        } else if self.protection.algorithm != Algorithm::Aes256 && !self.password.chars().all(|c| (' '..='~').contains(&c)) {
            Some("This compatibility level supports only plain ASCII passwords.")
        } else if pdfcraft_cos::effective_password(self.protection.algorithm, &self.password)
            == pdfcraft_cos::effective_password(self.protection.algorithm, "")
        {
            Some("Add at least one letter, number, or punctuation mark to your password.")
        } else {
            None
        };
        let confirm = if self.confirm.is_empty() {
            Some("Confirm your password.")
        } else if self.confirm != self.password {
            Some("Passwords do not match.")
        } else {
            None
        };
        Problems { password, confirm }
    }

    /// The problems to show now: a missing field only after Apply, a mismatch once the
    /// confirmation has been filled in and left.
    pub fn shown(&self) -> Problems {
        let all = self.problems();
        Problems {
            password: all.password.filter(|_| self.submit_attempted || !self.password.is_empty()),
            confirm: all.confirm.filter(|_| self.submit_attempted || (self.confirm_touched && !self.confirm.is_empty())),
        }
    }

    /// Something has been typed that a stray click beside the dialog must not throw away.
    pub fn has_input(&self) -> bool {
        !self.password.is_empty() || !self.confirm.is_empty()
    }

    /// The edit Apply makes.
    pub fn edit(&self) -> Edit {
        let mut p = self.protection.clone();
        match self.mode {
            ProtectionMode::OpenDocument => {
                p.open_password = Some(self.password.clone());
                p.permissions_password = None;
            }
            ProtectionMode::RestrictEditing => {
                p.open_password = None;
                p.permissions_password = Some(self.password.clone());
            }
        }
        Edit::Protect(p)
    }
}

/// What the dialog asks its host to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Apply,
    Cancel,
    /// Take the document's password protection off.
    Remove,
}

/// The width the dialog's content gets: 620 including its padding, narrower in a small window.
pub(crate) fn content_width(ctx: &egui::Context) -> f32 {
    let pad = 2.0 * f32::from(widgets::DIALOG_PAD_X);
    (DIALOG_WIDTH - pad).min(ctx.content_rect().width() - 2.0 * GUTTER - pad).max(240.0)
}

/// The border of an outlined control: at least 3:1 against the surface it sits on, in either theme.
fn control_border(t: &Tokens) -> Color32 {
    t.border.lerp_to_gamma(t.text_muted, 0.7)
}

fn error_color(t: &Tokens) -> Color32 {
    if t.dark() { Color32::from_rgb(0xFF, 0x8A, 0x8A) } else { Color32::from_rgb(0xB4, 0x23, 0x23) }
}

fn section_label(ui: &mut egui::Ui, t: &Tokens, text: &str) -> egui::Response {
    ui.label(RichText::new(tl!(text)).font(theme::semibold(14.0)).color(t.text))
}

fn helper(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add(egui::Label::new(RichText::new(tl!(text)).font(theme::regular(13.0)).color(t.text_muted)).wrap());
}

/// The two purpose cards, side by side or stacked when two readable cards don't fit.
fn purpose_cards(ui: &mut egui::Ui, t: &Tokens, mode: &mut ProtectionMode) {
    const CARDS: [(ProtectionMode, &str, &str); 2] = [
        (ProtectionMode::OpenDocument, "Open the document", "Password required to view the PDF."),
        (ProtectionMode::RestrictEditing, "Restrict editing", "Password required to change editing permissions."),
    ];
    const PAD: f32 = 12.0;
    const RADIO: f32 = 20.0;
    let width = ui.available_width();
    let stacked = (width - CARD_GAP) / 2.0 < 240.0;
    let card_w = if stacked { width } else { (width - CARD_GAP) / 2.0 };
    let text_w = card_w - PAD - RADIO - PAD - PAD;
    let layout = |ui: &egui::Ui, text: &str, font: egui::FontId, color: Color32, wrap: f32| {
        ui.fonts_mut(|f| f.layout_job(egui::text::LayoutJob::simple(text.to_owned(), font, color, wrap)))
    };
    let galleys: Vec<_> = CARDS
        .iter()
        .map(|(_, title, description)| {
            (layout(ui, tl!(title), theme::semibold(14.0), t.text, text_w), layout(ui, tl!(description), theme::regular(13.0), t.text_muted, text_w))
        })
        .collect();
    let natural = |i: usize| (PAD + galleys[i].0.size().y + 6.0 + galleys[i].1.size().y + PAD).max(96.0);
    let shared = natural(0).max(natural(1));
    let mut draw = |ui: &mut egui::Ui, i: usize| {
        let (value, title, description) = CARDS[i];
        let height = if stacked { natural(i) } else { shared };
        let (rect, resp) = ui.allocate_exact_size(vec2(card_w, height), Sense::click());
        let on = *mode == value;
        let name = tl!(title).to_string();
        let selected = on;
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, name.clone()));
        let help = tl!(description).to_string();
        ui.ctx().accesskit_node_builder(resp.id, |node| node.set_description(help));
        let press = theme::Press::track(ui, &resp);
        let (fill, stroke) = if on {
            (t.accent_soft, Stroke::new(1.5, t.accent))
        } else {
            (press.fill(t.card, t.hover, t.pressed), Stroke::new(1.0, control_border(t)))
        };
        ui.painter().rect(rect, CornerRadius::same(8), fill, stroke, StrokeKind::Inside);
        let centre = rect.left_top() + vec2(PAD + RADIO / 2.0, PAD + RADIO / 2.0);
        if on {
            ui.painter().circle_filled(centre, RADIO / 2.0, t.accent);
            ui.painter().circle_filled(centre, 3.5, Color32::WHITE);
        } else {
            ui.painter().circle_stroke(centre, RADIO / 2.0 - 0.75, Stroke::new(1.5, control_border(t)));
        }
        let x = rect.left() + PAD + RADIO + PAD;
        let title_h = galleys[i].0.size().y;
        ui.painter().galley(egui::pos2(x, rect.top() + PAD), galleys[i].0.clone(), t.text);
        ui.painter().galley(egui::pos2(x, rect.top() + PAD + title_h + 6.0), galleys[i].1.clone(), t.text_muted);
        if resp.has_focus() {
            ui.painter().rect_stroke(rect.expand(2.0), CornerRadius::same(10), Stroke::new(2.0, t.accent_text), StrokeKind::Outside);
        }
        if resp.clicked() {
            *mode = value;
        }
    };
    if stacked {
        for i in 0..CARDS.len() {
            if i > 0 {
                ui.add_space(CARD_GAP);
            }
            draw(ui, i);
        }
    } else {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for i in 0..CARDS.len() {
                draw(ui, i);
            }
        });
    }
}

/// What names a password field: its caption, the id that keeps its focus, and what its reveal
/// button is called while the text is hidden and while it shows.
struct Secret {
    label: &'static str,
    id: &'static str,
    show: &'static str,
    hide: &'static str,
}

/// A labelled password field with its reveal button inside the field, and its error below.
fn secret_field(ui: &mut egui::Ui, t: &Tokens, field: &Secret, text: &mut String, visible: &mut bool, error: Option<&str>) -> egui::Response {
    let (label, id, reveal_names) = (field.label, field.id, (field.show, field.hide));
    let caption = section_label(ui, t, label);
    ui.add_space(LABEL_GAP);
    let background = ui.painter().add(egui::Shape::Noop);
    let width = ui.available_width();
    let mut edit = None;
    let row = ui.allocate_ui_with_layout(vec2(width, INPUT_H), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(width, INPUT_H));
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(FIELD_PAD);
        let field_w = width - EYE_W - FIELD_PAD;
        let response = ui.add(
            egui::TextEdit::singleline(text)
                .id(egui::Id::new(id))
                .password(!*visible)
                .frame(egui::Frame::NONE)
                .desired_width(field_w)
                .min_size(vec2(field_w, INPUT_H))
                .vertical_align(Align::Center),
        );
        let (rect, eye) = ui.allocate_exact_size(vec2(EYE_W, INPUT_H), Sense::click());
        let name = if *visible { reveal_names.1 } else { reveal_names.0 };
        let name = tl!(name).to_string();
        eye.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone()));
        let press = theme::Press::track(ui, &eye);
        press.wash(ui, rect.shrink2(vec2(4.0, 6.0)), 6, false);
        icons::paint(ui, rect, if *visible { "eye-off" } else { "eye" }, 18.0, if eye.hovered() { t.text } else { t.icon });
        if eye.has_focus() {
            ui.painter().rect_stroke(rect.shrink(3.0), CornerRadius::same(6), Stroke::new(2.0, t.accent_text), StrokeKind::Inside);
        }
        if eye.clicked() {
            *visible = !*visible;
            // The caret stays in the field it was in.
            ui.memory_mut(|m| m.request_focus(egui::Id::new(id)));
        }
        theme::hand(eye).on_hover_text(name);
        edit = Some(response);
    });
    let response = edit.unwrap_or_else(|| row.response.clone());
    response.clone().labelled_by(caption.id);
    let stroke = if response.has_focus() {
        Stroke::new(2.0, t.accent_text)
    } else if error.is_some() {
        Stroke::new(1.0, error_color(t))
    } else {
        Stroke::new(1.0, control_border(t))
    };
    ui.painter().set(
        background,
        egui::Shape::Rect(egui::epaint::RectShape::new(row.response.rect, CornerRadius::same(8), t.field, stroke, StrokeKind::Inside)),
    );
    if let Some(error) = error {
        let message = tl!(error).to_string();
        ui.ctx().accesskit_node_builder(response.id, |node| node.set_description(message.clone()));
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(message).font(theme::regular(13.0)).color(error_color(t))).wrap());
    }
    response
}

/// The "Advanced settings" row: text, a chevron and a rule running to the edge.
fn disclosure(ui: &mut egui::Ui, t: &Tokens, expanded: &mut bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    let label = tl!("Advanced settings").to_string();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
    let open = *expanded;
    ui.ctx().accesskit_node_builder(resp.id, |node| node.set_expanded(open));
    let galley = ui.painter().layout_no_wrap(label, theme::semibold(14.0), t.text);
    let text_pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    let chevron = Rect::from_center_size(egui::pos2(rect.left() + galley.size().x + 16.0, rect.center().y), vec2(18.0, 18.0));
    ui.painter().galley(text_pos, galley, t.text);
    icons::paint(ui, chevron, if open { "chevron-up" } else { "chevron-down" }, 16.0, if resp.hovered() { t.text } else { t.icon });
    let from = chevron.right() + 12.0;
    if from < rect.right() {
        ui.painter().hline(from..=rect.right(), rect.center().y, Stroke::new(1.0, t.divider));
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), CornerRadius::same(6), Stroke::new(2.0, t.accent_text), StrokeKind::Outside);
    }
    if theme::hand(resp).clicked() {
        *expanded = !*expanded;
    }
}

/// A 20-point radio with an outline that shows in both themes, its label to the right.
fn radio_row(ui: &mut egui::Ui, t: &Tokens, on: bool, label: &str) -> egui::Response {
    const RADIO: f32 = 20.0;
    let enabled = ui.is_enabled();
    let text = tl!(label).to_string();
    let color = if enabled { t.text } else { t.text_faint };
    let galley = ui.fonts_mut(|f| {
        f.layout_job(egui::text::LayoutJob::simple(text.clone(), theme::regular(14.0), color, (ui.available_width() - RADIO - 12.0).max(40.0)))
    });
    let height = galley.size().y.max(RADIO + 4.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, on, text.clone()));
    let centre = egui::pos2(rect.left() + RADIO / 2.0, rect.top() + height / 2.0);
    let ring = if enabled { control_border(t) } else { t.divider };
    if on {
        ui.painter().circle_filled(centre, RADIO / 2.0, if enabled { t.accent } else { ring });
        ui.painter().circle_filled(centre, 3.5, Color32::WHITE);
    } else {
        ui.painter().circle_stroke(centre, RADIO / 2.0 - 0.75, Stroke::new(1.5, ring));
    }
    if resp.has_focus() {
        ui.painter().circle_stroke(centre, RADIO / 2.0 + 2.0, Stroke::new(2.0, t.accent_text));
    }
    ui.painter().galley(egui::pos2(rect.left() + RADIO + 12.0, rect.top() + (height - galley.size().y) / 2.0), galley, color);
    theme::hand(resp)
}

/// A dialog dropdown as tall as an input and as wide as the content.
fn select<T: PartialEq + Copy>(ui: &mut egui::Ui, t: &Tokens, id: &str, value: &mut T, choices: &[(T, &str)]) {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = INPUT_H;
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.bg_stroke = Stroke::new(1.0, control_border(t));
        w.inactive.weak_bg_fill = t.field;
        for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            state.corner_radius = CornerRadius::same(8);
        }
        let shown = choices.iter().find(|(v, _)| v == value).map_or("", |(_, label)| *label);
        egui::ComboBox::from_id_salt(id).width(ui.available_width()).selected_text(tl!(shown)).show_ui(ui, |ui| {
            for &(v, label) in choices {
                ui.selectable_value(value, v, tl!(label));
            }
        });
    });
}

/// The Advanced settings: compatibility, what to encrypt and, for a restricting password, the permissions.
fn advanced(ui: &mut egui::Ui, t: &Tokens, d: &mut ProtectDraft) {
    let restricting = d.mode == ProtectionMode::RestrictEditing;
    let p = &mut d.protection;
    ui.add_space(FIELD_GAP);
    section_label(ui, t, "Compatibility");
    ui.add_space(LABEL_GAP);
    select(
        ui,
        t,
        "protect-compatibility",
        &mut p.algorithm,
        &[
            (Algorithm::Aes256, "Acrobat X and later (256-bit AES)"),
            (Algorithm::Aes128, "Acrobat 7.0 and later (128-bit AES)"),
            (Algorithm::Rc4_128, "Acrobat 5.0 and later (128-bit RC4)"),
            (Algorithm::Rc4_40, "Acrobat 3.0 and later (40-bit RC4)"),
        ],
    );
    // Unencrypted metadata needs crypt filters (Acrobat 6.0 and later).
    let legacy = matches!(p.algorithm, Algorithm::Rc4_40 | Algorithm::Rc4_128);
    if legacy {
        p.encrypt_metadata = true;
    }
    ui.add_space(FIELD_GAP);
    section_label(ui, t, "Content to encrypt");
    ui.add_space(LABEL_GAP);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 12.0;
        if radio_row(ui, t, p.encrypt_metadata, "All document contents").clicked() {
            p.encrypt_metadata = true;
        }
        ui.add_enabled_ui(!legacy, |ui| {
            if radio_row(ui, t, !p.encrypt_metadata, "All contents except metadata").clicked() {
                p.encrypt_metadata = false;
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(32.0);
            helper(ui, t, "Metadata includes details such as title and author.");
        });
    });
    if restricting {
        ui.add_space(FIELD_GAP);
        section_label(ui, t, "Printing allowed");
        ui.add_space(LABEL_GAP);
        select(
            ui,
            t,
            "protect-printing",
            &mut p.printing,
            &[(Printing::None, "None"), (Printing::Low, "Low Resolution (150 dpi)"), (Printing::High, "High Resolution")],
        );
        ui.add_space(FIELD_GAP);
        section_label(ui, t, "Changes allowed");
        ui.add_space(LABEL_GAP);
        select(
            ui,
            t,
            "protect-changes",
            &mut p.changes,
            &[
                (Changes::None, "None"),
                (Changes::Pages, "Inserting, deleting, and rotating pages"),
                (Changes::FillSign, "Filling in form fields and signing existing signature fields"),
                (Changes::CommentFillSign, "Commenting, filling in form fields, and signing existing signature fields"),
                (Changes::AnyExceptExtract, "Any except extracting pages"),
            ],
        );
        ui.add_space(FIELD_GAP);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            ui.add(widgets::checkbox(&mut p.copy, tl!("Enable copying of text, images, and other content")));
            ui.add(widgets::checkbox(&mut p.accessibility, tl!("Enable text access for screen reader devices for the visually impaired")));
        });
    }
}

/// Ask for the first field that is wrong, after Apply found something missing.
fn focus_first_problem(ctx: &egui::Context, problems: Problems) {
    let id = if problems.password.is_some() { PASSWORD_ID } else { CONFIRM_ID };
    ctx.memory_mut(|m| m.request_focus(egui::Id::new(id)));
}

/// The form: purpose, passwords, Advanced settings. Returns whether Enter was pressed in a password field.
fn form(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> bool {
    let d = &mut app.protect_draft;
    ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
    section_label(ui, t, "Require a password to");
    ui.add_space(LABEL_GAP);
    purpose_cards(ui, t, &mut d.mode);
    ui.add_space(SECTION_GAP);
    let shown = d.shown();
    let first = Secret { label: "Password", id: PASSWORD_ID, show: "Show password", hide: "Hide password" };
    let password = secret_field(ui, t, &first, &mut d.password, &mut d.password_visible, shown.password);
    ui.add_space(FIELD_GAP);
    let second = Secret { label: "Confirm password", id: CONFIRM_ID, show: "Show confirm password", hide: "Hide confirm password" };
    let confirm = secret_field(ui, t, &second, &mut d.confirm, &mut d.confirm_visible, shown.confirm);
    if confirm.lost_focus() {
        d.confirm_touched = true;
    }
    ui.add_space(SECTION_GAP);
    egui::Frame::new()
        .fill(if d.advanced { if t.dark() { t.pressed } else { t.hover } } else { Color32::TRANSPARENT })
        .stroke(if d.advanced { Stroke::new(1.0, t.border) } else { Stroke::NONE })
        .corner_radius(CornerRadius::same(8))
        .inner_margin(if d.advanced { 16 } else { 0 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            disclosure(ui, t, &mut d.advanced);
            if d.advanced {
                advanced(ui, t, d);
            }
        });
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    enter && (password.lost_focus() || confirm.lost_focus())
}

/// Draw the dialog body.
pub(crate) fn body(ui: &mut egui::Ui, app: &mut PdfCraftApp, t: &Tokens) -> Action {
    let screen = ui.ctx().content_rect();
    let mut action = Action::None;
    // Only a document that has password security (or is about to get it) has any to remove.
    let removable =
        app.active_ids().and_then(|(_, id)| app.session.get(id)).is_some_and(|d| d.allows_security_change() && d.security_summary().is_some());
    egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 8)).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        ui.label(RichText::new(tl!("Protect PDF with a password")).font(theme::semibold(22.0)).color(t.text));
        ui.add_space(SECTION_GAP);
        // Title and footer stay in reach; the form scrolls in a short window.
        let room = (screen.height() - 2.0 * GUTTER - CHROME_H).max(120.0);
        let mut enter = false;
        // As tall as the form, up to `room`. The modal remembers the largest size it has had, so
        // an auto-shrinking scroll area would leave the dialog tall after Advanced settings is
        // collapsed. The area is therefore sized from the height the form measured last frame
        // (the first frame has none and sizes to its content), and another frame is asked for
        // whenever that height changes.
        let height_id = ui.id().with("protect-form-height");
        let last: Option<f32> = ui.ctx().data(|d| d.get_temp(height_id));
        let mut area = egui::ScrollArea::vertical().id_salt("protect-form").max_height(room).auto_shrink([false, true]);
        if let Some(last) = last {
            area = area.min_scrolled_height(last.min(room)).max_height(last.min(room));
        }
        let out = area.show(ui, |ui| {
            enter = form(ui, app, t);
            // Keep the last control clear of the scroll bar and the rule below.
            ui.add_space(4.0);
        });
        let content = out.content_size.y;
        ui.ctx().data_mut(|d| d.insert_temp(height_id, content));
        if last.is_none_or(|l| (l - content).abs() > 0.5) {
            ui.ctx().request_repaint();
        }
        ui.add_space(FIELD_GAP);
        let (rule, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(rule, 0.0, t.divider);
        ui.add_space(FIELD_GAP);
        helper(ui, t, "Protection takes effect when you save the PDF.");
        ui.add_space(12.0);
        // The footer: Remove security on the left when there is protection to take off, Cancel and
        // Apply protection on the right. A narrow window gives Remove security a row of its own.
        let (remove, cancel, apply) = (tl!("Remove security"), tl!("Cancel"), tl!("Apply protection"));
        let font = theme::medium(14.0);
        let button_w = |ui: &egui::Ui, label: &str| widgets::text_width(ui, label, &font) + BUTTON_H - 2.0;
        let fits = removable && {
            let all = button_w(ui, remove) + button_w(ui, cancel) + button_w(ui, apply) + 2.0 * 12.0;
            all <= ui.available_width()
        };
        if removable && !fits {
            if widgets::pill_button_sized(ui, remove, false, BUTTON_H).clicked() {
                action = Action::Remove;
            }
            ui.add_space(12.0);
        }
        // Exactly one button row tall: a layout filling the height left over from an expanded
        // dialog would keep the card from shrinking back.
        ui.allocate_ui_with_layout(vec2(ui.available_width(), BUTTON_H), Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if fits && widgets::pill_button_sized(ui, remove, false, BUTTON_H).clicked() {
                action = Action::Remove;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::pill_button_sized(ui, apply, true, BUTTON_H).clicked() || enter {
                    let d = &mut app.protect_draft;
                    d.submit_attempted = true;
                    let problems = d.problems();
                    if problems == Problems::default() {
                        action = Action::Apply;
                    } else {
                        focus_first_problem(ui.ctx(), problems);
                    }
                }
                if widgets::pill_button_sized(ui, cancel, false, BUTTON_H).clicked() {
                    action = Action::Cancel;
                }
            });
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(password: &str, confirm: &str) -> ProtectDraft {
        ProtectDraft { password: password.into(), confirm: confirm.into(), ..ProtectDraft::default() }
    }

    #[test]
    fn problems_name_the_field_and_the_cause() {
        assert_eq!(draft("", "").problems(), Problems { password: Some("Enter a password."), confirm: Some("Confirm your password.") });
        assert_eq!(draft("abc", "").problems(), Problems { password: None, confirm: Some("Confirm your password.") });
        assert_eq!(draft("abc", "abd").problems().confirm, Some("Passwords do not match."));
        assert_eq!(draft("abc", "abc").problems(), Problems::default());
        let mut older = draft("Pässword", "Pässword");
        older.protection.algorithm = Algorithm::Aes128;
        assert_eq!(older.problems().password, Some("This compatibility level supports only plain ASCII passwords."));
        older.protection.algorithm = Algorithm::Aes256;
        assert_eq!(older.problems(), Problems::default());
    }

    #[test]
    fn a_password_the_encryption_ignores_is_refused_before_the_engine_is_asked() {
        // A soft hyphen is dropped by AES-256's password preparation, so the key would be the empty password's.
        let d = draft("\u{ad}", "\u{ad}");
        assert_eq!(d.problems().password, Some("Add at least one letter, number, or punctuation mark to your password."));
        assert_eq!(draft("\u{ad}x", "\u{ad}x").problems(), Problems::default());
    }

    #[test]
    fn mistakes_show_when_they_become_the_users_to_fix() {
        let mut d = draft("", "");
        assert_eq!(d.shown(), Problems::default(), "a fresh dialog shows no errors");
        d.password = "abc".into();
        assert_eq!(d.shown(), Problems::default(), "an untouched empty confirmation says nothing");
        d.confirm = "ab".into();
        assert_eq!(d.shown().confirm, None, "typing in the confirmation isn't an error yet");
        d.confirm_touched = true;
        assert_eq!(d.shown().confirm, Some("Passwords do not match."), "after leaving it, a mismatch shows");
        let mut empty = draft("", "");
        empty.submit_attempted = true;
        assert_eq!(empty.shown(), empty.problems(), "Apply reports every missing field");
    }

    #[test]
    fn the_mode_picks_which_password_is_set() {
        let mut d = draft("abc", "abc");
        match d.edit() {
            Edit::Protect(p) => assert_eq!((p.open_password.as_deref(), p.permissions_password), (Some("abc"), None)),
            other => panic!("{other:?}"),
        }
        d.mode = ProtectionMode::RestrictEditing;
        match d.edit() {
            Edit::Protect(p) => assert_eq!((p.open_password, p.permissions_password.as_deref()), (None, Some("abc"))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_draft_never_prints_its_passwords() {
        let shown = format!("{:?}", draft("Open-Sesame-42", "Open-Sesame-42"));
        assert!(!shown.contains("Sesame"), "{shown}");
    }
}
