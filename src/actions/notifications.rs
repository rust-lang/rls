// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! One-way notifications that the RLS receives from the client.

use actions::ActionContext;
use actions::FileWatch;
use vfs::Change;
use config::Config;
use serde::Deserialize;
use serde::de::Error;
use serde_json;
use Span;

use build::*;
use lsp_data::*;
use server::{Action, BlockingNotificationAction, LsState, NoParams, Output};

use std::thread;

/// Notification from the client that it has completed initialization.
#[derive(Debug, PartialEq)]
pub struct Initialized;

impl Action for Initialized {
    type Params = NoParams;
    const METHOD: &'static str = "initialized";
}

impl<'a> BlockingNotificationAction<'a> for Initialized {
    fn new(_: &'a mut LsState) -> Self {
        Initialized
    }

    // Respond to the `initialized` notification. We take this opportunity to
    // dynamically register some options.
    fn handle<O: Output>(
        &mut self,
        _params: Self::Params,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        const WATCH_ID: &'static str = "rls-watch";

        let ctx = ctx.inited();

        let options = FileWatch::new(&ctx).watchers_config();
        let output = serde_json::to_string(&RequestMessage::new(
            out.provide_id(),
            NOTIFICATION__RegisterCapability.to_owned(),
            RegistrationParams {
                registrations: vec![
                    Registration {
                        id: WATCH_ID.to_owned(),
                        method: NOTIFICATION__DidChangeWatchedFiles.to_owned(),
                        register_options: options,
                    },
                ],
            },
        )).unwrap();
        out.response(output);
        Ok(())
    }
}

/// Notification from the client that the given text document has been
/// opened. The client is responsible for managing its clean up.
#[derive(Debug)]
pub struct DidOpen;

impl Action for DidOpen {
    type Params = DidOpenTextDocumentParams;
    const METHOD: &'static str = "textDocument/didOpen";
}

impl<'a> BlockingNotificationAction<'a> for DidOpen {
    fn new(_: &'a mut LsState) -> Self {
        DidOpen
    }

    fn handle<O: Output>(
        &mut self,
        params: Self::Params,
        ctx: &mut ActionContext,
        _out: O,
    ) -> Result<(), ()> {
        trace!("on_open: {:?}", params.text_document.uri);
        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_open")?;

        ctx.vfs.set_file(&file_path, &params.text_document.text);
        Ok(())
    }
}

/// Notification from the client that the given document changed.
#[derive(Debug)]
pub struct DidChange;

impl Action for DidChange {
    type Params = DidChangeTextDocumentParams;
    const METHOD: &'static str = "textDocument/didChange";
}

impl<'a> BlockingNotificationAction<'a> for DidChange {
    fn new(_: &'a mut LsState) -> Self {
        DidChange
    }

    fn handle<O: Output>(
        &mut self,
        params: Self::Params,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!(
            "on_change: {:?}, thread: {:?}",
            params,
            thread::current().id()
        );

        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_change")?;

        let changes: Vec<Change> = params
            .content_changes
            .iter()
            .map(|i| {
                if let Some(range) = i.range {
                    let range = ls_util::range_to_rls(range);
                    Change::ReplaceText {
                        span: Span::from_range(range, file_path.clone()),
                        len: i.range_length,
                        text: i.text.clone(),
                    }
                } else {
                    Change::AddFile {
                        file: file_path.clone(),
                        text: i.text.clone(),
                    }
                }
            })
            .collect();
        ctx.vfs
            .on_changes(&changes)
            .expect("error committing to VFS");
        if !changes.is_empty() {
            ctx.build_queue
                .mark_file_dirty(file_path, params.text_document.version)
        }

        if !ctx.config.lock().unwrap().build_on_save {
            ctx.build_current_project(BuildPriority::Normal, out);
        }
        Ok(())
    }
}

/// Notification from the client that they've canceled their previous request.
#[derive(Debug)]
pub struct Cancel;

impl Action for Cancel {
    type Params = CancelParams;
    const METHOD: &'static str = "$/cancelRequest";
}

impl<'a> BlockingNotificationAction<'a> for Cancel {
    fn new(_: &'a mut LsState) -> Self {
        Cancel
    }

    fn handle<O: Output>(
        &mut self,
        _params: CancelParams,
        _ctx: &mut ActionContext,
        _out: O,
    ) -> Result<(), ()> {
        // Nothing to do.
        Ok(())
    }
}

/// Notification from the client that the workspace's configuration settings
/// changed.
#[derive(Debug)]
pub struct DidChangeConfiguration;

impl Action for DidChangeConfiguration {
    type Params = DidChangeConfigurationParams;
    const METHOD: &'static str = "workspace/didChangeConfiguration";
}

