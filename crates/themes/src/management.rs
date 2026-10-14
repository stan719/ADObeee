//! Structured operations for CLI, UI and future automation clients.
use crate::{ColorToken, CustomThemeId, PaletteDocument, THEME_SCHEMA_VERSION, ThemeDefinition, ThemeKind, ThemeLibrary, ThemePreference};
use crate::{contrast_diagnostics, contrast_warnings};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeState {
    pub schema_version: u32,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default)]
    pub theme_library: ThemeLibrary,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}
impl Default for ThemeState {
    fn default() -> Self {
        Self {
            schema_version: THEME_SCHEMA_VERSION,
            revision: 0,
            theme: ThemePreference::Light,
            theme_library: ThemeLibrary::default(),
            extensions: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ThemeError {
    pub code: String,
    pub message: String,
}
impl ThemeError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_args", message)
    }
    fn missing(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }
    fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported_schema", message)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new("conflict", message)
    }
}

/// Execute a stable operation name with JSON arguments. Mutations apply to a clone
/// and commit only after every field and invariant has passed validation.
impl ThemeState {
    pub fn execute(&mut self, operation: &str, args: &Value) -> Result<Value, ThemeError> {
        let allowed: &[&str] = match operation {
            "theme_list" => &["expected_revision"],
            "theme_inspect" | "theme_export" => &["id", "expected_revision"],
            "theme_create" => &["name", "base_theme_id", "palette", "id", "expected_revision"],
            "theme_clone" => &["id", "name", "new_id", "expected_revision"],
            "theme_update" => &["id", "palette", "expected_revision"],
            "theme_rename" => &["id", "name", "expected_revision"],
            "theme_activate" | "theme_delete" => &["id", "expected_revision"],
            "theme_import" => &["document", "expected_revision"],
            _ => return Err(ThemeError::new("unknown_operation", format!("unknown theme operation: {operation}"))),
        };
        let object = args.as_object().ok_or_else(|| ThemeError::invalid("arguments must be a JSON object"))?;
        if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
            return Err(ThemeError::invalid(format!("unknown argument: {key}")));
        }
        if let Some(expected) = object.get("expected_revision") {
            let expected = expected.as_u64().ok_or_else(|| ThemeError::invalid("expected_revision must be an unsigned integer"))?;
            if expected != self.revision {
                return Err(ThemeError::new("revision_conflict", "theme state changed; inspect again before applying this operation"));
            }
        }
        match operation {
            "theme_list" => self.list(),
            "theme_inspect" => self.inspect(required_str(args, "id")?),
            "theme_create" => self.mutate(|next| next.create(args)),
            "theme_clone" => self.mutate(|next| next.clone_theme(args)),
            "theme_update" => self.mutate(|next| next.update(args)),
            "theme_rename" => self.mutate(|next| next.rename(args)),
            "theme_activate" => self.mutate(|next| next.activate(args)),
            "theme_delete" => self.mutate(|next| next.delete(args)),
            "theme_export" => self.export(required_str(args, "id")?),
            "theme_import" => self.mutate(|next| next.import(args)),
            _ => Err(ThemeError::new("unknown_operation", "unknown theme operation")),
        }
    }

    fn mutate<F>(&mut self, operation: F) -> Result<Value, ThemeError>
    where
        F: FnOnce(&mut Self) -> Result<Value, ThemeError>,
    {
        if self.schema_version != THEME_SCHEMA_VERSION || !self.theme_library.supported() {
            return Err(ThemeError::unsupported("theme state schema is unsupported; data is read-only"));
        }
        let mut next = self.clone();
        let mut output = operation(&mut next)?;
        next.revision = self.revision.checked_add(1).ok_or_else(|| ThemeError::conflict("theme revision overflow"))?;
        if let Some(object) = output.as_object_mut() {
            object.insert("revision".into(), json!(next.revision));
        }
        *self = next;
        Ok(output)
    }

    fn list(&self) -> Result<Value, ThemeError> {
        let mut themes = Vec::new();
        for preference in ThemePreference::ALL {
            let palette = preference.palette(&self.theme_library, None, ThemeKind::Light);
            themes.push(json!({"id":preference.id(),"name":preference.label(),"kind":kind_name(palette.kind),"built_in":true,"active":self.theme==preference,"palette":PaletteDocument::from_palette(&palette)}));
        }
        for theme in self.theme_library.definitions() {
            let palette = theme.resolve();
            themes.push(json!({"id":format!("custom:{}",theme.id),"name":theme.name,"kind":kind_name(palette.kind),"built_in":false,"active":self.theme==ThemePreference::Custom(theme.id),"palette":PaletteDocument::from_palette(&palette),"warnings":contrast_warnings(&palette),"contrast_diagnostics":contrast_diagnostics(&palette)}));
        }
        Ok(json!({"revision":self.revision,"active_theme":self.theme.id(),"themes":themes,"diagnostics":self.theme_library.diagnostics()}))
    }

    fn inspect(&self, id: &str) -> Result<Value, ThemeError> {
        let preference = ThemePreference::parse(id).ok_or_else(|| ThemeError::missing(format!("unknown theme ID: {id}")))?;
        let follows_system = preference == ThemePreference::System;
        let palette = preference.palette(&self.theme_library, None, ThemeKind::Light);
        let mut result = json!({"revision":self.revision,"id":preference.id(),"name":preference.label(),"kind":kind_name(palette.kind),"built_in":true,"active":self.theme==preference,"follows_system":follows_system,"schema_version":THEME_SCHEMA_VERSION,"metadata":{},"base_theme_id":null,"extensions":{},"palette":PaletteDocument::from_palette(&palette),"warnings":contrast_warnings(&palette),"contrast_diagnostics":contrast_diagnostics(&palette)});
        if let ThemePreference::Custom(custom_id) = preference {
            let definition = self.theme_library.get(custom_id).ok_or_else(|| ThemeError::missing(format!("unknown custom theme ID: {custom_id}")))?;
            result["name"] = json!(definition.name);
            result["built_in"] = json!(false);
            result["base_theme_id"] = json!(definition.base_theme_id);
            result["metadata"] = json!(definition.metadata);
            result["extensions"] = json!(definition.extensions);
            result["schema_version"] = json!(definition.schema_version);
            if definition.schema_version == THEME_SCHEMA_VERSION {
                for token in ColorToken::ALL {
                    if let Some(value) = definition.palette.0.get(token.key()).filter(|v| v.as_str().and_then(crate::parse_color).is_some()) {
                        result["palette"][token.key()] = value.clone();
                    }
                }
            }
        }
        Ok(result)
    }

    fn create(&mut self, args: &Value) -> Result<Value, ThemeError> {
        let name = nonempty(required_str(args, "name")?)?.to_owned();
        let base = optional_str(args, "base_theme_id")?.unwrap_or("light");
        let base_preference = ThemePreference::parse(base).ok_or_else(|| ThemeError::missing(format!("unknown base theme ID: {base}")))?;
        let id = parse_or_generate_id(optional_str(args, "id")?)?;
        let (mut definition, kind) = if let ThemePreference::Custom(base_id) = base_preference {
            let source = self.theme_library.get(base_id).ok_or_else(|| ThemeError::missing(format!("unknown base theme ID: {base}")))?;
            ensure_current(&source)?;
            let mut copied = snapshot(source.clone());
            copied.id = id;
            copied.name = name.clone();
            copied.base_theme_id = source.base_theme_id.clone();
            (copied, source.kind)
        } else {
            if base_preference == ThemePreference::System {
                return Err(ThemeError::invalid("System cannot be used as a custom theme base"));
            }
            (
                ThemeDefinition::duplicate_builtin(base_preference, id, name.clone()).map_err(ThemeError::invalid)?,
                base_preference.palette(&self.theme_library, None, ThemeKind::Light).kind,
            )
        };
        definition.id = id;
        definition.name = name;
        definition.kind = kind;
        definition.schema_version = THEME_SCHEMA_VERSION;
        if let Some(patch) = args.get("palette") {
            apply_patch(&mut definition.palette, patch)?;
        }
        validate_known_tokens(&definition.palette)?;
        let id = definition.id;
        self.theme_library.insert(definition.clone()).map_err(map_library_error)?;
        Ok(
            json!({"theme":definition,"id":format!("custom:{id}"),"warnings":contrast_warnings(&definition.resolve()),"contrast_diagnostics":contrast_diagnostics(&definition.resolve())}),
        )
    }

    fn clone_theme(&mut self, args: &Value) -> Result<Value, ThemeError> {
        let source_id = required_str(args, "id")?;
        let source = ThemePreference::parse(source_id).ok_or_else(|| ThemeError::missing(format!("unknown theme ID: {source_id}")))?;
        let name = nonempty(required_str(args, "name")?)?.to_owned();
        let id = parse_or_generate_id(optional_str(args, "new_id")?)?;
        let mut definition = if let ThemePreference::Custom(custom) = source {
            let mut d = self.theme_library.get(custom).ok_or_else(|| ThemeError::missing(format!("unknown custom theme ID: {custom}")))?;
            ensure_current(&d)?;
            d = snapshot(d);
            d.id = id;
            d.name = name;
            d
        } else {
            ThemeDefinition::duplicate_builtin(source, id, name).map_err(ThemeError::invalid)?
        };
        validate_known_tokens(&definition.palette)?;
        definition.schema_version = THEME_SCHEMA_VERSION;
        self.theme_library.insert(definition.clone()).map_err(map_library_error)?;
        Ok(
            json!({"theme":definition,"id":format!("custom:{id}"),"warnings":contrast_warnings(&definition.resolve()),"contrast_diagnostics":contrast_diagnostics(&definition.resolve())}),
        )
    }

    fn update(&mut self, args: &Value) -> Result<Value, ThemeError> {
        let id = custom_id(required_str(args, "id")?)?;
        let mut definition = self.theme_library.get(id).ok_or_else(|| ThemeError::missing(format!("unknown custom theme ID: {id}")))?;
        if definition.schema_version != THEME_SCHEMA_VERSION {
            return Err(ThemeError::unsupported("custom theme schema is read-only"));
        }
        apply_patch(&mut definition.palette, args.get("palette").ok_or_else(|| ThemeError::invalid("missing palette object"))?)?;
        validate_known_tokens(&definition.palette)?;
        self.theme_library.replace(definition.clone()).map_err(map_library_error)?;
        Ok(
            json!({"theme":definition,"warnings":contrast_warnings(&definition.resolve()),"contrast_diagnostics":contrast_diagnostics(&definition.resolve())}),
        )
    }

    fn rename(&mut self, args: &Value) -> Result<Value, ThemeError> {
        let id = custom_id(required_str(args, "id")?)?;
        let name = nonempty(required_str(args, "name")?)?.to_owned();
        self.theme_library.rename(id, name).map_err(map_library_error)?;
        let renamed = self.theme_library.get(id).ok_or_else(|| ThemeError::missing("renamed theme disappeared"))?;
        Ok(json!({"id":format!("custom:{id}"),"name":renamed.name}))
    }

    fn activate(&mut self, args: &Value) -> Result<Value, ThemeError> {
        let id = required_str(args, "id")?;
        let preference = ThemePreference::parse(id).ok_or_else(|| ThemeError::missing(format!("unknown theme ID: {id}")))?;
        if let ThemePreference::Custom(custom) = preference {
            let definition = self.theme_library.get(custom).ok_or_else(|| ThemeError::missing(format!("unknown custom theme ID: {custom}")))?;
            ensure_current(&definition)?;
            validate_known_tokens(&definition.palette)?;
        }
        self.theme = preference;
        Ok(
            json!({"active_theme":preference.id(),"warnings":contrast_warnings(&preference.palette(&self.theme_library, None, ThemeKind::Light)),"contrast_diagnostics":contrast_diagnostics(&preference.palette(&self.theme_library, None, ThemeKind::Light))}),
        )
    }

    fn delete(&mut self, args: &Value) -> Result<Value, ThemeError> {
        let id = custom_id(required_str(args, "id")?)?;
        let definition = self.theme_library.get(id).ok_or_else(|| ThemeError::missing(format!("unknown custom theme ID: {id}")))?;
        if definition.schema_version != THEME_SCHEMA_VERSION {
            return Err(ThemeError::unsupported("custom theme schema is read-only"));
        }
        let was_active = self.theme == ThemePreference::Custom(id);
        self.theme_library.remove(id).map_err(map_library_error)?;
        if was_active {
            self.theme = definition
                .base_theme_id
                .as_deref()
                .and_then(ThemePreference::parse)
                .filter(|base| !matches!(base, ThemePreference::Custom(_)))
                .unwrap_or(ThemePreference::Light);
        }
        Ok(json!({"deleted":format!("custom:{id}"),"active_theme":self.theme.id(),"fallback_applied":was_active}))
    }

    fn export(&self, id: &str) -> Result<Value, ThemeError> {
        let custom = custom_id(id)?;
        let definition = self.theme_library.get(custom).ok_or_else(|| ThemeError::missing(format!("unknown custom theme ID: {custom}")))?;
        if definition.schema_version != THEME_SCHEMA_VERSION {
            return Err(ThemeError::unsupported("custom theme schema cannot be exported by this version"));
        }
        serde_json::to_value(definition).map_err(|e| ThemeError::invalid(e.to_string()))
    }

    fn import(&mut self, args: &Value) -> Result<Value, ThemeError> {
        let document =
            args.get("document").filter(|value| value.is_object()).ok_or_else(|| ThemeError::invalid("document must be a versioned JSON object"))?;
        if document.get("schema_version").and_then(Value::as_u64) != Some(u64::from(THEME_SCHEMA_VERSION)) {
            return Err(ThemeError::unsupported("unsupported import document schema"));
        }
        let raw = document;
        let mut definition: ThemeDefinition =
            serde_json::from_value(raw.clone()).map_err(|e| ThemeError::invalid(format!("invalid theme definition: {e}")))?;
        if definition.schema_version != THEME_SCHEMA_VERSION {
            return Err(ThemeError::unsupported("unsupported theme definition schema"));
        }
        if definition.name.trim().is_empty() {
            return Err(ThemeError::invalid("theme name cannot be empty"));
        }
        validate_known_tokens(&definition.palette)?;
        if let Some(base) = crate::imports::validate_import_base(&definition, &self.theme_library)? {
            for (key, color) in base.0 {
                definition.palette.0.entry(key).or_insert(color);
            }
        }
        let id = definition.id;
        self.theme_library.insert(definition.clone()).map_err(map_library_error)?;
        Ok(
            json!({"id":format!("custom:{id}"),"theme":definition,"warnings":contrast_warnings(&definition.resolve()),"contrast_diagnostics":contrast_diagnostics(&definition.resolve())}),
        )
    }
}

