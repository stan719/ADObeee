//! Native theme transactions. A separate advisory lock survives atomic replacement of
//! the JSON file; locks are released by the OS on close/crash, never removed by us.
use crate::{THEME_SCHEMA_VERSION, ThemeError, ThemeState};
use pdfcraft_platform::staging::{StagingName, create_staging, staging_suffixes};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;

fn error(code: &str, message: impl ToString) -> ThemeError {
    ThemeError { code: code.into(), message: message.to_string() }
}
fn io_error(e: impl ToString) -> ThemeError {
    error("storage_error", e)
}

#[derive(Clone, Debug)]
pub struct ThemeStore {
    path: PathBuf,
}
impl ThemeStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn lock_path(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_os_string();
        name.push(".lock");
        PathBuf::from(name)
    }
    pub fn legacy_path(&self) -> Option<PathBuf> {
        (self.path.file_name() == Some(std::ffi::OsStr::new("themes.json"))).then(|| self.path.with_file_name("app.ron"))
    }
    fn lock(&self) -> Result<File, ThemeError> {
        let path = self.lock_path();
        regular_or_missing(&path)?;
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(io_error)?;
        file.try_lock().map_err(|e| error("store_busy", format!("theme store is in use; retry: {e}")))?;
        Ok(file)
    }
    /// Atomic rename makes a read observe either the old or the new complete snapshot.
    /// Listing/inspection never creates files, directories or locks.
    pub fn load(&self) -> Result<ThemeState, ThemeError> {
        if let Some(bytes) = read_optional(&self.path)? {
            return decode_state(&bytes);
        }
        self.legacy_seed()
    }
    fn legacy_seed(&self) -> Result<ThemeState, ThemeError> {
        let mut state = ThemeState::default();
        let Some(path) = self.legacy_path() else { return Ok(state) };
        let Some(bytes) = read_optional(&path)? else { return Ok(state) };
        let entries: std::collections::HashMap<String, String> = ron::de::from_bytes(&bytes).map_err(|e| error("invalid_legacy_settings", e))?;
        if let Some(json) = entries.get("pdfcraft").or_else(|| entries.get("printcraft")) {
            let value: Value = serde_json::from_str(json).map_err(|e| error("invalid_legacy_settings", e))?;
            if let Some(theme) = value.get("theme") {
                // Match the GUI's historical unknown-preference fallback.
                state.theme = serde_json::from_value(theme.clone()).unwrap_or_default();
            }
            if let Some(library) = value.get("theme_library") {
                state.theme_library = serde_json::from_value(library.clone()).map_err(|e| error("invalid_legacy_settings", e))?;
            }
        }
        Ok(state)
    }
    /// Import a GUI's restored legacy settings only if no dedicated store exists.
    pub fn initialize(&self, seed: ThemeState) -> Result<ThemeState, ThemeError> {
        let _lock = self.lock()?;
        if regular_or_missing(&self.path)? {
            return self.load();
        }
        if seed.schema_version != THEME_SCHEMA_VERSION {
            return Err(error("unsupported_schema", "unsupported seed version"));
        }
        self.write(&seed)?;
        Ok(seed)
    }
    pub fn execute(&self, tool: &str, args: &Value) -> Result<Value, ThemeError> {
        self.execute_with_state(tool, args).map(|(result, _)| result)
    }
    /// Return the committed snapshot with the result, avoiding a second full parse.
    pub fn execute_with_state(&self, tool: &str, args: &Value) -> Result<(Value, ThemeState), ThemeError> {
        let read_only = matches!(tool, "theme_list" | "theme_inspect" | "theme_export");
        let _lock = if read_only { None } else { Some(self.lock()?) };
        let mut state = self.load()?;
        let mut args = args.clone();
        if let Some(expected) = args.as_object_mut().and_then(|a| a.remove("expected_revision")) {
            let expected = expected.as_u64().ok_or_else(|| error("invalid_args", "expected_revision must be an unsigned integer"))?;
            if expected != state.revision {
                return Err(error("revision_conflict", "theme store changed; inspect again before applying this edit"));
            }
        }
        let result = state.execute(tool, &args)?;
        if !read_only {
            self.write(&state)?;
        }
        Ok((result, state))
    }
    fn write(&self, state: &ThemeState) -> Result<(), ThemeError> {
        regular_or_missing(&self.path)?;
        let bytes = serde_json::to_vec_pretty(state).map_err(io_error)?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err(error("storage_limit", "theme store exceeds the 64 MiB safety limit"));
        }
        let parent = self.path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        let name = self.path.file_name().and_then(|s| s.to_str()).ok_or_else(|| error("invalid_path", "store needs a UTF-8 file name"))?;
        let (temp, mut file) = create_staging(parent, name, StagingName::SuffixThenTag, staging_suffixes()).map_err(io_error)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(io_error)
    }
}

pub(crate) fn decode_state(bytes: &[u8]) -> Result<ThemeState, ThemeError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| error("invalid_store", e))?;
    if value.get("theme").is_none() || value.get("theme_library").is_none() {
        return Err(error(
            "invalid_store",
            "expected a theme store with theme and theme_library; import standalone theme definitions using theme_import",
        ));
    }
    let state: ThemeState = serde_json::from_value(value).map_err(|e| error("invalid_store", e))?;
    if state.schema_version != THEME_SCHEMA_VERSION {
        return Err(error("unsupported_schema", "theme store version is unsupported; file left unchanged"));
    }
    Ok(state)
}

