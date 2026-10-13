//! Help ▸ Check for updates (issue #28): ask for the latest release and offer its download page.
//!
//! The desktop app supplies how to ask ([`PdfCraftApp::update_source`]), so this crate has no
//! network code; without a source (the web build, tests) the command opens the releases page.
//! Package links open in the browser; installation remains manual.
//! Metadata and package policy are shared with headless consumers in `pdfcraft-platform`.
//! It asks only when the user does: there is no check at start (the owner's decision).

use std::sync::Arc;

use egui::{Align, Layout};

use crate::{PdfCraftApp, theme, widgets};

pub use pdfcraft_platform::updates::*;

/// Asks for the latest release (blocking; it runs on its own thread).
pub type UpdateSource = Arc<dyn Fn() -> Result<Release, String> + Send + Sync>;

/// Where a check is.
#[derive(Default)]
pub(crate) enum Check {
    #[default]
    Idle,
    #[cfg(not(target_arch = "wasm32"))]
    Running(std::sync::mpsc::Receiver<Result<Release, String>>),
    Done(Result<Release, String>),
    Managed,
}

#[derive(Default)]
pub(crate) struct Updates {
    pub(crate) check: Check,
    /// The Updates dialog is showing.
    pub(crate) open: bool,
}

impl PdfCraftApp {
    /// Help ▸ Check for updates: ask for the latest release and show the outcome.
    pub fn check_for_updates(&mut self) {
        if self.update_context.platform == Platform::Linux
            && matches!(self.update_context.kind, InstallKind::Deb | InstallKind::Rpm | InstallKind::Flatpak)
        {
            self.updates.open = true;
            self.updates.check = Check::Managed;
            return;
        }
        let Some(source) = self.update_source.clone() else {
            self.open_url(RELEASES_PAGE);
            return;
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.updates.open = true;
            if matches!(self.updates.check, Check::Running(_)) {
                return;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = self.ctx.clone();
            std::thread::spawn(move || {
                // The receiver may be gone (the app quit): nothing to report to then.
                let _ = tx.send(source());
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
            self.updates.check = Check::Running(rx);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = source;
            self.open_url(RELEASES_PAGE);
        }
    }

    /// Pick up a finished check (each frame).
    pub(crate) fn poll_updates(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Check::Running(rx) = &self.updates.check {
            let result = match rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the update check stopped unexpectedly".into()),
            };
            self.updates.check = Check::Done(result);
        }
    }
}

/// The Updates dialog.
pub(crate) fn dialog(app: &mut PdfCraftApp, ctx: &egui::Context) {
    if !app.updates.open {
        return;
    }
    let t = theme::Tokens::get(ctx);
    let current = env!("CARGO_PKG_VERSION");
    let mut close = false;
    let mut download: Option<(String, bool)> = None;
    let mut retry = false;
    let modal = crate::widgets::show_modal(ctx, "updates", |ui| {
        ui.set_width(480.0);
        ui.horizontal(|ui| {
            ui.add(crate::icons::image("cloud", 22.0, t.accent));
            ui.label(egui::RichText::new(tl!("Check for updates")).font(theme::semibold(16.0)));
        });
        ui.add_space(8.0);
        ui.label(crate::i18n::fmt(tl!("Installed version: {v}"), &[("v", &pdfcraft_engine::build_info::version())]));
        match &app.updates.check {
            Check::Idle => {
                ui.label(tl!("No check has run yet."));
            }
            #[cfg(not(target_arch = "wasm32"))]
            Check::Running(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Checking for a newer version…"));
                });
            }
            Check::Managed => {
                managed_guidance(ui, app.update_context.kind);
            }
            Check::Done(Ok(r)) if r.draft || r.prerelease => {
                ui.label(tl!("The release is not a published stable update. Try again later."));
            }
            Check::Done(Ok(r)) if is_newer(&r.version, current) => {
                let version = r.version.trim_start_matches('v');
                ui.label(egui::RichText::new(crate::i18n::fmt(tl!("PdfCraft {v} is available."), &[("v", version)])).strong());
                ui.label(crate::i18n::fmt(tl!("Available version: {v}"), &[("v", version)]));
                let page = if valid_release_url(&r.url, &r.version) { &r.url } else { RELEASES_PAGE };
                match select_package(r, app.update_context) {
                    PackageSelection::Package(asset) => {
                        ui.label(crate::i18n::fmt(tl!("Package: {name}"), &[("name", &asset.name)]));
                        ui.label(crate::i18n::fmt(tl!("Download size: {size}"), &[("size", &download_size(asset.size))]));
                        download = Some((asset.url.clone(), true));
                    }
                    PackageSelection::Managed(kind) => managed_guidance(ui, kind),
                    PackageSelection::Unknown => {
                        ui.label(tl!("Installation type is unknown or this is a development build. Choose a package on the official release page."));
                        download = Some((page.to_string(), false));
                    }
                    PackageSelection::Missing => {
                        ui.label(tl!("No compatible package is available for this installation."));
                        download = Some((page.to_string(), false));
                    }
                    PackageSelection::Ambiguous => {
                        ui.label(tl!("The release has ambiguous packages. Choose a package on the official release page."));
                        download = Some((page.to_string(), false));
                    }
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new(tl!("Release notes")).strong());
                if r.notes.is_empty() {
                    ui.label(tl!("No release notes were provided."));
                } else {
                    // Plain text only: no remote HTML, images, or automatically opened links.
                    egui::ScrollArea::vertical().id_salt("update-notes").max_height(180.0).show(ui, |ui| {
                        ui.add(egui::Label::new(&r.notes).wrap());
                    });
                }
            }
            Check::Done(Ok(_)) => {
                ui.label(crate::i18n::fmt(tl!("PdfCraft {c} is up to date."), &[("c", current)]));
            }
            Check::Done(Err(e)) => {
                ui.label(crate::i18n::fmt(tl!("Couldn't check for updates: {e}"), &[("e", &e.to_string())]));
                ui.label(
                    egui::RichText::new(crate::i18n::fmt(
                        tl!("You have version {c}. All releases are listed at {page}."),
                        &[("c", current), ("page", RELEASES_PAGE)],
                    ))
                    .color(t.text_muted),
                );
            }
        }
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(tl!("Downloads open in your browser. Installation is manual; nothing is installed automatically."))
                .color(t.text_muted)
                .small(),
        );
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some((url, package)) = download.take() {
                let label = if package { tl!("Download package…") } else { tl!("View release page…") };
                let get = widgets::pill_button(ui, label, true).clicked();
                let later = widgets::pill_button(ui, tl!("Remind Me Later"), false).clicked();
                close = get || later;
                download = get.then_some((url, package));
            } else {
                close = widgets::pill_button(ui, tl!("Close"), true).clicked();
                if matches!(app.updates.check, Check::Done(Err(_))) {
                    retry = widgets::pill_button(ui, tl!("Retry"), false).clicked();
                }
            }
        });
    });
    if modal.should_close() {
        close = true;
        download = None;
    }
    if retry {
        app.check_for_updates();
    }
    if close {
        app.updates.open = false;
        if let Some((url, _)) = download {
            app.open_url(&url);
        }
    }
}

/// Byte units are binary and deliberately separate from translated surrounding text.
fn download_size(bytes: u64) -> String {
    crate::i18n::fmt(tl!("{mib} MiB ({bytes} bytes)"), &[("mib", &format!("{:.1}", bytes as f64 / 1_048_576.0)), ("bytes", &bytes.to_string())])
}

fn managed_guidance(ui: &mut egui::Ui, kind: InstallKind) {
    ui.label(match kind {
        InstallKind::Flatpak => tl!("Flatpak manages this installation. Check for updates in your system software manager or with flatpak update."),
        InstallKind::Deb => tl!("This installation is owned by a Debian package. Use your system software manager to update PdfCraft."),
        InstallKind::Rpm => tl!("This installation is owned by an RPM package. Use your system software manager to update PdfCraft."),
        _ => tl!("Use your system software manager to update PdfCraft."),
    });
}
