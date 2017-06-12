// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Utilities and infrastructure for testing. Tests in this module test the
// testing infrastructure *not* the RLS.

mod harness;

use std::sync::Arc;
use env_logger;

use analysis;
use build;
use server::{self as ls_server, ServerMessage, Request, Method};
use vfs;

use self::harness::{expect_messages, ExpectedMessage, init_env, mock_server, RecordOutput, src};

use url::Url;
use ls_types::*;
use std::path::Path;

impl ServerMessage {
    pub fn initialize(id: usize, root_path: Option<String>) -> ServerMessage {
        ServerMessage::Request(Request {
            id: id,
            method: Method::Initialize(InitializeParams {
                process_id: None,
                root_path: root_path,
                root_uri: None,
                initialization_options: None,
                capabilities: ClientCapabilities {
                    workspace: None,
                    text_document: None,
                    experimental: None,
                },
                trace: TraceOption::Off,
            })
        })
    }

    pub fn request(id: usize, method: Method) -> ServerMessage {
        ServerMessage::Request(Request { id: id, method: method })
    }
}

#[test]
fn test_goto_def() {
    let (mut cache, _tc) = init_env("goto_def");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0,root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(11, Method::GotoDefinition(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 22, "world"))
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    // TODO structural checking of result, rather than looking for a string - src(&source_file_path, 12, "world")
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#""start":{"line":20,"character":8}"#)]);
}

#[test]
fn test_hover() {
    let (mut cache, _tc) = init_env("hover");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(11, Method::Hover(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 22, "world"))
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#"[{"language":"rust","value":"&str"}]"#)]);
}

#[test]
fn test_find_all_refs() {
    let (mut cache, _tc) = init_env("find_all_refs");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::References(ReferenceParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 10, "Bar")),
            context: ReferenceContext { include_declaration: true }
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":9,"character":7},"end":{"line":9,"character":10}}"#)
                                                                     .expect_contains(r#"{"start":{"line":15,"character":14},"end":{"line":15,"character":17}}"#)
                                                                     .expect_contains(r#"{"start":{"line":23,"character":15},"end":{"line":23,"character":18}}"#)]);
}

#[test]
fn test_find_all_refs_no_cfg_test() {
    let (mut cache, _tc) = init_env("find_all_refs_no_cfg_test");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::References(ReferenceParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 10, "Bar")),
            context: ReferenceContext { include_declaration: true }
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":9,"character":7},"end":{"line":9,"character":10}}"#)
                                                                     .expect_contains(r#"{"start":{"line":23,"character":15},"end":{"line":23,"character":18}}"#)]);
}

#[test]
fn test_borrow_error() {
    let (cache, _tc) = init_env("borrow_error");

    let root_path = cache.abs_path(Path::new("."));
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned()))
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains(r#"secondaryRanges":[{"start":{"line":2,"character":17},"end":{"line":2,"character":18},"label":"first mutable borrow occurs here"}"#),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_highlight() {
    let (mut cache, _tc) = init_env("highlight");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::DocumentHighlight(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 22, "world"))
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":20,"character":8},"end":{"line":20,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":21,"character":27},"end":{"line":21,"character":32}}"#),]);
}

#[test]
fn test_rename() {
    let (mut cache, _tc) = init_env("rename");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::Rename(RenameParams {
            text_document: text_doc,
            position: cache.mk_ls_position(src(&source_file_path, 22, "world")),
            new_name: "foo".to_owned()
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":20,"character":8},"end":{"line":20,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":21,"character":27},"end":{"line":21,"character":32}}"#)
                                                                     .expect_contains(r#"{"changes""#),]);
}


#[test]
fn test_reformat() {
    let (cache, _tc) = init_env("reformat");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::Formatting(DocumentFormattingParams {
            text_document: text_doc,
            options: FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                properties: ::std::collections::HashMap::new(),
            },
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":0,"character":0},"end":{"line":11,"character":69}}"#)
                                            .expect_contains(r#"newText":"// Copyright 2017 The Rust Project Developers. See the COPYRIGHT\n// file at the top-level directory of this distribution and at\n// http://rust-lang.org/COPYRIGHT.\n//\n// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or\n// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license\n// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your\n// option. This file may not be copied, modified, or distributed\n// except according to those terms.\n\npub mod foo;\npub fn main() {\n    let world = \"world\";\n    println!(\"Hello, {}!\", world);\n}"#)]);
}

#[test]
fn test_reformat_with_range() {
    let (cache, _tc) = init_env("reformat");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::RangeFormatting(DocumentRangeFormattingParams {
            text_document: text_doc,
            range: Range {
                start: Position { line: 11, character: 0 },
                end: Position { line: 12, character: 0 },
            },
            options: FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                properties: ::std::collections::HashMap::new(),
            },
        })),
    ];

    let (server, results) = mock_server(messages);
    
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":0,"character":0},"end":{"line":11,"character":69}}"#)
                                            .expect_contains(r#"newText":"// Copyright 2017 The Rust Project Developers. See the COPYRIGHT\n// file at the top-level directory of this distribution and at\n// http://rust-lang.org/COPYRIGHT.\n//\n// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or\n// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license\n// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your\n// option. This file may not be copied, modified, or distributed\n// except according to those terms.\n\npub mod foo;\npub fn main() {\n    let world = \"world\";\n    println!(\"Hello, {}!\", world);\n}"#)]);
}

#[test]
fn test_completion() {
    let (mut cache, _tc) = init_env("completion");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(11, Method::Completion(TextDocumentPositionParams {
            text_document: text_doc.clone(),
            position: cache.mk_ls_position(src(&source_file_path, 22, "rld"))
        })),
        ServerMessage::request(22, Method::Completion(TextDocumentPositionParams {
            text_document: text_doc.clone(),
            position: cache.mk_ls_position(src(&source_file_path, 25, "x)"))
        })),
    ];

    let (server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#"[{"label":"world","kind":6,"detail":"let world = \"world\";"}]"#)]);

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(22)).expect_contains(r#"{"label":"x","kind":5,"detail":"u64"#)]);
}

#[test]
fn test_parse_error_on_malformed_input() {
    let _ = env_logger::init();
    struct NoneMsgReader;

    impl ls_server::MessageReader for NoneMsgReader {
        fn read_message(&self) -> Option<String> { None }
    }

    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    let reader = Box::new(NoneMsgReader);
    let output = Box::new(RecordOutput::new());
    let results = output.output.clone();
    let server = Arc::new(ls_server::LsService::new(analysis, vfs, build_queue, reader, output));

    assert_eq!(ls_server::LsService::handle_message(server.clone()),
               ls_server::ServerStateChange::Break);

    let error = results.lock().unwrap()
        .pop().expect("no error response");
    assert!(error.contains(r#""code": -32700"#))
}