impl<'a> BlockingNotificationAction<'a> for DidChangeConfiguration {
    fn new(_: &'a mut LsState) -> Self {
        DidChangeConfiguration
    }

    fn handle<O: Output>(
        &mut self,
        params: DidChangeConfigurationParams,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("config change: {:?}", params.settings);
        let ctx = ctx.inited();
        let config = params
            .settings
            .get("rust")
            .ok_or(serde_json::Error::missing_field("rust"))
            .and_then(|value| Config::deserialize(value));

        let new_config = match config {
            Ok(mut value) => {
                value.normalise();
                value
            }
            Err(err) => {
                debug!(
                    "Received unactionable config: {:?} (error: {:?})",
                    params.settings,
                    err
                );
                return Err(());
            }
        };

        let unstable_features = new_config.unstable_features;

        {
            let mut config = ctx.config.lock().unwrap();

            // User may specify null (to be inferred) options, in which case
            // we schedule further inference on a separate thread not to block
            // the main thread
            let needs_inference = new_config.needs_inference();
            // In case of null options, we provide default values for now
            config.update(new_config);
            trace!("Updated config: {:?}", *config);

            if needs_inference {
                let project_dir = ctx.current_project.clone();
                let config = ctx.config.clone();
                // Will lock and access Config just outside the current scope
                thread::spawn(move || {
                    let mut config = config.lock().unwrap();
                    if let Err(e) = config.infer_defaults(&project_dir) {
                        debug!(
                            "Encountered an error while trying to infer config \
                             defaults: {:?}",
                            e
                        );
                    }
                });
            }
        }
        // We do a clean build so that if we've changed any relevant options
        // for Cargo, we'll notice them. But if nothing relevant changes
        // then we don't do unnecessary building (i.e., we don't delete
        // artifacts on disk).
        ctx.build_current_project(BuildPriority::Cargo, out.clone());

        const RANGE_FORMATTING_ID: &'static str = "rls-range-formatting";
        // FIXME should handle the response
        if unstable_features {
            let output = serde_json::to_string(&RequestMessage::new(
                out.provide_id(),
                NOTIFICATION__RegisterCapability.to_owned(),
                RegistrationParams {
                    registrations: vec![
                        Registration {
                            id: RANGE_FORMATTING_ID.to_owned(),
                            method: REQUEST__RangeFormatting.to_owned(),
                            register_options: serde_json::Value::Null,
                        },
                    ],
                },
            )).unwrap();
            out.response(output);
        } else {
            let output = serde_json::to_string(&RequestMessage::new(
                out.provide_id(),
                NOTIFICATION__UnregisterCapability.to_owned(),
                UnregistrationParams {
                    unregisterations: vec![
                        Unregistration {
                            id: RANGE_FORMATTING_ID.to_owned(),
                            method: REQUEST__RangeFormatting.to_owned(),
                        },
                    ],
                },
            )).unwrap();
            out.response(output);
        }
        Ok(())
    }
}

/// Notification from the client that the given text document was saved.
#[derive(Debug)]
pub struct DidSave;

impl Action for DidSave {
    type Params = DidSaveTextDocumentParams;
    const METHOD: &'static str = "textDocument/didSave";
}

impl<'a> BlockingNotificationAction<'a> for DidSave {
    fn new(_: &'a mut LsState) -> Self {
        DidSave
    }

    fn handle<O: Output>(
        &mut self,
        params: DidSaveTextDocumentParams,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_save")?;

        ctx.vfs.file_saved(&file_path).unwrap();

        if ctx.config.lock().unwrap().build_on_save {
            ctx.build_current_project(BuildPriority::Normal, out);
        }

        Ok(())
    }
}

/// Notification from the client that there were changes to files that are being
/// watched.
#[derive(Debug)]
pub struct DidChangeWatchedFiles;

impl Action for DidChangeWatchedFiles {
    type Params = DidChangeWatchedFilesParams;
    const METHOD: &'static str = "workspace/didChangeWatchedFiles";
}

impl<'a> BlockingNotificationAction<'a> for DidChangeWatchedFiles {
    fn new(_: &'a mut LsState) -> Self {
        DidChangeWatchedFiles
    }

    fn handle<O: Output>(
        &mut self,
        params: DidChangeWatchedFilesParams,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("on_cargo_change: thread: {:?}", thread::current().id());

        let ctx = ctx.inited();
        let file_watch = FileWatch::new(&ctx);

        if params.changes.iter().any(|c| file_watch.is_relevant(c)) {
            ctx.build_current_project(BuildPriority::Cargo, out);
        }

        Ok(())
    }
}
