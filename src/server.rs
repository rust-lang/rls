// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use analysis::AnalysisHost;
use vfs::Vfs;
use serde_json;

use lsp_data::*;
use actions::ActionHandler;
use config::Config;

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering, AtomicU32};
use std::path::PathBuf;

use jsonrpc_core::{self as jsonrpc, Id, response, version};

pub fn server_failure(id: jsonrpc::Id, error: jsonrpc::Error) -> jsonrpc::Failure {
    jsonrpc::Failure {
        jsonrpc: Some(version::Version::V2),
        id,
        error,
    }
}

#[cfg(test)]
#[allow(non_upper_case_globals)]
pub const REQUEST__Deglob: &'static str = "rustWorkspace/deglob";

#[derive(Debug, Serialize)]
pub struct Ack;

#[derive(Debug)]
pub enum ServerMessage {
    Request(Request),
    Notification(Notification)
}

#[derive(Debug)]
pub struct Request {
    pub id: usize,
    pub method: Method
}

#[derive(Debug)]
pub enum Notification {
    Initialized,
    Exit,
    Cancel(CancelParams),
    DidChangeTextDocument(DidChangeTextDocumentParams),
    DidChangeWatchedFiles(DidChangeWatchedFilesParams),
    DidOpenTextDocument(DidOpenTextDocumentParams),
    DidSaveTextDocument(DidSaveTextDocumentParams),
    WorkspaceChangeConfiguration(DidChangeConfigurationParams),
}

/// Creates an public enum whose variants all contain a single serializable payload
/// with an automatic json to_string implementation
macro_rules! serializable_enum {
    ($enum_name:ident, $($variant_name:ident($variant_type:ty)),*) => (

        #[derive(Debug)]
        pub enum $enum_name {
            $(
                $variant_name($variant_type),
            )*
        }

        impl fmt::Display for $enum_name {
            fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
                let value = match *self {
                    $(
                        $enum_name::$variant_name(ref value) => serde_json::to_string(value),
                    )*
                }.unwrap();

                write!(f, "{}", value)
            }
        }
    )
}

serializable_enum!(ResponseData,
    Init(InitializeResult),
    SymbolInfo(Vec<SymbolInformation>),
    CompletionItems(Vec<CompletionItem>),
    WorkspaceEdit(WorkspaceEdit),
    TextEdit([TextEdit; 1]),
    Locations(Vec<Location>),
    Highlights(Vec<DocumentHighlight>),
    HoverSuccess(Hover),
    Commands(Vec<Command>),
    Ack(Ack)
);

