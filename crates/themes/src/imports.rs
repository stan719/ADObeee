//! Validation and resolution for imported theme base references.
//!
//! This module is intentionally only used by the import path. Existing stored
//! definitions are left as-is so unknown and future data remains forward
//! compatible until a user explicitly imports a document that depends on it.
use crate::{ColorToken, CustomThemeId, Palette, PaletteDocument, THEME_SCHEMA_VERSION, ThemeDefinition, ThemeLibrary, ThemePreference};
use std::collections::BTreeSet;

/// Validate and resolve the base declared by an incoming custom theme.
///
/// The returned palette is the fully resolved base for the incoming document;
/// callers can materialize its missing tokens while preserving the declared
/// `base_theme_id` and any unknown fields. `None` means the document has no
/// declared base and should retain the normal kind-specific defaults.
pub(crate) fn validate_import_base(definition: &ThemeDefinition, library: &ThemeLibrary) -> Result<Option<PaletteDocument>, crate::ThemeError> {
    let Some(base_id) = definition.base_theme_id.as_deref() else { return Ok(None) };

    let invalid = |reason: String| crate::ThemeError { code: "invalid_base_theme".into(), message: reason };
    let preference = ThemePreference::parse(base_id).ok_or_else(|| invalid(format!("unknown or malformed base theme ID: {base_id}")))?;
    match preference {
        ThemePreference::System => Err(invalid("System cannot be used as a custom theme base".into())),
        ThemePreference::Custom(id) => resolve_custom_base(definition, id, library).map(Some),
        builtin => Ok(Some(PaletteDocument::from_palette(&builtin.palette(library, None, definition.kind)))),
    }
}

/// Follow the single-base chain iteratively. The visited set detects both
/// cycles among stored records and cycles completed by the incoming record,
/// without imposing a theme-count limit.
fn resolve_custom_base(incoming: &ThemeDefinition, first_id: CustomThemeId, library: &ThemeLibrary) -> Result<PaletteDocument, crate::ThemeError> {
    let invalid = |reason: String| crate::ThemeError { code: "invalid_base_theme".into(), message: reason };
    let mut visited = BTreeSet::from([incoming.id]);
    let mut chain = Vec::new();
    let mut next_id = first_id;

    let mut palette = loop {
        if !visited.insert(next_id) {
            return Err(invalid(format!("custom theme base cycle includes {next_id}")));
        }
        if library.id_count(next_id) > 1 {
            return Err(invalid(format!("custom theme base ID is ambiguous because it is duplicated: custom:{next_id}")));
        }
        let theme = library.get(next_id).ok_or_else(|| invalid(format!("custom theme base does not exist or is malformed: custom:{next_id}")))?;
        if theme.schema_version != THEME_SCHEMA_VERSION {
            return Err(invalid(format!("custom theme base custom:{next_id} uses unsupported schema {}", theme.schema_version)));
        }

        let Some(base_id) = theme.base_theme_id.as_deref() else {
            chain.push(theme.clone());
            break PaletteDocument::from_palette(&Palette::for_kind(theme.kind));
        };
        let preference =
            ThemePreference::parse(base_id).ok_or_else(|| invalid(format!("unknown or malformed base theme ID in custom:{next_id}: {base_id}")))?;

        match preference {
            ThemePreference::System => return Err(invalid(format!("System cannot be used as a base for custom:{next_id}"))),
            ThemePreference::Custom(parent_id) => {
                chain.push(theme);
                next_id = parent_id;
            }
            builtin => {
                chain.push(theme.clone());
                break PaletteDocument::from_palette(&builtin.palette(library, None, theme.kind));
            }
        }
    };

    // Apply the oldest stored ancestor first, then each child toward the
    // incoming document. This resolves partial palettes through the chain.
    for theme in chain.into_iter().rev() {
        if !theme.palette.invalid_tokens().is_empty() {
            return Err(invalid(format!("custom theme base custom:{} has invalid color tokens", theme.id)));
        }
        for token in ColorToken::ALL {
            if let Some(value) = theme.palette.0.get(token.key()) {
                palette.0.insert(token.key().into(), value.clone());
            }
        }
    }
    Ok(palette)
}
