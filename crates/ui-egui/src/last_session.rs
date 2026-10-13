//! Reopen the files from the last session (Preferences ▸ Documents and view, #442): which files
//! were open when PdfCraft last closed, the page each showed and the active tab. Local only, like
//! the recent-files list, and kept only while the preference is on.

use serde::Serialize;

use crate::PdfCraftApp;

/// More files than anyone keeps open. Settings are untrusted, so a longer list is cut here.
pub const MAX_FILES: usize = 64;

/// The files open when PdfCraft last closed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LastSession {
    pub files: Vec<SessionFile>,
    /// The path of the tab that was active.
    pub active: Option<String>,
}

/// One open file and where the reader was in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionFile {
    /// Absolute, so a file opened from the command line by a relative path is found again.
    pub path: String,
    /// The page the tab showed (0-based).
    pub page: usize,
}

impl LastSession {
    /// Read what [`PdfCraftApp::persist`] wrote. Malformed entries are skipped and the list is
    /// capped at [`MAX_FILES`].
    pub(crate) fn from_json(v: &serde_json::Value) -> Self {
        let files = v["files"]
            .as_array()
            .map(|files| {
                files
                    .iter()
                    .filter_map(|f| {
                        let path = f["path"].as_str().filter(|p| !p.is_empty())?.to_string();
                        // `go_to_page` clamps a page past the end.
                        let page = f["page"].as_u64().and_then(|p| usize::try_from(p).ok()).unwrap_or(0);
                        Some(SessionFile { path, page })
                    })
                    .take(MAX_FILES)
                    .collect()
            })
            .unwrap_or_default();
        Self { files, active: v["active"].as_str().map(str::to_string) }
    }
}

/// Files whose last-read page is kept. Oldest are dropped first.
pub const MAX_POSITIONS: usize = 500;

/// Where a file was left: the page at the top of the window, how far down it, and the zoom.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReadingPlace {
    /// Absolute path of the file.
    pub path: String,
    pub page: usize,
    /// Fraction of the page above the window's top edge.
    pub frac: f32,
    /// `fit-width`, `fit-page`, `fit-height` or a percentage such as `150%`.
    pub zoom: String,
}

/// Read the saved places. Malformed entries are skipped and the list is capped.
pub(crate) fn places_from_json(v: &serde_json::Value) -> Vec<ReadingPlace> {
    let Some(items) = v.as_array() else { return Vec::new() };
    let all: Vec<ReadingPlace> = items
        .iter()
        .filter_map(|e| {
            let path = e["path"].as_str().filter(|p| !p.is_empty())?.to_string();
            let page = usize::try_from(e["page"].as_u64()?).ok()?;
            let frac = e["frac"].as_f64().filter(|f| f.is_finite()).map_or(0.0, |f| f.clamp(0.0, 1.0) as f32);
            let zoom = e["zoom"].as_str().map_or_else(|| "fit-width".to_string(), str::to_string);
            Some(ReadingPlace { path, page, frac, zoom })
        })
        .collect();
    let skip = all.len().saturating_sub(MAX_POSITIONS);
    all.into_iter().skip(skip).collect()
}

/// `path` made absolute against the current directory, without resolving links (and without
/// Windows' `\\?\` prefix, which `canonicalize` adds).
#[cfg(not(target_arch = "wasm32"))]
fn absolute(path: &str) -> String {
    std::path::absolute(path).map_or_else(|_| path.to_string(), |p| p.to_string_lossy().into_owned())
}

