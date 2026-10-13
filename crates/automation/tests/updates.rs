use pdfcraft_automation::{Automation, Content, ToolError};
use serde_json::{Value, json};

fn release(tag: &str, draft: bool, prerelease: bool, names: &[&str]) -> String {
    let assets: Vec<Value> = names
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "size": 2048,
                "browser_download_url": format!("https://github.com/storytold/pdfcraft/releases/download/{tag}/{name}"),
                "digest": null
            })
        })
        .collect();
    json!({
        "tag_name": tag,
        "html_url": format!("https://github.com/storytold/pdfcraft/releases/tag/{tag}"),
        "body": "Release notes",
        "draft": draft,
        "prerelease": prerelease,
        "assets": assets
    })
    .to_string()
}

fn call(auto: &mut Automation, metadata: &str, installed: &str, platform: &str, arch: &str, installation: &str) -> Result<Value, ToolError> {
    auto.call(
        "update_release_info",
        &json!({
            "metadata_json": metadata,
            "installed_version": installed,
            "platform": platform,
            "architecture": arch,
            "installation": installation
        }),
    )
    .map(|mut content| match content.remove(0) {
        Content::Json(value) => value,
        _ => Value::Null,
    })
}

#[test]
fn selects_exact_package_and_semver_upgrade_without_io() {
    let mut auto = Automation::new();
    let metadata = release("v0.7.2", false, false, &["pdfcraft-0.7.2-windows-x64.msi"]);
    let result = call(&mut auto, &metadata, "0.7.1", "windows", "x64", "windows_msi").unwrap();
    assert_eq!(result["version"], "v0.7.2");
    assert_eq!(result["newer"], true);
    assert_eq!(result["selection"], "package");
    assert_eq!(result["package"]["name"], "pdfcraft-0.7.2-windows-x64.msi");
    assert_eq!(result["package"]["size"], 2048);
    assert_eq!(result["notes"], "Release notes");
}

#[test]
fn managed_unknown_and_non_stable_results_are_explicit() {
    let mut auto = Automation::new();
    let stable = release("v0.7.2", false, false, &[]);
    let managed = call(&mut auto, &stable, "0.7.1", "linux", "x64", "deb").unwrap();
    assert_eq!(managed["selection"], "managed");
    assert!(managed["package"].is_null());
    let unknown = call(&mut auto, &stable, "0.7.1", "linux", "unknown", "appimage").unwrap();
    assert_eq!(unknown["selection"], "unknown");

    let prerelease = release("v0.8.0-rc.1", false, true, &[]);
    let unstable = call(&mut auto, &prerelease, "0.7.1", "linux", "x64", "appimage").unwrap();
    assert_eq!(unstable["selection"], "non_stable");
    assert_eq!(unstable["newer"], false);
    assert_eq!(unstable["prerelease"], true);
}

#[test]
fn malformed_metadata_and_unknown_context_values_are_rejected() {
    let mut auto = Automation::new();
    for metadata in ["<html>", r#"{"message":"Not Found"}"#] {
        assert!(matches!(call(&mut auto, metadata, "0.7.1", "linux", "x64", "deb"), Err(ToolError::InvalidArgs(_))));
    }
    let stable = release("v0.7.2", false, false, &[]);
    assert!(matches!(call(&mut auto, &stable, "0.7.1", "linux", "mips", "deb"), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(call(&mut auto, &stable, "0.7.1", "linux", "x64", "archive"), Err(ToolError::InvalidArgs(_))));

    assert!(matches!(call(&mut auto, &stable, "0.7.1", &"x".repeat(1000), "x64", "deb"), Err(ToolError::InvalidArgs(_))));

    let tool = pdfcraft_automation::tools().into_iter().find(|tool| tool.name == "update_release_info").unwrap();
    assert!(tool.read_only);
    assert!(!tool.destructive);
    assert_eq!(tool.command, None);
}
