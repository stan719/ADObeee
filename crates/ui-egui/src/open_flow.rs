//! UI-owned opening continuations. The worker never mutates application state.

use crate::{OpenRequest, PasswordPrompt, PdfCraftApp, PendingOpen, open_worker};

impl PdfCraftApp {
    pub(super) fn schedule_open(&mut self, request: OpenRequest) -> Result<u64, String> {
        // Count completed-but-unconsumed results too, not only worker jobs.
        if self.pending_opens.len() >= open_worker::MAX_QUEUED_OPENS {
            return Err("too many documents are waiting to open".into());
        }
        let retry = request.password.is_some();
        if self.open_worker.is_none() {
            self.open_worker = Some(
                open_worker::OpenWorker::new(
                    #[cfg(test)]
                    self.open_prepare_gate.clone(),
                )
                .map_err(|e| format!("couldn't start the document-open worker: {e}"))?,
            );
        }
        let token = self.next_open_token.wrapping_add(1);
        let worker = self.open_worker.as_ref().ok_or("the document-open worker is unavailable")?;
        if let Some(ctx) = &self.ctx {
            worker.set_context(ctx);
        }
        let context = self.session.open_context();
        let receiver = worker.submit(token, context.clone(), request.name.clone(), request.path.clone(), request.password.clone(), request.input)?;
        self.next_open_token = token;
        let pending = PendingOpen {
            token,
            receiver,
            context,
            password: request.password,
            name: request.name,
            path: request.path,
            stamp: request.stamp,
            activate: request.activate,
            after_open: request.after_open,
            recovered: request.recovered,
            then: request.then,
        };
        // Finish the current password interaction before showing another file's prompt.
        if retry {
            self.pending_opens.insert(0, pending);
        } else {
            self.pending_opens.push(pending);
        }
        Ok(token)
    }

    /// Enable native background opening before any startup file is read.
    pub fn enable_background_open(&mut self, ctx: &egui::Context) {
        self.background_open = true;
        self.ctx = Some(ctx.clone());
    }

    /// Apply command-line view options to the startup request that will be active.
    pub fn set_startup_options(&mut self, options: Vec<(String, String)>) {
        self.startup_open = self.pending_opens.iter().rev().find(|pending| pending.activate).map(|pending| pending.token);
        self.startup_options = options;
        self.apply_startup_options();
    }

    pub(super) fn apply_startup_options(&mut self) {
        if self.startup_open.is_some() || self.password_prompt.is_some() {
            return;
        }
        for (key, value) in std::mem::take(&mut self.startup_options) {
            if let Err(error) = self.set_option(&key, &value) {
                self.notify(error);
            }
        }
    }

    pub(super) fn cancel_startup_open(&mut self, token: u64) {
        if self.startup_open == Some(token) {
            self.startup_open = None;
            self.startup_options.clear();
        }
    }

    pub(super) fn complete_startup_open(&mut self, token: u64) {
        if self.startup_open == Some(token) {
            self.startup_open = None;
            self.apply_startup_options();
        }
    }

    pub(super) fn poll_pending_opens(&mut self) {
        // A completed file must not replace a password, import or tool dialog. Keep its
        // result in the bounded queue until that interaction finishes.
        while !self.modal_open() && self.image_import.is_none() {
            let Some(pending) = self.pending_opens.first() else { break };
            let result = pending.receiver.try_recv();
            if matches!(result, Err(std::sync::mpsc::TryRecvError::Empty)) {
                break;
            }
            let pending = self.pending_opens.remove(0);
            match result {
                Ok(result) if result.token == pending.token => self.finish_pending_open(pending, result.kind),
                _ => {
                    self.cancel_startup_open(pending.token);
                    if let Some(meta) = pending.recovered {
                        self.recoverable.push(meta);
                    }
                    self.notify_fmt("Couldn't open {name}: the background worker stopped", &[("name", &pending.name)]);
                }
            }
        }
        self.apply_startup_options();
    }

