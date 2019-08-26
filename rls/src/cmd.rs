//! This module presents the RLS as a command line interface, it takes simple
//! versions of commands, turns them into messages the RLS will understand, runs
//! the RLS as usual and prints the JSON result back on the command line.

use crate::actions::requests;
use crate::config::Config;
use crate::server::{self, LsService, Notification, Request, RequestId};
use rls_analysis::{AnalysisHost, Target};
use rls_vfs::Vfs;
use std::sync::atomic::{AtomicU64, Ordering};

use lsp_types::{
    ClientCapabilities, CodeActionContext, CodeActionParams, CompletionItem,
    DocumentFormattingParams, DocumentRangeFormattingParams, DocumentSymbolParams,
    FormattingOptions, InitializeParams, Position, Range, RenameParams, TextDocumentIdentifier,
    TextDocumentPositionParams, TraceOption, WindowClientCapabilities, WorkspaceSymbolParams,
};

use std::collections::HashMap;
use std::fmt;
use std::io::{stdin, stdout, Write};
use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use url::Url;

const VERBOSE: bool = false;
macro_rules! print_verb {
    ($($arg:tt)*) => {
        if VERBOSE {
            println!($($arg)*);
        }
    }
}

/// Runs the RLS in command line mode.
pub fn run() {
    let sender = init();

    loop {
        // Present a prompt and read from stdin.
        print!("> ");
        stdout().flush().unwrap();
        let mut input = String::new();
        stdin().read_line(&mut input).expect("Could not read from stdin");

        // Split the input into an action command and args
        let mut bits = input.split_whitespace();
        let action = bits.next();
        let action = match action {
            Some(a) => a,
            None => continue,
        };

        // Switch on the action and build an appropriate message.
        let msg = match action {
            "def" => {
                let file_name = bits.next().expect("Expected file name");
                let row = bits.next().expect("Expected line number");
                let col = bits.next().expect("Expected column number");
                def(file_name, row, col).to_string()
            }
            "rename" => {
                let file_name = bits.next().expect("Expected file name");
                let row = bits.next().expect("Expected line number");
                let col = bits.next().expect("Expected column number");
                let new_name = bits.next().expect("Expected new name");
                rename(file_name, row, col, new_name).to_string()
            }
            "hover" => {
                let file_name = bits.next().expect("Expected file name");
                let row = bits.next().expect("Expected line number");
                let col = bits.next().expect("Expected column number");
                hover(file_name, row, col).to_string()
            }
            "symbol" => {
                let query = bits.next().expect("Expected a query");
                workspace_symbol(query).to_string()
            }
            "document" => {
                let query = bits.next().expect("Expected file name");
                document_symbol(query).to_string()
            }
            "format" => {
                let file_name = bits.next().expect("Expected file name");
                let tab_size: u64 = bits
                    .next()
                    .unwrap_or("4")
                    .parse()
                    .expect("Tab size should be an unsigned integer");
                let insert_spaces: bool = bits
                    .next()
                    .unwrap_or("true")
                    .parse()
                    .expect("Insert spaces should be 'true' or 'false'");
                format(file_name, tab_size, insert_spaces).to_string()
            }
            "range_format" => {
                let file_name = bits.next().expect("Expected file name");
                let start_row: u64 =
                    bits.next().expect("Expected start line").parse().expect("Bad start line");
                let start_col: u64 =
                    bits.next().expect("Expected start column").parse().expect("Bad start column");
                let end_row: u64 =
                    bits.next().expect("Expected end line").parse().expect("Bad end line");
                let end_col: u64 =
                    bits.next().expect("Expected end column").parse().expect("Bad end column");
                let tab_size: u64 = bits
                    .next()
                    .unwrap_or("4")
                    .parse()
                    .expect("Tab size should be an unsigned integer");
                let insert_spaces: bool = bits
                    .next()
                    .unwrap_or("true")
                    .parse()
                    .expect("Insert spaces should be 'true' or 'false'");
                range_format(
                    file_name,
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                    tab_size,
                    insert_spaces,
                )
                .to_string()
            }
            "code_action" => {
                let file_name = bits.next().expect("Expect file name");
                let start_row: u64 =
                    bits.next().expect("Expect start line").parse().expect("Bad start line");
                let start_col: u64 =
                    bits.next().expect("Expect start column").parse().expect("Bad start column");
                let end_row: u64 =
                    bits.next().expect("Expect end line").parse().expect("Bad end line");
                let end_col: u64 =
                    bits.next().expect("Expect end column").parse().expect("Bad end column");
                code_action(file_name, start_row, start_col, end_row, end_col).to_string()
            }
            "resolve" => {
                let label = bits.next().expect("Expect label");
                let detail = bits.next().expect("Expect detail");
                resolve_completion(label, detail).to_string()
            }
            "h" | "help" => {
                help();
                continue;
            }
            "q" | "quit" => {
                sender.send(shutdown().to_string()).expect("Error sending on channel");
                sender.send(exit().to_string()).expect("Error sending on channel");
                // Sometimes we don't quite exit in time and we get an error on the channel. Hack it.
                thread::sleep(Duration::from_millis(100));
                return;
            }
            _ => {
                println!("Unknown action. Type 'help' to see available actions.");
                continue;
            }
        };

        // Send the message to the server.
        print_verb!("message: {:?}", msg);
        sender.send(msg).expect("Error sending on channel");
        // Give the result time to print before printing the prompt again.
        thread::sleep(Duration::from_millis(100));
    }
}