// Generates the Method enum and parse_message function.
macro_rules! messages {
    (
        methods {
            // $method_arg is really a 0-1 repetition
            $($method_str: pat => $method_name: ident $(($method_arg: ty))*;)*
        }
        notifications {
            $($notif_str: pat => $notif_name: ident $(($notif_arg: ty))*;)*
        }
        $($other_str: pat => $other_expr: expr;)*
    ) => {
        #[derive(Debug)]
        pub enum Method {
            $($method_name$(($method_arg))*,)*
        }

        // Notifications can't return a response, hence why Err is an Option
        fn parse_message(input: &str) -> Result<ServerMessage, Option<jsonrpc::Failure>>  {
            trace!("parse_message `{}`", input);
            let ls_command: serde_json::Value = match serde_json::from_str(input) {
                Ok(value) => value,
                Err(_) => return Err(Some(server_failure(Id::Null, jsonrpc::Error::parse_error()))),
            };

            // Per JSON-RPC/LSP spec, Requests must have id, whereas Notifications can't
            let id = ls_command.get("id").map(|id| serde_json::from_value(id.to_owned()).unwrap());
            // TODO: We only support numeric responses, ideally we should switch from using parsed usize
            // to using jsonrpc_core::Id
            let parsed_numeric_id = match &id {
                &Some(Id::Num(n)) => Some(n as usize),
                &Some(Id::Str(ref s)) => usize::from_str_radix(s, 10).ok(),
                _ => None,
            };

            let params = ls_command.get("params");

            macro_rules! params_as {
                ($ty: ty) => ({
                    let params = match params {
                        Some(value) => value,
                        None => return Err(Some(server_failure(id.unwrap_or(Id::Null), jsonrpc::Error::invalid_request()))),
                    };

                    let method: $ty = match serde_json::from_value(params.to_owned()) {
                        Ok(value) => value,
                        Err(_) => return Err(Some(server_failure(id.unwrap_or(Id::Null),
                            jsonrpc::Error::invalid_params(format!("Expected {}", stringify!($ty)))))),
                    };
                    method
                });
            }

            if let Some(v) = ls_command.get("method") {
                if let Some(name) = v.as_str() {
                    match name {
                        $(
                            $method_str => {
                                match parsed_numeric_id {
                                    Some(id) => Ok(ServerMessage::Request(Request{id, method: Method::$method_name$((params_as!($method_arg)))* })),
                                    None => match id {
                                        None => Err(Some(server_failure(Id::Null, jsonrpc::Error::invalid_request()))),
                                        // FIXME: This behaviour is custom and non conformant to the protocol
                                        Some(id) => Err(Some(server_failure(id,
                                            jsonrpc::Error::invalid_params("Id is not a number or numeric string")))),
                                    }
                                }
                            }
                        )*
                        $(
                            $notif_str => {
                                Ok(ServerMessage::Notification(Notification::$notif_name$((params_as!($notif_arg)))*))
                            }
                        )*
                        $(
                            // If $other_expr is Err, then we need to pass id of actual message we're handling
                            $other_str => { ($other_expr).map_err(|err| id.map(|id| server_failure(id, err))) }
                        )*
                    }
                } else {
                    // Message has a "method" field, so it can be a Notification/Request - if it doesn't have id then we
                    // assume it's a Notification for which we can't return a response, so return Err(None)
                    Err(id.map(|id| server_failure(id, jsonrpc::Error::invalid_request())))
                }
            } else {
                // FIXME: Handle possible client responses to server->client requests (which don't have "method" field)
                Err(Some(server_failure(id.unwrap_or(Id::Null), jsonrpc::Error::invalid_request())))
            }
        }

        // Helper macro that's used to replace optional enum payload with a given tree,
        // allows to give an arbitrary identifier to payload (or `_`) instead of a type.
        #[cfg(test)]
        macro_rules! expand_into {
            ($tt: ty => $target: tt) => ($target)
        }

        #[cfg(test)]
        macro_rules! expand_into_ref {
            ($tt: tt => $target: tt) => (ref $target)
        }

        impl ServerMessage {
            // Returns an LSP method name (e.g. "textDocument/hover")
            // corresponding to the server message type.
            #[cfg(test)]
            pub fn get_method_name(&self) -> &'static str {
                match self {
                    &ServerMessage::Request(ref request) => {
                        match &request.method {
                            $(
                                &Method::$method_name$((expand_into!($method_arg => _)))* => {
                                    concat_idents!(REQUEST__, $method_name)
                                }
                            )*
                        }
                    },
                    &ServerMessage::Notification(ref notification) => {
                        match notification {
                            $(
                                &Notification::$notif_name$((expand_into!($notif_arg => _)))* => {
                                    concat_idents!(NOTIFICATION__, $notif_name)
                                }
                            )*
                        }
                    }
                }
            }

            // Returns a JSON-RPC string representing given message.
            // Effectively an inverse of `parse_message` function.
            #[cfg(test)]
            pub fn to_message_str(&self) -> String {
                match self {
                    &ServerMessage::Request(ref request) => {
                        match &request.method {
                            $(
                                &Method::$method_name$((expand_into_ref!($method_arg => params)))* => {
                                    json!({
                                        "jsonrpc": "2.0",
                                        "id": request.id,
                                        "method": concat_idents!(REQUEST__, $method_name),
                                        $("params": expand_into!($method_arg => params))*

                                    }).to_string()
                                }
                            )*
                        }
                    },
                    &ServerMessage::Notification(ref notification) => {
                        match notification {
                            $(
                                &Notification::$notif_name$((expand_into_ref!($notif_arg => params)))* => {
                                    json!({
                                        "jsonrpc": "2.0",
                                        "method": concat_idents!(NOTIFICATION__, $notif_name),
                                        $("params": expand_into!($notif_arg => params))*
                                    }).to_string()
                                }
                            )*
                        }
                    }
                }
            }
        }
    };
}