fn regular_or_missing(path: &Path) -> Result<bool, ThemeError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(true),
        Ok(_) => Err(error("invalid_path", "theme store, legacy settings and lock must be regular files, never symlinks or directories")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(e)),
    }
}
pub(crate) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ThemeError> {
    if !regular_or_missing(path)? {
        return Ok(None);
    }
    let file = File::open(path).map_err(io_error)?;
    let mut bytes = Vec::new();
    file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes).map_err(io_error)?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(error("storage_limit", "theme data exceeds the 64 MiB safety limit"));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn store(name: &str) -> ThemeStore {
        let dir = std::env::temp_dir().join(format!("pdfcraft-themes-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        ThemeStore::new(dir.join("themes.json"))
    }
    #[test]
    fn read_only_and_failed_edits_do_not_write() {
        let store = store("failures");
        store.execute("theme_list", &json!({})).unwrap();
        assert!(!store.path.exists());
        assert!(!store.lock_path().exists());
        store.execute("theme_activate", &json!({"id":"dark"})).unwrap();
        let original = fs::read(&store.path).unwrap();
        assert!(store.execute("theme_create", &json!({"name":"Bad", "palette":{"text":"bogus"}})).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), original);
        let standalone =
            serde_json::json!({"schema_version":1,"id":"00000000-0000-0000-0000-000000000001","name":"Export","kind":"Light"}).to_string();
        fs::write(&store.path, &standalone).unwrap();
        assert_eq!(store.execute("theme_activate", &json!({"id":"light"})).unwrap_err().code, "invalid_store");
        assert_eq!(fs::read_to_string(&store.path).unwrap(), standalone);
        fs::write(&store.path, b"{broken").unwrap();
        assert!(store.execute("theme_activate", &json!({"id":"light"})).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), b"{broken");
    }
    #[test]
    fn locks_and_revisions_prevent_lost_updates() {
        let store = store("revisions");
        store.initialize(ThemeState::default()).unwrap();
        let lock = store.lock().unwrap();
        assert_eq!(store.execute("theme_activate", &json!({"id":"dark"})).unwrap_err().code, "store_busy");
        drop(lock);
        let revision = store.load().unwrap().revision;
        store.execute("theme_activate", &json!({"id":"dark", "expected_revision":revision})).unwrap();
        assert_eq!(store.execute("theme_activate", &json!({"id":"light", "expected_revision":revision})).unwrap_err().code, "revision_conflict");
        assert_eq!(store.load().unwrap().theme, crate::ThemePreference::Dark);
        let mut handles = vec![];
        for n in 1..=8 {
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                let args = json!({"id":format!("00000000-0000-0000-0000-{n:012x}"),"name":"Duplicate names allowed"});
                for _ in 0..100 {
                    match store.execute("theme_create", &args) {
                        Ok(_) => return,
                        Err(e) if e.code == "store_busy" => std::thread::sleep(std::time::Duration::from_millis(2)),
                        Err(e) => panic!("{e}"),
                    }
                }
                panic!("store stayed busy");
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.load().unwrap().theme_library.definitions().count(), 8);
    }
    #[test]
    fn legacy_migration_is_one_time_and_retains_selection() {
        let store = store("legacy");
        let settings = serde_json::json!({"theme":"System", "unrelated":"keep"}).to_string();
        let ron = ron::to_string(&std::collections::HashMap::from([("pdfcraft", settings)])).unwrap();
        fs::write(store.legacy_path().unwrap(), &ron).unwrap();
        assert_eq!(store.load().unwrap().theme, crate::ThemePreference::System);
        store.execute("theme_create", &json!({"name":"New theme"})).unwrap();
        assert_eq!(store.load().unwrap().theme, crate::ThemePreference::System);
        assert_eq!(fs::read_to_string(store.legacy_path().unwrap()).unwrap(), ron);
        store.execute("theme_activate", &json!({"id":"ocean"})).unwrap();
        assert_eq!(store.initialize(ThemeState::default()).unwrap().theme, crate::ThemePreference::Ocean);
    }
    #[test]
    fn future_store_and_failed_replace_stay_intact() {
        let store = store("future");
        let mut value = serde_json::to_value(ThemeState::default()).unwrap();
        value["schema_version"] = json!(99);
        fs::write(&store.path, value.to_string()).unwrap();
        let old = fs::read(&store.path).unwrap();
        assert!(store.execute("theme_activate", &json!({"id":"light"})).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), old);
        fs::remove_file(&store.path).unwrap();
        fs::create_dir(&store.path).unwrap();
        assert!(store.execute("theme_activate", &json!({"id":"light"})).is_err());
        assert!(store.path.is_dir());
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let store = store("symlinks");
        let target = store.path.with_file_name("keep");
        fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, store.lock_path()).unwrap();
        assert!(store.execute("theme_activate", &json!({"id":"light"})).is_err());
        fs::remove_file(store.lock_path()).unwrap();
        std::os::unix::fs::symlink(&target, &store.path).unwrap();
        assert!(store.load().is_err());
        assert_eq!(fs::read(target).unwrap(), b"keep");
    }
}
