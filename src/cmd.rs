// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// This module presents the RLS as a command line interface, it takes simple
// versions of commands, turns them into messages the RLS will understand, runs
// the RLS as usual and prints the JSON result back on the command line.

use analysis::{AnalysisHost, Target};
use build::BuildQueue;
use vfs::Vfs;
use server::{self, ServerMessage, Request, Notification, Method, LsService, ParseError, ResponseData};

use ls_types::{ClientCapabilities, TextDocumentPositionParams, TextDocumentIdentifier, TraceOption, Position, InitializeParams, RenameParams};
use std::time::Duration;
use std::io::{stdin, stdout, Write};
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{channel, Sender, Receiver};
use std::thread;
use url::Url;

const VERBOSE: bool = false;
macro_rules! print_verb {
    ($($arg:tt)*) => {
        if VERBOSE {
            println!($($arg)*);
        }
    }
}

// Run in command line mode.
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
                def(file_name, row, col)
            }
            "rename" => {
                let file_name = bits.next().expect("Expected file name");
                let row = bits.next().expect("Expected line number");
                let col = bits.next().expect("Expected column number");
                let new_name = bits.next().expect("Expected new name");
                rename(file_name, row, col, new_name)
            }
            "hover" => {
                let file_name = bits.next().expect("Expected file name");
                let row = bits.next().expect("Expected line number");
                let col = bits.next().expect("Expected column number");
                hover(file_name, row, col)
            }
            "h" | "help" => {
                help();
                continue;
            }
            "q" | "quit" => {
                sender.send(shutdown()).expect("Error sending on channel");
                sender.send(exit()).expect("Error sending on channel");
                // Sometimes we don't quite exit in time and we get an error on the channel. Hack it.
                thread::sleep(Duration::from_millis(100));
                return;
            }
            _ => panic!("unknown action"),
        };

        // Send the message to the server.
        print_verb!("message: {:?}", msg);
        sender.send(msg).expect("Error sending on channel");
        // Give the result time to print before printing the prompt again.
        thread::sleep(Duration::from_millis(100));
    }
}

// `def` command
fn def(file_name: &str, row: &str, col: &str) -> ServerMessage {
    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(u64::from_str(row).expect("Bad line number"),
                                u64::from_str(col).expect("Bad column number")),
    };
    let request = Request {
        id: next_id(),
        method: Method::GotoDefinition(params),
    };
    ServerMessage::Request(request)
}

// `rename` command
fn rename(file_name: &str, row: &str, col: &str, new_name: &str) -> ServerMessage {
    let params = RenameParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(u64::from_str(row).expect("Bad line number"),
                                u64::from_str(col).expect("Bad column number")),
        new_name: new_name.to_owned(),
    };
    let request = Request {
        id: next_id(),
        method: Method::Rename(params),
    };
    ServerMessage::Request(request)
}

fn hover(file_name: &str, row: &str, col: &str) -> ServerMessage {
    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(u64::from_str(row).expect("Bad line number"),
                                u64::from_str(col).expect("Bad column number")),
    };
    let request = Request {
        id: next_id(),
        method: Method::Hover(params),
    };
    ServerMessage::Request(request)
}

fn shutdown() -> ServerMessage {
    let request = Request {
        id: next_id(),
        method: Method::Shutdown,
    };
    ServerMessage::Request(request)
}

fn exit() -> ServerMessage {
    ServerMessage::Notification(Notification::Exit)
}

fn initialize(root_path: String) -> ServerMessage {
    let params = InitializeParams {
        process_id: None,
        root_path: Some(root_path), // FIXME(#299): This property is deprecated. Instead Use `root_uri`.
        root_uri: None,
        initialization_options: None,
        capabilities: ClientCapabilities {
            workspace: None,
            text_document: None,
            experimental: None,
        },
        trace: TraceOption::Off,
    };
    let request = Request {
        id: next_id(),
        method: Method::Initialize(params),
    };
    ServerMessage::Request(request)    
}

fn url(file_name: &str) -> Url {
    let path = Path::new(file_name).canonicalize().expect("Could not canonicalize file name");
    Url::parse(&format!("file://{}", path.to_str().unwrap())).expect("Bad file name")
}

fn next_id() -> usize {
    static mut ID: usize = 0;
    unsafe {
        ID += 1;
        ID
    }
}

// Custom reader and output for the RLS server.
struct PrintlnOutput;

impl server::Output for PrintlnOutput {
    fn response(&self, output: String) {
        println!("{}", output);
    }

    fn success(&self, id: usize, data: ResponseData) {
        println!("{}: {:#?}", id, data);
    }
}

struct ChannelMsgReader {
    channel: Mutex<Receiver<ServerMessage>>,
}

impl ChannelMsgReader {
    fn new(rx: Receiver<ServerMessage>) -> ChannelMsgReader {
        ChannelMsgReader {
            channel: Mutex::new(rx),
        }
    }
}

impl server::MessageReader for ChannelMsgReader {
    fn parsed_message(&self) -> Option<Result<ServerMessage, ParseError>> {
        let channel = self.channel.lock().unwrap();
        let msg = channel.recv().expect("Error reading from channel");
        Some(Ok(msg))
    }
}

// Initialise a server, returns the sender end of a channel for posting messages.
// The initialised server will live on its own thread and look after the receiver.
fn init() -> Sender<ServerMessage> {
    let analysis = Arc::new(AnalysisHost::new(Target::Debug));
    let vfs = Arc::new(Vfs::new());
    let build_queue = Arc::new(BuildQueue::new(vfs.clone()));
    let (sender, receiver) = channel();

    let service = LsService::new(analysis,
                                 vfs,
                                 build_queue,
                                 Box::new(ChannelMsgReader::new(receiver)),
                                 Box::new(PrintlnOutput));
    thread::spawn(move || LsService::run(service));

    sender.send(initialize(::std::env::current_dir().unwrap().to_str().unwrap().to_owned())).expect("Error sending init");
    println!("Initialising (look for `diagnosticsEnd` message)...");

    sender
}

// Display help message.
fn help() {
    println!("RLS command line interface.");
    println!("\nSupported commands:");
    println!("    help    display this message");
    println!("    quit    exit");
    println!("");
    println!("    def     file_name line_number column_number");
    println!("            textDocument/definition");
    println!("            used for 'goto def'");
    println!("");
    println!("    rename  file_name line_number column_number new_name");
    println!("            textDocument/rename");
    println!("            used for 'rename'");
    println!("");
    println!("    hover   file_name line_number column_number");
    println!("            textDocument/hover");
    println!("            used for 'hover'");
}