messages! {
    methods {
        "shutdown" => Shutdown;
        "initialize" => Initialize(InitializeParams);
        "textDocument/hover" => Hover(TextDocumentPositionParams);
        "textDocument/definition" => GotoDefinition(TextDocumentPositionParams);
        "textDocument/references" => References(ReferenceParams);
        "textDocument/completion" => Completion(TextDocumentPositionParams);
        "textDocument/documentHighlight" => DocumentHighlight(TextDocumentPositionParams);
        // currently, we safely ignore this as a pass-through since we fully handle
        // textDocument/completion.  In the future, we may want to use this method as a
        // way to more lazily fill out completion information
        "completionItem/resolve" => ResolveCompletionItem(CompletionItem);
        "textDocument/documentSymbol" => DocumentSymbols(DocumentSymbolParams);
        "textDocument/rename" => Rename(RenameParams);
        "textDocument/formatting" => Formatting(DocumentFormattingParams);
        "textDocument/rangeFormatting" => RangeFormatting(DocumentRangeFormattingParams);
        "textDocument/codeAction" => CodeAction(CodeActionParams);
        "workspace/executeCommand" => ExecuteCommand(ExecuteCommandParams);
        "rustWorkspace/deglob" => Deglob(Location);
    }
    notifications {
        "initialized" => Initialized;
        "exit" => Exit;
        "textDocument/didChange" => DidChangeTextDocument(DidChangeTextDocumentParams);
        "textDocument/didOpen" => DidOpenTextDocument(DidOpenTextDocumentParams);
        "textDocument/didSave" => DidSaveTextDocument(DidSaveTextDocumentParams);
        "$/cancelRequest" => Cancel(CancelParams);
        "workspace/didChangeConfiguration" => WorkspaceChangeConfiguration(DidChangeConfigurationParams);
        "workspace/didChangeWatchedFiles" => DidChangeWatchedFiles(DidChangeWatchedFilesParams);
    }
    _ => Err(jsonrpc::Error::method_not_found()); // TODO: Handle more possible messages
}

pub struct LsService<O: Output> {
    shut_down: AtomicBool,
    msg_reader: Box<MessageReader + Send + Sync>,
    output: O,
    handler: HandlerState,
}

enum HandlerState {
    Uninit {
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>
    },
    Init(ActionHandler),
}

impl HandlerState {
    fn inited(&self) -> &ActionHandler {
        match *self {
            HandlerState::Uninit { .. } => panic!("Handler not initialized"),
            HandlerState::Init(ref handler) => handler,
        }
    }

    fn init(&mut self, project_path: PathBuf) {
        let handler = match *self {
            HandlerState::Uninit { ref analysis, ref vfs, ref config } => ActionHandler::new(analysis.clone(), vfs.clone(), config.clone(), project_path),
            HandlerState::Init(_) => panic!("Handler already initialized"),
        };
        *self = HandlerState::Init(handler);
    }
}

#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub enum ServerStateChange {
    Continue,
    Break,
}

