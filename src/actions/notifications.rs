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
use std::sync::atomic::Ordering;

use build::*;
use lsp_data::*;
use lsp_data::request::{RangeFormatting, RegisterCapability, UnregisterCapability};
use server::Request;

pub use lsp_data::notification::{
    Initialized,
    DidOpenTextDocument,
    DidChangeTextDocument,
    DidSaveTextDocument,
    DidChangeConfiguration,
    DidChangeWatchedFiles,
    Cancel,
};

use server::{BlockingNotificationAction, Output};

use std::thread;

impl BlockingNotificationAction for Initialized {
    // Respond to the `initialized` notification. We take this opportunity to
    // dynamically register some options.
    fn handle<O: Output>(_params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        const WATCH_ID: &str = "rls-watch";

        let ctx = ctx.inited();

        let id = out.provide_id() as usize;
        let params = RegistrationParams {
            registrations: vec![
                Registration {
                    id: WATCH_ID.to_owned(),
                    method: <DidChangeWatchedFiles as LSPNotification>::METHOD.to_owned(),
                    register_options: FileWatch::new(&ctx).watchers_config(),
                },
            ],
        };

        let request = Request::<RegisterCapability>::new(id, params);
        out.request(request);
        Ok(())
    }
}

impl BlockingNotificationAction for DidOpenTextDocument {
    fn handle<O: Output>(params: Self::Params, ctx: &mut ActionContext, _out: O) -> Result<(), ()> {
        trace!("on_open: {:?}", params.text_document.uri);
        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_open")?;

        ctx.vfs.set_file(&file_path, &params.text_document.text);
        Ok(())
    }
}

impl BlockingNotificationAction for DidChangeTextDocument {
    fn handle<O: Output>(params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        trace!(
            "on_change: {:?}, thread: {:?}",
            params,
            thread::current().id()
        );

        if params.content_changes.is_empty() {
            return Ok(());
        }

        let ctx = ctx.inited();
        ctx.quiescent.store(false, Ordering::SeqCst);
        let file_path = parse_file_path!(&params.text_document.uri, "on_change")?;
        let version_num = params.text_document.version.unwrap();

        ctx.check_change_version(&file_path, version_num);

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

        ctx.build_queue.mark_file_dirty(file_path, version_num);

        if !ctx.config.lock().unwrap().build_on_save {
            ctx.build_current_project(BuildPriority::Normal, &out);
        }
        Ok(())
    }
}

impl BlockingNotificationAction for Cancel {
    fn handle<O: Output>(
        _params: CancelParams,
        _ctx: &mut ActionContext,
        _out: O,
    ) -> Result<(), ()> {
        // Nothing to do.
        Ok(())
    }
}

impl BlockingNotificationAction for DidChangeConfiguration {
    fn handle<O: Output>(
        params: DidChangeConfigurationParams,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("config change: {:?}", params.settings);
        let ctx = ctx.inited();
        let config = params
            .settings
            .get("rust")
            .ok_or_else(|| serde_json::Error::missing_field("rust"))
            .and_then(Config::deserialize);

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
        ctx.build_current_project(BuildPriority::Cargo, &out);

        const RANGE_FORMATTING_ID: &str = "rls-range-formatting";
        // FIXME should handle the response
        let id = out.provide_id() as usize;
        if unstable_features {
            let params = RegistrationParams {
                    registrations: vec![
                        Registration {
                            id: RANGE_FORMATTING_ID.to_owned(),
                            method: <RangeFormatting as LSPRequest>::METHOD.to_owned(),
                            register_options: serde_json::Value::Null,
                        },
                    ],
            };

            let request = Request::<RegisterCapability>::new(id, params);
            out.request(request);
        } else {
            let params = UnregistrationParams {
                unregisterations: vec![
                    Unregistration {
                        id: RANGE_FORMATTING_ID.to_owned(),
                        method: <RangeFormatting as LSPRequest>::METHOD.to_owned(),
                    },
                ],
            };

            let request = Request::<UnregisterCapability>::new(id, params);
            out.request(request);
        }
        Ok(())
    }
}

impl BlockingNotificationAction for DidSaveTextDocument {
    fn handle<O: Output>(
        params: DidSaveTextDocumentParams,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        let ctx = ctx.inited();
        let file_path = parse_file_path!(&params.text_document.uri, "on_save")?;

        ctx.vfs.file_saved(&file_path).unwrap();

        if ctx.config.lock().unwrap().build_on_save {
            ctx.build_current_project(BuildPriority::Normal, &out);
        }

        Ok(())
    }
}

impl BlockingNotificationAction for DidChangeWatchedFiles {
    fn handle<O: Output>(
        params: DidChangeWatchedFilesParams,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("on_cargo_change: thread: {:?}", thread::current().id());

        let ctx = ctx.inited();
        let file_watch = FileWatch::new(&ctx);

        if params.changes.iter().any(|c| file_watch.is_relevant(c)) {
            ctx.build_current_project(BuildPriority::Cargo, &out);
        }

        Ok(())
    }
}