fn def(file_name: &str, row: &str, col: &str) -> Request<requests::Definition> {
    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(
            u64::from_str(row).expect("Bad line number"),
            u64::from_str(col).expect("Bad column number"),
        ),
    };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn rename(file_name: &str, row: &str, col: &str, new_name: &str) -> Request<requests::Rename> {
    let params = RenameParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url(file_name)),
            position: Position::new(
                u64::from_str(row).expect("Bad line number"),
                u64::from_str(col).expect("Bad column number"),
            ),
        },
        new_name: new_name.to_owned(),
    };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn hover(file_name: &str, row: &str, col: &str) -> Request<requests::Hover> {
    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(
            u64::from_str(row).expect("Bad line number"),
            u64::from_str(col).expect("Bad column number"),
        ),
    };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn workspace_symbol(query: &str) -> Request<requests::WorkspaceSymbol> {
    let params = WorkspaceSymbolParams { query: query.to_owned() };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn format(file_name: &str, tab_size: u64, insert_spaces: bool) -> Request<requests::Formatting> {
    // no optional properties
    let properties = HashMap::default();

    let params = DocumentFormattingParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        options: FormattingOptions { tab_size, insert_spaces, properties },
    };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn document_symbol(file_name: &str) -> Request<requests::Symbols> {
    let params =
        DocumentSymbolParams { text_document: TextDocumentIdentifier::new(url(file_name)) };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn range_format(
    file_name: &str,
    start_row: u64,
    start_col: u64,
    end_row: u64,
    end_col: u64,
    tab_size: u64,
    insert_spaces: bool,
) -> Request<requests::RangeFormatting> {
    // no optional properties
    let properties = HashMap::default();

    let params = DocumentRangeFormattingParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        range: Range {
            start: Position::new(start_row, start_col),
            end: Position::new(end_row, end_col),
        },
        options: FormattingOptions { tab_size, insert_spaces, properties },
    };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn code_action(
    file_name: &str,
    start_row: u64,
    start_col: u64,
    end_row: u64,
    end_col: u64,
) -> Request<requests::CodeAction> {
    let params = CodeActionParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        range: Range {
            start: Position::new(start_row, start_col),
            end: Position::new(end_row, end_col),
        },
        context: CodeActionContext { diagnostics: Vec::new(), only: None },
    };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn resolve_completion(label: &str, detail: &str) -> Request<requests::ResolveCompletion> {
    let params = CompletionItem::new_simple(label.to_owned(), detail.to_owned());

    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn shutdown() -> Request<server::ShutdownRequest> {
    Request { id: next_id(), params: (), received: Instant::now(), _action: PhantomData }
}

fn exit() -> Notification<server::ExitNotification> {
    Notification { params: (), _action: PhantomData }
}

fn initialize(root_path: String) -> Request<server::InitializeRequest> {
    let params = InitializeParams {
        process_id: None,
        root_uri: Some(url(&root_path)),
        root_path: Some(root_path),
        initialization_options: None,
        capabilities: ClientCapabilities {
            workspace: None,
            window: Some(WindowClientCapabilities { progress: Some(true) }),
            text_document: None,
            experimental: None,
        },
        trace: Some(TraceOption::Off),
        workspace_folders: None,
    };
    Request { id: next_id(), params, received: Instant::now(), _action: PhantomData }
}

fn url(file_name: &str) -> Url {
    let canonical = Path::new(file_name).canonicalize().expect("Could not canonicalize file name");
    let mut path = canonical.to_str().unwrap();

    // workaround for UNC path see https://github.com/rust-lang/rust/issues/42869
    if path.starts_with(r"\\?\") {
        path = &path[r"\\?\".len()..];
    }

    Url::parse(&format!("file://{}", path)).expect("Bad file name")
}

fn next_id() -> RequestId {
    static ID: AtomicU64 = AtomicU64::new(1);
    RequestId::Num(ID.fetch_add(1, Ordering::SeqCst))
}

// Custom reader and output for the RLS server.
#[derive(Clone)]
struct PrintlnOutput;

impl server::Output for PrintlnOutput {
    fn response(&self, output: String) {
        println!("{}", output);
    }

    fn provide_id(&self) -> RequestId {
        RequestId::Num(0)
    }

    fn success<D: ::serde::Serialize + fmt::Debug>(&self, id: RequestId, data: &D) {
        println!("{}: {:#?}", id, data);
    }
}

struct ChannelMsgReader {
    channel: Mutex<Receiver<String>>,
}

impl ChannelMsgReader {
    fn new(rx: Receiver<String>) -> ChannelMsgReader {
        ChannelMsgReader { channel: Mutex::new(rx) }
    }
}

impl server::MessageReader for ChannelMsgReader {
    fn read_message(&self) -> Option<String> {
        let channel = self.channel.lock().unwrap();
        let msg = channel.recv().expect("Error reading from channel");
        Some(msg)
    }
}

// Initialize a server, returns the sender end of a channel for posting messages.
// The initialized server will live on its own thread and look after the receiver.
fn init() -> Sender<String> {
    let analysis = Arc::new(AnalysisHost::new(Target::Debug));
    let vfs = Arc::new(Vfs::new());
    let (sender, receiver) = channel();

    let service = LsService::new(
        analysis,
        vfs,
        // Don't clear `RUST_LOG` in CLI mode since it's intended for debugging purposes.
        Arc::new(Mutex::new(Config { clear_env_rust_log: false, ..Default::default() })),
        Box::new(ChannelMsgReader::new(receiver)),
        PrintlnOutput,
    );
    thread::spawn(|| LsService::run(service));

    sender
        .send(
            initialize(::std::env::current_dir().unwrap().to_str().unwrap().to_owned()).to_string(),
        )
        .expect("Error sending init");
    println!("Initializing (look for `progress[done:true]` message)...");

    sender
}

// Display help message.
fn help() {
    println!(
        "\
RLS command line interface.

Line and column numbers are zero indexed

Supported commands:
    help          display this message
    quit          exit

    def           file_name line_number column_number
                  textDocument/definition
                  used for 'goto def'

    rename        file_name line_number column_number new_name
                  textDocument/rename
                  used for 'rename'

    hover         file_name line_number column_number
                  textDocument/hover
                  used for 'hover'

    symbol        query
                  workspace/symbol

    document      file_name
                  textDocument/documentSymbol

    format        file_name [tab_size [insert_spaces]]
                  textDocument/formatting
                  tab_size defaults to 4 and insert_spaces to 'true'

    range_format  file_name start_line start_col end_line end_col [tab_size [insert_spaces]]
                  textDocument/rangeFormatting
                  tab_size defaults to 4 and insert_spaces to 'true'

    code_action   file_name start_line start_col end_line end_col
                  textDocument/codeAction

    resolve       label detail
                  completionItem/resolve"
    );
}

// Reproducible on Windows where current dir can produce a url something like
// `file:////?\C:\Users\alex\project\rls` which will later cause issues
#[test]
fn url_workaround_unc_canonicals() {
    let current_dir = ::std::env::current_dir().unwrap();
    let url = url(current_dir.to_str().unwrap());

    let url_str = format!("{}", url);
    assert!(!url_str.starts_with(r"file:////?\"), "Unexpected UNC url {}", url);
}
