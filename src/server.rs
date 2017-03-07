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

use build::*;
use lsp_data::*;
use actions::ActionHandler;

use std::fmt;
use std::io::{self, Read, Write, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::path::PathBuf;


#[derive(Debug, Serialize)]
pub struct Ack {}

#[derive(Debug, new)]
struct ParseError {
    kind: ErrorKind,
    message: &'static str,
    id: Option<usize>,
}

#[derive(Debug)]
enum ServerMessage {
    Request(Request),
    Notification(Notification)
}

#[derive(Debug)]
struct Request {
    id: usize,
    method: Method
}

#[derive(Debug)]
enum Notification {
    Exit,
    CancelRequest(CancelParams),
    Change(DidChangeTextDocumentParams),
    Open(DidOpenTextDocumentParams),
    Save(DidSaveTextDocumentParams),
}

/// Creates an public enum whose variants all contain a single serializable payload
/// with an automatic json to_string implementation
macro_rules! serializable_enum {
    ($enum_name:ident, $($variant_name:ident($variant_type:ty)),*) => (

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
        enum Method {
            $($method_name$(($method_arg))*,)*
        }
        fn parse_message(input: &str) -> Result<ServerMessage, ParseError>  {
            let ls_command: serde_json::Value = serde_json::from_str(input).unwrap();

            let params = ls_command.get("params");

            macro_rules! params_as {
                ($ty: ty) => ({
                    let method: $ty =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    method
                });
            }
            macro_rules! id {
                () => ((ls_command.get("id").map(|id| id.as_u64().unwrap() as usize)));
            }

            if let Some(v) = ls_command.get("method") {
                if let Some(name) = v.as_str() {
                    match name {
                        $(
                            $method_str => {
                                let id = ls_command.get("id").unwrap().as_u64().unwrap() as usize;
                                Ok(ServerMessage::Request(Request{id: id, method: Method::$method_name$((params_as!($method_arg)))* }))
                            }
                        )*
                        $(
                            $notif_str => {
                                Ok(ServerMessage::Notification(Notification::$notif_name$((params_as!($notif_arg)))*))
                            }
                        )*
                        $(
                            $other_str => $other_expr,
                        )*
                    }
                } else {
                    Err(ParseError::new(ErrorKind::InvalidData, "Method is not a string", id!()))
                }
            } else {
                Err(ParseError::new(ErrorKind::InvalidData, "Method not found", id!()))
            }
        }
    };
}

messages! {
    methods {
        "shutdown" => Shutdown;
        "initialize" => Initialize(InitializeParams);
        "textDocument/hover" => Hover(TextDocumentPositionParams);
        "textDocument/definition" => GotoDef(TextDocumentPositionParams);
        "textDocument/references" => FindAllRef(ReferenceParams);
        "textDocument/completion" => Complete(TextDocumentPositionParams);
        "textDocument/documentHighlight" => Highlight(TextDocumentPositionParams);
        // currently, we safely ignore this as a pass-through since we fully handle
        // textDocument/completion.  In the future, we may want to use this method as a
        // way to more lazily fill out completion information
        "completionItem/resolve" => CompleteResolve(CompletionItem);
        "textDocument/documentSymbol" => Symbols(DocumentSymbolParams);
        "textDocument/rename" => Rename(RenameParams);
        "textDocument/formatting" => Reformat(DocumentFormattingParams);
        "textDocument/rangeFormatting" => ReformatRange(DocumentRangeFormattingParams);
    }
    notifications {
        "exit" => Exit;
        "textDocument/didChange" => Change(DidChangeTextDocumentParams);
        "textDocument/didOpen" => Open(DidOpenTextDocumentParams);
        "textDocument/didSave" => Save(DidSaveTextDocumentParams);
        "$/cancelRequest" => CancelRequest(CancelParams);
    }
    // TODO handle me
    "$/setTraceNotification" => Err(ParseError::new(ErrorKind::InvalidData, "setTraceNotification", None));
    // TODO handle me
    "workspace/didChangeConfiguration" => Err(ParseError::new(ErrorKind::InvalidData, "didChangeConfiguration", None));
    _ => Err(ParseError::new(ErrorKind::InvalidData, "Unknown command", id!()));
}

pub struct LsService {
    shut_down: AtomicBool,
    msg_reader: Box<MessageReader + Sync + Send>,
    output: Box<Output + Sync + Send>,
    handler: ActionHandler,
}

#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub enum ServerStateChange {
    Continue,
    Break,
}