impl<O: Output> LsService<O> {
    pub fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               config: Arc<Mutex<Config>>,
               reader: Box<MessageReader + Send + Sync>,
               output: O)
               -> LsService<O> {
        LsService {
            shut_down: AtomicBool::new(false),
            msg_reader: reader,
            output: output,
            handler: HandlerState::Uninit { analysis, vfs, config },
        }
    }

    pub fn run(mut self) {
        while self.handle_message() == ServerStateChange::Continue {}
    }

    fn init(&mut self, id: usize, params: InitializeParams) {
        // TODO we should check that the client has the capabilities we require.

        let result = InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncKind::Incremental),
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
                    commands: vec!["rls.applySuggestion".to_owned()],
                }),
                // These are supported if the `unstable_features` option is set.
                // We'll update these capabilities dynamically when we get config
                // info from the client.
                document_range_formatting_provider: Some(false),
                rename_provider: Some(false),

                code_lens_provider: None,
                document_on_type_formatting_provider: None,
                signature_help_provider: None,
            }
        };
        self.output.success(id, ResponseData::Init(result));

        let root_path = params.root_path.map(PathBuf::from).expect("No root path");
        let init_options: Option<InitializationOptions> = params.initialization_options
                              .and_then(|options| serde_json::from_value(options)
                                            .unwrap_or_else(|err| {
                                                debug!("Error parsing initialization_options: {:?}", err);
                                                None
                                            }));
        self.handler.init(root_path);
        self.handler.inited().init(init_options, self.output.clone());
    }

    pub fn handle_message(&mut self) -> ServerStateChange {
        // Allows to delegate message handling to a handler with
        // a default signature or to execute an arbitrary expression
        macro_rules! action {
            (args: { $($args: tt),+ }; action: $name: ident) => {
                self.handler.inited().$name( $($args),+, self.output.clone()  );
            };
            (args: { $($args: tt),* }; $expr: expr) => { $expr };
            (args: { $($args: tt),* }; ) => {};
        }

        macro_rules! trace_params {
            () => { "" };
            ( $($args: tt),* ) => { $($args),* };
        }

        macro_rules! handle {
            (
                message: $message: expr;
                methods {
                    id: $id: ident;
                    // $method_arg is really a 0-1 repetition
                    $($method_name: ident$(($method_arg: ident))* => { $($method_action: tt)* };)*
                }
                notifications {
                    $($notif_name: ident$(($notif_arg: ident))* => { $($notif_action: tt)* };)*
                }
            )
            =>
            {
                match $message {
                    Ok(ServerMessage::Request(Request{id, method})) => {
                        match method {
                            $(
                                Method::$method_name$(($method_arg))* => {
                                    trace!("Handling {} ({}) (params: {:?})", stringify!($method_name), id, trace_params!($($method_arg)*));
                                    // Due to macro hygiene, we need to pass to a nested macro destructured
                                    // id, which will be passed to scope of a possible arbitrary expresion
                                    let $id = id;
                                    action!(args: { $id, { $($method_arg)* } }; $($method_action)*);
                                }
                            ),*
                        }
                    },
                    Ok(ServerMessage::Notification(notification)) => {
                        match notification {
                            $(
                                Notification::$notif_name$(($notif_arg))* => {
                                    trace!("Handling {} (params: {:?})", stringify!($notif_name), trace_params!($($notif_arg)*));
                                    action!(args: { $($notif_arg)* }; $($notif_action)*);
                                }
                            ),*
                        }
                    },
                    Err(e) => {
                        trace!("parsing invalid message: {:?}", e);
                        if let Some(failure) = e {
                            self.output.failure(failure.id, failure.error);
                        }
                    },
                }
            };
        }

        let message = match self.msg_reader.parsed_message() {
            Some(m) => m,
            None => {
                self.output.failure(Id::Null, jsonrpc::Error::parse_error());
                return ServerStateChange::Break
            },
        };

        {
            let shut_down = self.shut_down.load(Ordering::SeqCst);
            if shut_down {
                if let Ok(ServerMessage::Notification(Notification::Exit)) = message {
                } else {
                    // We're shutdown, ignore any messages other than 'exit'. This is not actually
                    // in the spec, I'm not sure we should do this, but it kinda makes sense.
                    return ServerStateChange::Continue;
                }
            }
        }

        handle! {
            message: message;
            methods {
                id: id;
                Shutdown => {{
                    self.shut_down.store(true, Ordering::SeqCst);
                    self.output.success(id, ResponseData::Ack(Ack));
                }};
                Initialize(params) => { self.init(id, params) };
                Hover(params) => { action: hover };
                GotoDefinition(params) => { action: goto_def };
                References(params) => { action: find_all_refs };
                Completion(params) => { action: complete };
                DocumentHighlight(params) => { action: highlight };
                ResolveCompletionItem(params) => {
                    self.output.success(id, ResponseData::CompletionItems(vec![params]))
                };
                DocumentSymbols(params) => { action: symbols };
                Rename(params) => { action: rename };
                Formatting(params) => {
                    self.handler.inited().reformat(id, params.text_document, None, self.output.clone(), &params.options)
                };
                RangeFormatting(params) => {
                    self.handler.inited().reformat(id, params.text_document, Some(params.range), self.output.clone(), &params.options)
                };
                Deglob(params) => { action: deglob };
                ExecuteCommand(params) => { action: execute_command };
                CodeAction(params) => { action: code_action };
            }
            notifications {
                Initialized => {{ self.handler.inited().initialized(self.output.clone()) }};
                Exit => {{
                    let shut_down = self.shut_down.load(Ordering::SeqCst);
                    ::std::process::exit(if shut_down { 0 } else { 1 });
                }};
                Cancel(params) => {};
                DidChangeTextDocument(change) => { action: on_change };
                DidOpenTextDocument(open) => { action: on_open };
                DidSaveTextDocument(save) => { action: on_save };
                WorkspaceChangeConfiguration(params) => { action: on_change_config };
                DidChangeWatchedFiles(_params) => {{
                    // We only subscribe to notifications about changes to Cargo.toml/lock.
                    // If we get a notification about something else, this is probably incorrect
                    // behviour, but it is the clients fault.
                    self.handler.inited().on_cargo_change(self.output.clone())
                }};
            }
        };
        ServerStateChange::Continue
    }
}

