//! Versioned theme documents and their resolved, typed application palette.
//! Unknown data survives a save; malformed entries cannot invalidate their siblings.

use super::{Palette, ThemeKind, ThemePreference};
use ecolor::Color32;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const THEME_SCHEMA_VERSION: u32 = 1;

/// Identity is independent of a theme's editable display name. Creation supplies
/// 16 random bytes; the future editor owns platform entropy and collision retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CustomThemeId([u8; 16]);

impl CustomThemeId {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() != 36 {
            return None;
        }
        let mut bytes = [0; 16];
        let mut digits =
            value.bytes().enumerate().filter_map(|(i, b)| if [8, 13, 18, 23].contains(&i) { (b != b'-').then_some(b'?') } else { Some(b) });
        for byte in &mut bytes {
            let hi = (digits.next()? as char).to_digit(16)?;
            let lo = (digits.next()? as char).to_digit(16)?;
            *byte = (hi * 16 + lo) as u8;
        }
        if digits.next().is_some() {
            return None;
        }
        Some(Self(bytes))
    }
}
impl std::fmt::Display for CustomThemeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if [4, 6, 8, 10].contains(&i) {
                write!(f, "-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
impl Serialize for CustomThemeId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for CustomThemeId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| serde::de::Error::custom("expected a UUID-shaped theme ID"))
    }
}

/// A single registry drives typed access, persistence and future editor rows.
macro_rules! palette_tokens {
    ($($variant:ident => $field:ident, $group:literal;)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum ColorToken { $($variant,)+ }
        impl ColorToken {
            // Exhaustive destructuring makes adding a color without registering it
            // a compile error, not a silently uneditable future token.
            fn check_complete(palette: &Palette) {
                let Palette { kind: _, radius: _, legacy: _, $($field: _,)+ } = palette;
            }
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
            pub fn key(self) -> &'static str { match self { $(Self::$variant => stringify!($field),)+ } }
            pub fn group(self) -> &'static str { match self { $(Self::$variant => $group,)+ } }
            pub fn get(self, palette: &Palette) -> Color32 { match self { $(Self::$variant => palette.$field,)+ } }
            pub fn set(self, palette: &mut Palette, color: Color32) { match self { $(Self::$variant => palette.$field = color,)+ } }
        }
    }
}
palette_tokens! {
    Titlebar => titlebar, "Surfaces";
    Chrome => chrome, "Surfaces";
    Panel => panel, "Surfaces";
    Pasteboard => pasteboard, "Surfaces";
    Card => card, "Surfaces";
    Field => field, "Surfaces";
    Border => border, "Borders";
    Divider => divider, "Borders";
    Text => text, "Text";
    TextMuted => text_muted, "Text";
    TextFaint => text_faint, "Text";
    Icon => icon, "Text";
    Hover => hover, "Controls";
    Pressed => pressed, "Controls";
    Selected => selected, "Controls";
    Focus => focus, "Controls";
    SelectionText => selection_text, "Controls";
    Accent => accent, "Accents";
    AccentText => accent_text, "Accents";
    AccentSoft => accent_soft, "Accents";
    OnAccent => on_accent, "Accents";
    BadgeNew => badge_new, "Accents";
    Success => success, "Feedback";
    Warning => warning, "Feedback";
    Error => error, "Feedback";
    Info => info, "Feedback";
    NoticeBackground => notice_bg, "Feedback";
    NoticeText => notice_text, "Feedback";
    PageShadow => page_shadow, "Shadows";
    WindowShadow => window_shadow, "Shadows";
    PopupShadow => popup_shadow, "Shadows";
}

