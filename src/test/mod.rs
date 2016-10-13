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

use actions_http::Provider;
use analysis;
use build;
use ide::Output;
use lsproto;
use server;
use vfs;

use self::types::src;

use serde_json;
use std::path::PathBuf;

// TODO we should wait for all threads to exit, rather than use a hacky timeout
const TEST_WAIT_TIME: u64 = 500;

#[test]
fn test_simple_goto_def() {
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(".");
    mock_server(|server| {
        // Build.
        assert_non_empty(&server.handle_action("/on_save", &cache.mk_save_input("src/main.rs")));

        // Goto def.
        let output = server.handle_action("/goto_def", &cache.mk_input(src("src/main.rs", 13, "world")));
        assert_output(&mut cache, &output, src("src/main.rs", 12, "world"), Provider::Compiler);
    });
}

#[test]
fn test_abs_path() {
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
    let mut cache = types::Cache::new(cwd_copy.canonicalize().unwrap().to_str().unwrap());

    mock_server(|server| {
        // Build.
        assert_non_empty(&server.handle_action("/on_save", &cache.mk_save_input("src/main.rs")));

        // Goto def.
        let output = server.handle_action("/goto_def", &cache.mk_input(src("src/main.rs", 13, "world")));
        assert_output(&mut cache, &output, src("src/main.rs", 12, "world"), Provider::Compiler);
    });
}

#[test]
fn test_simple_goto_def_ls() {
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(".");
    let messages = vec![Message::new("initialize",
                                     vec![("processId", "0".to_owned()), ("rootPath", format!("\"{}\"", cache.abs_path(".")))]),
                        Message::new("textDocument/definition",
                                     vec![("textDocument", format!("{{\"uri\":\"file://{}\"}}", cache.abs_path("src/main.rs"))),
                                          ("position", cache.mk_ls_position(src("src/main.rs", 13, "world")))])];
    let (server, results) = mock_lsp_server(messages);
    // Initialise and build.
    assert_eq!(lsproto::LsService::handle_message(server.clone()),
               lsproto::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains("capabilities")]);

    // Goto def.
    assert_eq!(lsproto::LsService::handle_message(server.clone()),
               lsproto::ServerStateChange::Continue);
    // TODO structural checking of result, rather than looking for a string - src("src/main.rs", 12, "world")
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains("\"start\":{\"line\":11,\"character\":8}")]);
}

// Initialise and run the internals of an RLS server.
fn mock_server<F>(f: F)
    where F: FnOnce(&server::MyService)
{
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    let handler = server::MyService {
        analysis: analysis,
        vfs: vfs,
        build_queue: build_queue,
    };

    f(&handler);
}

// Initialise and run the internals of an LS protocol RLS server.
fn mock_lsp_server(messages: Vec<Message>) -> (Arc<lsproto::LsService>, LsResultList)
{
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    let reader = Box::new(MockMsgReader { messages: messages, cur: AtomicUsize::new(0) });
    let output = Box::new(RecordOutput::new());
    let results = output.output.clone();
    let logger = Arc::new(lsproto::Logger::new());
    (lsproto::LsService::new(analysis, vfs, build_queue, reader, output, logger), results)
}

// Despite the use of AtomicUsize and thus being Sync, this struct is not properly
// thread-safe, the assumption is we will process one message at a time.
// In particular, we do not expect simultaneus calls to `read_message`.
struct MockMsgReader {
    messages: Vec<Message>,
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

impl lsproto::MessageReader for MockMsgReader {
    fn read_message(&self) -> Option<String> {
        if self.cur.load(Ordering::SeqCst) >= self.messages.len() {
            return None;
        }

        let message = &self.messages[self.cur.load(Ordering::SeqCst)];
        self.cur.fetch_add(1, Ordering::SeqCst);

        let params = message.params.iter().map(|&(k, ref v)| format!("\"{}\":{}", k, v)).collect::<Vec<String>>().join(",");
        // TODO don't hardcode the id, we should use fresh ids and use them to look up responses
        let result = format!("{{\"method\":\"{}\",\"id\":42,\"params\":{{{}}}}}", message.method, params);
        println!("read_message: `{}`", result);

        Some(result)
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

impl lsproto::Output for RecordOutput {
    fn response(&self, output: String) {
        let mut records = self.output.lock().unwrap();
        records.push(output);
    }
}

// Initialise the environment for a test.
fn init_env(project_dir: &str) {
    let mut cwd = env::current_dir().expect(FAIL_MSG);
    cwd.push("test_data");
    cwd.push(project_dir);
    env::set_current_dir(cwd).expect(FAIL_MSG);
}

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

// Assert that the result of a query is a certain span given by a certain provider.
fn assert_output(cache: &mut types::Cache, output: &[u8], src: types::Src, p: Provider) {
    assert_non_empty(output);
    let output = serde_json::from_slice(output).expect("Couldn't deserialise output");
    match output {
        Output::Ok(pos, provider) => {
            assert_eq!(pos, cache.mk_position(src));
            assert_eq!(provider, p)
        }
        Output::Err => panic!("Output was error"),
    }
}

// Assert that the output of a query is not an empty struct.
fn assert_non_empty(output: &[u8]) {
    if output == b"{}\n" {
        panic!("Empty output");
    }    
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
