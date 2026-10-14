//! Shared, GUI-independent application theme palettes and validated management operations.
//! PDF document colors are intentionally outside this crate.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use ecolor::Color32;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;
#[cfg(not(target_arch = "wasm32"))]
pub mod watch;
#[cfg(not(target_arch = "wasm32"))]
pub use store::ThemeStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ThemeKind {
    #[default]
    Light,
    Dark,
}

/// The saved user choice, independent of the light/dark colours currently displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ThemePreference {
    System,
    #[default]
    Light,
    Dark,
    #[serde(rename = "catppuccin")]
    Catppuccin,
    #[serde(rename = "solarized")]
    Solarized,
    #[serde(rename = "forest")]
    Forest,
    #[serde(rename = "ocean")]
    Ocean,
    #[serde(rename = "sunset")]
    Sunset,
    #[serde(rename = "paper-ink")]
    PaperInk,
    Custom(CustomThemeId),
}

mod cache;
mod imports;
pub use cache::ActivePaletteCache;
mod library;
pub use library::*;

pub mod contrast;
pub use contrast::{
    ContrastDiagnostic, PrimaryButtonFills, contrast_diagnostics, contrast_ratio, contrast_warnings, primary_button_fill, primary_button_fills,
};

impl ThemePreference {
    pub fn resolve(self, system: Option<ThemeKind>, fallback: ThemeKind) -> ThemeKind {
        match self {
            Self::Light => ThemeKind::Light,
            Self::Dark | Self::Catppuccin | Self::Solarized | Self::Forest | Self::Ocean | Self::Sunset => ThemeKind::Dark,
            Self::PaperInk => ThemeKind::Light,
            Self::Custom(_) => fallback,
            Self::System => system.unwrap_or(fallback),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub kind: ThemeKind,
    /// Title/tab strip.
    pub titlebar: Color32,
    /// Mode bar, panels.
    pub chrome: Color32,
    pub panel: Color32,
    /// Document area behind pages.
    pub pasteboard: Color32,
    pub card: Color32,
    pub border: Color32,
    pub divider: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub selected: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub accent_soft: Color32,
    pub field: Color32,
    pub badge_new: Color32,
    pub page_shadow: Color32,
    pub radius: u8,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    pub info: Color32,
    pub on_accent: Color32,
    pub notice_bg: Color32,
    pub notice_text: Color32,
    pub focus: Color32,
    pub selection_text: Color32,
    pub window_shadow: Color32,
    pub popup_shadow: Color32,
    /// Preserve egui's historical defaults for the two original palettes.
    pub legacy: bool,
}

pub type Tokens = Palette;

impl Palette {
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Light => Self {
                kind,
                titlebar: Color32::from_rgb(0xE9, 0xE9, 0xEB),
                chrome: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                panel: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                pasteboard: Color32::from_rgb(0xF1, 0xF1, 0xF3),
                card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                border: Color32::from_rgb(0xDA, 0xDA, 0xDE),
                divider: Color32::from_rgb(0xE8, 0xE8, 0xEB),
                text: Color32::from_rgb(0x22, 0x22, 0x26),
                text_muted: Color32::from_rgb(0x5E, 0x5E, 0x66),
                text_faint: Color32::from_rgb(0x6B, 0x6B, 0x73),
                icon: Color32::from_rgb(0x44, 0x44, 0x4B),
                hover: Color32::from_rgb(0xF0, 0xF0, 0xF3),
                pressed: Color32::from_rgb(0xE4, 0xE4, 0xE9),
                selected: Color32::from_rgb(0xE6, 0xEE, 0xFD),
                accent: Color32::from_rgb(0x1B, 0x63, 0xE0),
                accent_text: Color32::from_rgb(0x17, 0x55, 0xC4),
                accent_soft: Color32::from_rgb(0xE3, 0xEC, 0xFD),
                field: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                badge_new: Color32::from_rgb(0x1B, 0x63, 0xE0),
                page_shadow: Color32::from_black_alpha(34),
                radius: 6,
                success: Color32::from_rgb(40, 140, 80),
                warning: Color32::from_rgb(200, 130, 20),
                error: Color32::from_rgb(200, 50, 50),
                info: Color32::from_rgb(40, 110, 200),
                on_accent: Color32::WHITE,
                notice_bg: Color32::from_rgb(42, 42, 47),
                notice_text: Color32::WHITE,
                focus: Color32::from_rgb(27, 99, 224),
                selection_text: Color32::from_rgb(27, 99, 224),
                window_shadow: Color32::from_black_alpha(38),
                popup_shadow: Color32::from_black_alpha(30),
                legacy: true,
            },
            ThemeKind::Dark => Self {
                kind,
                titlebar: Color32::from_rgb(0x1B, 0x1B, 0x1E),
                chrome: Color32::from_rgb(0x26, 0x26, 0x2A),
                panel: Color32::from_rgb(0x26, 0x26, 0x2A),
                pasteboard: Color32::from_rgb(0x19, 0x19, 0x1C),
                card: Color32::from_rgb(0x2E, 0x2E, 0x33),
                border: Color32::from_rgb(0x3C, 0x3C, 0x43),
                divider: Color32::from_rgb(0x33, 0x33, 0x39),
                text: Color32::from_rgb(0xEC, 0xEC, 0xEF),
                text_muted: Color32::from_rgb(0xAE, 0xAE, 0xB6),
                text_faint: Color32::from_rgb(0x97, 0x97, 0x9E),
                icon: Color32::from_rgb(0xD4, 0xD4, 0xDA),
                hover: Color32::from_rgb(0x34, 0x34, 0x3A),
                pressed: Color32::from_rgb(0x3E, 0x3E, 0x45),
                selected: Color32::from_rgb(0x23, 0x3A, 0x63),
                accent: Color32::from_rgb(0x4B, 0x8B, 0xF5),
                accent_text: Color32::from_rgb(0x8C, 0xB6, 0xFA),
                accent_soft: Color32::from_rgb(0x24, 0x36, 0x57),
                field: Color32::from_rgb(0x1E, 0x1E, 0x22),
                badge_new: Color32::from_rgb(0x3D, 0x7D, 0xEE),
                page_shadow: Color32::from_black_alpha(120),
                radius: 6,
                success: Color32::from_rgb(40, 140, 80),
                warning: Color32::from_rgb(200, 130, 20),
                error: Color32::from_rgb(200, 50, 50),
                info: Color32::from_rgb(40, 110, 200),
                on_accent: Color32::WHITE,
                notice_bg: Color32::from_rgb(236, 236, 239),
                notice_text: Color32::from_rgb(34, 34, 38),
                focus: Color32::from_rgb(75, 139, 245),
                selection_text: Color32::from_rgb(75, 139, 245),
                window_shadow: Color32::from_black_alpha(110),
                popup_shadow: Color32::from_black_alpha(90),
                legacy: true,
            },
        }
    }

    /// Keep the original Light/Dark chrome exact while routing extended palettes
    /// through a semantic role. Document content must not call this adapter.
    pub fn compatible(&self, original: Color32, token: ColorToken) -> Color32 {
        if self.overrides(token) { token.get(self) } else { original }
    }

    pub fn overrides(&self, token: ColorToken) -> bool {
        !self.legacy || token.get(self) != token.get(&Self::for_kind(self.kind))
    }

    pub fn dark(&self) -> bool {
        self.kind == ThemeKind::Dark
    }
}

mod management;
pub use management::*;
