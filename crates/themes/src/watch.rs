//! Worker-side change detection. Metadata is a fast hint, never the sole source of truth.
//! Periodic byte verification catches atomic replacements and timestamp-preserving edits.
use crate::{ThemeError, ThemeState, ThemeStore};
use std::{
    fs,
    time::{Duration, Instant, SystemTime},
};
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
fn stamp(store: &ThemeStore) -> Result<Option<Stamp>, ThemeError> {
    match fs::symlink_metadata(store.path()) {
        Ok(meta) if meta.is_file() => Ok(Some(Stamp {
            length: meta.len(),
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
            },
        })),
        Ok(_) => Err(ThemeError { code: "invalid_path".into(), message: "theme store must be a regular file, never a symlink or directory".into() }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ThemeError { code: "storage_error".into(), message: error.to_string() }),
    }
}
/// No arbitrary library count limit; reads retain the store's existing 64 MiB bound.
pub struct ThemeWatcher {
    store: ThemeStore,
    previous_stamp: Option<Stamp>,
    bytes: Option<Vec<u8>>,
    verified: Option<Instant>,
    #[cfg(test)]
    parses: usize,
}
impl ThemeWatcher {
    pub fn new(store: ThemeStore) -> Self {
        Self {
            store,
            previous_stamp: None,
            bytes: None,
            verified: None,
            #[cfg(test)]
            parses: 0,
        }
    }
    /// `verify` explicitly forces content verification (also useful in deterministic tests).
    /// GUI workers verify at least once every ten seconds even when all metadata matches.
    pub fn poll(&mut self, verify: bool) -> Result<Option<ThemeState>, ThemeError> {
        let current = stamp(&self.store)?;
        let verify = verify || self.verified.is_none_or(|at| at.elapsed() >= Duration::from_secs(10));
        if !verify && current == self.previous_stamp {
            return Ok(None);
        }
        let bytes = crate::store::read_optional(self.store.path())?;
        // A replacement during the read is retried on the next poll, never tagged as current.
        if stamp(&self.store)? != current {
            return Ok(None);
        }
        if bytes == self.bytes && self.verified.is_some() {
            self.previous_stamp = current;
            self.verified = Some(Instant::now());
            return Ok(None);
        }
        let decoded = match &bytes {
            Some(bytes) => {
                #[cfg(test)]
                {
                    self.parses += 1;
                }
                crate::store::decode_state(bytes)
            }
            None if self.verified.is_some() => return Ok(None),
            None => self.store.load(),
        };
        if stamp(&self.store)? != current {
            return Ok(None);
        }
        let state = match decoded {
            Ok(state) => {
                if let crate::ThemePreference::Custom(id) = state.theme
                    && let Some(definition) = state.theme_library.get(id)
                    && definition.schema_version == crate::THEME_SCHEMA_VERSION
                    && !definition.palette.invalid_tokens().is_empty()
                {
                    self.previous_stamp = current;
                    self.bytes = bytes;
                    self.verified = Some(Instant::now());
                    return Err(ThemeError {
                        code: "invalid_store".into(),
                        message: format!("selected theme {id} has invalid color tokens; keeping the last accepted palette"),
                    });
                }
                state
            }
            Err(error) => {
                // Remember rejected contents too: unchanged corrupt files must not cause parse loops.
                self.previous_stamp = current;
                self.bytes = bytes;
                self.verified = Some(Instant::now());
                return Err(error);
            }
        };
        // Build selector/ID metadata off the UI thread as part of the accepted snapshot.
        let _ = state.theme_library.choices();
        if stamp(&self.store)? != current {
            return Ok(None);
        }
        self.previous_stamp = current;
        self.bytes = bytes;
        self.verified = Some(Instant::now());
        Ok(Some(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CustomThemeId, ThemeDefinition, ThemePreference};
    fn fixture(label: &str) -> (std::path::PathBuf, ThemeStore) {
        let dir = std::env::temp_dir().join(format!("pdfcraft-watch-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let store = ThemeStore::new(dir.join("themes.json"));
        (dir, store)
    }
    #[test]
    fn unchanged_ten_thousand_theme_store_is_not_reparsed() {
        let (dir, store) = fixture("large");
        let mut definition =
            ThemeDefinition::duplicate_builtin(ThemePreference::Ocean, CustomThemeId::from_bytes([0; 16]), "Template".into()).unwrap();
        let entries: Vec<_> = (0_u128..10_000)
            .map(|n| {
                definition.id = CustomThemeId::from_bytes(n.to_be_bytes());
                serde_json::to_value(&definition).unwrap()
            })
            .collect();
        let raw = serde_json::json!({"schema_version":1,"theme":"Light","revision":0,"theme_library":{"schema_version":1,"themes":entries}});
        fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
        let mut watcher = ThemeWatcher::new(store.clone());
        let start = Instant::now();
        assert!(watcher.poll(true).unwrap().is_some());
        let first = start.elapsed();
        for _ in 0..60 {
            assert!(watcher.poll(false).unwrap().is_none());
        }
        for _ in 0..3 {
            assert!(watcher.poll(true).unwrap().is_none());
        }
        assert_eq!(watcher.parses, 1);
        eprintln!("10,000-theme store: initial decode/index {first:?}; total parses after 63 unchanged polls {}", watcher.parses);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn atomic_replacements_invalid_writes_and_coarse_metadata_are_safe() {
        let (dir, store) = fixture("changes");
        store.initialize(ThemeState::default()).unwrap();
        let mut watcher = ThemeWatcher::new(store.clone());
        let initial = watcher.poll(true).unwrap().unwrap();
        store.execute("theme_activate", &serde_json::json!({"id":"ocean"})).unwrap();
        let changed = watcher.poll(false).unwrap().or_else(|| watcher.poll(true).unwrap()).unwrap();
        assert_eq!(changed.theme, ThemePreference::Ocean);
        // Force the metadata fast path to model coarse timestamps or preserved metadata.
        let mut raw = serde_json::to_value(&changed).unwrap();
        raw["theme"] = serde_json::json!("forest");
        fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
        watcher.previous_stamp = stamp(&store).unwrap();
        assert!(watcher.poll(false).unwrap().is_none());
        let verified = watcher.poll(true).unwrap().unwrap();
        assert_eq!(verified.theme, ThemePreference::Forest);
        fs::write(store.path(), "{incomplete").unwrap();
        assert_eq!(watcher.poll(true).unwrap_err().code, "invalid_store");
        assert_eq!(verified.theme, ThemePreference::Forest, "last accepted snapshot remains untouched");
        let rejected_parses = watcher.parses;
        assert!(watcher.poll(false).unwrap().is_none());
        assert!(watcher.poll(true).unwrap().is_none());
        assert_eq!(watcher.parses, rejected_parses, "unchanged invalid contents are not parsed again");
        fs::write(store.path(), serde_json::to_vec(&initial).unwrap()).unwrap();
        assert_eq!(watcher.poll(true).unwrap().unwrap().theme, ThemePreference::Light);
        fs::remove_file(store.path()).unwrap();
        assert!(watcher.poll(true).unwrap().is_none(), "removed file cannot erase accepted state");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn invalid_selected_colors_cannot_replace_a_valid_snapshot() {
        let (dir, store) = fixture("invalid-colors");
        store.initialize(ThemeState::default()).unwrap();
        let id = "00000000-0000-4000-8000-000000000031";
        store.execute("theme_create", &serde_json::json!({"id":id,"name":"Valid","base_theme_id":"ocean","palette":{"text":"#fff4df"}})).unwrap();
        store.execute("theme_activate", &serde_json::json!({"id":format!("custom:{id}")})).unwrap();
        let mut watcher = ThemeWatcher::new(store.clone());
        let before = watcher.poll(true).unwrap().unwrap();
        let mut raw = serde_json::to_value(&before).unwrap();
        raw["theme_library"]["themes"][0]["palette"]["text"] = serde_json::json!("#invalid");
        fs::write(store.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
        assert_eq!(watcher.poll(true).unwrap_err().code, "invalid_store");
        let attempts = watcher.parses;
        assert!(watcher.poll(true).unwrap().is_none());
        assert_eq!(watcher.parses, attempts);
        assert_eq!(before.theme_library.get(CustomThemeId::parse(id).unwrap()).unwrap().palette.0["text"], "#fff4df");
        fs::write(store.path(), serde_json::to_vec(&before).unwrap()).unwrap();
        assert!(watcher.poll(true).unwrap().is_some());
        fs::remove_dir_all(dir).unwrap();
    }
}
