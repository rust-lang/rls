use std::sync::atomic::{AtomicUsize, Ordering};

use crate::server::{Notification, Output};
use lazy_static::lazy_static;
use lsp_types::notification::{Progress, PublishDiagnostics, ShowMessage};
use lsp_types::{MessageType, ProgressParams, PublishDiagnosticsParams, ShowMessageParams};

/// Communication of build progress back to the client.
pub trait ProgressNotifier: Send {
    fn notify_begin_progress(&self);
    fn notify_progress(&self, update: ProgressUpdate);
    fn notify_end_progress(&self);
}

/// Kinds of progress updates.
pub enum ProgressUpdate {
    Message(String),
    Percentage(f64),
}

/// Trait for communication of diagnostics (i.e., build results) back to the rest of
/// the RLS (and on to the client).
// This trait only really exists to work around the object safety rules (Output
// is not object-safe).
pub trait DiagnosticsNotifier: Send {
    fn notify_begin_diagnostics(&self);
    fn notify_publish_diagnostics(&self, _: PublishDiagnosticsParams);
    fn notify_error_diagnostics(&self, msg: String);
    fn notify_end_diagnostics(&self);
}

/// Generates a new progress params with a unique ID and the given title.
fn new_progress_params(title: String) -> ProgressParams {
    // Counter to generate unique IDs for each chain-of-progress notification.
    lazy_static! {
        static ref PROGRESS_ID_COUNTER: AtomicUsize = AtomicUsize::new(0);
    }

    ProgressParams {
        id: format!("progress_{}", PROGRESS_ID_COUNTER.fetch_add(1, Ordering::SeqCst)),
        title,
        message: None,
        percentage: None,
        done: None,
    }
}

/// Notifier of progress for the build (window/progress notifications).
/// the same instance is used for the entirety of one single build.
pub struct BuildProgressNotifier<O: Output> {
    out: O,
    // These params are used as a template and are cloned for each
    // message that is actually notified.
    progress_params: ProgressParams,
}

impl<O: Output> BuildProgressNotifier<O> {
    pub fn new(out: O) -> BuildProgressNotifier<O> {
        BuildProgressNotifier { out, progress_params: new_progress_params("Building".into()) }
    }
}

impl<O: Output> ProgressNotifier for BuildProgressNotifier<O> {
    fn notify_begin_progress(&self) {
        let params = self.progress_params.clone();
        self.out.notify(Notification::<Progress>::new(params));
    }
    fn notify_progress(&self, update: ProgressUpdate) {
        let mut params = self.progress_params.clone();
        match update {
            ProgressUpdate::Message(s) => params.message = Some(s),
            ProgressUpdate::Percentage(p) => params.percentage = Some(p),
        }
        self.out.notify(Notification::<Progress>::new(params));
    }
    fn notify_end_progress(&self) {
        let mut params = self.progress_params.clone();
        params.done = Some(true);
        self.out.notify(Notification::<Progress>::new(params));
    }
}

/// Notifier of diagnostics after the build has completed.
pub struct BuildDiagnosticsNotifier<O: Output> {
    out: O,
    // These params are used as a template, and are cloned for each
    // message that is actually notified.
    progress_params: ProgressParams,
}

impl<O: Output> BuildDiagnosticsNotifier<O> {
    pub fn new(out: O) -> BuildDiagnosticsNotifier<O> {
        BuildDiagnosticsNotifier {
            out,
            // We emit diagnostics then index, since emitting diagnostics is really
            // quick and always has a message, "indexing" is usually a more useful
            // title.
            progress_params: new_progress_params("Indexing".into()),
        }
    }
}

impl<O: Output> DiagnosticsNotifier for BuildDiagnosticsNotifier<O> {
    fn notify_begin_diagnostics(&self) {
        let params = self.progress_params.clone();
        self.out.notify(Notification::<Progress>::new(params));
    }
    fn notify_publish_diagnostics(&self, params: PublishDiagnosticsParams) {
        self.out.notify(Notification::<PublishDiagnostics>::new(params));
    }
    fn notify_error_diagnostics(&self, message: String) {
        self.out.notify(Notification::<ShowMessage>::new(ShowMessageParams {
            typ: MessageType::Error,
            message,
        }));
    }
    fn notify_end_diagnostics(&self) {
        let mut params = self.progress_params.clone();
        params.done = Some(true);
        self.out.notify(Notification::<Progress>::new(params));
    }
}
