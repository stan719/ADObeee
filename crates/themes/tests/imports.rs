use pdfcraft_themes::{THEME_SCHEMA_VERSION, ThemeState};
use serde_json::{Value, json};

const INCOMING: &str = "00000000-0000-4000-8000-000000000010";
const PARENT: &str = "00000000-0000-4000-8000-000000000011";
const REFERRER: &str = "00000000-0000-4000-8000-000000000012";
const INCOMING_ID: &str = "custom:00000000-0000-4000-8000-000000000010";
const PARENT_ID: &str = "custom:00000000-0000-4000-8000-000000000011";

fn import_doc(id: &str, base: &str, palette: Value) -> Value {
    json!({
        "schema_version": THEME_SCHEMA_VERSION,
        "id": id,
        "name": "Imported",
        "kind": "Dark",
        "base_theme_id": base,
        "palette": palette,
        "future_extension": {"kept": true}
    })
}

fn import(state: &mut ThemeState, document: Value) -> Result<Value, pdfcraft_themes::ThemeError> {
    state.execute("theme_import", &json!({"document": document}))
}

fn assert_invalid_base(state: &mut ThemeState, document: Value) {
    let before = state.clone();
    let error = import(state, document).unwrap_err();
    assert_eq!(error.code, "invalid_base_theme");
    assert!(!error.message.is_empty());
    assert_eq!(*state, before, "failed import must not change state");
}

#[test]
fn partial_import_materializes_builtin_base_and_round_trips() {
    let mut state = ThemeState::default();
    let builtin = state.execute("theme_inspect", &json!({"id":"ocean"})).unwrap();
    let document = import_doc(INCOMING, "ocean", json!({"accent":"#ffeedd","future_role":{"kept":true}}));

    import(&mut state, document).unwrap();
    let inspected = state.execute("theme_inspect", &json!({"id":INCOMING_ID})).unwrap();
    assert_eq!(inspected["base_theme_id"], "ocean");
    assert_eq!(inspected["palette"]["accent"], "#ffeedd");
    assert_eq!(inspected["palette"]["text"], builtin["palette"]["text"]);
    assert_eq!(inspected["palette"]["focus"], builtin["palette"]["focus"]);

    let exported = state.execute("theme_export", &json!({"id":INCOMING_ID})).unwrap();
    assert_eq!(exported["future_extension"], json!({"kept":true}));
    assert_eq!(exported["palette"]["future_role"], json!({"kept":true}));
    let mut restored: ThemeState = serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
    assert_eq!(restored.execute("theme_inspect", &json!({"id":INCOMING_ID})).unwrap()["palette"], inspected["palette"]);
}

#[test]
fn partial_import_resolves_existing_custom_base_chain() {
    let mut state = ThemeState::default();
    import(&mut state, import_doc(PARENT, "ocean", json!({"text":"#fefefe"}))).unwrap();
    import(&mut state, import_doc(INCOMING, PARENT_ID, json!({"accent":"#ffccaa"}))).unwrap();

    let child = state.execute("theme_inspect", &json!({"id":INCOMING_ID})).unwrap();
    let parent = state.execute("theme_inspect", &json!({"id":PARENT_ID})).unwrap();
    assert_eq!(child["base_theme_id"], PARENT_ID);
    assert_eq!(child["palette"]["text"], "#fefefe");
    assert_eq!(child["palette"]["pasteboard"], parent["palette"]["pasteboard"]);
    assert_eq!(child["palette"]["accent"], "#ffccaa");
}

#[test]
fn unknown_system_self_and_completed_cycle_bases_fail_atomically() {
    let mut state = ThemeState::default();
    let mut cycle_state: ThemeState = serde_json::from_value(json!({
        "schema_version":1,
        "theme":"Light",
        "theme_library":{"schema_version":1,"themes":[{
            "schema_version":1,"id":REFERRER,"name":"Referrer","kind":"Dark",
            "base_theme_id":INCOMING_ID,"palette":{"text":"#eeeeee"}
        }]}
    }))
    .unwrap();

    assert_invalid_base(&mut state, import_doc(INCOMING, "custom:00000000-0000-4000-8000-000000000099", json!({})));
    assert_invalid_base(&mut state, import_doc(INCOMING, "system", json!({})));
    assert_invalid_base(&mut state, import_doc(INCOMING, INCOMING_ID, json!({})));
    assert_invalid_base(&mut cycle_state, import_doc(INCOMING, "custom:00000000-0000-4000-8000-000000000012", json!({})));
    assert_invalid_base(&mut state, import_doc(INCOMING, "custom:not-a-uuid", json!({})));
}

#[test]
fn unrelated_import_does_not_rewrite_unknown_or_future_stored_records() {
    let raw = json!({
        "schema_version":1,
        "theme":"Light",
        "theme_library":{"schema_version":1,"themes":[
            {"schema_version":1,"id":PARENT,"name":"Dangling","kind":"Dark",
             "base_theme_id":"custom:00000000-0000-4000-8000-000000000098","palette":{"text":"#eeeeee"},"unknown":{"kept":true}},
            {"schema_version":99,"id":REFERRER,"name":"Future","kind":"Dark","palette":{"text":"#ffffff"},"future_data":[1,2]}
        ]}
    });
    let mut state: ThemeState = serde_json::from_value(raw.clone()).unwrap();
    import(&mut state, import_doc(INCOMING, "ocean", json!({}))).unwrap();

    let serialized = serde_json::to_value(state).unwrap();
    assert_eq!(serialized["theme_library"]["themes"][0], raw["theme_library"]["themes"][0]);
    assert_eq!(serialized["theme_library"]["themes"][1], raw["theme_library"]["themes"][1]);
}

#[test]
fn inherited_custom_colors_keep_exact_translucent_input() {
    let mut state = ThemeState::default();
    state.execute("theme_create", &json!({"id":PARENT,"name":"Translucent base","base_theme_id":"ocean","palette":{"accent":"#12345601"}})).unwrap();
    import(&mut state, import_doc(INCOMING, PARENT_ID, json!({}))).unwrap();
    assert_eq!(state.execute("theme_inspect", &json!({"id":INCOMING_ID})).unwrap()["palette"]["accent"], "#12345601");
}
