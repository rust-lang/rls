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

use std::env;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use env_logger;

use analysis;
use build;
use server as ls_server;
use vfs;

use self::types::src;

use hyper::Url;
use serde_json;
use std::path::{Path, PathBuf};

use rust_lsp::lsp::*;
use rust_lsp::jsonrpc::service_util::MessageWriter;
use rust_lsp::jsonrpc::service_util::MessageReader;
use rust_lsp::jsonrpc::service_util::GResult;
use rust_lsp::jsonrpc::RequestHandler;
use rust_lsp::jsonrpc::EndpointHandler;


// TODO we should wait for all threads to exit, rather than use a hacky timeout
const TEST_WAIT_TIME: u64 = 1500;

#[test]
fn test_abs_path() {
    let _ = env_logger::init();
    let _cr = CwdRestorer::new();

    // Change directory to 'src', just a directory that is not an ancestor of
    // the test data.
    let mut cwd = env::current_dir().unwrap();
    let mut cwd_copy = cwd.clone();
    cwd.push("src");
    env::set_current_dir(cwd).unwrap();

    // Initialise the file cache with an absolute path, this is the path that
    // will end up getting passed to the RLS.
    cwd_copy.push("test_data");
    cwd_copy.push("hello");
    let mut cache = types::Cache::new(&cwd_copy);

    let source_file_path = Path::new("src").join("main.rs");
    let root_path = format!("{}", serde_json::to_string(&cache.abs_path(Path::new(".")))
                                      .expect("couldn't convert path to JSON"));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = format!("{{\"uri\":{}}}", serde_json::to_string(&url.as_str().to_owned())
                                                        .expect("couldn't convert path to JSON"));
    let messages = vec![Message::new("initialize", vec![("processId", "0".to_owned()),
                                                        ("capabilities", "null".to_owned()),
                                                        ("rootPath", root_path)]),
                        Message::new("textDocument/definition",
                                     vec![("textDocument", text_doc),
                                          ("position", cache.mk_ls_position(src(&source_file_path, 13, "world")))])];
    
    let (mut server_ep, mut msg_reader, results) = mock_server(messages);
    // Initialise and build.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    // Goto def.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    // TODO structural checking of result, rather than looking for a string - src(&source_file_path, 12, "world")
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#""start":{"character":8,"line":11}"#)]);
    
    server_ep.endpoint.shutdown_and_join();
}

#[test]
fn test_goto_def() {
    let _ = env_logger::init();
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(Path::new("."));

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = format!("{}", serde_json::to_string(&cache.abs_path(Path::new(".")))
                                      .expect("couldn't convert path to JSON"));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = format!("{{\"uri\":{}}}", serde_json::to_string(&url.as_str().to_owned())
                                                 .expect("couldn't convert path to JSON"));
    let messages = vec![Message::new("initialize", vec![("processId", "0".to_owned()),
                                                        ("capabilities", "null".to_owned()),
                                                        ("rootPath", root_path)]),
                        Message::new("textDocument/definition",
                                     vec![("textDocument", text_doc),
                                          ("position", cache.mk_ls_position(src(&source_file_path, 13, "world")))])];
    
    let (mut server_ep, mut msg_reader, results) = mock_server(messages);
    
    // Initialise and build.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    // TODO structural checking of result, rather than looking for a string - src(&source_file_path, 12, "world")
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#""start":{"character":8,"line":11}"#)]);
    
    server_ep.endpoint.shutdown_and_join();
}

#[test]
fn test_hover() {
    let _ = env_logger::init();
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(Path::new("."));

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = format!("{}", serde_json::to_string(&cache.abs_path(Path::new(".")))
                                      .expect("couldn't convert path to JSON"));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = format!("{{\"uri\":{}}}", serde_json::to_string(&url.as_str().to_owned())
                                                 .expect("couldn't convert path to JSON"));
    let messages = vec![Message::new("initialize", vec![("processId", "0".to_owned()),
                                                        ("capabilities", "null".to_owned()),
                                                        ("rootPath", root_path)]),
                        Message::new("textDocument/hover",
                                     vec![("textDocument", text_doc),
                                          ("position", cache.mk_ls_position(src(&source_file_path, 13, "world")))])];

    let (mut server_ep, mut msg_reader, results) = mock_server(messages);
    
    // Initialise and build.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains("[{\"language\":\"rust\",\"value\":\"&str\"}]")]);
    
    server_ep.endpoint.shutdown_and_join();
}

