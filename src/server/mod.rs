// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Implementation of the server loop, and traits for extending server
//! interactions (for example, to add support for handling new types of
//! requests).

use analysis::AnalysisHost;
use jsonrpc_core::{self as jsonrpc, Id};
use vfs::Vfs;
use serde_json;

use version;
use lsp_data;
use lsp_data::{LSPNotification, LSPRequest, InitializationOptions};
use actions::{notifications, requests, ActionContext};
use config::Config;
use server::dispatch::Dispatcher;
pub use server::dispatch::{RequestAction, ResponseError, DEFAULT_REQUEST_TIMEOUT};
pub use server::io::{MessageReader, Output};
use server::io::{StdioMsgReader, StdioOutput};
use server::message::RawMessage;
pub use server::message::{Ack, NoResponse, Response, Request, BlockingRequestAction, BlockingNotificationAction, Notification};

pub use ls_types::request::Shutdown as ShutdownRequest;
pub use ls_types::request::Initialize as InitializeRequest;
pub use ls_types::notification::Exit as ExitNotification;
use ls_types::{InitializeResult, InitializeParams, ServerCapabilities, CompletionOptions, TextDocumentSyncCapability, TextDocumentSyncKind, ExecuteCommandOptions};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::Ordering;

mod io;
mod dispatch;
mod message;

/// Run the Rust Language Server.
pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>) {
    debug!("Language Server starting up. Version: {}", version());
    let service = LsService::new(
        analysis,
        vfs,
        Arc::new(Mutex::new(Config::default())),
        Box::new(StdioMsgReader),
        StdioOutput::new(),
    );
    LsService::run(service);
    debug!("Server shutting down");
}

impl BlockingRequestAction for ShutdownRequest {
    type Response = Ack;

    fn handle<O: Output>(
        _id: usize,
        _params: Self::Params,
        ctx: &mut ActionContext,
        _out: O,
    ) -> Result<Self::Response, ()> {
        // Currently we don't perform an explicit cleanup, other than storing state
        ctx.inited().shut_down.store(true, Ordering::SeqCst);

        Ok(Ack)
    }
}

impl BlockingNotificationAction for ExitNotification {
    fn handle<O: Output>(
        _params: Self::Params,
        ctx: &mut ActionContext,
        _out: O,
    ) -> Result<(), ()> {
        let shut_down = ctx.inited().shut_down.load(Ordering::SeqCst);
        ::std::process::exit(if shut_down { 0 } else { 1 });
    }
}

impl BlockingRequestAction for InitializeRequest {
    type Response = NoResponse;

    fn handle<O: Output>(
        id: usize,
        params: Self::Params,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<NoResponse, ()> {
        let init_options: InitializationOptions = params
            .initialization_options
            .as_ref()
            .and_then(|options| serde_json::from_value(options.to_owned()).ok())
            .unwrap_or_default();

        trace!("init: {:?}", init_options);

        let result = InitializeResult { capabilities: server_caps() };
        out.success(id, &result);

        let capabilities = lsp_data::ClientCapabilities::new(&params);
        ctx.init(get_root_path(&params), &init_options, capabilities, &out);

        Ok(NoResponse)
    }
}

/// A service implementing a language server.
pub struct LsService<O: Output> {
    msg_reader: Box<MessageReader + Send + Sync>,
    output: O,
    ctx: ActionContext,
    dispatcher: Dispatcher,
}

impl<O: Output> LsService<O> {
    /// Construct a new language server service.
    pub fn new(
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>,
        reader: Box<MessageReader + Send + Sync>,
        output: O,
    ) -> LsService<O> {
        let dispatcher = Dispatcher::new(output.clone());

        LsService {
            msg_reader: reader,
            output: output,
            ctx: ActionContext::new(analysis, vfs, config),
            dispatcher,
        }
    }

    /// Run this language service.
    pub fn run(mut self) {
        while self.handle_message() == ServerStateChange::Continue {}
    }

    fn dispatch_message(&mut self, msg: &RawMessage) -> Result<(), jsonrpc::Error> {
        macro_rules! match_action {
            (
                $method: expr;
                notifications: $($n_action: ty),*;
                blocking_requests: $($br_action: ty),*;
                requests: $($request: ty),*;
            ) => {
                let mut handled = false;
                trace!("Handling `{}`", $method);
                $(
                    if $method == <$n_action as LSPNotification>::METHOD {
                        let notification: Notification<$n_action> = msg.parse_as_notification()?;
                        if notification.dispatch(&mut self.ctx, self.output.clone()).is_err() {
                            debug!("Error handling notification: {:?}", msg);
                        }
                        handled = true;
                    }
                )*
                $(
                    if $method == <$br_action as LSPRequest>::METHOD {
                        let request: Request<$br_action> = msg.parse_as_request()?;

                        // block until all nonblocking requests have been handled ensuring ordering
                        self.dispatcher.await_all_dispatched();

                        if request.blocking_dispatch(&mut self.ctx, &self.output).is_err() {
                            debug!("Error handling request: {:?}", msg);
                        }
                        handled = true;
                    }
                )*
                $(
                    if $method == <$request as LSPRequest>::METHOD {
                        let request: Request<$request> = msg.parse_as_request()?;
                        let request = (request, self.ctx.inited());
                        self.dispatcher.dispatch(request);
                        handled = true;
                    }
                )*
                if !handled {
                    debug!("Method not found: {}", $method);
                }
            }
        }

        // Notifications and blocking requests are handled immediately on the
        // main thread. They will never be dropped.
        // Blocking requests wait for all non-blocking requests to complete,
        // notifications do not.
        // Other requests are read and then forwarded to a worker thread, they
        // might timeout and will return an error but should not be dropped.
        // Some requests might block again when executing due to waiting for a
        // build or access to the VFS or real file system.
        // Requests must not mutate RLS state, but may ask the client to mutate
        // the client state.
        match_action!(
            msg.method;
            notifications:
                ExitNotification,
                notifications::Initialized,
                notifications::DidOpenTextDocument,
                notifications::DidChangeTextDocument,
                notifications::DidSaveTextDocument,
                notifications::DidChangeConfiguration,
                notifications::DidChangeWatchedFiles,
                notifications::Cancel;
            blocking_requests:
                ShutdownRequest,
                InitializeRequest;
            requests:
                requests::ExecuteCommand,
                requests::Formatting,
                requests::RangeFormatting,
                requests::ResolveCompletion,
                requests::Rename,
                requests::CodeAction,
                requests::DocumentHighlight,
                requests::FindImpls,
                requests::Symbols,
                requests::Hover,
                requests::WorkspaceSymbol,
                requests::Definition,
                requests::References,
                requests::Completion;
        );
        Ok(())
    }