fn ensure_current(theme: &ThemeDefinition) -> Result<(), ThemeError> {
    if theme.schema_version != THEME_SCHEMA_VERSION {
        return Err(ThemeError::unsupported("custom theme schema is read-only"));
    }
    validate_known_tokens(&theme.palette)
}
fn snapshot(mut theme: ThemeDefinition) -> ThemeDefinition {
    let complete = PaletteDocument::from_palette(&theme.resolve());
    // Retain forward-compatible keys while freezing every currently supported role.
    for (key, value) in complete.0 {
        theme.palette.0.entry(key).or_insert(value);
    }
    theme
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ThemeError> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| ThemeError::invalid(format!("{key} must be a string")))
}
fn optional_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, ThemeError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v)),
        _ => Err(ThemeError::invalid(format!("{key} must be a string"))),
    }
}
fn nonempty(s: &str) -> Result<&str, ThemeError> {
    if s.trim().is_empty() { Err(ThemeError::invalid("name cannot be empty")) } else { Ok(s) }
}
fn custom_id(value: &str) -> Result<CustomThemeId, ThemeError> {
    match ThemePreference::parse(value) {
        Some(ThemePreference::Custom(id)) => Ok(id),
        Some(_) => Err(ThemeError::new("builtin_protected", "built-in themes cannot be modified, deleted or exported as custom themes; clone first")),
        None => Err(ThemeError::missing("unknown theme ID; custom IDs use custom:<uuid>")),
    }
}
fn parse_or_generate_id(value: Option<&str>) -> Result<CustomThemeId, ThemeError> {
    if let Some(value) = value {
        return CustomThemeId::parse(value).ok_or_else(|| ThemeError::invalid("id must be a UUID-shaped string"));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| ThemeError::new("entropy_unavailable", format!("cannot generate theme ID: {e}")))?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(CustomThemeId::from_bytes(bytes))
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(ThemeError::invalid("provide id explicitly when creating themes in WASM"))
    }
}
fn apply_patch(palette: &mut PaletteDocument, value: &Value) -> Result<(), ThemeError> {
    let object = value.as_object().ok_or_else(|| ThemeError::invalid("palette must be a JSON object"))?;
    for (key, value) in object {
        let token = ColorToken::ALL
            .iter()
            .copied()
            .find(|token| token.key() == key)
            .ok_or_else(|| ThemeError::invalid(format!("unknown palette token: {key}")))?;
        let text = value
            .as_str()
            .filter(|text| crate::parse_color(text).is_some())
            .ok_or_else(|| ThemeError::invalid(format!("{key} must be #RRGGBB or #RRGGBBAA")))?;
        // Preserve exact input instead of quantizing translucent RGB through Color32.
        palette.0.insert(token.key().into(), json!(text.to_ascii_lowercase()));
    }
    Ok(())
}
fn validate_known_tokens(palette: &PaletteDocument) -> Result<(), ThemeError> {
    let invalid = palette.invalid_tokens();
    if invalid.is_empty() { Ok(()) } else { Err(ThemeError::invalid(format!("invalid color values for: {}", invalid.join(", ")))) }
}
fn map_library_error(value: String) -> ThemeError {
    if value.contains("already exists") || value.contains("duplicate") {
        ThemeError::conflict(value)
    } else if value.contains("does not exist") {
        ThemeError::missing(value)
    } else if value.contains("unsupported") || value.contains("future theme") {
        ThemeError::unsupported(value)
    } else {
        ThemeError::invalid(value)
    }
}
fn kind_name(kind: ThemeKind) -> &'static str {
    match kind {
        ThemeKind::Light => "light",
        ThemeKind::Dark => "dark",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(value: Value) -> Value {
        value
    }
    fn state() -> ThemeState {
        ThemeState::default()
    }
    #[test]
    fn api_create_edit_activate_export_import_delete_is_transactional() {
        let mut s = state();
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let out = s
            .execute(
                "theme_create",
                &args(json!({"name":"Navy and cream","base_theme_id":"paper-ink","id":id,"palette":{"accent":"#24466a","chrome":"#fffaf0"}})),
            )
            .unwrap();
        assert_eq!(out["id"], format!("custom:{id}"));
        assert_eq!(s.revision, 1);
        let custom = format!("custom:{id}");
        let inspect = s.execute("theme_inspect", &json!({"id":custom})).unwrap();
        assert_eq!(inspect["palette"]["accent"], "#24466a");
        s.execute("theme_update", &json!({"id":custom,"palette":{"text":"#222222"}})).unwrap();
        s.execute("theme_rename", &json!({"id":custom,"name":"Navy cream"})).unwrap();
        s.execute("theme_activate", &json!({"id":custom})).unwrap();
        let document = s.execute("theme_export", &json!({"id":custom})).unwrap();
        assert_eq!(document["schema_version"], 1);
        assert!(document.get("theme").is_none());
        let duplicate_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let mut imported = document.clone();
        imported["id"] = json!(duplicate_id);
        s.execute("theme_import", &json!({"document":imported})).unwrap();
        let list = s.execute("theme_list", &json!({})).unwrap();
        assert_eq!(list["themes"].as_array().unwrap().len(), 11);
        s.execute("theme_delete", &json!({"id":custom})).unwrap();
        assert_eq!(s.theme, ThemePreference::PaperInk);
        assert_eq!(s.revision, 6); // create, update, rename, activate, import, delete
    }
    #[test]
    fn clone_and_import_reject_collisions_without_mutating_state() {
        let mut s = state();
        s.execute("theme_clone", &json!({"id":"ocean","name":"Copy","new_id":"cccccccc-cccc-4ccc-8ccc-cccccccccccc"})).unwrap();
        let before = s.clone();
        let error = s.execute("theme_clone", &json!({"id":"dark","name":"Collision","new_id":"cccccccc-cccc-4ccc-8ccc-cccccccccccc"})).unwrap_err();
        assert_eq!(error.code, "conflict");
        assert_eq!(s, before);
    }
    #[test]
    fn invalid_palette_unknown_id_builtin_protection_and_unknown_op_are_structured() {
        let mut s = state();
        for (op, args) in [
            ("theme_create", json!({"name":"Bad","id":"dddddddd-dddd-4ddd-8ddd-dddddddddddd","palette":{"text":"bad"}})),
            ("theme_update", json!({"id":"custom:eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee","palette":{"text":"#ffffff"}})),
            ("theme_delete", json!({"id":"light"})),
            ("theme_activate", json!({"id":"custom:eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"})),
            ("theme_import", json!({"document":{"schema_version":99,"id":"not-a-uuid"}})),
            ("oops", json!({})),
        ] {
            let before = s.clone();
            assert!(s.execute(op, &args).is_err(), "{op}");
            assert_eq!(s, before, "{op} mutation");
        }
        assert_eq!(s.execute("theme_inspect", &json!({"id":"system"})).unwrap()["follows_system"], true);
    }
    #[test]
    fn schema_and_revision_guard_mutations_but_allow_read_only_list() {
        let mut s = state();
        s.schema_version = 2;
        assert!(s.execute("theme_list", &json!({})).is_ok());
        assert_eq!(s.execute("theme_activate", &json!({"id":"dark"})).unwrap_err().code, "unsupported_schema");
    }
    #[test]
    fn all_palette_fields_are_present_and_accessibility_reports_without_adjustment() {
        let palette = ThemePreference::Ocean.palette(&ThemeLibrary::default(), None, ThemeKind::Light);
        let doc = PaletteDocument::from_palette(&palette);
        assert_eq!(doc.0.len(), 31);
        let mut bad = palette;
        bad.text = bad.panel;
        let before = bad;
        let warnings = contrast_warnings(&bad);
        assert!(warnings.iter().any(|w| w.starts_with("text contrast")));
        assert_eq!(bad, before);
    }
}