impl PdfCraftApp {
    /// The files open now. Documents that were never saved to a file are left out (crash
    /// recovery keeps their unsaved work), and so is everything on the web, which has no paths.
    pub(crate) fn open_session(&self) -> LastSession {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = |v: &crate::DocView| self.session.get(v.id).and_then(|d| d.path.as_deref()).map(absolute);
            let files = self.views.iter().filter_map(|v| Some(SessionFile { path: path(v)?, page: v.current })).take(MAX_FILES).collect();
            let active = self.active.and_then(|i| self.views.get(i)).and_then(path);
            LastSession { files, active }
        }
        #[cfg(target_arch = "wasm32")]
        LastSession::default()
    }

    /// What to save as the last session: the files open when the quit began, once the app is
    /// really quitting (unsaved tabs close one by one before that), else the files open now.
    pub(crate) fn session_to_save(&self) -> LastSession {
        match &self.quit_session {
            Some(s) if self.allow_quit => s.clone(),
            _ => self.open_session(),
        }
    }

    /// Note where a document was left, under its file path (documents without a file have none).
    pub(crate) fn remember_position(&mut self, id: crate::DocId, view: &crate::canvas::DocView) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(place) = self.place_of(id, view) {
            self.reading_places.retain(|p| p.path != place.path);
            self.reading_places.push(place);
            let excess = self.reading_places.len().saturating_sub(MAX_POSITIONS);
            self.reading_places.drain(..excess);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (id, view);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn place_of(&self, id: crate::DocId, view: &crate::canvas::DocView) -> Option<ReadingPlace> {
        let path = self.session.get(id).and_then(|d| d.path.as_deref()).map(absolute)?;
        Some(ReadingPlace { path, page: view.read_anchor.0, frac: view.read_anchor.1, zoom: view.zoom_token() })
    }

    /// Where `path` was last left, if known.
    pub(crate) fn saved_position(&self, path: &str) -> Option<ReadingPlace> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = absolute(path);
            self.reading_places.iter().rev().find(|p| p.path == path).cloned()
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = path;
            None
        }
    }

    /// The remembered places plus those of the documents open now, for saving.
    pub(crate) fn positions_to_save(&self) -> Vec<ReadingPlace> {
        let mut out = self.reading_places.clone();
        #[cfg(not(target_arch = "wasm32"))]
        for v in &self.views {
            if let Some(place) = self.place_of(v.id, v) {
                out.retain(|p| p.path != place.path);
                out.push(place);
            }
        }
        let excess = out.len().saturating_sub(MAX_POSITIONS);
        out.drain(..excess);
        out
    }

    /// A quit is about to close its first tab: remember everything that is still open.
    pub(crate) fn note_quit_session(&mut self) {
        if self.quit_session.is_none() {
            self.quit_session = Some(self.open_session());
        }
    }

    /// The quit was cancelled: what is open from now on is the session again.
    pub(crate) fn forget_quit_session(&mut self) {
        self.quit_session = None;
    }

    /// At startup, with the preference on: reopen the last session's files at the pages they
    /// showed and bring its active tab forward. Files that are gone are skipped, and so are
    /// `skip` (files the app is about to open anyway, e.g. from the command line) and documents
    /// the Recovery dialog offers, so neither opens twice.
    ///
    /// Only one password prompt can wait at a time: the first encrypted file asks for its
    /// password and later ones stay closed (they are in the recent-files list).
    pub fn reopen_last_files(&mut self, skip: &[String]) {
        let last = std::mem::take(&mut self.last_session);
        #[cfg(not(target_arch = "wasm32"))]
        if self.reopen_last_session {
            let skip: Vec<String> =
                skip.iter().map(|p| absolute(p)).chain(self.recoverable.iter().filter_map(|m| m.path.as_deref().map(absolute))).collect();
            let mut active = None;
            let mut prompt = self.password_prompt.take();
            for f in &last.files {
                let open = |app: &Self| {
                    app.views.iter().any(|v| app.session.get(v.id).and_then(|d| d.path.as_deref()).map(absolute).as_deref() == Some(f.path.as_str()))
                };
                if skip.contains(&f.path) || !std::path::Path::new(&f.path).is_file() || open(self) {
                    continue;
                }
                let before = self.views.len();
                self.open_path(&f.path);
                if let Some(p) = self.password_prompt.take() {
                    prompt.get_or_insert(p);
                }
                if self.views.len() > before
                    && let Some(view) = self.views.last_mut()
                {
                    view.go_to_page(f.page);
                    if last.active.as_ref() == Some(&f.path) {
                        active = Some(self.views.len() - 1);
                    }
                }
            }
            self.password_prompt = prompt;
            if active.is_some() {
                self.active = active;
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (last, skip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_places_are_skipped_and_capped() {
        let v = serde_json::json!([
            {"path": "/a.pdf", "page": 3, "frac": 0.25, "zoom": "150%"},
            {"path": "", "page": 1}, {"path": 5, "page": 1}, {"path": "/b.pdf", "page": -2}, {"path": "/c.pdf"},
            {"path": "/d.pdf", "page": 9, "frac": 7.0},
        ]);
        let places = places_from_json(&v);
        let got: Vec<_> = places.iter().map(|p| (p.path.as_str(), p.page, p.frac, p.zoom.as_str())).collect();
        assert_eq!(got, [("/a.pdf", 3, 0.25, "150%"), ("/d.pdf", 9, 1.0, "fit-width")]);
        assert!(places_from_json(&serde_json::json!("x")).is_empty());
        let many: Vec<_> = (0..MAX_POSITIONS + 20).map(|i| serde_json::json!({"path": format!("/{i}.pdf"), "page": i})).collect();
        let kept = places_from_json(&serde_json::Value::Array(many));
        assert_eq!(kept.len(), MAX_POSITIONS);
        assert_eq!(kept[0].path, "/20.pdf");
    }

    #[test]
    fn malformed_sessions_are_skipped_and_capped() {
        let v = serde_json::json!({
            "files": [{"path": 5}, {"path": ""}, "x", {"path": "/a.pdf", "page": -3}, {"path": "/b.pdf", "page": 1e300}, {"path": "/c.pdf", "page": 7}],
            "active": 3,
        });
        let s = LastSession::from_json(&v);
        let pages: Vec<(&str, usize)> = s.files.iter().map(|f| (f.path.as_str(), f.page)).collect();
        assert_eq!(pages, [("/a.pdf", 0), ("/b.pdf", 0), ("/c.pdf", 7)]);
        assert_eq!(s.active, None);
        let many: Vec<serde_json::Value> = (0..1000).map(|i| serde_json::json!({"path": format!("/{i}.pdf")})).collect();
        assert_eq!(LastSession::from_json(&serde_json::json!({"files": many})).files.len(), MAX_FILES);
        assert_eq!(LastSession::from_json(&serde_json::json!("garbage")), LastSession::default());
    }
}