    /// Read a message from the language server reader input and handle it with
    /// the appropriate action. Returns a `ServerStateChange` that describes how
    /// the service should proceed now that the message has been handled.
    pub fn handle_message(&mut self) -> ServerStateChange {
        let msg_string = match self.msg_reader.read_message() {
            Some(m) => m,
            None => {
                error!("Can't read message");
                self.output.failure(Id::Null, jsonrpc::Error::parse_error());
                return ServerStateChange::Break;
            }
        };

        trace!("Read message `{}`", msg_string);

        let raw_message = match RawMessage::try_parse(&msg_string) {
            Ok(Some(rm)) => rm,
            Ok(None) => return ServerStateChange::Continue,
            Err(e) => {
                error!("parsing error, {:?}", e);
                self.output.failure(Id::Null, jsonrpc::Error::parse_error());
                return ServerStateChange::Break;
            }
        };

        trace!("Parsed message `{:?}`", raw_message);

        // If we're in shutdown mode, ignore any messages other than 'exit'.
        // This is not actually in the spec, I'm not sure we should do this,
        // but it kinda makes sense.
        {
            let shutdown_mode = match self.ctx {
                ActionContext::Init(ref ctx) => ctx.shut_down.load(Ordering::SeqCst),
                _ => false,
            };

            if shutdown_mode && raw_message.method != <ExitNotification as LSPNotification>::METHOD {
                trace!("In shutdown mode, ignoring {:?}!", raw_message);
                return ServerStateChange::Continue;
            }
        }

        if let Err(e) = self.dispatch_message(&raw_message) {
            error!("dispatch error, {:?}", e);
            self.output.failure(raw_message.id.unwrap_or(Id::Null), e);
            return ServerStateChange::Break;
        }

        ServerStateChange::Continue
    }
}

/// How should the server proceed?
#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub enum ServerStateChange {
    /// Continue serving responses to requests and sending notifications to the
    /// client.
    Continue,
    /// Stop the server.
    Break,
}

fn server_caps() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::Incremental)),
        hover_provider: Some(true),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(true),
            trigger_characters: vec![".".to_string(), ":".to_string()],
        }),
        definition_provider: Some(true),
        references_provider: Some(true),
        document_highlight_provider: Some(true),
        document_symbol_provider: Some(true),
        workspace_symbol_provider: Some(true),
        code_action_provider: Some(true),
        document_formatting_provider: Some(true),
        execute_command_provider: Some(ExecuteCommandOptions {
            commands: vec![
                "rls.applySuggestion".to_owned(),
                "rls.deglobImports".to_owned(),
            ],
        }),
        rename_provider: Some(true),
        // These are supported if the `unstable_features` option is set.
        // We'll update these capabilities dynamically when we get config
        // info from the client.
        document_range_formatting_provider: Some(false),

        code_lens_provider: None,
        document_on_type_formatting_provider: None,
        signature_help_provider: None,
    }
}

fn get_root_path(params: &InitializeParams) -> PathBuf {
    params
        .root_uri
        .as_ref()
        .map(|uri| {
            assert!(uri.scheme() == "file");
            uri.to_file_path().expect("Could not convert URI to path")
        })
        .unwrap_or_else(|| {
            params
                .root_path
                .as_ref()
                .map(PathBuf::from)
                .expect("No root path or URI")
        })
}

#[cfg(test)]
mod test {
    use super::*;
    use url::Url;

    fn get_default_params() -> InitializeParams {
        InitializeParams {
            process_id: None,
            root_path: None,
            root_uri: None,
            initialization_options: None,
            capabilities: ::ls_types::ClientCapabilities {
                workspace: None,
                text_document: None,
                experimental: None,
            },
            trace: ::ls_types::TraceOption::Off,
        }
    }

    fn make_platform_path(path: &'static str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:/{}", path))
        } else {
            PathBuf::from(format!("/{}", path))
        }
    }

    #[test]
    fn test_use_root_uri() {
        let mut params = get_default_params();

        let root_path = make_platform_path("path/a");
        let root_uri = make_platform_path("path/b");
        params.root_path = Some(root_path.to_str().unwrap().to_owned());
        params.root_uri = Some(Url::from_directory_path(&root_uri).unwrap());

        assert_eq!(get_root_path(&params), root_uri);
    }

    #[test]
    fn test_use_root_path() {
        let mut params = get_default_params();

        let root_path = make_platform_path("path/a");
        params.root_path = Some(root_path.to_str().unwrap().to_owned());
        params.root_uri = None;

        assert_eq!(get_root_path(&params), root_path);
    }
}
