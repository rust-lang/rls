// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use actions::ActionContext;
use vfs::Change;
use config::Config;
use serde::Deserialize;
use serde::de::Error;
use serde_json;
use Span;

use build::*;
use lsp_data::*;
use server::{Output, Action, NotificationAction, LsState, NoParams};

use std::thread;

#[derive(Debug, PartialEq)]
pub struct Initialized;

impl<'a> Action<'a> for Initialized {
    type Params = NoParams;
    const METHOD: &'static str = "initialized";

    fn new(_: &'a mut LsState) -> Self {
        Initialized
    }
}

impl<'a> NotificationAction<'a> for Initialized {
    // Respond to the `initialized` notification. We take this opportunity to
    // dynamically register some options.
    fn handle<O: Output>(&mut self, _params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        const WATCH_ID: &'static str = "rls-watch";

        let ctx = ctx.inited();

        // TODO we should watch for workspace Cargo.tomls too
        let pattern = format!("{}/Cargo{{.toml,.lock}}", ctx.current_project.to_str().unwrap());
        let target_pattern = format!("{}/target", ctx.current_project.to_str().unwrap());
        // For target, we only watch if it gets deleted.
        let options = json!({
            "watchers": [{ "globPattern": pattern }, { "globPattern": target_pattern, "kind": 4 }]
        });
        let output = serde_json::to_string(
            &RequestMessage::new(out.provide_id(),
                                 NOTIFICATION__RegisterCapability.to_owned(),
                                 RegistrationParams { registrations: vec![Registration { id: WATCH_ID.to_owned(), method: NOTIFICATION__DidChangeWatchedFiles.to_owned(), register_options: options } ]})
        ).unwrap();
        out.response(output);
        Ok(())
    }
}

#[derive(Debug)]
pub struct DidOpen;

impl<'a> Action<'a> for DidOpen {
    type Params = DidOpenTextDocumentParams;
    const METHOD: &'static str = "textDocument/didOpen";

    fn new(_: &'a mut LsState) -> Self {
        DidOpen
    }
}

impl<'a> NotificationAction<'a> for DidOpen {
    fn handle<O: Output>(&mut self, params: Self::Params, ctx: &mut ActionContext, _out: O) -> Result<(), ()> {
        trace!("on_open: {:?}", params.text_document.uri);
        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_open")?;

        ctx.vfs.set_file(&file_path, &params.text_document.text);
        Ok(())
    }
}

#[derive(Debug)]
pub struct DidChange;

impl<'a> Action<'a> for DidChange {
    type Params = DidChangeTextDocumentParams;
    const METHOD: &'static str = "textDocument/didChange";

    fn new(_: &'a mut LsState) -> Self {
        DidChange
    }
}

