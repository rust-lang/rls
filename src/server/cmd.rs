// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This module presents the RLS as a command line interface, it takes simple
//! versions of commands, turns them into messages the RLS will understand, runs
//! the RLS as usual and prints the JSON result back on the command line.
use actions::requests;
use server::{self, Request, Notification, NoParams};
use ls_types::{ClientCapabilities, TextDocumentPositionParams, TextDocumentIdentifier, TraceOption, Position, InitializeParams, RenameParams, WorkspaceSymbolParams, DocumentFormattingParams, DocumentRangeFormattingParams, Range, FormattingOptions};

use std::collections::HashMap;
use std::io::{stdin, stdout, Write};
use std::marker::PhantomData;
use std::path::Path;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;
use url::Url;

const VERBOSE: bool = false;
macro_rules! print_verb {
    ($($arg:tt)*) => {
        if VERBOSE {
            println!($($arg)*);
        }
    }
}

/// Run the RLS in command line mode.
pub fn run(tx: Sender<String>) {
    println!("Type 'init' to begin initialization process.\nThe process will finish when a `diagnosticEnd` message is printed.");

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
            "init" => {
                tx.send(initialize(::std::env::current_dir().unwrap().to_str().unwrap().to_owned()).to_string()).expect("Error sending init");
                continue;
            }
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
            "format" => {
                let file_name = bits.next().expect("Expected file name");
                let tab_size : u64 = bits.next().unwrap_or("4").parse().expect("Tab size should be an unsigned integer");
                let insert_spaces : bool = bits.next().unwrap_or("true").parse().expect("Insert spaces should be 'true' or 'false'");;
                format(file_name, tab_size, insert_spaces).to_string()
            }
            "range_format" => {
                let file_name = bits.next().expect("Expected file name");
                let start_row : u64 = bits.next().expect("Expected start line").parse().expect("Bad start line");
                let start_col : u64 = bits.next().expect("Expected start column").parse().expect("Bad start column");
                let end_row : u64 = bits.next().expect("Expected end line").parse().expect("Bad end line");
                let end_col : u64 = bits.next().expect("Expected end column").parse().expect("Bad end column");
                let tab_size : u64 = bits.next().unwrap_or("4").parse().expect("Tab size should be an unsigned integer");
                let insert_spaces : bool = bits.next().unwrap_or("true").parse().expect("Insert spaces should be 'true' or 'false'");;
                range_format(file_name, start_row, start_col, end_row, end_col, tab_size, insert_spaces).to_string()
            }
            "h" | "help" => {
                help();
                continue;
            }
            "q" | "quit" => {
                tx.send(shutdown().to_string()).expect("Error sending on channel");
                tx.send(exit().to_string()).expect("Error sending on channel");
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
        tx.send(msg).expect("Error sending on channel");
        // Give the result time to print before printing the prompt again.
        thread::sleep(Duration::from_millis(100));
    }
}

fn def<'a>(file_name: &str, row: &str, col: &str) -> Request<'a, requests::Definition> {
    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(u64::from_str(row).expect("Bad line number"),
                                u64::from_str(col).expect("Bad column number")),
    };
    Request {
        id: next_id(),
        params,
        _action: PhantomData,
    }
}

fn rename<'a>(file_name: &str, row: &str, col: &str, new_name: &str) -> Request<'a, requests::Rename> {
    let params = RenameParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(u64::from_str(row).expect("Bad line number"),
                                u64::from_str(col).expect("Bad column number")),
        new_name: new_name.to_owned(),
    };
    Request {
        id: next_id(),
        params,
        _action: PhantomData,
    }
}

fn hover<'a>(file_name: &str, row: &str, col: &str) -> Request<'a, requests::Hover> {
    let params = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        position: Position::new(u64::from_str(row).expect("Bad line number"),
                                u64::from_str(col).expect("Bad column number")),
    };
    Request {
        id: next_id(),
        params,
        _action: PhantomData,
    }
}

fn workspace_symbol<'a>(query: &str) -> Request<'a, requests::WorkspaceSymbol> {
    let params = WorkspaceSymbolParams {
        query: query.to_owned()
    };
    Request {
        id: next_id(),
        params,
        _action: PhantomData,
    }
}

fn format<'a>(file_name: &str, tab_size: u64, insert_spaces: bool) -> Request<'a, requests::Formatting> {
    // no optional properties
    let properties = HashMap::default();

    let params = DocumentFormattingParams {
        text_document: TextDocumentIdentifier::new(url(file_name)),
        options: FormattingOptions { tab_size, insert_spaces, properties },
    };
    Request {
        id: next_id(),
        params,
        _action: PhantomData,
    }
}

fn range_format<'a>(file_name: &str, start_row: u64, start_col: u64, end_row: u64, end_col: u64, tab_size: u64, insert_spaces: bool) -> Request<'a, requests::RangeFormatting> {
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
    Request {
        id: next_id(),
        params,
        _action: PhantomData,
    }
}

fn shutdown<'a>() -> Request<'a, server::ShutdownRequest<'a>> {
    Request {
        id: next_id(),
        params: NoParams {},
        _action: PhantomData,
    }
}

fn exit<'a>() -> Notification<'a, server::ExitNotification<'a>> {
    Notification {
        params: NoParams {},
        _action: PhantomData,
    }
}

fn initialize<'a>(root_path: String) -> Request<'a, server::InitializeRequest> {
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
    Request {
        id: next_id(),
        params,
        _action: PhantomData,
    }
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

// Display help message.
fn help() {
    println!("RLS command line interface.");
    println!("\nLine and column numbers are zero indexed");
    println!("\nSupported commands:");
    println!("    help          display this message");
    println!("    quit          exit");
    println!("");
    println!("    def           file_name line_number column_number");
    println!("                  textDocument/definition");
    println!("                  used for 'goto def'");
    println!("");
    println!("    rename        file_name line_number column_number new_name");
    println!("                  textDocument/rename");
    println!("                  used for 'rename'");
    println!("");
    println!("    hover         file_name line_number column_number");
    println!("                  textDocument/hover");
    println!("                  used for 'hover'");
    println!("");
    println!("    symbol        query");
    println!("                  workspace/symbol");
    println!("");
    println!("    format        file_name [tab_size [insert_spaces]]");
    println!("                  textDocument/formatting");
    println!("                  tab_size defaults to 4 and insert_spaces to 'true'");
    println!("");
    println!("    range_format  file_name start_line start_col end_line end_col [tab_size [insert_spaces]]");
    println!("                  textDocument/rangeFormatting");
    println!("                  tab_size defaults to 4 and insert_spaces to 'true'");
}