pub fn parse_color(value: &str) -> Option<Color32> {
    let hex = value.strip_prefix('#')?;
    if ![6, 8].contains(&hex.len()) || !hex.is_ascii() {
        return None;
    }
    let mut bytes = [255; 4];
    for (out, pair) in bytes.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        let [hi, lo] = pair;
        *out = ((*hi as char).to_digit(16)? * 16 + (*lo as char).to_digit(16)?) as u8;
    }
    Some(Color32::from_rgba_unmultiplied(bytes[0], bytes[1], bytes[2], bytes[3]))
}
fn color_hex(color: Color32) -> String {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

/// Wire representation shared by built-in copies and custom themes. Values stay
/// raw so unknown or invalid tokens are retained, while resolution is typed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaletteDocument(pub BTreeMap<String, Value>);
impl PaletteDocument {
    pub fn from_palette(palette: &Palette) -> Self {
        ColorToken::check_complete(palette);
        Self(ColorToken::ALL.iter().map(|t| (t.key().to_owned(), Value::String(color_hex(t.get(palette))))).collect())
    }
    pub fn resolve(&self, mut base: Palette) -> Palette {
        for token in ColorToken::ALL {
            if let Some(color) = self.0.get(token.key()).and_then(Value::as_str).and_then(parse_color) {
                token.set(&mut base, color);
            }
        }
        base.legacy = false;
        base
    }
    pub fn set(&mut self, token: ColorToken, color: Color32) {
        self.0.insert(token.key().into(), Value::String(color_hex(color)));
    }

    pub fn reset(&mut self, token: ColorToken) {
        self.0.remove(token.key());
    }

    pub fn invalid_tokens(&self) -> Vec<&'static str> {
        ColorToken::ALL
            .iter()
            .filter_map(|token| self.0.get(token.key()).filter(|value| value.as_str().and_then(parse_color).is_none()).map(|_| token.key()))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeDefinition {
    pub schema_version: u32,
    pub id: CustomThemeId,
    pub name: String,
    pub kind: ThemeKind,
    /// Carry the original egui/chrome defaults when copying Light or Dark.
    /// Individual edited colors still override their legacy role.
    #[serde(default)]
    pub legacy_defaults: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_theme_id: Option<String>,
    #[serde(default)]
    pub palette: PaletteDocument,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}
impl ThemeDefinition {
    pub fn duplicate_builtin(source: ThemePreference, id: CustomThemeId, name: String) -> Result<Self, String> {
        if !ThemePreference::ALL.contains(&source) || source == ThemePreference::System {
            return Err("choose a concrete built-in palette to duplicate".into());
        }
        let palette = source.palette(&ThemeLibrary::default(), None, ThemeKind::Light);
        Ok(Self {
            schema_version: THEME_SCHEMA_VERSION,
            id,
            name,
            kind: palette.kind,
            legacy_defaults: palette.legacy,
            base_theme_id: Some(source.id().into()),
            palette: PaletteDocument::from_palette(&palette),
            metadata: BTreeMap::new(),
            extensions: BTreeMap::new(),
        })
    }
    pub fn resolve(&self) -> Palette {
        let base = self
            .base_theme_id
            .as_deref()
            .and_then(ThemePreference::parse)
            .filter(|p| ThemePreference::ALL.contains(p) && *p != ThemePreference::System)
            .map(|p| p.palette(&ThemeLibrary::default(), None, self.kind))
            .unwrap_or_else(|| Palette::for_kind(self.kind));
        if self.schema_version != THEME_SCHEMA_VERSION {
            return Palette::for_kind(self.kind);
        }
        let mut palette = self.palette.resolve(base);
        palette.kind = self.kind;
        palette.legacy = self.legacy_defaults;
        palette
    }
}

/// Preserve the entire envelope, including future schemas and malformed entries.
/// There is no arbitrary count limit. Mutating an unsupported schema is refused.
#[derive(Clone, Debug)]
pub struct ThemeLibrary(std::sync::Arc<LibraryData>);
#[derive(Debug)]
struct LibraryData {
    raw: Value,
    index: std::sync::OnceLock<LibraryIndex>,
    choices: std::sync::OnceLock<ChoiceIndex>,
}
impl Clone for LibraryData {
    fn clone(&self) -> Self {
        Self { raw: self.raw.clone(), index: Default::default(), choices: Default::default() }
    }
}
#[derive(Debug, Default)]
struct LibraryIndex {
    entries: BTreeMap<CustomThemeId, Vec<usize>>,
}
#[derive(Debug, Default)]
struct ChoiceIndex {
    choices: std::sync::Arc<[ThemeChoice]>,
    names: BTreeMap<CustomThemeId, String>,
}
/// Cached selector metadata; palette records are decoded only when selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeChoice {
    pub id: CustomThemeId,
    pub name: String,
}
impl PartialEq for ThemeLibrary {
    fn eq(&self, other: &Self) -> bool {
        self.same_snapshot(other) || self.0.raw == other.0.raw
    }
}
impl Serialize for ThemeLibrary {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.raw.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for ThemeLibrary {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(Value::deserialize(deserializer)?))
    }
}
impl Default for ThemeLibrary {
    fn default() -> Self {
        Self::from_value(serde_json::json!({"schema_version": THEME_SCHEMA_VERSION, "themes": []}))
    }
}
impl ThemeLibrary {
    pub fn supported(&self) -> bool {
        self.0.raw.get("schema_version").and_then(Value::as_u64) == Some(u64::from(THEME_SCHEMA_VERSION))
    }
    pub fn definitions(&self) -> impl Iterator<Item = ThemeDefinition> + '_ {
        self.0.raw.get("themes").and_then(Value::as_array).into_iter().flatten().filter_map(|entry| serde_json::from_value(entry.clone()).ok())
    }
    pub(crate) fn id_count(&self, id: CustomThemeId) -> usize {
        self.index().entries.get(&id).map_or(0, Vec::len)
    }
    pub fn get(&self, id: CustomThemeId) -> Option<ThemeDefinition> {
        if !self.supported() {
            return None;
        }
        let entries = self.0.raw.get("themes")?.as_array()?;
        self.index().entries.get(&id)?.iter().find_map(|position| serde_json::from_value(entries.get(*position)?.clone()).ok())
    }
    fn from_value(raw: Value) -> Self {
        Self(std::sync::Arc::new(LibraryData { raw, index: Default::default(), choices: Default::default() }))
    }
    fn raw_mut(&mut self) -> &mut Value {
        let data = std::sync::Arc::make_mut(&mut self.0);
        data.index.take();
        data.choices.take();
        &mut data.raw
    }
    /// Identity changes on every mutation/deserialization; clones share their immutable data.
    pub fn same_snapshot(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
    fn index(&self) -> &LibraryIndex {
        self.0.index.get_or_init(|| {
            let mut entries: BTreeMap<CustomThemeId, Vec<usize>> = BTreeMap::new();
            if self.supported() {
                for (position, entry) in self.0.raw.get("themes").and_then(Value::as_array).into_iter().flatten().enumerate() {
                    if let Some(id) = entry.get("id").and_then(Value::as_str).and_then(CustomThemeId::parse) {
                        entries.entry(id).or_default().push(position);
                    }
                }
            }
            LibraryIndex { entries }
        })
    }
    fn choice_index(&self) -> &ChoiceIndex {
        self.0.choices.get_or_init(|| {
            if !self.supported() {
                return ChoiceIndex::default();
            }
            let mut names = BTreeMap::new();
            let mut choices = Vec::new();
            for definition in self.definitions().filter(|_| self.supported()) {
                if names.contains_key(&definition.id) {
                    continue;
                }
                names.insert(definition.id, definition.name.clone());
                if definition.schema_version == THEME_SCHEMA_VERSION {
                    choices.push(ThemeChoice { id: definition.id, name: definition.name });
                }
            }
            ChoiceIndex { names, choices: choices.into() }
        })
    }
    pub fn name(&self, id: CustomThemeId) -> Option<&str> {
        if let Some(choices) = self.0.choices.get() {
            return choices.names.get(&id).map(String::as_str);
        }
        // Startup can retrieve the active label without deserializing every theme.
        let entries = self.0.raw.get("themes")?.as_array()?;
        self.index().entries.get(&id)?.iter().find_map(|position| {
            let entry = entries.get(*position)?;
            serde_json::from_value::<ThemeDefinition>(entry.clone()).ok()?;
            entry.get("name")?.as_str()
        })
    }
    pub fn choices_if_ready(&self) -> Option<std::sync::Arc<[ThemeChoice]>> {
        self.0.choices.get().map(|index| index.choices.clone())
    }
    pub fn choices(&self) -> std::sync::Arc<[ThemeChoice]> {
        let _ = self.index();
        self.choice_index().choices.clone()
    }
    /// Diagnostics are collected at load time, never on every render frame.
    pub fn diagnostics(&self) -> Vec<String> {
        if !self.supported() {
            return vec!["unsupported theme library schema; data retained".into()];
        }
        let Some(entries) = self.0.raw.get("themes").and_then(Value::as_array) else { return vec!["invalid theme list; data retained".into()] };
        let mut warnings = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        for entry in entries {
            match serde_json::from_value::<ThemeDefinition>(entry.clone()) {
                Ok(theme) => {
                    if !ids.insert(theme.id) {
                        warnings.push(format!("duplicate theme ID {}; first valid entry wins", theme.id));
                    }
                    if theme.schema_version != THEME_SCHEMA_VERSION {
                        warnings.push(format!("unsupported theme schema for {}; displaying fallback", theme.id));
                    }
                    for token in theme.palette.invalid_tokens() {
                        warnings.push(format!("invalid {token} in {}; using base color", theme.id));
                    }
                }
                Err(error) => warnings.push(format!("invalid theme entry retained: {error}")),
            }
        }
        warnings
    }

    fn ensure_unambiguous(&self, id: CustomThemeId) -> Result<(), String> {
        let count = self
            .0
            .raw
            .get("themes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|entry| entry.get("id").and_then(Value::as_str).and_then(CustomThemeId::parse) == Some(id))
            .count();
        match count {
            0 => Err("theme ID does not exist".into()),
            1 => Ok(()),
            _ => Err("duplicate theme ID; repair the library before editing this theme".into()),
        }
    }

    /// Replace a current-schema record, leaving unrelated and unknown data intact.
    pub fn replace(&mut self, theme: ThemeDefinition) -> Result<(), String> {
        if !self.supported() || theme.schema_version != THEME_SCHEMA_VERSION {
            return Err("unsupported theme schema version".into());
        }
        if theme.name.trim().is_empty() {
            return Err("theme name cannot be empty".into());
        }
        self.ensure_unambiguous(theme.id)?;
        let current = self.get(theme.id).ok_or("theme ID does not exist")?;
        if current.schema_version != THEME_SCHEMA_VERSION {
            return Err("future theme is read-only".into());
        }
        let id = theme.id;
        let value = serde_json::to_value(theme).map_err(|e| e.to_string())?;
        let entry = self
            .raw_mut()
            .get_mut("themes")
            .and_then(Value::as_array_mut)
            .and_then(|entries| entries.iter_mut().find(|entry| entry.get("id").and_then(Value::as_str).and_then(CustomThemeId::parse) == Some(id)))
            .ok_or("theme ID does not exist")?;
        *entry = value;
        Ok(())
    }

    pub fn rename(&mut self, id: CustomThemeId, name: String) -> Result<(), String> {
        let mut theme = self.get(id).ok_or("theme ID does not exist")?;
        theme.name = name;
        self.replace(theme)
    }

    pub fn remove(&mut self, id: CustomThemeId) -> Result<(), String> {
        if !self.supported() {
            return Err("unsupported theme library schema".into());
        }
        self.ensure_unambiguous(id)?;
        let current = self.get(id).ok_or("theme ID does not exist")?;
        if current.schema_version != THEME_SCHEMA_VERSION {
            return Err("future theme is read-only".into());
        }
        let entries = self.raw_mut().get_mut("themes").and_then(Value::as_array_mut).ok_or("invalid theme library")?;
        entries.retain(|entry| entry.get("id").and_then(Value::as_str).and_then(CustomThemeId::parse) != Some(id));
        Ok(())
    }

    pub fn insert(&mut self, theme: ThemeDefinition) -> Result<(), String> {
        if !self.supported() || theme.schema_version != THEME_SCHEMA_VERSION {
            return Err("unsupported theme schema version".into());
        }
        if theme.name.trim().is_empty() {
            return Err("theme name cannot be empty".into());
        }
        if self
            .0
            .raw
            .get("themes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|entry| entry.get("id").and_then(Value::as_str).and_then(CustomThemeId::parse) == Some(theme.id))
        {
            return Err("theme ID already exists; create a new copy ID".into());
        }
        let value = serde_json::to_value(theme).map_err(|e| e.to_string())?;
        self.raw_mut().get_mut("themes").and_then(Value::as_array_mut).ok_or("invalid theme library")?.push(value);
        Ok(())
    }
}

impl ThemePreference {
    pub const ALL: [Self; 9] =
        [Self::System, Self::Light, Self::Dark, Self::Catppuccin, Self::Solarized, Self::Forest, Self::Ocean, Self::Sunset, Self::PaperInk];
    pub fn id(self) -> std::borrow::Cow<'static, str> {
        let id = match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Catppuccin => "catppuccin",
            Self::Solarized => "solarized",
            Self::Forest => "forest",
            Self::Ocean => "ocean",
            Self::Sunset => "sunset",
            Self::PaperInk => "paper-ink",
            Self::Custom(id) => return format!("custom:{id}").into(),
        };
        id.into()
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Use system setting",
            Self::Light => "Light gray",
            Self::Dark => "Dark gray",
            Self::Catppuccin => "Atelier",
            Self::Solarized => "Editorial",
            Self::Forest => "Heritage",
            Self::Ocean => "Executive",
            Self::Sunset => "Sienna",
            Self::PaperInk => "Paper & Ink",
            Self::Custom(_) => "Custom theme",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == value).or_else(|| value.strip_prefix("custom:").and_then(CustomThemeId::parse).map(Self::Custom))
    }
    pub fn palette(self, library: &ThemeLibrary, system: Option<ThemeKind>, fallback: ThemeKind) -> Palette {
        if let Self::Custom(id) = self {
            return library.get(id).map(|d| d.resolve()).unwrap_or_else(|| Palette::for_kind(ThemeKind::Light));
        }
        if matches!(self, Self::System | Self::Light | Self::Dark) {
            return Palette::for_kind(self.resolve(system, fallback));
        }
        preset(self)
    }
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb(((hex >> 16) & 255) as u8, ((hex >> 8) & 255) as u8, (hex & 255) as u8)
}