#[test]
fn test_find_all_refs() {
    let _ = env_logger::init();
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(Path::new("."));

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = format!("{}", serde_json::to_string(&cache.abs_path(Path::new(".")))
                                      .expect("couldn't convert path to JSON"));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = format!("{{\"uri\":{}}}", serde_json::to_string(&url.as_str().to_owned())
                                                 .expect("couldn't convert path to JSON"));
    let messages = vec![format!(r#"{{
        "jsonrpc": "2.0",
        "method": "initialize",
        "id": 0,
        "params": {{
            "processId": "0",
            "capabilities": null,
            "rootPath": {}
        }}
    }}"#, root_path), format!(r#"{{
        "jsonrpc": "2.0",
        "method": "textDocument/references",
        "id": 42,
        "params": {{
            "textDocument": {},
            "position": {},
            "context": {{
                "includeDeclaration": true
            }}
        }}
    }}"#, text_doc, cache.mk_ls_position(src(&source_file_path, 13, "world")))];

    let (mut server_ep, mut msg_reader, results) = mock_raw_server(messages);
    // Initialise and build.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());

    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":11,"character":8},"end":{"line":11,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":12,"character":27},"end":{"line":12,"character":32}}"#),]);
}

#[test]
fn test_highlight() {
    let _ = env_logger::init();
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(Path::new("."));

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = format!("{}", serde_json::to_string(&cache.abs_path(Path::new(".")))
                                      .expect("couldn't convert path to JSON"));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = format!("{{\"uri\":{}}}", serde_json::to_string(&url.as_str().to_owned())
                                                 .expect("couldn't convert path to JSON"));
    let messages = vec![format!(r#"{{
        "jsonrpc": "2.0",
        "method": "initialize",
        "id": 0,
        "params": {{
            "processId": "0",
            "capabilities": null,
            "rootPath": {}
        }}
    }}"#, root_path), format!(r#"{{
        "jsonrpc": "2.0",
        "method": "textDocument/documentHighlight",
        "id": 42,
        "params": {{
            "textDocument": {},
            "position": {}
        }}
    }}"#, text_doc, cache.mk_ls_position(src(&source_file_path, 13, "world")))];

    let (mut server_ep, mut msg_reader, results) = mock_raw_server(messages);
    // Initialise and build.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":11,"character":8},"end":{"line":11,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":12,"character":27},"end":{"line":12,"character":32}}"#),]);
}

#[test]
fn test_rename() {
    let _ = env_logger::init();
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(Path::new("."));

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = format!("{}", serde_json::to_string(&cache.abs_path(Path::new(".")))
                                      .expect("couldn't convert path to JSON"));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = format!("{{\"uri\":{}}}", serde_json::to_string(&url.as_str().to_owned())
                                                 .expect("couldn't convert path to JSON"));
    let messages = vec![format!(r#"{{
        "jsonrpc": "2.0",
        "method": "initialize",
        "id": 0,
        "params": {{
            "processId": "0",
            "capabilities": null,
            "rootPath": {}
        }}
    }}"#, root_path), format!(r#"{{
        "jsonrpc": "2.0",
        "method": "textDocument/rename",
        "id": 42,
        "params": {{
            "textDocument": {},
            "position": {},
            "newName": "foo"
        }}
    }}"#, text_doc, cache.mk_ls_position(src(&source_file_path, 13, "world")))];

    let (mut server_ep, mut msg_reader, results) = mock_raw_server(messages);
    // Initialise and build.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":11,"character":8},"end":{"line":11,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":12,"character":27},"end":{"line":12,"character":32}}"#)
                                                                     .expect_contains(r#"{"changes""#),]);
}

#[test]
fn test_completion() {
    let _ = env_logger::init();
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(Path::new("."));

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = format!("{}", serde_json::to_string(&cache.abs_path(Path::new(".")))
                                      .expect("couldn't convert path to JSON"));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = format!("{{\"uri\":{}}}", serde_json::to_string(&url.as_str().to_owned())
                                                 .expect("couldn't convert path to JSON"));
    let messages = vec![Message::new("initialize", vec![("processId", "0".to_owned()),
                                                        ("capabilities", "null".to_owned()),
                                                        ("rootPath", root_path)]),
                        Message::new("textDocument/completion",
                                     vec![("textDocument", text_doc),
                                          ("position", cache.mk_ls_position(src(&source_file_path, 13, "rld")))])];

    let (mut server_ep, mut msg_reader, results) = mock_server(messages);
    
    // Initialise and build.
    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    server_ep.handle_incoming_message(&msg_reader.read_next().unwrap());
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"[{"detail":"let world = \"world\";","label":"world"}]"#)]);
    
    server_ep.endpoint.shutdown_and_join();
}


// Initialise and run the internals of an LS protocol RLS server.
fn mock_server(messages: Vec<Message>) -> (EndpointHandler, MockMsgReader, LsResultList)
{
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    let reader = Box::new(MockMsgReader { messages: messages, cur: AtomicUsize::new(0) });
    let output = Box::new(RecordOutput::new());
    let results = output.output.clone();
    
    let output = *output;
   	let ep = LSPEndpoint::create_lsp_output(|| output );
    let ls_svc = ls_server::LsService::new(analysis, vfs, build_queue, ep.clone());
	let req_handler : Box<RequestHandler> = Box::new(ServerRequestHandler(ls_svc));
    let endpoint = EndpointHandler::create(ep, req_handler);
    
    (endpoint, *reader, results)
}

