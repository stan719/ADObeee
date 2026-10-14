//! Contrast diagnostics and derived interaction colours for application widgets.
//!
//! Saved palette values are never changed here. Button fills are derived at paint time;
//! diagnostics remain advisory, so importing or activating a low-contrast custom theme is safe.
//! Text uses WCAG AA's 4.5:1 threshold; the focus outline uses 3:1 because it is a
//! non-text UI indicator (WCAG 1.4.11).

use ecolor::Color32;
use serde::Serialize;

use crate::{ColorToken, Palette, ThemeKind, ThemeLibrary, ThemePreference};

const NORMAL_TEXT: f32 = 4.5;
const GRAPHIC: f32 = 3.0;

/// Machine-readable accessibility finding for a foreground/background pair or a
/// translucent token whose final contrast depends on compositing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContrastDiagnostic {
    /// Stable UI location such as `field.placeholder` or `primary_button.pressed`.
    pub role: String,
    pub foreground_token: String,
    pub background_token: Option<String>,
    /// `None` when translucency makes the rendered surface unknowable here.
    pub ratio: Option<f32>,
    pub threshold: f32,
    pub kind: String,
    pub message: String,
}

/// The actual colors consumed by the primary-button painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimaryButtonFills {
    pub rest: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub foreground: Color32,
}

impl PrimaryButtonFills {
    /// Match the GUI press animation: rest→hover, then hover→pressed.
    pub fn fill(self, hover: f32, down: f32) -> Color32 {
        self.rest.lerp_to_gamma(self.hover, hover.clamp(0.0, 1.0)).lerp_to_gamma(self.pressed, down.clamp(0.0, 1.0))
    }
}

/// Return primary-button state fills while keeping the palette itself untouched.
/// Each adjustment is constrained to the line between the accent and its original
/// state color, and only moves as far as needed to keep the button label legible.
pub fn primary_button_fills(palette: &Palette) -> PrimaryButtonFills {
    let foreground = palette.on_accent;
    // Keep arbitrary user colors exactly as selected. Known built-in palettes may
    // derive a minimally adjusted resting fill for accessibility; this never writes
    // back to the stored palette, and an exact custom copy renders identically.
    let rest = if is_known_builtin_palette(palette) { contrast_safe_anchor(foreground, palette.accent) } else { palette.accent };
    let hover = constrain_state(foreground, rest, palette.accent_text);
    let original_pressed = palette.accent.lerp_to_gamma(Color32::BLACK, 0.22);
    let pressed = constrain_state(foreground, rest, original_pressed);
    PrimaryButtonFills { rest, hover, pressed, foreground }
}

fn is_known_builtin_palette(palette: &Palette) -> bool {
    let library = ThemeLibrary::default();
    ThemePreference::ALL.into_iter().any(|choice| choice.palette(&library, None, ThemeKind::Light) == *palette)
}

/// Calculate a button fill using the same state interpolation as the GUI painter.
pub fn primary_button_fill(palette: &Palette, hover: f32, down: f32) -> Color32 {
    primary_button_fills(palette).fill(hover, down)
}

fn constrain_state(foreground: Color32, safe_anchor: Color32, candidate: Color32) -> Color32 {
    if foreground.a() < 255 || candidate.a() < 255 || safe_anchor.a() < 255 || contrast_ratio(foreground, safe_anchor) < NORMAL_TEXT {
        return candidate;
    }
    if segment_has_required_contrast(foreground, safe_anchor, candidate) {
        return candidate;
    }
    // Keep a continuous path from the safe resting fill; binary search the furthest
    // portion of the original interaction treatment that remains contrast-safe.
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..24 {
        let mid = (low + high) * 0.5;
        let state = safe_anchor.lerp_to_gamma(candidate, mid);
        if segment_has_required_contrast(foreground, safe_anchor, state) {
            low = mid;
        } else {
            high = mid;
        }
    }
    safe_anchor.lerp_to_gamma(candidate, low)
}

