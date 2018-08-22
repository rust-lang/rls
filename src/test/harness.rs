// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![allow(expect_fun_call)]

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rls_analysis::{AnalysisHost, Target};
use crate::config::{Config, Inferrable};
use env_logger;
use languageserver_types as ls_types;
use serde_json;
use crate::server as ls_server;
use rls_vfs::Vfs;
use lazy_static::lazy_static;

crate struct Environment {
    crate config: Option<Config>,
    crate cache: Cache,
    crate target_path: PathBuf,
}

impl Environment {
    crate fn new(project_dir: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};

        lazy_static! {
            static ref COUNTER: AtomicUsize = AtomicUsize::new(0);
        }

        let _ = env_logger::try_init();
        if env::var("RUSTC").is_err() {
            env::set_var("RUSTC", "rustc");
        }

        // Acquire the current directory, but this is changing when tests are
        // running so we need to be sure to access it in a synchronized fashion.
        let cur_dir = {
            use crate::build::environment::{EnvironmentLock, Environment};
            let env = EnvironmentLock::get();
            let (guard, _other) = env.lock();
            let env = Environment::push_with_lock(&HashMap::new(), None, guard);
            match env::var_os("RLS_TEST_WORKSPACE_DIR") {
                Some(cur_dir) => cur_dir.into(),
                None => env.get_old_cwd().to_path_buf(),
            }
        };
        let project_path = cur_dir.join("test_data").join(project_dir);

        let target_dir = env::var("CARGO_TARGET_DIR")
            .map(|s| Path::new(&s).to_owned())
            .unwrap_or_else(|_| {
                cur_dir.join("target")
            });

        let working_dir = target_dir
            .join("tests")
            .join(format!("{}", COUNTER.fetch_add(1, Ordering::Relaxed)));

        let mut config = Config::default();
        config.target_dir = Inferrable::Specified(Some(working_dir.clone()));
        config.unstable_features = true;

        let cache = Cache::new(project_path);

        Self {
            config: Some(config),
            cache,
            target_path: working_dir,
        }
    }
}

impl Environment {
    crate fn with_config<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Config),
    {
        let config = self.config.as_mut().unwrap();
        f(config);
    }

    // Initialize and run the internals of an LS protocol RLS server.
    crate fn mock_server(
        &mut self,
        messages: Vec<String>,
    ) -> (ls_server::LsService<RecordOutput>, LsResultList) {
        let analysis = Arc::new(AnalysisHost::new(Target::Debug));
        let vfs = Arc::new(Vfs::new());
        let config = Arc::new(Mutex::new(self.config.take().unwrap()));
        let reader = Box::new(MockMsgReader::new(messages));
        let output = RecordOutput::new();
        let results = output.output.clone();
        (
            ls_server::LsService::new(analysis, vfs, config, reader, output),
            results,
        )
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        use std::fs;

        if fs::metadata(&self.target_path).is_ok() {
            fs::remove_dir_all(&self.target_path).expect("failed to tidy up");
        }
    }
}

struct MockMsgReader {
    messages: Vec<String>,
    cur: AtomicUsize,
}

impl MockMsgReader {
    fn new(messages: Vec<String>) -> MockMsgReader {
        MockMsgReader {
            messages,
            cur: AtomicUsize::new(0),
        }
    }
}

impl ls_server::MessageReader for MockMsgReader {
    fn read_message(&self) -> Option<String> {
        // Note that we hold this lock until the end of the function, thus meaning
        // that we must finish processing one message before processing the next.
        let index = self.cur.fetch_add(1, Ordering::SeqCst);
        if index >= self.messages.len() {
            return None;
        }

        let message = &self.messages[index];

        Some(message.to_owned())
    }
}

type LsResultList = Arc<Mutex<Vec<String>>>;

#[derive(Clone)]
crate struct RecordOutput {
    crate output: LsResultList,
    output_id: Arc<Mutex<u64>>,
}

impl RecordOutput {
    crate fn new() -> RecordOutput {
        RecordOutput {
            output: Arc::new(Mutex::new(vec![])),
            // use some distinguishable value
            output_id: Arc::new(Mutex::new(0x0100_0000)),
        }
    }
}

impl ls_server::Output for RecordOutput {
    fn response(&self, output: String) {
        let mut records = self.output.lock().unwrap();
        records.push(output);
    }

    fn provide_id(&self) -> ls_server::RequestId {
        let mut id = self.output_id.lock().unwrap();
        *id += 1;
        ls_server::RequestId::Num(*id)
    }
}

#[derive(Clone, Debug)]
crate struct ExpectedMessage {
    id: Option<u64>,
    contains: Vec<String>,
}

impl ExpectedMessage {
    crate fn new(id: Option<u64>) -> ExpectedMessage {
        ExpectedMessage {
            id,
            contains: vec![],
        }
    }

    crate fn expect_contains(&mut self, s: &str) -> &mut ExpectedMessage {
        self.contains.push(s.to_owned());
        self
    }
}