impl LsService {
    pub fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               build_queue: Arc<BuildQueue>,
               reader: Box<MessageReader + Send + Sync>,
               output: Box<Output + Send + Sync>)
               -> Arc<LsService> {
        Arc::new(LsService {
            shut_down: AtomicBool::new(false),
            msg_reader: reader,
            output: output,
            handler: ActionHandler::new(analysis, vfs, build_queue),
        })
    }

    pub fn run(this: Arc<Self>) {
        while LsService::handle_message(this.clone()) == ServerStateChange::Continue {}
    }

    fn init(&self, id: usize, init: InitializeParams) {
        let result = InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncKind::Incremental),
                hover_provider: Some(true),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(true),
                    trigger_characters: vec![".".to_string(), ":".to_string()],
                }),
                // TODO
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec![]),
                }),
                definition_provider: Some(true),
                references_provider: Some(true),
                document_highlight_provider: Some(true),
                document_symbol_provider: Some(true),
                workspace_symbol_provider: Some(true),
                code_action_provider: Some(false),
                // TODO maybe?
                code_lens_provider: None,
                document_formatting_provider: Some(true),
                document_range_formatting_provider: Some(true),
                document_on_type_formatting_provider: None, // TODO: review this, maybe add?
                rename_provider: Some(true),
            }
        };
        self.output.success(id, ResponseData::Init(result));
        let root_path = init.root_path.map(PathBuf::from);
        if let Some(root_path) = root_path {
            self.handler.init(root_path, &*self.output);
        }
    }

    pub fn handle_message(this: Arc<Self>) -> ServerStateChange {
        let c = match this.msg_reader.read_message() {
            Some(c) => c,
            None => {
                this.output.parse_error();
                return ServerStateChange::Break
            },
        };

        let this = this.clone();
        thread::spawn(move || {
            // FIXME(45) refactor to generate this match.
            let message = parse_message(&c);
            {
                let shut_down = this.shut_down.load(Ordering::SeqCst);
                if shut_down {
                    if let Ok(ServerMessage::Notification(Notification::Exit)) = message {
                    } else {
                        // We're shutdown, ignore any messages other than 'exit'. This is not actually
                        // in the spec, I'm not sure we should do this, but it kinda makes sense.
                        return;
                    }
                }
            }
            match message {
                Ok(ServerMessage::Notification(method)) => {
                    match method {
                        Notification::Exit => {
                            trace!("exiting...");
                            let shut_down = this.shut_down.load(Ordering::SeqCst);
                            ::std::process::exit(if shut_down { 0 } else { 1 });
                        }
                        Notification::CancelRequest(params) => {
                            trace!("request to cancel {:?}", params.id);
                        }
                        Notification::Change(change) => {
                            trace!("notification(change): {:?}", change);
                            this.handler.on_change(change, &*this.output);
                        }
                        Notification::Open(open) => {
                            trace!("notification(open): {:?}", open);
                            this.handler.on_open(open, &*this.output);
                        }
                        Notification::Save(save) => {
                            trace!("notification(save): {:?}", save);
                            this.handler.on_save(save, &*this.output);
                        }
                    }
                }
                Ok(ServerMessage::Request(Request{id, method})) => {
                    match method {
                        Method::Initialize(init) => {
                            trace!("command(init): {:?}", init);
                            this.init(id, init);
                        }
                        Method::Shutdown => {
                            trace!("shutting down...");
                            this.shut_down.store(true, Ordering::SeqCst);

                            let out = &*this.output;
                            out.success(id, ResponseData::Ack(Ack {}));
                        }
                        Method::Hover(params) => {
                            trace!("command(hover): {:?}", params);
                            this.handler.hover(id, params, &*this.output);
                        }
                        Method::GotoDef(params) => {
                            trace!("command(goto): {:?}", params);
                            this.handler.goto_def(id, params, &*this.output);
                        }
                        Method::Complete(params) => {
                            trace!("command(complete): {:?}", params);
                            this.handler.complete(id, params, &*this.output);
                        }
                        Method::CompleteResolve(params) => {
                            trace!("command(complete): {:?}", params);
                            this.output.success(id, ResponseData::CompletionItems(vec![params]))
                        }
                        Method::Highlight(params) => {
                            trace!("command(highlight): {:?}", params);
                            this.handler.highlight(id, params, &*this.output);
                        }
                        Method::Symbols(params) => {
                            trace!("command(goto): {:?}", params);
                            this.handler.symbols(id, params, &*this.output);
                        }
                        Method::FindAllRef(params) => {
                            trace!("command(find_all_refs): {:?}", params);
                            this.handler.find_all_refs(id, params, &*this.output);
                        }
                        Method::Rename(params) => {
                            trace!("command(rename): {:?}", params);
                            this.handler.rename(id, params, &*this.output);
                        }
                        Method::Reformat(params) => {
                            // FIXME take account of options.
                            trace!("command(reformat): {:?}", params);
                            this.handler.reformat(id, params.text_document, &*this.output);
                        }
                        Method::ReformatRange(params) => {
                            // FIXME reformats the whole file, not just a range.
                            // FIXME take account of options.
                            trace!("command(reformat range): {:?}", params);
                            this.handler.reformat(id, params.text_document, &*this.output);
                        }
                    }
                }
                Err(e) => {
                    trace!("parsing invalid message: {:?}", e);
                    if let Some(id) = e.id {
                        this.output.failure(id, "Unsupported message");
                    }
                },
            }
        });
        ServerStateChange::Continue
    }
}

