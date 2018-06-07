// Copyright 2018 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::atomic::{AtomicUsize, Ordering};

use lsp_data::{ProgressParams, PublishDiagnosticsParams, Progress, ShowMessageParams, MessageType};
use server::{Sender, Notification};
use ls_types::notification::{PublishDiagnostics, ShowMessage};

/// Trait for communication of build progress back to the client.
pub trait ProgressNotifier: Send {
    fn notify_begin_progress(&self);
    fn notify_progress(&self, update: ProgressUpdate);
    fn notify_end_progress(&self);
}

/// Kinds of progress updates
pub enum ProgressUpdate {
    Message(String),
    Percentage(f64),
}

/// Trait for communication of diagnostics (i.e. build results) back to the rest of
/// the RLS (and on to the client).
// This trait only really exists to work around the object safety rules (Output
// is not object-safe).
pub trait DiagnosticsNotifier: Send {
    fn notify_begin_diagnostics(&self);
    fn notify_publish_diagnostics(&self, PublishDiagnosticsParams);
    fn notify_error_diagnostics(&self, msg: String);
    fn notify_end_diagnostics(&self);
}

/// Generate a new progress params with a unique ID and the given title.
fn new_progress_params(title: String) -> ProgressParams {

    // counter to generate unique ID for each chain-of-progress notifications.
    lazy_static! {
        static ref PROGRESS_ID_COUNTER: AtomicUsize = {
            AtomicUsize::new(0)
        };
    }

    ProgressParams {
        id: format!("progress_{}", PROGRESS_ID_COUNTER.fetch_add(1, Ordering::SeqCst)),
        title: Some(title),
        message: None,
        percentage: None,
        done: None,
    }
}

/// Notifier of progress for the build (window/progress notifications).
/// the same instance is used for the entirety of one single build.
pub struct BuildProgressNotifier<S: Sender> {
    sender: S,
    // these params are used as a template and are cloned for each
    // message that is actually notified.
    progress_params: ProgressParams,
}

impl<S: Sender> BuildProgressNotifier<S> {
    pub fn new(sender: S) -> BuildProgressNotifier<S> {
        BuildProgressNotifier {
            sender,
            progress_params: new_progress_params("Building".into()),
        }
    }
}

impl<S: Sender> ProgressNotifier for BuildProgressNotifier<S> {
    fn notify_begin_progress(&self) {
        let params = self.progress_params.clone();
        self.sender.notify(Notification::<Progress>::new(params));
    }
    fn notify_progress(&self, update: ProgressUpdate) {
        let mut params = self.progress_params.clone();
        match update {
            ProgressUpdate::Message(s) => params.message = Some(s),
            ProgressUpdate::Percentage(p) => params.percentage = Some(p),
        }
        self.sender.notify(Notification::<Progress>::new(params));
    }
    fn notify_end_progress(&self) {
        let mut params = self.progress_params.clone();
        params.done = Some(true);
        self.sender.notify(Notification::<Progress>::new(params));
    }
}


/// Notifier of diagnostics after the build has completed.
pub struct BuildDiagnosticsNotifier<S: Sender> {
    sender: S,
    // these params are used as a template and are cloned for each
    // message that is actually notified.
    progress_params: ProgressParams,
}

impl<S: Sender> BuildDiagnosticsNotifier<S> {
    pub fn new(sender: S) -> BuildDiagnosticsNotifier<S> {
        BuildDiagnosticsNotifier {
            sender,
            // We emit diagnostics then index, since emitting diagnostics is really
            // quick and always has a message, "indexing" is usually a more useful
            // title.
            progress_params: new_progress_params("Indexing".into()),
        }
    }
}

impl<S: Sender> DiagnosticsNotifier for BuildDiagnosticsNotifier<S> {
    fn notify_begin_diagnostics(&self) {
        let params = self.progress_params.clone();
        self.sender.notify(Notification::<Progress>::new(params));
    }
    fn notify_publish_diagnostics(&self, params: PublishDiagnosticsParams) {
        self.sender.notify(Notification::<PublishDiagnostics>::new(params));
    }
    fn notify_error_diagnostics(&self, message: String) {
        self.sender.notify(Notification::<ShowMessage>::new(ShowMessageParams {
             typ: MessageType::Error,
             message: message.to_owned(),
         }));
    }
    fn notify_end_diagnostics(&self) {
        let mut params = self.progress_params.clone();
        params.done = Some(true);
        self.sender.notify(Notification::<Progress>::new(params));
    }
}
