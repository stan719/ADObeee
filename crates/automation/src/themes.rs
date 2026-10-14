//! Thin adapter from the shared theme-management API to CLI/MCP tools.
#[cfg(not(target_arch = "wasm32"))]
use pdfcraft_themes::ThemeStore;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use crate::{Automation, Result, ToolError};

impl Automation {
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn theme_call(&self, _tool: &str, _args: &Value) -> Result<Value> {
        Err(ToolError::Theme {
            code: "unsupported_platform".into(),
            message: "Filesystem theme tools require a native host; browser editors use ThemeState directly".into(),
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn theme_call(&self, tool: &str, args: &Value) -> Result<Value> {
        let store_arg = args
            .get("store")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Theme { code: "invalid_args".into(), message: "store must be a file path".into() })?;
        let path = self.resolve(store_arg, true).map_err(|e| ToolError::Theme { code: "invalid_path".into(), message: e.to_string() })?;
        let store = ThemeStore::new(&path);

        // `ThemeStore` also reads its `.lock` and, for the conventional themes.json location,
        // may migrate app.ron. Resolve each sibling through the same root boundary before access.
        let lock = store.lock_path();
        self.resolve(&lock.to_string_lossy(), true).map_err(|e| ToolError::Theme { code: "invalid_path".into(), message: e.to_string() })?;
        if let Some(legacy) = store.legacy_path()
            && std::fs::symlink_metadata(&legacy).is_ok()
        {
            self.resolve(&legacy.to_string_lossy(), false).map_err(|e| ToolError::Theme { code: "invalid_path".into(), message: e.to_string() })?;
        }

        let mut operation = args.clone();
        if let Some(object) = operation.as_object_mut() {
            object.remove("store");
        }
        store.execute(tool, &operation).map_err(|e| ToolError::Theme { code: e.code, message: e.message })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Automation, Content};

    #[test]
    fn theme_store_path_and_lock_are_confined() {
        let base = std::env::temp_dir().join(format!("pdfcraft-theme-tool-{}", std::process::id()));
        let root = base.join("root");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&root).unwrap();
        let mut automation = Automation::new().with_root(&root).unwrap();
        let outside = base.join("outside.json");
        let result = automation.call("theme_list", &json!({ "store": outside }));
        assert!(matches!(result, Err(ToolError::Theme { code, .. }) if code == "invalid_path"));
        let result = automation.call("theme_list", &json!({ "store": "themes.json" }));
        assert!(matches!(result, Ok(content) if matches!(content.first(), Some(Content::Json(_)))));
        let _ = std::fs::remove_dir_all(base);
    }
}