pub trait MessageReader {
    fn read_message(&self) -> Option<String>;
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

        if res[0] == "Content-length:" {
            info!("Header is missing 'Content-length'");
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

pub trait Output {
    fn response(&self, output: String);

    fn parse_error(&self) {
        self.response(r#"{"jsonrpc": "2.0", "error": {"code": -32700, "message": "Parse error"}, "id": null}"#.to_owned());
    }

    fn failure(&self, id: usize, message: &str) {
        // For now this is a catch-all for any error back to the consumer of the RLS
        const METHOD_NOT_FOUND: i64 = -32601;

        #[derive(Serialize)]
        struct ResponseError {
            code: i64,
            message: String
        }

        #[derive(Serialize)]
        struct ResponseFailure {
            jsonrpc: &'static str,
            id: usize,
            error: ResponseError,
        }

        let rf = ResponseFailure {
            jsonrpc: "2.0",
            id: id,
            error: ResponseError {
                code: METHOD_NOT_FOUND,
                message: message.to_owned(),
            },
        };
        let output = serde_json::to_string(&rf).unwrap();
        self.response(output);
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

struct StdioOutput;

impl Output for StdioOutput {
    fn response(&self, output: String) {
        let o = format!("Content-Length: {}\r\n\r\n{}", output.len(), output);

        debug!("response: {:?}", o);

        print!("{}", o);
        io::stdout().flush().unwrap();
    }
}

pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>, build_queue: Arc<BuildQueue>) {
    debug!("Language Server Starting up");
    let service = LsService::new(analysis,
                                 vfs,
                                 build_queue,
                                 Box::new(StdioMsgReader),
                                 Box::new(StdioOutput));
    LsService::run(service);
    debug!("Server shutting down");
}