    pub(super) fn finish_pending_open(&mut self, pending: PendingOpen, kind: open_worker::ResultKind) {
        // Settings can change while preparation is in flight. Reprepare under the new
        // JavaScript/trust policy on the worker, rather than committing stale decisions.
        if let open_worker::ResultKind::Prepared { bytes, result } = &kind
            && result.is_ok()
            && !pending.context.is_current(&self.session)
        {
            let recovery = pending.recovered.clone();
            let name = pending.name.clone();
            match self.schedule_open(OpenRequest {
                name: pending.name,
                path: pending.path,
                stamp: pending.stamp,
                input: open_worker::Input::Bytes(bytes.clone()),
                password: pending.password,
                activate: pending.activate,
                after_open: pending.after_open,
                recovered: pending.recovered,
                then: pending.then,
            }) {
                Ok(token) => {
                    if self.startup_open == Some(pending.token) {
                        self.startup_open = Some(token);
                    }
                    if let Some(index) = self.pending_opens.iter().position(|p| p.token == token) {
                        let retry = self.pending_opens.remove(index);
                        self.pending_opens.insert(0, retry);
                    }
                }
                Err(error) => {
                    self.cancel_startup_open(pending.token);
                    if let Some(meta) = recovery {
                        self.recoverable.push(meta);
                    }
                    self.notify_fmt("Couldn't open {name}: {e}", &[("name", &name), ("e", &error)]);
                }
            }
            return;
        }
        let failed = matches!(
            &kind,
            open_worker::ResultKind::ReadFailed(_)
                | open_worker::ResultKind::RemoteFailed(_)
                | open_worker::ResultKind::RecoveryFailed(_)
                | open_worker::ResultKind::Failed(_)
        ) || matches!(&kind, open_worker::ResultKind::Prepared { result, .. }
                if result.as_ref().as_ref().is_err_and(|e| !matches!(e, pdfcraft_render::OpenError::NeedsPassword | pdfcraft_render::OpenError::WrongPassword)));
        if failed {
            self.cancel_startup_open(pending.token);
        }
        if failed && let Some(meta) = &pending.recovered {
            self.recoverable.push(meta.clone());
        }
        match kind {
            open_worker::ResultKind::ReadFailed(reason) => {
                self.notify_fmt("Couldn't read {name}: {e}", &[("name", &pending.name), ("e", &reason)]);
            }
            open_worker::ResultKind::RemoteFailed(reason) => {
                self.notify_fmt("Couldn't follow linked PDF: {reason}", &[("reason", &reason)]);
            }
            open_worker::ResultKind::RecoveryFailed(reason) => {
                self.notify_fmt("Couldn't recover {name}: {e}", &[("name", &pending.name), ("e", &reason)]);
            }
            open_worker::ResultKind::Failed(reason) => {
                self.notify_fmt("Couldn't open {name}: {reason}", &[("name", &pending.name), ("reason", &reason)]);
            }
            open_worker::ResultKind::Conversion(bytes) => {
                let active = self.active;
                match self.open_converted(&pending.name, &bytes) {
                    Some(Ok(())) => {
                        if let Some(destination) = pending.after_open {
                            self.apply_open_destination(destination);
                        }
                        if let Some(command) = pending.then {
                            self.execute(command);
                        }
                        self.complete_startup_open(pending.token);
                    }
                    Some(Err(error)) => {
                        self.cancel_startup_open(pending.token);
                        self.notify_fmt("Couldn't open {name}: {e}", &[("name", &pending.name), ("e", &error)]);
                    }
                    None => self.cancel_startup_open(pending.token),
                }
                if !pending.activate {
                    self.active = active;
                }
            }
            open_worker::ResultKind::Prepared { bytes, result } => match *result {
                Ok(prepared) => match self.finish_open(&pending.name, pending.path.clone(), bytes.len(), prepared, pending.activate) {
                    Ok(id) => {
                        if pending.path.is_some() {
                            self.watch.known_as(id, pending.stamp);
                        }
                        if let Some(destination) = pending.after_open {
                            self.apply_open_destination(destination);
                        }
                        if let Some(meta) = &pending.recovered {
                            self.finish_recovery(meta);
                        }
                        if let Some(command) = pending.then {
                            self.execute(command);
                        }
                        self.complete_startup_open(pending.token);
                    }
                    Err(error) => {
                        self.cancel_startup_open(pending.token);
                        self.notify_fmt("Couldn't open {name}: {e}", &[("name", &pending.name), ("e", &error)]);
                    }
                },
                Err(error @ (pdfcraft_render::OpenError::NeedsPassword | pdfcraft_render::OpenError::WrongPassword)) => {
                    let password_error =
                        matches!(error, pdfcraft_render::OpenError::WrongPassword).then(|| "Incorrect password. Try again.".to_string());
                    self.password_prompt = Some(PasswordPrompt {
                        open_token: Some(pending.token),
                        name: pending.name,
                        path: pending.path,
                        bytes,
                        stamp: pending.stamp,
                        input: String::new(),
                        error: password_error,
                        activate: pending.activate,
                        recovered: pending.recovered,
                        then: pending.then,
                        after_open: pending.after_open,
                    });
                }
                Err(error) => self.notify_fmt("Couldn't open {name}: {e}", &[("name", &pending.name), ("e", &error.to_string())]),
            },
        }
    }
}
