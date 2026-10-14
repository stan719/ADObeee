use pdfcraft_themes::{ActivePaletteCache, CustomThemeId, PaletteDocument, ThemeDefinition, ThemeKind, ThemeLibrary, ThemePreference};
use serde_json::json;
use std::time::Instant;
fn large_library() -> (ThemeLibrary, CustomThemeId) {
    let base = ThemeDefinition::duplicate_builtin(ThemePreference::Ocean, CustomThemeId::from_bytes([0; 16]), "Template".into()).unwrap();
    let entries: Vec<_> = (0_u128..10_000)
        .map(|number| {
            let mut definition = base.clone();
            definition.id = CustomThemeId::from_bytes(number.to_be_bytes());
            definition.name = format!("Theme {number}");
            serde_json::to_value(definition).unwrap()
        })
        .collect();
    (serde_json::from_value(json!({"schema_version":1,"themes":entries})).unwrap(), CustomThemeId::from_bytes(9999_u128.to_be_bytes()))
}
#[test]
fn ten_thousand_themes_reuse_the_index_and_active_palette() {
    let (mut library, id) = large_library();
    let started = Instant::now();
    let first_choices = library.choices();
    let mut cache = ActivePaletteCache::default();
    let (palette, miss) = cache.resolve(ThemePreference::Custom(id), &library, None, ThemeKind::Light);
    assert!(miss);
    let warm = started.elapsed();
    let started = Instant::now();
    for _ in 0..60 {
        assert!(std::sync::Arc::ptr_eq(&first_choices, &library.choices()));
        assert_eq!(cache.resolve(ThemePreference::Custom(id), &library, Some(ThemeKind::Dark), ThemeKind::Light), (palette, false));
        assert_eq!(library.name(id), Some("Theme 9999"));
    }
    eprintln!("10,000 complete themes: index + first resolution {warm:?}; 60 cached resolutions {:?}", started.elapsed());
    let before = library.clone();
    let mut edited = library.get(id).unwrap();
    edited.palette.0.insert("text".into(), json!("#fefefe"));
    library.replace(edited).unwrap();
    assert!(!library.same_snapshot(&before));
    let (edited, miss) = cache.resolve(ThemePreference::Custom(id), &library, None, ThemeKind::Light);
    assert!(miss);
    assert_eq!(edited.text, ecolor::Color32::from_rgb(254, 254, 254));
    assert_eq!(before.get(id).unwrap().resolve(), palette);
    let wire = serde_json::to_value(&library).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 2, "derived cache fields never enter the wire schema");
    assert_eq!(PaletteDocument::from_palette(&edited).0.len(), 31);
}
#[test]
fn cache_tracks_selection_library_and_system_but_ignores_irrelevant_os_changes() {
    let library = ThemeLibrary::default();
    let mut cache = ActivePaletteCache::default();
    assert!(cache.resolve(ThemePreference::System, &library, Some(ThemeKind::Light), ThemeKind::Light).1);
    assert!(!cache.resolve(ThemePreference::System, &library, Some(ThemeKind::Light), ThemeKind::Light).1);
    let (dark, miss) = cache.resolve(ThemePreference::System, &library, Some(ThemeKind::Dark), ThemeKind::Light);
    assert!(miss);
    assert_eq!(dark.kind, ThemeKind::Dark);
    let (unknown, miss) = cache.resolve(ThemePreference::System, &library, None, ThemeKind::Light);
    assert!(miss);
    assert_eq!(unknown.kind, ThemeKind::Light);
    assert!(cache.resolve(ThemePreference::Ocean, &library, None, ThemeKind::Light).1);
    assert!(!cache.resolve(ThemePreference::Ocean, &library, Some(ThemeKind::Dark), ThemeKind::Light).1);
    let restored: ThemeLibrary = serde_json::from_value(serde_json::to_value(library).unwrap()).unwrap();
    assert!(cache.resolve(ThemePreference::Ocean, &restored, None, ThemeKind::Light).1);
}