// Initialise and run the internals of an LS protocol RLS server.
fn mock_raw_server(messages: Vec<String>) -> (EndpointHandler, MockRawMsgReader, LsResultList)
{
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    let reader = Box::new(MockRawMsgReader { messages: messages, cur: AtomicUsize::new(0) });
    let output = Box::new(RecordOutput::new());
    let results = output.output.clone();
    
    let output = *output;
   	let ep = LSPEndpoint::create_lsp_output(|| output );
    let ls_svc = ls_server::LsService::new(analysis, vfs, build_queue, ep.clone());
	let req_handler : Box<RequestHandler> = Box::new(ServerRequestHandler(ls_svc));
    let endpoint = EndpointHandler::create(ep, req_handler);
    
    (endpoint, *reader, results)
}

// Despite the use of AtomicUsize and thus being Sync, this struct is not properly
// thread-safe, the assumption is we will process one message at a time.
// In particular, we do not expect simultaneus calls to `read_message`.
struct MockMsgReader {
    messages: Vec<Message>,
    cur: AtomicUsize,
}

// Despite the use of AtomicUsize and thus being Sync, this struct is not properly
// thread-safe, the assumption is we will process one message at a time.
// In particular, we do not expect simultaneus calls to `read_message`.
struct MockRawMsgReader {
    messages: Vec<String>,
    cur: AtomicUsize,
}

// TODO should have a structural way of making params, rather than taking Strings
struct Message {
    method: &'static str,
    params: Vec<(&'static str, String)>,
}

impl Message {
    fn new(method: &'static str, params: Vec<(&'static str, String)>) -> Message {
        Message {
            method: method,
            params: params,
        }
    }
}

impl MessageReader for MockMsgReader {
   	fn read_next(&mut self) -> GResult<String> {
   	    
        if self.cur.load(Ordering::SeqCst) >= self.messages.len() {
            return Err("End of stream reached.".into());
        }

        let message = &self.messages[self.cur.load(Ordering::SeqCst)];
        self.cur.fetch_add(1, Ordering::SeqCst);

        let params = message.params.iter().map(|&(k, ref v)| format!("\"{}\":{}", k, v)).collect::<Vec<String>>().join(",");
        // TODO don't hardcode the id, we should use fresh ids and use them to look up responses
        let result = format!(r#"{{"method":"{}","jsonrpc":"2.0","id":42,"params":{{{}}}}}"#, message.method, params);
        // println!("read_message: `{}`", result);

        Ok(result)
    }
}

impl MessageReader for MockRawMsgReader {
    fn read_next(&mut self) -> GResult<String> {
        if self.cur.load(Ordering::SeqCst) >= self.messages.len() {
            return Err("End of stream reached.".into());
        }

        let message = &self.messages[self.cur.load(Ordering::SeqCst)];
        self.cur.fetch_add(1, Ordering::SeqCst);

        Ok(message.clone())
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

impl MessageWriter for RecordOutput {
    fn write_message(&mut self, msg: &str) -> GResult<()> {
        let mut records = self.output.lock().unwrap();
        records.push(msg.into());
        Ok(())
    }
}

// Initialise the environment for a test.
fn init_env(project_dir: &str) {
    let mut cwd = env::current_dir().expect(FAIL_MSG);
    cwd.push("test_data");
    cwd.push(project_dir);
    env::set_current_dir(cwd).expect(FAIL_MSG);
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
    thread::sleep(Duration::from_millis(TEST_WAIT_TIME));
    let mut results = results.lock().unwrap();
    println!("expect_messages: results: {:?},\nexpected: {:?}", *results, expected);
    assert_eq!(results.len(), expected.len());
    for (found, expected) in results.iter().zip(expected.iter()) {
        let values: serde_json::Value = serde_json::from_str(found).unwrap();
        assert!(values.lookup("jsonrpc").expect("Missing jsonrpc field").as_str().unwrap() == "2.0", "Bad jsonrpc field");
        if let Some(id) = expected.id {
            assert_eq!(values.lookup("id").expect("Missing id field").as_u64().unwrap(), id, "Unexpected id");
        }
        for c in expected.contains.iter() {
            found.find(c).expect(&format!("Could not find `{}` in `{}`", c, found));
        }
    }
    *results = vec![];
}

const FAIL_MSG: &'static str = "Error initialising environment";

struct CwdRestorer {
    old: PathBuf,
}

impl CwdRestorer {
    fn new() -> CwdRestorer{
        CwdRestorer {
            old: env::current_dir().expect(FAIL_MSG),
        }
    }
}

impl Drop for CwdRestorer {
    fn drop(&mut self) {
        env::set_current_dir(self.old.clone()).expect(FAIL_MSG);
    }
}
