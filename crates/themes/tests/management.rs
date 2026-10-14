use pdfcraft_themes::{ColorToken, ThemeState};
use serde_json::json;
const ID: &str = "00000000-0000-4000-8000-000000000001";
const CUSTOM: &str = "custom:00000000-0000-4000-8000-000000000001";

#[test]
fn every_token_is_editable_and_bad_patches_are_atomic() {
    let mut state = ThemeState::default();
    state.execute("theme_create", &json!({"id":ID,"name":"Editable"})).unwrap();
    for token in ColorToken::ALL {
        state.execute("theme_update", &json!({"id":CUSTOM,"palette":{token.key():"#123456"}})).unwrap();
        let view = state.execute("theme_inspect", &json!({"id":CUSTOM})).unwrap();
        assert_eq!(view["palette"][token.key()], "#123456", "{}", token.key());
    }
    state.execute("theme_update", &json!({"id":CUSTOM,"palette":{"window_shadow":"#12345678"}})).unwrap();
    assert_eq!(state.execute("theme_inspect", &json!({"id":CUSTOM})).unwrap()["palette"]["window_shadow"], "#12345678");
    assert_eq!(state.execute("theme_export", &json!({"id":CUSTOM})).unwrap()["palette"]["window_shadow"], "#12345678");
    let before = state.clone();
    for patch in [json!({"accent":"#ffffff","text":"#invalid"}), json!({"typo":"#ffffff"}), json!({"text":null}), json!({"text":"#12"})] {
        assert!(state.execute("theme_update", &json!({"id":CUSTOM,"palette":patch})).is_err());
        assert_eq!(state, before);
    }
    assert!(state.execute("theme_rename", &json!({"id":CUSTOM,"name":"Valid","typo":1})).is_err());
    assert!(state.execute("theme_update", &json!({"id":CUSTOM,"palette":{},"expected_revision":0})).is_err());
    assert_eq!(state, before);
}

#[test]
fn partial_import_clone_and_export_preserve_extensions() {
    let mut state = ThemeState::default();
    let doc = json!({"schema_version":1,"id":ID,"name":"Partial", "kind":"Dark","base_theme_id":"ocean",
        "palette":{"accent":"#ffeedd","future_role":{"value":"keep"}},"metadata":{"author":"Example"},"future_extension":[1,2]});
    state.execute("theme_import", &json!({"document":doc})).unwrap();
    let exported = state.execute("theme_export", &json!({"id":CUSTOM})).unwrap();
    assert_eq!(exported["future_extension"], json!([1, 2]));
    assert_eq!(exported["metadata"]["author"], "Example");
    let copy = state.execute("theme_clone", &json!({"id":CUSTOM,"name":"Partial"})).unwrap();
    assert_eq!(copy["theme"]["palette"].as_object().unwrap().len(), 32);
    assert_eq!(copy["theme"]["palette"]["future_role"], doc["palette"]["future_role"]);
    assert_eq!(copy["theme"]["base_theme_id"], "ocean");
    let before = state.clone();
    assert!(state.execute("theme_import", &json!({"document":exported})).is_err());
    assert_eq!(state, before);
    let serialized = serde_json::to_string(&state).unwrap();
    let mut restored: ThemeState = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored.execute("theme_inspect", &json!({"id":CUSTOM})).unwrap()["id"], CUSTOM);
}

#[test]
fn future_custom_records_are_not_reinterpreted_or_mutated() {
    let raw = json!({"schema_version":1,"theme":"Light","theme_library":{"schema_version":1,"themes":[
        {"schema_version":99,"id":ID,"name":"Future","kind":"Dark","palette":{"text":"#eeeeee"},"future":{"a":1}}
    ]}});
    let mut state: ThemeState = serde_json::from_value(raw).unwrap();
    for (op, args) in [
        ("theme_clone", json!({"id":CUSTOM,"name":"Copy"})),
        ("theme_create", json!({"name":"Copy","base_theme_id":CUSTOM})),
        ("theme_activate", json!({"id":CUSTOM})),
        ("theme_update", json!({"id":CUSTOM,"palette":{"text":"#ffffff"}})),
        ("theme_delete", json!({"id":CUSTOM})),
    ] {
        let before = state.clone();
        assert_eq!(state.execute(op, &args).unwrap_err().code, "unsupported_schema");
        assert_eq!(state, before);
    }
    assert_eq!(state.execute("theme_inspect", &json!({"id":CUSTOM})).unwrap()["schema_version"], 99);
}

#[test]
fn renamed_presets_keep_ids_and_imported_base_palettes() {
    use pdfcraft_themes::ThemePreference;
    for (id, label) in [
        ("catppuccin", "Atelier"),
        ("solarized", "Editorial"),
        ("forest", "Heritage"),
        ("ocean", "Executive"),
        ("sunset", "Sienna"),
        ("paper-ink", "Paper & Ink"),
    ] {
        let preference: ThemePreference = serde_json::from_value(json!(id)).unwrap();
        assert_eq!(preference.id(), id);
        assert_eq!(preference.label(), label);
        assert_eq!(serde_json::to_value(preference).unwrap(), json!(id));
        let mut state = ThemeState::default();
        let builtin = state.execute("theme_inspect", &json!({"id":id})).unwrap();
        assert_eq!(builtin["name"], label);
        // Import an old partial definition: absent tokens still resolve via the old base ID.
        let document = json!({"schema_version":1,"id":ID,"name":"My original name",
            "kind":if id == "paper-ink" { "Light" } else { "Dark" },"base_theme_id":id,"palette":{}});
        state.execute("theme_import", &json!({"document":document})).unwrap();
        let serialized = serde_json::to_string(&state).unwrap();
        let mut restored: ThemeState = serde_json::from_str(&serialized).unwrap();
        let custom = restored.execute("theme_inspect", &json!({"id":CUSTOM})).unwrap();
        assert_eq!(custom["name"], "My original name");
        assert_eq!(custom["base_theme_id"], id);
        assert_eq!(custom["palette"], builtin["palette"]);
        assert_eq!(restored.execute("theme_export", &json!({"id":CUSTOM})).unwrap()["base_theme_id"], id);
        restored.execute("theme_activate", &json!({"id":CUSTOM})).unwrap();
        restored.execute("theme_delete", &json!({"id":CUSTOM})).unwrap();
        assert_eq!(restored.theme, preference);
    }
}