fn preset(choice: ThemePreference) -> Palette {
    // Surfaces: title, chrome, card, canvas, input, hover, pressed, selected.
    // Text: primary, secondary, faint, accent, accent-hover/link, on-accent.
    // Feedback: success, warning, error, information.
    let (surfaces, text, feedback) = match choice {
        ThemePreference::Catppuccin => (
            [0x181825, 0x1e1e2e, 0x262637, 0x11111b, 0x181825, 0x313244, 0x3b3d52, 0x34334c],
            [0xcdd6f4, 0xbac2de, 0xaeb7d5, 0xcba6f7, 0xb4befe, 0x181825],
            [0xa6e3a1, 0xf9e2af, 0xf38ba8, 0x89dceb],
        ),
        ThemePreference::Solarized => (
            [0x00212b, 0x002b36, 0x073642, 0x001c24, 0x00212b, 0x103e49, 0x164650, 0x124451],
            [0xeee8d5, 0xbdc8c5, 0xaabbb9, 0x62b9d6, 0x85ccd9, 0x002b36],
            [0xb0c561, 0xe0bb67, 0xf18b82, 0x77bfd2],
        ),
        ThemePreference::Forest => (
            [0x14231d, 0x1a2b23, 0x20372c, 0x101c16, 0x14231d, 0x2b4435, 0x334d3d, 0x314736],
            [0xe9eddf, 0xc5d2bb, 0xb6c6ab, 0xa3ce8c, 0xc4daa1, 0x14231d],
            [0x9cd5a1, 0xe2c384, 0xefaaa0, 0x9bcbd0],
        ),
        ThemePreference::Ocean => (
            [0x101b30, 0x14243a, 0x1c2e47, 0x0b1425, 0x101c32, 0x253c56, 0x2d4560, 0x21445a],
            [0xe0effa, 0xbad2e7, 0xa8c5dd, 0x67cbe7, 0x91ddef, 0x102438],
            [0x82d5b5, 0xf0cd86, 0xf3a3aa, 0x92c5fa],
        ),
        ThemePreference::Sunset => (
            [0x261a29, 0x302034, 0x3b293e, 0x201622, 0x271a2c, 0x4a3247, 0x563b4f, 0x503448],
            [0xffece5, 0xe9c8ce, 0xdcbbc7, 0xf3ae9a, 0xf6c69c, 0x302034],
            [0xb7d4a0, 0xf5d087, 0xffa9ba, 0xc4b8ef],
        ),
        ThemePreference::PaperInk => (
            [0xede6d8, 0xfaf6ed, 0xfffcf5, 0xe9e2d5, 0xfffdf8, 0xeae3d6, 0xded5c5, 0xe5ddce],
            [0x292722, 0x514b42, 0x5d554b, 0x754733, 0x643924, 0xfffcf5],
            [0x356142, 0x78520c, 0x9d3935, 0x365d78],
        ),
        _ => return Palette::for_kind(ThemeKind::Light),
    };
    let [title, chrome, card, canvas, field, hover, pressed, selected] = surfaces.map(rgb);
    let [text, muted, faint, accent, link, on_accent] = text.map(rgb);
    let [success, warning, error, info] = feedback.map(rgb);
    let light = choice == ThemePreference::PaperInk;
    Palette {
        kind: if light { ThemeKind::Light } else { ThemeKind::Dark },
        titlebar: title,
        chrome,
        panel: chrome,
        pasteboard: canvas,
        card,
        field,
        border: if light { rgb(0xb5aa96) } else { muted.gamma_multiply(0.45) },
        divider: if light { rgb(0xd5ccbc) } else { muted.gamma_multiply(0.25) },
        text,
        text_muted: muted,
        text_faint: faint,
        icon: muted,
        hover,
        pressed,
        selected,
        accent,
        accent_text: link,
        accent_soft: selected,
        badge_new: accent,
        on_accent,
        success,
        warning,
        error,
        info,
        focus: accent,
        selection_text: text,
        notice_bg: card,
        notice_text: text,
        page_shadow: Color32::from_black_alpha(if light { 34 } else { 120 }),
        window_shadow: Color32::from_black_alpha(if light { 38 } else { 110 }),
        popup_shadow: Color32::from_black_alpha(if light { 30 } else { 90 }),
        radius: 6,
        legacy: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contrast(a: Color32, b: Color32) -> f32 {
        let lum = |c: Color32| {
            let f = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let (a, b) = (lum(a) + 0.05, lum(b) + 0.05);
        a.max(b) / a.min(b)
    }
    #[test]
    fn stable_ids_and_complete_palettes() {
        assert_eq!(
            ThemePreference::ALL.map(ThemePreference::id),
            ["system", "light", "dark", "catppuccin", "solarized", "forest", "ocean", "sunset", "paper-ink"]
        );
        for choice in ThemePreference::ALL {
            assert_eq!(ThemePreference::parse(&choice.id()), Some(choice));
            let encoded = serde_json::to_string(&choice).unwrap();
            assert_eq!(serde_json::from_str::<ThemePreference>(&encoded).unwrap(), choice);
            let palette = choice.palette(&ThemeLibrary::default(), None, ThemeKind::Light);
            let document = PaletteDocument::from_palette(&palette);
            assert_eq!(document.0.len(), ColorToken::ALL.len());
            let restored: PaletteDocument = serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
            let resolved = restored.resolve(palette);
            for token in ColorToken::ALL {
                assert_eq!(token.get(&resolved), token.get(&palette), "{}", token.key());
            }
        }
    }
    #[test]
    fn preset_contrast() {
        for choice in ThemePreference::ALL.into_iter().skip(3) {
            let p = choice.palette(&ThemeLibrary::default(), None, ThemeKind::Light);
            for foreground in [p.text, p.text_muted, p.text_faint, p.icon] {
                for background in [p.titlebar, p.chrome, p.card, p.pasteboard, p.field, p.hover, p.pressed, p.selected] {
                    assert!(
                        contrast(foreground, background) >= 4.5,
                        "{choice:?} {foreground:?} on {background:?}: {}",
                        contrast(foreground, background)
                    );
                }
            }
            for bg in [p.accent, p.accent_text, p.badge_new] {
                assert!(contrast(p.on_accent, bg) >= 4.5, "{choice:?} button contrast");
            }
            for color in [p.success, p.warning, p.error, p.info, p.accent_text] {
                assert!(contrast(color, p.panel) >= 4.5, "{choice:?} feedback");
            }
            assert!(contrast(p.focus, p.field) >= 3.0);
            assert!(contrast(p.selection_text, p.accent_soft) >= 4.5);
            assert!(contrast(p.notice_text, p.notice_bg) >= 4.5);
        }
    }
    #[test]
    fn custom_roundtrip_repairs_individual_tokens_and_preserves_unknown_data() {
        let id = CustomThemeId::parse("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        let mut definition = ThemeDefinition::duplicate_builtin(ThemePreference::Ocean, id, "Personal ocean".into()).unwrap();
        definition.palette.0.insert("text".into(), Value::String("broken".into()));
        definition.palette.0.insert("future_token".into(), serde_json::json!({"future":true}));
        definition.palette.0.remove("chrome");
        definition.palette.0.insert("accent".into(), Value::String("#abcdef".into()));
        assert_eq!(definition.palette.invalid_tokens(), ["text"]);
        let expected = ThemePreference::Ocean.palette(&ThemeLibrary::default(), None, ThemeKind::Light);
        assert_eq!(definition.resolve().text, expected.text);
        assert_eq!(definition.resolve().chrome, expected.chrome);
        assert_eq!(definition.resolve().accent, rgb(0xabcdef));
        let mut library = ThemeLibrary::default();
        library.insert(definition.clone()).unwrap();
        assert!(library.insert(definition.clone()).is_err());
        let saved = serde_json::to_string(&library).unwrap();
        let restored: ThemeLibrary = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.get(id), Some(definition));
        let choice = ThemePreference::Custom(id);
        assert_eq!(serde_json::from_str::<ThemePreference>(&serde_json::to_string(&choice).unwrap()).unwrap(), choice);
        assert_eq!(ThemePreference::parse(&format!("custom:{id}")), Some(choice));
    }
    #[test]
    fn future_and_corrupt_library_data_survives() {
        for value in [
            serde_json::json!({"schema_version":99,"future":42,"themes":[null]}),
            serde_json::json!({"schema_version":1,"themes":[null,{"bad":true}],"future":42}),
        ] {
            let library: ThemeLibrary = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(&library).unwrap(), value);
            let missing = ThemePreference::Custom(CustomThemeId::from_bytes([1; 16]));
            assert_eq!(missing.palette(&library, None, ThemeKind::Dark), Palette::for_kind(ThemeKind::Light));
        }
        assert!(CustomThemeId::parse("0123456789ab-cdef-0123-456789abcdef-").is_none());
        assert!(parse_color("#日本語").is_none());
        assert!(parse_color("#zzzzzz").is_none());
        assert_eq!(parse_color("#00000022"), Some(Color32::from_black_alpha(34)));
    }
}

#[cfg(test)]
mod editing_tests {
    use super::*;
    #[test]
    fn library_edits_preserve_ids_unknown_data_and_duplicate_names() {
        let a = CustomThemeId::from_bytes([1; 16]);
        let b = CustomThemeId::from_bytes([2; 16]);
        assert_ne!(ThemePreference::Custom(a).id(), ThemePreference::Custom(b).id());
        let mut library = ThemeLibrary::default();
        for id in [a, b] {
            library.insert(ThemeDefinition::duplicate_builtin(ThemePreference::Sunset, id, "Same name".into()).unwrap()).unwrap();
        }
        library.raw_mut()["future"] = serde_json::json!({"retained":true});
        library.raw_mut()["themes"].as_array_mut().unwrap().push(serde_json::json!({"malformed":true}));
        library.rename(a, "Renamed".into()).unwrap();
        assert_eq!(library.get(a).unwrap().name, "Renamed");
        assert_eq!(library.get(b).unwrap().name, "Same name");
        let mut edit = library.get(a).unwrap();
        edit.palette.0.insert("unknown".into(), serde_json::json!([1, 2, 3]));
        for token in ColorToken::ALL {
            edit.palette.set(*token, Color32::from_rgb(12, 34, 56));
            assert_eq!(token.get(&edit.resolve()), Color32::from_rgb(12, 34, 56));
            edit.palette.reset(*token);
        }
        library.replace(edit).unwrap();
        assert_eq!(library.get(a).unwrap().palette.0["unknown"], serde_json::json!([1, 2, 3]));
        library.remove(b).unwrap();
        assert!(library.get(b).is_none());
        assert_eq!(library.0.raw["themes"].as_array().unwrap().len(), 2);
        assert_eq!(library.0.raw["future"]["retained"], true);
        assert_eq!(library.0.raw["themes"][1]["malformed"], true);
    }
    #[test]
    fn duplicate_or_malformed_ids_cannot_be_overwritten_or_deleted() {
        let id = CustomThemeId::from_bytes([9; 16]);
        let valid = ThemeDefinition::duplicate_builtin(ThemePreference::Ocean, id, "Kept".into()).unwrap();
        for first in [serde_json::json!({"id": id, "corrupt": true}), serde_json::to_value(&valid).unwrap()] {
            let mut library = ThemeLibrary::from_value(serde_json::json!({"schema_version":1,"themes":[first,valid]}));
            let original = library.clone();
            assert_eq!(library.get(id).unwrap().name, "Kept");
            assert!(library.rename(id, "Changed".into()).is_err());
            assert!(library.remove(id).is_err());
            assert!(library.insert(valid.clone()).is_err());
            assert_eq!(library, original);
        }
    }

    #[test]
    fn identifiers_require_canonical_separators_and_no_theme_count_cap() {
        let canonical = "01234567-89ab-cdef-0123-456789abcdef";
        for index in [8, 13, 18, 23] {
            let mut bytes = canonical.as_bytes().to_vec();
            bytes[index] = b'x';
            assert!(CustomThemeId::parse(&String::from_utf8(bytes).unwrap()).is_none());
        }
        let mut library = ThemeLibrary::default();
        for i in 0u128..1024 {
            library
                .insert(
                    ThemeDefinition::duplicate_builtin(ThemePreference::Ocean, CustomThemeId::from_bytes(i.to_be_bytes()), "Copy".into()).unwrap(),
                )
                .unwrap();
        }
        let restored: ThemeLibrary = serde_json::from_str(&serde_json::to_string(&library).unwrap()).unwrap();
        assert_eq!(restored.definitions().count(), 1024);
    }
}
