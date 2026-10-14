//! Native frontend worker. One request and result can be in flight; selector changes coalesce.
use crate::theme::{ActivePaletteCache, ThemeError, ThemeKind, ThemePreference, ThemeState, ThemeStore, watch::ThemeWatcher};
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};
enum Command {
    Poll(u64),
    Activate(u64, ThemePreference),
}
pub(crate) struct ThemeSnapshot {
    pub state: ThemeState,
    pub cache: ActivePaletteCache,
}
fn prepare(state: ThemeState) -> ThemeSnapshot {
    let _ = state.theme_library.choices();
    let mut cache = ActivePaletteCache::default();
    cache.resolve(state.theme, &state.theme_library, None, ThemeKind::Light);
    ThemeSnapshot { state, cache }
}
// Lock contention is transient; keep retries on the worker and bound shutdown delay.
fn retry_store_busy<T>(mut operation: impl FnMut() -> Result<T, ThemeError>) -> Result<T, ThemeError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match operation() {
            Err(error) if error.code == "store_busy" && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            result => return result,
        }
    }
}
struct Completion {
    generation: u64,
    result: Result<Option<ThemeSnapshot>, ThemeError>,
}
pub(crate) struct ThemeLoading {
    commands: Option<SyncSender<Command>>,
    worker: Option<std::thread::JoinHandle<()>>,
    results: Receiver<Completion>,
    generation: u64,
    busy: bool,
    pending: Option<ThemePreference>,
    last_poll: Instant,
}
impl ThemeLoading {
    pub fn start(store: ThemeStore, seed: Option<ThemeState>) -> Result<Self, ThemeError> {
        let busy = seed.is_some();
        let (commands, requests) = mpsc::sync_channel(2);
        let (completed, results) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("pdfcraft-themes".into())
            .spawn(move || {
                let mut watcher = ThemeWatcher::new(store.clone());
                if let Some(seed) = seed {
                    // Prewarm legacy fallback metadata off-thread even if the dedicated file is invalid.
                    let _ = seed.theme_library.choices();
                    let result = store.initialize(seed).and_then(|_| watcher.poll(true)).map(|state| state.map(prepare));
                    let _ = completed.send(Completion { generation: 0, result });
                }
                while let Ok(request) = requests.recv() {
                    let (generation, result) = match request {
                        Command::Poll(generation) => (generation, watcher.poll(false).map(|state| state.map(prepare))),
                        Command::Activate(generation, preference) => {
                            let result = retry_store_busy(|| store.execute_with_state("theme_activate", &serde_json::json!({"id":preference.id()})))
                                .map(|(_, state)| Some(prepare(state)));
                            (generation, result)
                        }
                    };
                    let _ = completed.send(Completion { generation, result });
                }
            })
            .map_err(|error| ThemeError { code: "worker_unavailable".into(), message: error.to_string() })?;
        Ok(Self { commands: Some(commands), worker: Some(worker), results, generation: 0, busy, pending: None, last_poll: Instant::now() })
    }
    pub fn activate(&mut self, preference: ThemePreference) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some(preference);
    }
    /// A synchronous API edit supersedes any older worker completion.
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }
    pub fn pending(&self) -> bool {
        self.busy || self.pending.is_some()
    }
    pub fn tick(&mut self) -> Option<Result<ThemeSnapshot, ThemeError>> {
        let mut output = None;
        if let Ok(done) = self.results.try_recv() {
            self.busy = false;
            if done.generation == self.generation {
                output = match done.result {
                    Ok(state) => state.map(Ok),
                    Err(error) => Some(Err(error)),
                };
            }
        }
        if !self.busy {
            let command = if let Some(preference) = self.pending {
                Some(Command::Activate(self.generation, preference))
            } else if self.last_poll.elapsed() >= Duration::from_secs(1) {
                Some(Command::Poll(self.generation))
            } else {
                None
            };
            if let Some(command) = command
                && self.commands.as_ref().is_some_and(|commands| commands.try_send(command).is_ok())
            {
                self.pending = None;
                self.busy = true;
                self.last_poll = Instant::now();
            }
        }
        output
    }
}