fn contrast_safe_anchor(foreground: Color32, background: Color32) -> Color32 {
    if contrast_ratio(foreground, background) >= NORMAL_TEXT || foreground.a() < 255 || background.a() < 255 {
        return background;
    }
    let endpoint =
        if contrast_ratio(foreground, Color32::BLACK) >= contrast_ratio(foreground, Color32::WHITE) { Color32::BLACK } else { Color32::WHITE };
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..24 {
        let mid = (low + high) * 0.5;
        let candidate = background.lerp_to_gamma(endpoint, mid);
        if contrast_ratio(foreground, candidate) >= NORMAL_TEXT {
            high = mid;
        } else {
            low = mid;
        }
    }
    background.lerp_to_gamma(endpoint, high)
}

fn segment_has_required_contrast(foreground: Color32, start: Color32, end: Color32) -> bool {
    (0..=32).all(|step| {
        let amount = step as f32 / 32.0;
        contrast_ratio(foreground, start.lerp_to_gamma(end, amount)) >= NORMAL_TEXT
    })
}

/// WCAG 2.x contrast ratio for opaque sRGB colors. For translucent colors, callers
/// should report a compositing-dependent diagnostic instead of using this value.
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let luminance = |color: Color32| {
        let linear = |channel: u8| {
            let srgb = f32::from(channel) / 255.0;
            if srgb <= 0.04045 { srgb / 12.92 } else { ((srgb + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    };
    let (first, second) = (luminance(a) + 0.05, luminance(b) + 0.05);
    first.max(second) / first.min(second)
}

/// Analyze foreground/background combinations used by the current GUI theme painters.
/// The function does not reject, rewrite, or persist any color values.
pub fn contrast_diagnostics(palette: &Palette) -> Vec<ContrastDiagnostic> {
    let button = primary_button_fills(palette);
    let button_fg_token = "on_accent";
    let checks = [
        ("text.panel", "text", "panel", palette.text, palette.panel, NORMAL_TEXT),
        ("text.card", "text", "card", palette.text, palette.card, NORMAL_TEXT),
        ("text_muted.panel", "text_muted", "panel", palette.text_muted, palette.panel, NORMAL_TEXT),
        ("text_muted.card", "text_muted", "card", palette.text_muted, palette.card, NORMAL_TEXT),
        ("link.panel", "accent_text", "panel", palette.accent_text, palette.panel, NORMAL_TEXT),
        ("icon.panel", "icon", "panel", palette.icon, palette.panel, GRAPHIC),
        ("disabled.text", "text_muted", "panel", palette.text_muted, palette.panel, NORMAL_TEXT),
        ("field.text", "text", "field", palette.text, palette.field, NORMAL_TEXT),
        ("field.placeholder_egui", "text_muted", "field", palette.text_muted, palette.field, NORMAL_TEXT),
        ("field.placeholder", "text_faint", "field", palette.text_faint, palette.field, NORMAL_TEXT),
        ("secondary_button.normal", "text", "card", palette.text, palette.card, NORMAL_TEXT),
        ("secondary_button.hover", "text", "hover", palette.text, palette.hover, NORMAL_TEXT),
        ("secondary_button.pressed", "text", "pressed", palette.text, palette.pressed, NORMAL_TEXT),
        ("selected_item.text", "selection_text", "selected", palette.selection_text, palette.selected, NORMAL_TEXT),
        ("selection.text", "selection_text", "accent_soft", palette.selection_text, palette.accent_soft, NORMAL_TEXT),
        ("selected_item.body_text", "text", "accent_soft", palette.text, palette.accent_soft, NORMAL_TEXT),
        ("badge.text", button_fg_token, "badge_new", palette.on_accent, palette.badge_new, NORMAL_TEXT),
        ("feedback.success", "success", "panel", palette.success, palette.panel, NORMAL_TEXT),
        ("feedback.warning", "warning", "panel", palette.warning, palette.panel, NORMAL_TEXT),
        ("feedback.error", "error", "panel", palette.error, palette.panel, NORMAL_TEXT),
        ("feedback.info", "info", "panel", palette.info, palette.panel, NORMAL_TEXT),
        ("feedback.success.card", "success", "card", palette.success, palette.card, NORMAL_TEXT),
        ("feedback.warning.card", "warning", "card", palette.warning, palette.card, NORMAL_TEXT),
        ("feedback.error.card", "error", "card", palette.error, palette.card, NORMAL_TEXT),
        ("feedback.info.card", "info", "card", palette.info, palette.card, NORMAL_TEXT),
        ("text.chrome", "text", "chrome", palette.text, palette.chrome, NORMAL_TEXT),
        ("text.titlebar", "text", "titlebar", palette.text, palette.titlebar, NORMAL_TEXT),
        ("text_faint.card", "text_faint", "card", palette.text_faint, palette.card, NORMAL_TEXT),
        ("text_faint.panel", "text_faint", "panel", palette.text_faint, palette.panel, NORMAL_TEXT),
        ("primary_button.normal", button_fg_token, "accent", button.foreground, button.rest, NORMAL_TEXT),
        ("primary_button.hover", button_fg_token, "accent_text", button.foreground, button.hover, NORMAL_TEXT),
        ("primary_button.pressed", button_fg_token, "accent", button.foreground, button.pressed, NORMAL_TEXT),
        ("focus.card", "focus", "card", palette.focus, palette.card, GRAPHIC),
        ("primary_button.focus", "focus", "field", palette.focus, palette.field, GRAPHIC),
        ("notice.text", "notice_text", "notice_bg", palette.notice_text, palette.notice_bg, NORMAL_TEXT),
    ];
    let mut diagnostics = Vec::new();
    for (role, fg_token, bg_token, foreground, background, threshold) in checks {
        if foreground.a() < 255 || background.a() < 255 {
            continue;
        }
        let ratio = contrast_ratio(foreground, background);
        if ratio < threshold {
            let legacy_label = match fg_token {
                "text_muted" => "muted_text",
                "text_faint" => "faint_text",
                _ => fg_token,
            };
            diagnostics.push(ContrastDiagnostic {
                role: role.into(),
                foreground_token: fg_token.into(),
                background_token: Some(bg_token.into()),
                ratio: Some(ratio),
                threshold,
                kind: "contrast".into(),
                message: format!("{legacy_label} contrast {ratio:.2}:1 is below {threshold:.1}:1 ({role}: {fg_token} on {bg_token})"),
            });
        }
    }
    for token in ColorToken::ALL {
        if matches!(token, ColorToken::PageShadow | ColorToken::WindowShadow | ColorToken::PopupShadow) {
            continue;
        }
        let color = token.get(palette);
        if color.a() < 255 {
            diagnostics.push(ContrastDiagnostic {
                role: format!("token.{}", token.key()),
                foreground_token: token.key().into(),
                background_token: None,
                ratio: None,
                threshold: NORMAL_TEXT,
                kind: "translucent".into(),
                message: format!("{} is translucent; contrast depends on underlying surfaces", token.key()),
            });
        }
    }
    diagnostics
}

/// Backward-compatible human-readable projection used by theme management operations.
pub fn contrast_warnings(palette: &Palette) -> Vec<String> {
    contrast_diagnostics(palette).into_iter().map(|diagnostic| diagnostic.message).collect()
}

/// Diagnostics for every shipped palette, useful to audit built-ins and System fallback.
pub fn builtin_contrast_diagnostics() -> Vec<(String, Vec<ContrastDiagnostic>)> {
    ThemePreference::ALL
        .into_iter()
        .map(|choice| {
            let palette = choice.palette(&ThemeLibrary::default(), None, ThemeKind::Light);
            (choice.id().into_owned(), contrast_diagnostics(&palette))
        })
        .collect()
}
