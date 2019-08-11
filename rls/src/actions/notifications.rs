//! One-way notifications that the RLS receives from the client.

use crate::actions::{FileWatch, InitActionContext, VersionOrdering};
use crate::Span;
use log::{debug, trace, warn};
use rls_vfs::{Change, VfsSpan};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::build::*;
use crate::lsp_data::request::{RangeFormatting, RegisterCapability, UnregisterCapability};
use crate::lsp_data::*;
use crate::server::Request;
use lsp_types::notification::ShowMessage;

pub use crate::lsp_data::notification::{
    Cancel, DidChangeConfiguration, DidChangeTextDocument, DidChangeWatchedFiles,
    DidOpenTextDocument, DidSaveTextDocument, Initialized,
};

use crate::server::{BlockingNotificationAction, Notification, Output};

use std::thread;

impl BlockingNotificationAction for Initialized {
    // Respond to the `initialized` notification. We take this opportunity to
    // dynamically register some options.
    fn handle<O: Output>(
        _params: Self::Params,
        ctx: &mut InitActionContext,
        out: O,
    ) -> Result<(), ()> {
        const WATCH_ID: &str = "rls-watch";

        let id = out.provide_id();
        let params = RegistrationParams {
            registrations: vec![Registration {
                id: WATCH_ID.to_owned(),
                method: <DidChangeWatchedFiles as LSPNotification>::METHOD.to_owned(),
                register_options: Some(FileWatch::new(&ctx).watchers_config()),
            }],
        };

        let request = Request::<RegisterCapability>::new(id, params);
        out.request(request);
        Ok(())
    }
}

impl BlockingNotificationAction for DidOpenTextDocument {
    fn handle<O: Output>(
        params: Self::Params,
        ctx: &mut InitActionContext,
        _out: O,
    ) -> Result<(), ()> {
        trace!("on_open: {:?}", params.text_document.uri);
        let file_path = parse_file_path!(&params.text_document.uri, "on_open")?;
        ctx.reset_change_version(&file_path);
        ctx.vfs.set_file(&file_path, &params.text_document.text);
        Ok(())
    }
}

impl BlockingNotificationAction for DidChangeTextDocument {
    fn handle<O: Output>(
        params: Self::Params,
        ctx: &mut InitActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("on_change: {:?}, thread: {:?}", params, thread::current().id());

        if params.content_changes.is_empty() {
            return Ok(());
        }

        ctx.quiescent.store(false, Ordering::SeqCst);
        let file_path = parse_file_path!(&params.text_document.uri, "on_change")?;
        let version_num = params.text_document.version.unwrap();

        match ctx.check_change_version(&file_path, version_num) {
            VersionOrdering::Ok => {}
            VersionOrdering::Duplicate => return Ok(()),
            VersionOrdering::OutOfOrder => {
                out.notify(Notification::<ShowMessage>::new(ShowMessageParams {
                    typ: MessageType::Warning,
                    message: format!("Out of order change in {:?}", file_path),
                }));
                return Ok(());
            }
        }

        let changes: Vec<Change> = params
            .content_changes
            .iter()
            .map(|i| {
                if let Some(range) = i.range {
                    let range = ls_util::range_to_rls(range);
                    Change::ReplaceText {
                        // LSP sends UTF-16 code units based offsets and length
                        span: VfsSpan::from_utf16(
                            Span::from_range(range, file_path.clone()),
                            i.range_length,
                        ),
                        text: i.text.clone(),
                    }
                } else {
                    Change::AddFile { file: file_path.clone(), text: i.text.clone() }
                }
            })
            .collect();
        ctx.vfs.on_changes(&changes).expect("error committing to VFS");

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
        _ctx: &mut InitActionContext,
        _out: O,
    ) -> Result<(), ()> {
        // Nothing to do.
        Ok(())
    }
}