pub trait MessageReader {
    fn read_message(&self) -> Option<String> {
        None
    }

    fn parsed_message(&self) -> Option<Result<ServerMessage, Option<jsonrpc::Failure>>> {
        self.read_message().map(|m| parse_message(&m))
    }
}

struct StdioMsgReader;

impl MessageReader for StdioMsgReader {
    fn read_message(&self) -> Option<String> {
        macro_rules! handle_err {
            ($e: expr, $s: expr) => {
                match $e {
                    Ok(x) => x,
                    Err(_) => {
                        debug!($s);
                        return None;
                    }
                }
            }
        }

        // Read in the "Content-length: xx" part
        let mut buffer = String::new();
        handle_err!(io::stdin().read_line(&mut buffer), "Could not read from stdin");

        if buffer.is_empty() {
            info!("Header is empty");
            return None;
        }

        let res: Vec<&str> = buffer.split(' ').collect();

        // Make sure we see the correct header
        if res.len() != 2 {
            info!("Header is malformed");
            return None;
        }

        if res[0].to_lowercase() != "content-length:" {
            info!("Header is missing 'content-length'");
            return None;
        }

        let size = handle_err!(usize::from_str_radix(&res[1].trim(), 10), "Couldn't read size");
        trace!("reading: {} bytes", size);

        // Skip the new lines
        let mut tmp = String::new();
        handle_err!(io::stdin().read_line(&mut tmp), "Could not read from stdin");

        let mut content = vec![0; size];
        handle_err!(io::stdin().read_exact(&mut content), "Could not read from stdin");

        let content = handle_err!(String::from_utf8(content), "Non-utf8 input");

        Some(content)
    }
}

pub trait Output: Sync + Send + Clone + 'static {
    fn response(&self, output: String);
    fn provide_id(&self) -> u32;

    fn failure(&self, id: jsonrpc::Id, error: jsonrpc::Error) {
        let response = response::Failure {
            jsonrpc: Some(version::Version::V2),
            id: id,
            error: error
        };

        self.response(serde_json::to_string(&response).unwrap());
    }

    fn failure_message<M: Into<String>>(&self, id: usize, code: jsonrpc::ErrorCode, msg: M) {
        let error = jsonrpc::Error {
            code: code,
            message: msg.into(),
            data: None
        };
        self.failure(Id::Num(id as u64), error);
    }

    fn success(&self, id: usize, data: ResponseData) {
        // {
        //     jsonrpc: String,
        //     id: usize,
        //     result: String,
        // }
        let output = format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}", id, data);

        self.response(output);
    }

    fn notify(&self, message: &str) {
        let output = serde_json::to_string(
            &NotificationMessage::new(message.to_owned(), ())
        ).unwrap();
        self.response(output);
    }
}