impl Drop for ThemeLoading {
    fn drop(&mut self) {
        // Shutdown is the only join: finish the latest coalesced selection before the process exits.
        // The second bounded queue slot is reserved for this final request.
        if let Some(preference) = self.pending.take()
            && let Some(commands) = &self.commands
        {
            let _ = commands.try_send(Command::Activate(self.generation, preference));
        }
        self.commands.take();
        let (_, disconnected) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.results, disconnected));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_store_contention_is_retried_without_retrying_invalid_input() {
        let mut attempts = 0;
        let result = retry_store_busy(|| {
            attempts += 1;
            if attempts < 3 { Err(ThemeError { code: "store_busy".into(), message: "held lock".into() }) } else { Ok(()) }
        });
        assert!(result.is_ok());
        assert_eq!(attempts, 3);
        attempts = 0;
        let result: Result<(), _> = retry_store_busy(|| {
            attempts += 1;
            Err(ThemeError { code: "invalid_theme".into(), message: "invalid".into() })
        });
        assert_eq!(result.unwrap_err().code, "invalid_theme");
        assert_eq!(attempts, 1);
    }
    #[test]
    fn rapid_choices_coalesce_and_old_completions_cannot_overwrite_them() {
        let (commands, requests) = mpsc::sync_channel(2);
        let (completed, results) = mpsc::sync_channel(1);
        let mut loading =
            ThemeLoading { commands: Some(commands), worker: None, results, generation: 0, busy: true, pending: None, last_poll: Instant::now() };
        loading.activate(ThemePreference::Ocean);
        loading.activate(ThemePreference::Solarized);
        completed.send(Completion { generation: 0, result: Ok(Some(prepare(ThemeState::default()))) }).unwrap();
        assert!(loading.tick().is_none(), "old startup result is discarded");
        let Command::Activate(generation, preference) = requests.try_recv().unwrap() else { panic!("latest activation required") };
        assert_eq!(preference, ThemePreference::Solarized);
        assert_eq!(generation, 2);
        loading.activate(ThemePreference::Forest);
        completed.send(Completion { generation, result: Ok(Some(prepare(ThemeState { theme: preference, ..Default::default() }))) }).unwrap();
        assert!(loading.tick().is_none(), "an older activation cannot replace a newer choice");
        let Command::Activate(generation, preference) = requests.try_recv().unwrap() else { panic!("latest activation required") };
        assert_eq!(preference, ThemePreference::Forest);
        assert!(requests.try_recv().is_err(), "queue is bounded and no polls accumulate");
        loading.invalidate();
        completed.send(Completion { generation, result: Ok(Some(prepare(ThemeState::default()))) }).unwrap();
        assert!(loading.tick().is_none(), "explicit shared-API edits also supersede background results");
    }
    #[test]
    fn shutdown_commits_the_latest_pending_selection() {
        let dir = std::env::temp_dir().join(format!("pdfcraft-worker-shutdown-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ThemeStore::new(dir.join("themes.json"));
        let mut loading = ThemeLoading::start(store.clone(), Some(ThemeState::default())).unwrap();
        loading.activate(ThemePreference::Ocean);
        loading.activate(ThemePreference::Solarized);
        drop(loading);
        assert_eq!(store.load().unwrap().theme, ThemePreference::Solarized);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn large_native_startup_and_selection_run_on_one_background_worker() {
        use crate::theme::{CustomThemeId, ThemeDefinition};
        let dir = std::env::temp_dir().join(format!("pdfcraft-worker-large-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = ThemeStore::new(dir.join("themes.json"));
        let mut base = ThemeDefinition::duplicate_builtin(ThemePreference::Ocean, CustomThemeId::from_bytes([0; 16]), "Template".into()).unwrap();
        let entries: Vec<_> = (0_u128..10_000)
            .map(|n| {
                base.id = CustomThemeId::from_bytes(n.to_be_bytes());
                serde_json::to_value(&base).unwrap()
            })
            .collect();
        let state = serde_json::json!({"schema_version":1,"theme":"Light","theme_library":{"schema_version":1,"themes":entries}});
        std::fs::write(store.path(), serde_json::to_vec(&state).unwrap()).unwrap();
        let mut loading = ThemeLoading::start(store.clone(), Some(ThemeState::default())).unwrap();
        assert!(loading.pending());
        loading.activate(ThemePreference::Ocean);
        loading.activate(ThemePreference::Solarized);
        let limit = Instant::now() + Duration::from_secs(20);
        let final_state = loop {
            if let Some(result) = loading.tick() {
                break result.unwrap().state;
            }
            assert!(Instant::now() < limit, "worker did not finish");
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(final_state.theme, ThemePreference::Solarized);
        assert_eq!(final_state.theme_library.choices().len(), 10_000);
        assert_eq!(store.load().unwrap().theme, ThemePreference::Solarized);
        drop(loading);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