impl BlockingNotificationAction for DidChangeConfiguration {
    fn handle<O: Output>(
        params: DidChangeConfigurationParams,
        ctx: &mut InitActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("config change: {:?}", params.settings);
        use std::collections::HashMap;
        let mut dups = HashMap::new();
        let mut unknowns = vec![];
        let mut deprecated = vec![];
        let settings = ChangeConfigSettings::try_deserialize(
            &params.settings,
            &mut dups,
            &mut unknowns,
            &mut deprecated,
        );
        crate::server::maybe_notify_unknown_configs(&out, &unknowns);
        crate::server::maybe_notify_deprecated_configs(&out, &deprecated);
        crate::server::maybe_notify_duplicated_configs(&out, &dups);

        let new_config = match settings {
            Ok(mut value) => {
                value.rust.normalise();
                value.rust
            }
            Err(err) => {
                warn!("Received unactionable config: {:?} (error: {:?})", params.settings, err);
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
                let config = Arc::clone(&ctx.config);
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
        let id = out.provide_id();
        if unstable_features {
            let params = RegistrationParams {
                registrations: vec![Registration {
                    id: RANGE_FORMATTING_ID.to_owned(),
                    method: <RangeFormatting as LSPRequest>::METHOD.to_owned(),
                    register_options: None,
                }],
            };

            let request = Request::<RegisterCapability>::new(id, params);
            out.request(request);
        } else {
            let params = UnregistrationParams {
                unregisterations: vec![Unregistration {
                    id: RANGE_FORMATTING_ID.to_owned(),
                    method: <RangeFormatting as LSPRequest>::METHOD.to_owned(),
                }],
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
        ctx: &mut InitActionContext,
        out: O,
    ) -> Result<(), ()> {
        let file_path = parse_file_path!(&params.text_document.uri, "on_save")?;

        ctx.vfs.file_saved(&file_path).unwrap();

        if !ctx.client_use_change_watched && FileWatch::new(&ctx).is_relevant_save_doc(&params) {
            // support manifest change rebuilding for client's that don't send
            // workspace/didChangeWatchedFiles notifications
            ctx.build_current_project(BuildPriority::Cargo, &out);
            ctx.invalidate_project_model();
        } else if ctx.config.lock().unwrap().build_on_save {
            ctx.build_current_project(BuildPriority::Normal, &out);
        }

        Ok(())
    }
}

impl BlockingNotificationAction for DidChangeWatchedFiles {
    fn handle<O: Output>(
        params: DidChangeWatchedFilesParams,
        ctx: &mut InitActionContext,
        out: O,
    ) -> Result<(), ()> {
        trace!("on_cargo_change: thread: {:?}", thread::current().id());

        ctx.client_use_change_watched = true;
        let file_watch = FileWatch::new(&ctx);

        if params.changes.iter().any(|c| file_watch.is_relevant(c)) {
            ctx.build_current_project(BuildPriority::Cargo, &out);
            ctx.invalidate_project_model();
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::server::{Output, RequestId};
    use rls_analysis::{AnalysisHost, Target};
    use rls_vfs::Vfs;
    use std::sync::Arc;
    use url::Url;

    #[derive(Clone, Copy)]
    struct NoOutput;
    impl Output for NoOutput {
        /// Send a response string along the output.
        fn response(&self, _: String) {}
        fn provide_id(&self) -> RequestId {
            RequestId::Num(0)
        }
    }

    #[test]
    fn learn_client_use_change_watched() {
        let (project_root, lsp_project_manifest) = if cfg!(windows) {
            ("C:/some/dir", "file:c:/some/dir/Cargo.toml")
        } else {
            ("/some/dir", "file:///some/dir/Cargo.toml")
        };

        let mut ctx = InitActionContext::new(
            Arc::new(AnalysisHost::new(Target::Debug)),
            Arc::new(Vfs::new()),
            <_>::default(),
            <_>::default(),
            project_root.into(),
            123,
            false,
        );

        assert!(!ctx.client_use_change_watched);

        let manifest_change = Url::parse(lsp_project_manifest).unwrap();
        DidChangeWatchedFiles::handle(
            DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(manifest_change, FileChangeType::Changed)],
            },
            &mut ctx,
            NoOutput,
        )
        .unwrap();

        assert!(ctx.client_use_change_watched);

        ctx.wait_for_concurrent_jobs();
    }
}
