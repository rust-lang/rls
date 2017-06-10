// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};
use env_logger;

use analysis;
use build;
use server::{self as ls_server, ServerMessage};
use vfs;

use super::types;

use serde_json;
use std::path::{Path, PathBuf};

const TEST_TIMEOUT_IN_SEC: u64 = 10;

// Initialise and run the internals of an LS protocol RLS server.
pub fn mock_server(messages: Vec<ServerMessage>) -> (Arc<ls_server::LsService>, LsResultList)
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

pub struct RecordOutput {
    pub output: LsResultList,
}

impl RecordOutput {
    pub fn new() -> RecordOutput {
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

#[derive(Clone, Debug)]
pub struct ExpectedMessage {
    id: Option<u64>,
    contains: Vec<String>,
}

impl ExpectedMessage {
    pub fn new(id: Option<u64>) -> ExpectedMessage {
        ExpectedMessage {
            id: id,
            contains: vec![],
        }
    }

    pub fn expect_contains(&mut self, s: &str) -> &mut ExpectedMessage {
        self.contains.push(s.to_owned());
        self
    }
}

pub fn expect_messages(results: LsResultList, expected: &[&ExpectedMessage]) {
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

// Initialise the environment for a test.
pub fn init_env(project_dir: &str) -> (types::Cache, TestCleanup) {
    let _ = env_logger::init();

    let path = &Path::new("test_data").join(project_dir);
    let tc = TestCleanup { path: path.to_owned() };
    (types::Cache::new(path), tc)
}

pub struct TestCleanup {
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