/// This function checks for messages with a series of constraints (expecrations)
/// to appear in the buffer, removing valid messages and returning when encountering
/// some that didn't meet the expectation
crate fn expect_series(
    server: &mut ls_server::LsService<RecordOutput>,
    results: LsResultList,
    contains: Vec<&str>,
) {
    let mut expected = ExpectedMessage::new(None);
    for c in contains {
        expected.expect_contains(c);
    }
    while try_expect_message(server, results.clone(), &expected).is_ok() {}
}

/// Expect a single message
///
/// It panics if the message wasn't valid and removes it from the buffer
/// if it was
crate fn expect_message(
    server: &mut ls_server::LsService<RecordOutput>,
    results: LsResultList,
    expected: &ExpectedMessage,
) {
    if let Err(e) = try_expect_message(server, results, expected) {
        panic!("Assert failed: {}", e);
    }
}

/// Check a single message without panicking
///
/// A valid message is removed from the buffer while invalid messages
/// are left in place
fn try_expect_message(
    server: &mut ls_server::LsService<RecordOutput>,
    results: LsResultList,
    expected: &ExpectedMessage,
) -> Result<(), String> {
    server.wait_for_concurrent_jobs();
    let mut results = results.lock().unwrap();

    let found = match results.get(0) {
        Some(s) => s,
        None => return Err("No message found!".into())
    };

    let values: serde_json::Value = serde_json::from_str(&found).unwrap();
    if values
        .get("jsonrpc")
        .expect("Missing jsonrpc field")
        .as_str()
        .unwrap()
        != "2.0"
    {
        return Err("Bad jsonrpc field".into());
    }

    if let Some(id) = expected.id {
        if values
            .get("id")
            .expect("Missing id field")
            .as_u64()
            .unwrap()
            != id
        {
            return Err("Unexpected id".into());
        }
    }

    for c in &expected.contains {
        if found.find(c).is_none() {
            return Err(format!("Could not find `{}` in `{}`", c, found));
        }
    }

    results.remove(0);
    Ok(())
}

crate fn compare_json(actual: &serde_json::Value, expected: &str) {
    let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
    if actual != &expected {
        panic!(
            "JSON differs\nExpected:\n{}\nActual:\n{}\n",
            serde_json::to_string_pretty(&expected).unwrap(),
            serde_json::to_string_pretty(actual).unwrap(),
        );
    }
}

#[derive(Clone, Copy, Debug)]
crate struct Src<'a, 'b> {
    crate file_name: &'a Path,
    // 1 indexed
    crate line: usize,
    crate name: &'b str,
}

crate fn src<'a, 'b>(file_name: &'a Path, line: usize, name: &'b str) -> Src<'a, 'b> {
    Src {
        file_name,
        line,
        name,
    }
}

crate struct Cache {
    base_path: PathBuf,
    files: HashMap<PathBuf, Vec<String>>,
}

impl Cache {
    fn new(base_path: PathBuf) -> Cache {
        Cache {
            base_path,
            files: HashMap::new(),
        }
    }

    crate fn mk_ls_position(&mut self, src: Src<'_, '_>) -> ls_types::Position {
        let line = self.get_line(src);
        let col = line.find(src.name)
            .expect(&format!("Line does not contain name {}", src.name));
        ls_types::Position::new((src.line - 1) as u64, char_of_byte_index(&line, col) as u64)
    }

    /// Create a range convering the initial position on the line
    ///
    /// The line number uses a 0-based index.
    crate fn mk_ls_range_from_line(&mut self, line: u64) -> ls_types::Range {
        ls_types::Range::new(
            ls_types::Position::new(line, 0),
            ls_types::Position::new(line, 0),
        )
    }

    crate fn abs_path(&self, file_name: &Path) -> PathBuf {
        let result = self.base_path
            .join(file_name)
            .canonicalize()
            .expect("Couldn't canonicalise path");
        if cfg!(windows) {
            // FIXME: If the \\?\ prefix is not stripped from the canonical path, the HTTP server tests fail. Why?
            let result_string = result.to_str().expect("Path contains non-utf8 characters.");
            PathBuf::from(&result_string[r"\\?\".len()..])
        } else {
            result
        }
    }

    fn get_line(&mut self, src: Src<'_, '_>) -> String {
        let base_path = &self.base_path;
        let lines = self.files
            .entry(src.file_name.to_owned())
            .or_insert_with(|| {
                let file_name = &base_path.join(src.file_name);
                let file =
                    File::open(file_name).expect(&format!("Couldn't find file: {:?}", file_name));
                let lines = BufReader::new(file).lines();
                lines.collect::<Result<Vec<_>, _>>().unwrap()
            });

        if src.line > lines.len() {
            panic!("Line {} not in file, found {} lines", src.line, lines.len());
        }

        lines[src.line - 1].to_owned()
    }
}

fn char_of_byte_index(s: &str, byte: usize) -> usize {
    for (c, (b, _)) in s.char_indices().enumerate() {
        if b == byte {
            return c;
        }
    }

    panic!("Couldn't find byte {} in {:?}", byte, s);
}
