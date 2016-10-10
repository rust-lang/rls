extern crate serde;
extern crate serde_json;
extern crate racer;
extern crate rustfmt;

use analysis::{AnalysisHost, Span};
use vfs::{Vfs, Change};
use build::*;
use std::sync::Arc;
use std::path::Path;

use self::racer::core::complete_from_file;
use self::racer::core::find_definition;
use self::racer::core;
use self::rustfmt::{Input as FmtInput, format_input};
use self::rustfmt::config::{self, WriteMode};

use std::fs::{File, OpenOptions};
use std::fmt::Debug;
use std::panic;
use serde::{Serialize, Deserialize};
use ide::VscodeKind;

use std::io::{self, Read, Write, Error, ErrorKind};
use std::thread;
use std::time::Duration;

// Timeout = 0.5s (totally arbitrary).
const RUSTW_TIMEOUT: u64 = 500;

// For now this is a catch-all for any error back to the consumer of the RLS
const MethodNotFound: i64 = -32601;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Position {
    line: usize,
    character: usize
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Range {
    start: Position,
    end: Position,
}

#[derive(Debug, Deserialize, Serialize)]
struct Location {
    uri: String,
    range: Range,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct InitializeParams {
    processId: usize,
    rootPath: String
}

#[derive(Debug, Deserialize)]
struct Document {
    uri: String
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct VersionedTextDocumentIdentifier {
    version: u64,
    uri: String
}

// FIXME: range here is technically optional, but I don't know why
#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct TextDocumentContentChangeEvent {
    range: Range,
    rangeLength: Option<u32>,
    text: String
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct ReferenceContext {
    includeDeclaration: bool,
}

#[derive(Debug, Serialize)]
struct SymbolInformation {
    name: String,
    kind: u32,
    location: Location,
}

#[derive(Debug, Deserialize)]
struct CompilerMessageCode {
    code: String
}

#[derive(Debug, Deserialize)]
struct CompilerSpan {
    file_name: String,
    line_start: usize,
    column_start: usize,
    line_end: usize,
    column_end: usize,
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    message: String,
    code: Option<CompilerMessageCode>,
    level: String,
    spans: Vec<CompilerSpan>,
}

#[derive(Debug, Clone, Serialize)]
struct Diagnostic {
    range: Range,
    severity: u32,
    code: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct PublishDiagnosticsParams {
    uri: String,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Serialize)]
struct NotificationMessage<T> where T: Debug+Serialize {
    jsonrpc: String,
    method: String,
    params: T,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct ReferenceParams {
    textDocument: Document,
    position: Position,
    context: ReferenceContext,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct TextDocumentPositionParams {
    textDocument: Document,
    position: Position,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct ChangeParams {
    textDocument: VersionedTextDocumentIdentifier,
    contentChanges: Vec<TextDocumentContentChangeEvent>
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct HoverParams {
    textDocument: Document,
    position: Position
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct DocumentSymbolParams {
    textDocument: Document,
}

#[derive(Debug, Deserialize)]
struct CancelParams {
    id: usize
}


#[derive(Debug)]
enum Method {
    Shutdown,
    Initialize (InitializeParams),
    Hover (HoverParams),
    GotoDef (TextDocumentPositionParams),
    FindAllRef (ReferenceParams),
    Symbols (DocumentSymbolParams),
    Complete (TextDocumentPositionParams),
}

#[derive(Debug, Serialize)]
enum DocumentSyncKind {
    None = 0,
    Full = 1,
    Incremental = 2,
}

#[derive(Debug)]
struct Request {
    id: usize,
    method: Method
}

#[derive(Debug, Serialize)]
struct MarkedString {
    language: String,
    value: String
}

#[derive(Debug, Serialize)]
struct HoverSuccessContents {
    contents: Vec<MarkedString>
}

#[derive(Debug, Serialize)]
struct InitializeCapabilities {
    capabilities: ServerCapabilities
}

#[derive(Debug, Serialize)]
struct CompletionItem {
    label: String,
    detail: String,
}

#[derive(Debug, Serialize)]
struct ResponseSuccess<T> where T:Debug+Serialize {
    jsonrpc: String,
    id: usize,
    result: T,
}

// INTERNAL STRUCT
#[derive(Debug, Serialize)]
struct ResponseError {
    code: i64,
    message: String
}

#[derive(Debug, Serialize)]
struct ResponseFailure {
    jsonrpc: String,
    id: usize,
    error: ResponseError,
}

#[derive(Debug)]
enum Notification {
    CancelRequest(usize),
    Change(ChangeParams),
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
struct CompletionOptions {
    resolveProvider: bool,
    triggerCharacters: Vec<String>,
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
struct SignatureHelpOptions {
    triggerCharacters: Vec<String>,
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
struct CodeLensOptions {
    resolveProvider: bool,
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
struct ServerCapabilities {
    textDocumentSync: usize,
    hoverProvider: bool,
    completionProvider: CompletionOptions,
    signatureHelpProvider: SignatureHelpOptions,
    definitionProvider: bool,
    referencesProvider: bool,
    documentHighlightProvider: bool,
    documentSymbolProvider: bool,
    workshopSymbolProvider: bool,
    codeActionProvider: bool,
    codeLensProvider: bool,
    documentFormattingProvider: bool,
    documentRangeFormattingProvider: bool,
    //documentOnTypeFormattingProvider
    renameProvider: bool,
}

#[derive(Debug)]
enum ServerMessage {
    Request (Request),
    Notification (Notification)
}

// TODO error type is gross
fn parse_message(input: &str) -> Result<ServerMessage, (ErrorKind, &'static str, usize)>  {
    let ls_command: serde_json::Value = serde_json::from_str(input).unwrap();

    let params = ls_command.lookup("params");

    if let Some(v) = ls_command.lookup("method") {
        if let Some(name) = v.as_str() {
            match name {
                "shutdown" => {
                    let id = ls_command.lookup("id").unwrap().as_u64().unwrap() as usize;
                    Ok(ServerMessage::Request(Request{id: id, method: Method::Shutdown }))
                }
                "initialize" => {
                    let id = ls_command.lookup("id").unwrap().as_u64().unwrap() as usize;
                    let method: InitializeParams =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    Ok(ServerMessage::Request(Request{id: id, method: Method::Initialize(method)}))
                }
                "textDocument/hover" => {
                    let id = ls_command.lookup("id").unwrap().as_u64().unwrap() as usize;
                    let method: HoverParams =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    Ok(ServerMessage::Request(Request{id: id, method: Method::Hover(method)}))
                }
                "textDocument/didChange" => {
                    let method: ChangeParams =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    Ok(ServerMessage::Notification(Notification::Change(method)))
                }
                "textDocument/didOpen" => {
                    // TODO handle me
                    Err((ErrorKind::InvalidData, "didOpen", 0))
                }
                "textDocument/definition" => {
                    let id = ls_command.lookup("id").unwrap().as_u64().unwrap() as usize;
                    let method: TextDocumentPositionParams =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    Ok(ServerMessage::Request(Request{id: id, method: Method::GotoDef(method)}))
                }
                "textDocument/references" => {
                    let id = ls_command.lookup("id").unwrap().as_u64().unwrap() as usize;
                    let method: ReferenceParams =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    Ok(ServerMessage::Request(Request{id: id, method: Method::FindAllRef(method)}))
                }
                "textDocument/completion" => {
                    let id = ls_command.lookup("id").unwrap().as_u64().unwrap() as usize;
                    let method: TextDocumentPositionParams =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    Ok(ServerMessage::Request(Request{id: id, method: Method::Complete(method)}))
                }
                "textDocument/documentSymbol" => {
                    let id = ls_command.lookup("id").unwrap().as_u64().unwrap() as usize;
                    let method: DocumentSymbolParams =
                        serde_json::from_value(params.unwrap().to_owned()).unwrap();
                    Ok(ServerMessage::Request(Request{id: id, method: Method::Symbols(method)}))
                }
                "$/cancelRequest" => {
                    let params: CancelParams = serde_json::from_value(params.unwrap().to_owned())
                                               .unwrap();
                    Ok(ServerMessage::Notification(Notification::CancelRequest(params.id)))
                }
                "$/setTraceNotification" => {
                    // TODO handle me
                    Err((ErrorKind::InvalidData, "setTraceNotification", 0))
                }
                "workspace/didChangeConfiguration" => {
                    // TODO handle me
                    Err((ErrorKind::InvalidData, "didChangeConfiguration", 0))
                }
                _ => {
                    let id = ls_command.lookup("id").map(|id| id.as_u64().unwrap()).unwrap_or(0) as usize;
                    Err((ErrorKind::InvalidData, "Unknown command", id))
                }
            }
        }
        else {
            let id = ls_command.lookup("id").map(|id| id.as_u64().unwrap()).unwrap_or(0) as usize;
            Err((ErrorKind::InvalidData, "Method is not a string", id))
        }
    }
    else {
        let id = ls_command.lookup("id").map(|id| id.as_u64().unwrap()).unwrap_or(0) as usize;
        Err((ErrorKind::InvalidData, "Method not found", id))
    }
}

fn log(msg: String) {
    // let mut log = OpenOptions::new().append(true)
    //                                 .write(true)
    //                                 .create(true)
    //                                 .open("tmp/rls_log.txt").unwrap();
    // log.write_all(&format!("{}", msg).into_bytes()).unwrap();

    writeln!(::std::io::stderr(), "{}", msg);
}

fn output_response(output: String) {
    use std::io;
    let o = format!("Content-Length: {}\r\n\r\n{}", output.len(), output);

    log(format!("OUTPUT: {:?}", o));
    print!("{}", o);
    io::stdout().flush().unwrap();
}

#[derive(Clone)]
struct LSService {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    build_queue: Arc<BuildQueue>,
    current_project: Option<String>,
}

impl LSService {
    fn build(&self, project_path: &str, priority: BuildPriority) {
        let result = self.build_queue.request_build(project_path, priority);
        match result {
            BuildResult::Success(ref x) | BuildResult::Failure(ref x) => {
                let result: Vec<Diagnostic> = x.iter().filter_map(|msg| {
                    match serde_json::from_str::<CompilerMessage>(&msg) {
                        Ok(method) => {
                            if method.spans.is_empty() {
                                return None;
                            }
                            let diag = Diagnostic {
                                range: Range {
                                    start: Position {
                                        line: method.spans[0].line_start,
                                        character: method.spans[0].column_start,
                                    },
                                    end: Position {
                                        line: method.spans[0].line_end,
                                        character: method.spans[0].column_end,
                                    }
                                },
                                severity: if method.level == "error" { 1 } else { 2 },
                                code: match method.code {
                                    Some(c) => c.code.clone(),
                                    None => String::new(),
                                },
                                message: method.message.clone(),
                            };
                            let out = NotificationMessage {
                                jsonrpc: "2.0".into(),
                                method: "textDocument/publishDiagnostics".to_string(),
                                params: PublishDiagnosticsParams {
                                    uri: "file://".to_string() + &method.spans[0].file_name,
                                    diagnostics: vec![diag.clone()]
                                }
                            };
                            let output = serde_json::to_string(&out).unwrap();
                            output_response(output);
                            Some(diag)
                        }
                        Err(e) => {
                            log(format!("<<ERROR>> {:?}", e));
                            log(format!("<<FROM>> {}", msg));
                            None
                        }
                    }
                }).collect();

                //let reply = serde_json::to_string(&result).unwrap();
                // println!("build result: {:?}", result);
                //log(format!("build result: {:?}", result));

                log(format!("reload analysis: {}", project_path));
                self.analysis.reload(&project_path).unwrap();
            }
            BuildResult::Squashed => {},
            BuildResult::Err => {},
        }
    }

    fn convert_pos_to_span(&self, doc: Document, pos: Position) -> Option<Span> {
        let fname: String = doc.uri.chars().skip("file://".len()).collect();
        log(format!("\nWorking on: {:?} {:?}", fname, pos));
        let line = self.vfs.get_line(Path::new(&fname), pos.line);
        log(format!("\nGOT LINE: {:?}", line));
        let start_pos = {
            let mut tmp = Position { line: pos.line, character: 1 };
            for (i, c) in line.clone().unwrap().chars().enumerate() {
                if !(c.is_alphanumeric() || c == '_') {
                    tmp.character = i + 1;
                }
                if i == pos.character {
                    break;
                }
            }
            tmp
        };

        let end_pos = {
            let mut tmp = Position { line: pos.line, character: pos.character };
            for (i, c) in line.unwrap().chars().skip(pos.character).enumerate() {
                if !(c.is_alphanumeric() || c == '_') {
                    break;
                }
                tmp.character = i + pos.character + 1;
            }
            tmp
        };

        let span = Span {
            file_name: fname,
            line_start: start_pos.line,
            column_start: start_pos.character,
            line_end: end_pos.line,
            column_end: end_pos.character,
        };

        Some(span)
    }

    fn symbols(&self, id: usize, doc: DocumentSymbolParams) {
        let t = thread::current();
        let file_name: String = doc.textDocument.uri.chars().skip("file://".len()).collect();
        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let symbols = analysis.symbols(&file_name).unwrap_or(vec![]);
            t.unpark();

            symbols.into_iter().map(|s| {
                SymbolInformation {
                    name: s.name,
                    kind: VscodeKind::from(s.kind) as u32,
                    location: Location {
                        uri: "file://".to_string() + &s.span.file_name,
                        range: Range {
                            start: Position {
                                line: s.span.line_start,
                                character: s.span.column_start,
                            },
                            end: Position {
                                line: s.span.line_end,
                                character: s.span.column_end,
                            },
                        }
                    }
                }
            }).collect()
        });

        thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

        let result = rustw_handle.join().unwrap_or(vec![]);

        let out = ResponseSuccess {
            jsonrpc: "2.0".into(),
            id: id,
            result: result
        };

        let output = serde_json::to_string(&out).unwrap();
        output_response(output);
    }

    fn complete(&self, id: usize, params: TextDocumentPositionParams) {
        fn adjust_vscode_pos_for_racer(mut source: Position) -> Position {
            source.line += 1;
            source
        }

        fn adjust_racer_pos_for_vscode(mut source: Position) -> Position {
            if source.line > 0 {
                source.line -= 1;
            }
            source
        }

        let vfs: &Vfs = &self.vfs;

        let pos = adjust_vscode_pos_for_racer(params.position);
        let fname: String = params.textDocument.uri.chars().skip("file://".len()).collect();
        let file_path = &Path::new(&fname);

        let result: Vec<CompletionItem> = panic::catch_unwind(move || {

            let cache = core::FileCache::new();
            let session = core::Session::from_path(&cache, file_path, file_path);
            for (path, txt) in vfs.get_changed_files() {
                session.cache_file_contents(&path, txt);
            }

            let src = session.load_file(file_path);

            let pos = session.load_file(file_path).coords_to_point(pos.line, pos.character).unwrap();
            let results = complete_from_file(&src.code, file_path, pos, &session);

            results.map(|comp| CompletionItem {
                label: comp.matchstr.clone(),
                detail: comp.contextstr.clone(),
            }).collect()
        }).unwrap_or(vec![]);

        let out = ResponseSuccess {
            jsonrpc: "2.0".into(),
            id: id,
            result: result
        };

        let output = serde_json::to_string(&out).unwrap();
        output_response(output);
    }

    fn find_all_refs(&self, id: usize, params: ReferenceParams) {
        let t = thread::current();
        let uri = params.textDocument.uri.clone();
        let span = self.convert_pos_to_span(params.textDocument, params.position).unwrap();
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

        let mut result = rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]);
        let refs: Vec<Location> = result.iter().map(|item| {
            Location {
                uri: "file://".to_string() + &item.file_name,
                range: Range {
                    start: Position {
                        line: item.line_start,
                        character: item.column_start,
                    },
                    end: Position {
                        line: item.line_end,
                        character: item.column_end,
                    },
                }
            }
        }).collect();

        let out = ResponseSuccess {
            jsonrpc: "2.0".into(),
            id: id,
            result: refs
        };

        let output = serde_json::to_string(&out).unwrap();
        output_response(output);
    }

    fn goto_def(&self, id: usize, params: TextDocumentPositionParams) {
        // Save-analysis thread.
        let t = thread::current();
        let uri = params.textDocument.uri.clone();
        let span = self.convert_pos_to_span(params.textDocument, params.position).unwrap();
        let analysis = self.analysis.clone();
        let results = thread::spawn(move || {
            let result = if let Ok(s) = analysis.goto_def(&span) {
                vec![Location {
                    uri: "file://".to_string() + &s.file_name,
                    range: Range {
                        start: Position {
                            line: s.line_start,
                            character: s.column_start,
                        },
                        end: Position {
                            line: s.line_start,
                            character: s.column_start,
                        },
                    }
                }]
            } else {
                vec![]
            };

            t.unpark();

            result
        });
        thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

        let results = results.join();
        match results {
            Ok(r) => {
                let out = ResponseSuccess {
                    jsonrpc: "2.0".into(),
                    id: id,
                    result: r
                };
                log(format!("\nGOING TO: {:?}\n", out));

                let output = serde_json::to_string(&out).unwrap();
                output_response(output);
            }
            Err(e) => {
                let out = ResponseFailure {
                    jsonrpc: "2.0".into(),
                    id: id,
                    error: ResponseError {
                        code: MethodNotFound,
                        message: "GotoDef failed to complete successfully".into()
                    }
                };
                log(format!("\nERROR IN GOTODEF: {:?}\n", out));

                let output = serde_json::to_string(&out).unwrap();
                output_response(output);
            }
        };
    }

    fn hover(&self, id: usize, params: HoverParams) {
        let t = thread::current();
        log(format!("CREATING SPAN"));
        let span = self.convert_pos_to_span(params.textDocument, params.position).unwrap();

        log(format!("\nHovering span: {:?}\n", span));

        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let ty = analysis.show_type(&span).unwrap_or(String::new());
            let docs = analysis.docs(&span).unwrap_or(String::new());
            let doc_url = analysis.doc_url(&span).unwrap_or(String::new());
            t.unpark();

            let mut contents = vec![];
            if !docs.is_empty() {
                contents.push(MarkedString { language: "markdown".into(), value: docs });
            }
            if !doc_url.is_empty() {
                contents.push(MarkedString { language: "url".into(), value: doc_url });
            }
            if !ty.is_empty() {
                contents.push(MarkedString { language: "rust".into(), value: ty });
            }
            ResponseSuccess {
                jsonrpc: "2.0".into(),
                id: id,
                result: HoverSuccessContents {
                    contents: contents
                }
            }
        });

        thread::park_timeout(Duration::from_millis(RUSTW_TIMEOUT));

        let result = rustw_handle.join();
        match result {
            Ok(r) => {
                let output = serde_json::to_string(&r).unwrap();
                output_response(output);
            }
            Err(e) => {
                let r = ResponseFailure {
                    jsonrpc: "2.0".into(),
                    id: id,
                    error: ResponseError {
                        code: MethodNotFound,
                        message: "Hover failed to complete successfully".into()
                    }
                };
                let output = serde_json::to_string(&r).unwrap();
                output_response(output);
            }
        }
    }
}

pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>, build_queue: Arc<BuildQueue>)
    -> io::Result<()> {

    let mut service = LSService { analysis: analysis,
                                  vfs: vfs,
                                  build_queue: build_queue,
                                  current_project: None };

    // note: logging is totally optional, but it gives us a way to see behind the scenes
    let mut log_file = try!(OpenOptions::new().append(true)
                                              .write(true)
                                              .create(true)
                                              .open("tmp/rls_log.txt"));

    loop {
        // Read in the "Content-length: xx" part
        let mut buffer = String::new();
        try!(io::stdin().read_line(&mut buffer));

        let buffer_backup = buffer.clone();

        // Make sure we see the correct header
        let res: Vec<&str> = buffer.split(" ").collect();
        if res.len() != 2 {
            return Err(Error::new(ErrorKind::InvalidData,
                                  format!("Header is malformed: {}", buffer_backup)));
        }
        if res[0] == "Content-length:" {
            return Err(Error::new(ErrorKind::InvalidData, "Header is missing 'Content-length'"));
        }
        if let Ok(size) = usize::from_str_radix(&res[1].trim(), 10) {
            try!(log_file.write_all(&format!("now reading: {} bytes\n", size).into_bytes()));

            // Skip the new lines
            let mut tmp = String::new();
            try!(io::stdin().read_line(&mut tmp));

            let mut content = vec![0; size];

            try!(io::stdin().read_exact(&mut content));

            let c = String::from_utf8(content).unwrap();

            try!(log_file.write_all(&format!("in came: {}\n", c).into_bytes()));
            let msg = parse_message(&c);

            match msg {
                Ok(ServerMessage::Notification(Notification::CancelRequest(id))) => {
                    try!(log_file.write_all(&format!("request to cancel {}\n", id).into_bytes()));
                },
                Ok(ServerMessage::Notification(Notification::Change(change))) => {
                    let fname: String = change.textDocument.uri.chars().skip("file://".len()).collect();
                    try!(log_file.write_all(&format!("notification(change): {:?}\n", change).into_bytes()));
                    let changes: Vec<Change> = change.contentChanges.iter().map(move |i| {
                        Change {
                            span: Span {
                                file_name: fname.clone(),
                                line_start: i.range.start.line,
                                column_start: i.range.start.character,
                                line_end: i.range.end.line,
                                column_end: i.range.end.character,
                            },
                            text: i.text.clone()
                        }
                    }).collect();
                    service.vfs.on_change(&changes);

                    try!(log_file.write_all(&format!("CHANGES: {:?}", changes).into_bytes()));

                    let current_project = service.current_project.clone().unwrap_or_default();

                    service.build(&current_project, BuildPriority::Normal);
                }
                Ok(ServerMessage::Request(Request{id, method})) => {
                    match method {
                        Method::Shutdown => {
                            try!(log_file.write_all(&format!("shutting down...\n").into_bytes()));
                            break;
                        }
                        Method::Hover(params) => {
                            try!(log_file.write_all(&format!("command(hover): {:?}\n", params).into_bytes()));
                            service.hover(id, params);
                        }
                        Method::GotoDef(params) => {
                            try!(log_file.write_all(&format!("command(goto): {:?}\n", params).into_bytes()));
                            service.goto_def(id, params);
                        }
                        Method::Complete(params) => {
                            try!(log_file.write_all(&format!("command(complete): {:?}\n", params).into_bytes()));
                            service.complete(id, params);
                        }
                        Method::Symbols(params) => {
                            try!(log_file.write_all(&format!("command(goto): {:?}\n", params).into_bytes()));
                            service.symbols(id, params);
                        }
                        Method::FindAllRef(params) => {
                            try!(log_file.write_all(&format!("command(find_all_refs): {:?}\n", params).into_bytes()));
                            service.find_all_refs(id, params);
                        }
                        Method::Initialize(init) => {
                            try!(log_file.write_all(&format!("command(init): {:?}\n", init).into_bytes()));
                            let result = ResponseSuccess {
                                jsonrpc: "2.0".into(),
                                id: 0,
                                result: InitializeCapabilities {
                                    capabilities: ServerCapabilities {
                                        textDocumentSync: DocumentSyncKind::Incremental as usize,
                                        hoverProvider: true,
                                        completionProvider: CompletionOptions {
                                            resolveProvider: true,
                                            triggerCharacters: vec![".".to_string()],
                                        },
                                        signatureHelpProvider: SignatureHelpOptions {
                                            triggerCharacters: vec![".".to_string()],
                                        },
                                        definitionProvider: true,
                                        referencesProvider: true,
                                        documentHighlightProvider: true,
                                        documentSymbolProvider: true,
                                        workshopSymbolProvider: true,
                                        codeActionProvider: false,
                                        codeLensProvider: false,
                                        documentFormattingProvider: true,
                                        documentRangeFormattingProvider: true,
                                        renameProvider: true,
                                    }
                                }
                            };

                            service.current_project = Some(init.rootPath.clone());
                            service.build(&init.rootPath, BuildPriority::Immediate);

                            let output = serde_json::to_string(&result).unwrap();
                            output_response(output);
                        }
                    }
                }
                Err(e) => {
                    try!(log_file.write_all(&format!("parsing invalid message: {:?}", e).into_bytes()));
                    let id = e.2;
                    let r = ResponseFailure {
                        jsonrpc: "2.0".into(),
                        id: id,
                        error: ResponseError {
                            code: MethodNotFound,
                            message: "Unsupported message".into()
                        }
                    };
                    let output = serde_json::to_string(&r).unwrap();
                    output_response(output);
                },
            }
        }
        else {
            try!(log_file.write_all(&format!("Header is missing length: `{}`", res[1]).into_bytes()));
            break;
        }
    }
    Ok(())
}