impl<'a> NotificationAction<'a> for DidChange {
    fn handle<O: Output>(&mut self, params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        trace!("on_change: {:?}, thread: {:?}", params, thread::current().id());

        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_change")?;

        let changes: Vec<Change> = params.content_changes.iter().map(|i| {
            if let Some(range) = i.range {
                let range = ls_util::range_to_rls(range);
                Change::ReplaceText {
                    span: Span::from_range(range, file_path.clone()),
                    len: i.range_length,
                    text: i.text.clone()
                }
            } else {
                Change::AddFile {
                    file: file_path.clone(),
                    text: i.text.clone(),
                }
            }
        }).collect();
        ctx.vfs.on_changes(&changes).expect("error committing to VFS");
        if !changes.is_empty() {
            ctx.build_queue.mark_file_dirty(file_path, params.text_document.version)
        }

        if !ctx.config.lock().unwrap().build_on_save {
            ctx.build_current_project(BuildPriority::Normal, out);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Cancel;

impl<'a> Action<'a> for Cancel {
    type Params = CancelParams;
    const METHOD: &'static str = "$/cancelRequest";

    fn new(_: &'a mut LsState) -> Self {
        Cancel
    }
}

impl<'a> NotificationAction<'a> for Cancel {
    fn handle<O: Output>(&mut self, _params: CancelParams, _ctx: &mut ActionContext, _out: O) -> Result<(), ()> {
        // Nothing to do.
        Ok(())
    }
}

#[derive(Debug)]
pub struct DidChangeConfiguration;

impl<'a> Action<'a> for DidChangeConfiguration {
    type Params = DidChangeConfigurationParams;
    const METHOD: &'static str = "workspace/didChangeConfiguration";

    fn new(_: &'a mut LsState) -> Self {
        DidChangeConfiguration
    }
}

impl<'a> NotificationAction<'a> for DidChangeConfiguration {
    fn handle<O: Output>(&mut self, params: DidChangeConfigurationParams, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        trace!("config change: {:?}", params.settings);
        let ctx = ctx.inited();
        let config = params.settings.get("rust")
                         .ok_or(serde_json::Error::missing_field("rust"))
                         .and_then(|value| Config::deserialize(value));

        let new_config = match config {
            Ok(mut value) => {
                value.normalise();
                value
            }
            Err(err) => {
                debug!("Received unactionable config: {:?} (error: {:?})", params.settings, err);
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
                    if let Err(e)  = config.infer_defaults(&project_dir) {
                        debug!("Encountered an error while trying to infer config \
                            defaults: {:?}", e);
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
            let output = serde_json::to_string(
                &RequestMessage::new(out.provide_id(),
                                        NOTIFICATION__RegisterCapability.to_owned(),
                                        RegistrationParams { registrations: vec![Registration { id: RANGE_FORMATTING_ID.to_owned(), method: REQUEST__RangeFormatting.to_owned(), register_options: serde_json::Value::Null }] })
            ).unwrap();
            out.response(output);
        } else {
            let output = serde_json::to_string(
                &RequestMessage::new(out.provide_id(),
                                        NOTIFICATION__UnregisterCapability.to_owned(),
                                        UnregistrationParams { unregisterations: vec![Unregistration { id: RANGE_FORMATTING_ID.to_owned(), method: REQUEST__RangeFormatting.to_owned() }] })
            ).unwrap();
            out.response(output);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct DidSave;

impl<'a> Action<'a> for DidSave {
    type Params = DidSaveTextDocumentParams;
    const METHOD: &'static str = "textDocument/didSave";

    fn new(_: &'a mut LsState) -> Self {
        DidSave
    }
}

impl<'a> NotificationAction<'a> for DidSave {
    fn handle<O: Output>(&mut self, params: DidSaveTextDocumentParams, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_save")?;

        ctx.vfs.file_saved(&file_path).unwrap();

        if ctx.config.lock().unwrap().build_on_save {
            ctx.build_current_project(BuildPriority::Normal, out);
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct DidChangeWatchedFiles;

impl<'a> Action<'a> for DidChangeWatchedFiles {
    type Params = DidChangeWatchedFilesParams;
    const METHOD: &'static str = "workspace/didChangeWatchedFiles";

    fn new(_: &'a mut LsState) -> Self {
        DidChangeWatchedFiles
    }
}

impl<'a> NotificationAction<'a> for DidChangeWatchedFiles {
    fn handle<O: Output>(
        &mut self,
        params: DidChangeWatchedFilesParams,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("on_cargo_change: thread: {:?}", thread::current().id());

        // ignore irrelevant files from more spammy clients
        if !params.changes.iter().any(|change| {
                change.uri.as_str().ends_with("/Cargo.toml") ||
                change.uri.as_str().ends_with("/Cargo.lock") ||
                change.typ == FileChangeType::Deleted && change.uri.as_str().ends_with("/target")
            })
        {
            return Ok(());
        }

        let ctx = ctx.inited();
        ctx.build_current_project(BuildPriority::Cargo, out);

        Ok(())
    }
}