#[derive(Clone)]
struct StdioOutput {
    next_id: Arc<AtomicU32>,
}

impl StdioOutput {
    pub fn new() -> StdioOutput {
        StdioOutput {
            next_id: Arc::new(AtomicU32::new(1)),
        }
    }
}

impl Output for StdioOutput {
    fn response(&self, output: String) {
        let o = format!("Content-Length: {}\r\n\r\n{}", output.len(), output);

        trace!("response: {:?}", o);

        print!("{}", o);
        io::stdout().flush().unwrap();
    }

    fn provide_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>) {
    debug!("Language Server Starting up");
    let service = LsService::new(analysis,
                                 vfs,
                                 Arc::new(Mutex::new(Config::default())),
                                 Box::new(StdioMsgReader),
                                 StdioOutput::new());
    LsService::run(service);
    debug!("Server shutting down");
}

#[cfg(test)]
mod test {
    use url::Url;
    use server::*;
    use std::str::FromStr;

    #[test]
    fn server_message_get_method_name() {
        let test_url = Url::from_str("http://testurl").expect("Couldn't parse test URI");

        let request_shut = ServerMessage::request(1, Method::Shutdown);
        assert_eq!(request_shut.get_method_name(), "shutdown");

        let request_init = ServerMessage::initialize(1, None);
        assert_eq!(request_init.get_method_name(), "initialize");

        let request_hover = ServerMessage::request(1, Method::Hover(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: test_url.clone() },
            position: Position { line: 0, character: 0 },
        }));
        assert_eq!(request_hover.get_method_name(), "textDocument/hover");


        let request_resolve = ServerMessage::request(1, Method::ResolveCompletionItem(
            CompletionItem::new_simple("label".to_owned(), "detail".to_owned())
        ));
        assert_eq!(request_resolve.get_method_name(), "completionItem/resolve");

        let notif_exit = ServerMessage::Notification(Notification::Exit);
        assert_eq!(notif_exit.get_method_name(), "exit");

        let notif_change = ServerMessage::Notification(Notification::DidChangeTextDocument(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier { uri: test_url.clone(), version: 1 },
            content_changes: vec![],
        }));
        assert_eq!(notif_change.get_method_name(), "textDocument/didChange");

        let notif_cancel = ServerMessage::Notification(Notification::Cancel(CancelParams {
            id: NumberOrString::Number(1)
        }));
        assert_eq!(notif_cancel.get_method_name(), "$/cancelRequest");
    }

    #[test]
    fn server_message_to_str() {
        let request = ServerMessage::request(1, Method::Shutdown);
        let request_json: serde_json::Value = serde_json::from_str(&request.to_message_str()).unwrap();
        let expected_json = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": request.get_method_name()
        });
        assert_eq!(request_json, expected_json);

        //println!("{0}", request_json);

        let test_url = Url::from_str("http://testurl").expect("Couldn't parse test URI");
        let request = ServerMessage::request(2, Method::Hover(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: test_url.clone() },
            position: Position { line: 0, character: 0 },
        }));
        let request_json: serde_json::Value = serde_json::from_str(&request.to_message_str()).unwrap();
        assert_eq!(request_json.get("jsonrpc").unwrap().as_str().unwrap(), "2.0");
        assert_eq!(request_json.get("id").unwrap().as_i64().unwrap(), 2);
        assert_eq!(request_json.get("method").unwrap().as_str().unwrap(), "textDocument/hover");
        let request_params = request_json.get("params").unwrap();
        let expected_params = json!({
            "textDocument": TextDocumentIdentifier::new(test_url.clone()),
            "position": Position {line: 0, character: 0 }
        });
        assert_eq!(request_params, &expected_params);
    }
}
