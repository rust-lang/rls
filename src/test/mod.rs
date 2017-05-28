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

mod types;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};
use env_logger;

use analysis;
use build;
use server::{self as ls_server, ServerMessage, Request, Method};
use vfs;

use self::types::src;

use url::Url;
use ls_types::*;
use serde_json;
use std::path::{Path, PathBuf};

const TEST_TIMEOUT_IN_SEC: u64 = 10;

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

// Initialise and run the internals of an LS protocol RLS server.
fn mock_server(messages: Vec<ServerMessage>) -> (Arc<ls_server::LsService>, LsResultList)
{
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    let reader = Box::new(MockMsgReader::new(messages));
    let output = Box::new(RecordOutput::new());
    let results = output.output.clone();
    (Arc::new(ls_server::LsService::new(analysis, vfs, build_queue, reader, output)), results)
}

struct MockMsgReader {
    messages: Vec<ServerMessage>,
    cur: Mutex<usize>,
}

impl MockMsgReader {
    fn new(messages: Vec<ServerMessage>) -> MockMsgReader {
        MockMsgReader {
            messages: messages,
            cur: Mutex::new(0),
        }
    }
}

impl ls_server::MessageReader for MockMsgReader {
    fn read_message(&self) -> Option<String> {
        // Note that we hold this lock until the end of the function, thus meaning
        // that we must finish processing one message before processing the next.
        let mut cur = self.cur.lock().unwrap();
        let index = *cur;
        *cur += 1;

        if index >= self.messages.len() {
            return None;
        }

        let message = &self.messages[index];

        Some(message.to_message_str())
    }
}

type LsResultList = Arc<Mutex<Vec<String>>>;

struct RecordOutput {
    output: LsResultList,
}

impl RecordOutput {
    fn new() -> RecordOutput {
        RecordOutput {
            output: Arc::new(Mutex::new(vec![])),
        }
    }
}

impl ls_server::Output for RecordOutput {
    fn response(&self, output: String) {
        let mut records = self.output.lock().unwrap();
        records.push(output);
    }
}

// Initialise the environment for a test.
fn init_env(project_dir: &str) -> (types::Cache, TestCleanup) {
    let _ = env_logger::init();

    let path = &Path::new("test_data").join(project_dir);
    let tc = TestCleanup { path: path.to_owned() };
    (types::Cache::new(path), tc)
}

#[derive(Clone, Debug)]
struct ExpectedMessage {
    id: Option<u64>,
    contains: Vec<String>,
}

impl ExpectedMessage {
    fn new(id: Option<u64>) -> ExpectedMessage {
        ExpectedMessage {
            id: id,
            contains: vec![],
        }
    }

    fn expect_contains(&mut self, s: &str) -> &mut ExpectedMessage {
        self.contains.push(s.to_owned());
        self
    }
}

fn expect_messages(results: LsResultList, expected: &[&ExpectedMessage]) {
    let start_clock = SystemTime::now();
    let mut results_count = results.lock().unwrap().len();
    while (results_count != expected.len()) && (start_clock.elapsed().unwrap().as_secs() < TEST_TIMEOUT_IN_SEC) {
        thread::sleep(Duration::from_millis(100));
        results_count = results.lock().unwrap().len();
    }

    let mut results = results.lock().unwrap();

    println!("expect_messages: results: {:?},\nexpected: {:?}", *results, expected);
    assert_eq!(results.len(), expected.len());
    for (found, expected) in results.iter().zip(expected.iter()) {
        let values: serde_json::Value = serde_json::from_str(found).unwrap();
        assert!(values.get("jsonrpc").expect("Missing jsonrpc field").as_str().unwrap() == "2.0", "Bad jsonrpc field");
        if let Some(id) = expected.id {
            assert_eq!(values.get("id").expect("Missing id field").as_u64().unwrap(), id, "Unexpected id");
        }
        for c in expected.contains.iter() {
            found.find(c).expect(&format!("Could not find `{}` in `{}`", c, found));
        }
    }

    *results = vec![];
}

struct TestCleanup {
    path: PathBuf
}

impl Drop for TestCleanup {
    fn drop(&mut self) {
        use std::fs;

        let target_path = self.path.join("target");
        if fs::metadata(&target_path).is_ok() {
            fs::remove_dir_all(target_path).expect("failed to tidy up");
        }
    }
}
