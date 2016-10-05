#![feature(plugin, custom_derive)]
#![plugin(serde_macros)]

extern crate serde;
extern crate serde_json;

use analysis::{AnalysisHost, Span};
use vfs::Vfs;
use build::*;
use std::sync::Arc;
use std::path::Path;

use std::fs::{File, OpenOptions};

use std::io::{self, Read, Write, Error, ErrorKind};
use std::thread;
use std::time::Duration;

// Timeout = 0.5s (totally arbitrary).
const RUSTW_TIMEOUT: u64 = 500;

#[derive(Debug, Deserialize)]
struct Position {
    line: usize,
    character: usize
}

#[derive(Debug, Deserialize)]
struct Range {
    start: Position,
    end: Position,
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

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct TextDocumentContentChangeEvent {
    range: Option<Range>,
    rangeLength: Option<u32>,
    text: String
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

#[derive(Debug, Deserialize)]
struct CancelParams {
    id: usize
}

#[derive(Debug)]
enum Method {
    Shutdown,
    Initialize (InitializeParams),
    Hover (HoverParams)
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
    contents: Vec<String>
}

#[derive(Debug, Serialize)]
struct HoverSuccess {
    jsonrpc: String,
    id: usize,
    result: HoverSuccessContents,
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

#[derive(Debug, Serialize)]
struct InitializeCapabilities {
    capabilities: ServerCapabilities
}

#[derive(Debug, Serialize)]
struct InitializeResult {
    jsonrpc: String,
    id: usize,
    result: InitializeCapabilities
}

#[derive(Debug)]
enum ServerMessage {
    Request (Request),
    Notification (Notification)
}

fn parse_message(input: &str) -> io::Result<ServerMessage>  {
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
                "$/cancelRequest" => {
                    let params: CancelParams = serde_json::from_value(params.unwrap().to_owned())
                                               .unwrap();
                    Ok(ServerMessage::Notification(Notification::CancelRequest(params.id)))
                }
                _ => {
                    Err(Error::new(ErrorKind::InvalidData, "Unknown command"))
                }
            }
        }
        else {
            Err(Error::new(ErrorKind::InvalidData, "Method is not a string"))
        }
    }
    else {
        Err(Error::new(ErrorKind::InvalidData, "Method not found"))
    }
}

fn log(msg: String) {
    let mut log = OpenOptions::new().append(true)
                                    .write(true)
                                    .create(true)
                                    .open("/tmp/rls_log.txt").unwrap();
    log.write_all(&format!("{}", msg).into_bytes()).unwrap();
}

fn output_response(output: String) {
    use std::io;
    let o = format!("Content-Length: {}\r\n\r\n{}", output.len(), output);

    log(format!("{:?}", o));
    print!("{}", o);
    io::stdout().flush().unwrap();
}

#[derive(Clone)]
struct LSService {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    build_queue: Arc<BuildQueue>,
}

impl LSService {
    fn build(&self, project_path: &str, priority: BuildPriority) {
        let analysis = self.analysis.clone();
        let project_path_copy = project_path.to_owned();

        let result = self.build_queue.request_build(project_path, priority);
        match result {
            BuildResult::Success(_) | BuildResult::Failure(_) => {
                let reply = serde_json::to_string(&result).unwrap();
                // println!("build result: {:?}", result);
                log(format!("build result: {:?}", result));

                let file_name = Path::new(&project_path_copy).file_name()
                                                             .unwrap()
                                                             .to_str()
                                                             .unwrap();
                analysis.reload(file_name).unwrap();
            }
            BuildResult::Squashed => {},
            BuildResult::Err => {},
        }
    }

    fn hover(&self, id: usize, params: HoverParams) {
        let t = thread::current();
        let span = Span {
            file_name: params.textDocument.uri,
            line_start: params.position.line,
            column_start: params.position.character,
            line_end: params.position.line,
            column_end: params.position.character,
        };

        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let ty = analysis.show_type(&span).unwrap_or(String::new());
            let docs = analysis.docs(&span).unwrap_or(String::new());
            let doc_url = analysis.doc_url(&span).unwrap_or(String::new());
            t.unpark();

            HoverSuccess {
                jsonrpc: "2.0".into(),
                id: id,
                result: HoverSuccessContents {
                    contents: vec![ty, docs, doc_url]
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
                let r = HoverSuccess {
                    jsonrpc: "2.0".into(),
                    id: id,
                    result: HoverSuccessContents {
                        contents: vec![format!("hover failed")]
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

    let mut service = LSService { analysis: analysis, vfs: vfs, build_queue: build_queue };

    // note: logging is totally optional, but it gives us a way to see behind the scenes
    let mut log = try!(OpenOptions::new().append(true)
                                         .write(true)
                                         .create(true)
                                         .open("/tmp/rls_log.txt"));

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
            try!(log.write_all(&format!("now reading: {} bytes\n", size).into_bytes()));

            // Skip the new lines
            let mut tmp = String::new();
            try!(io::stdin().read_line(&mut tmp));

            // Create a buffer, filled with zeros
            let mut content = Vec::with_capacity(size);
            for i in 0..size {
                content.push(0);
            }

            try!(io::stdin().read_exact(&mut content));

            let c = String::from_utf8(content).unwrap();

            try!(log.write_all(&format!("in came: {}\n", c).into_bytes()));
            let msg = parse_message(&c);

            match msg {
                Ok(ServerMessage::Notification(Notification::CancelRequest(id))) => {
                    try!(log.write_all(&format!("request to cancel {}\n", id).into_bytes()));
                },
                Ok(ServerMessage::Notification(Notification::Change(change))) => {
                    try!(log.write_all(&format!("notification(change): {:?}\n", change).into_bytes()));
                }
                Ok(ServerMessage::Request(Request{id, method})) => {
                    match method {
                        Method::Shutdown => {
                            try!(log.write_all(&format!("shutting down...\n").into_bytes()));
                            break;
                        }
                        Method::Hover(params) => {
                            try!(log.write_all(&format!("command(hover): {:?}\n", params).into_bytes()));
                            service.hover(id, params);
                        }
                        Method::Initialize(init) => {
                            try!(log.write_all(&format!("command(init): {:?}\n", init).into_bytes()));
                            let result = InitializeResult {
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

                            let output = serde_json::to_string(&result).unwrap();
                            output_response(output);
                            service.build(&init.rootPath, BuildPriority::Immediate)
                        }
                    }
                }
                Err(e) => {
                    try!(log.write_all(&format!("parsing invalid message: {:?}", e).into_bytes()));
                },
            }
        }
        else {
            try!(log.write_all(&format!("Header is missing length: `{}`", res[1]).into_bytes()));
            break;
        }
    }
    Ok(())
}