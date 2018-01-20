// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use serde_json;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::str;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use std::panic;

use support::paths::TestPathExt;

pub mod paths;

/// Executes `func` and panics if it takes longer than `dur`.
pub fn timeout<F>(dur: Duration, func: F)
    where F: FnOnce() + Send + 'static + panic::UnwindSafe {
    let pair = Arc::new((Mutex::new(TestState::Running), Condvar::new()));
    let pair2 = pair.clone();

    thread::spawn(move|| {
        let &(ref lock, ref cvar) = &*pair2;
        match panic::catch_unwind(|| func()) {
            Ok(_) => *lock.lock().unwrap() = TestState::Success,
            Err(_) => *lock.lock().unwrap() = TestState::Fail,
        }

        // We notify the condvar that the value has changed.
        cvar.notify_one();
    });

    // Wait for the test to finish.
    let &(ref lock, ref cvar) = &*pair;
    let mut test_state = lock.lock().unwrap();
    // As long as the value inside the `Mutex` is false, we wait.
    while *test_state == TestState::Running {
        let result = cvar.wait_timeout(test_state, dur).unwrap();
        if result.1.timed_out() {
            panic!("Timed out")
        }
        if *result.0 == TestState::Fail {
            panic!("failed");
        }
        test_state = result.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TestState {
    Running,
    Success,
    Fail,
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

pub fn read_message<R: Read>(reader: &mut BufReader<R>) -> io::Result<String> {
    let mut content_length = None;
    // Read the headers
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header.len() == 0 {
            panic!("eof")
        }
        if header == "\r\n" {
            // This is the end of the headers
            break;
        }
        let parts: Vec<&str> = header.splitn(2, ": ").collect();
        if parts[0] == "Content-Length" {
            content_length = Some(parts[1].trim().parse::<usize>().unwrap())
        }
    }

    // Read the actual message
    let content_length = content_length.expect("did not receive Content-Length header");
    let mut msg = vec![0; content_length];
    reader.read_exact(&mut msg)?;
    let result = String::from_utf8_lossy(&msg).into_owned();
    Ok(result)
}

pub fn expect_messages<R: Read>(reader: &mut BufReader<R>, expected: &[&ExpectedMessage]) {
    let mut results: Vec<String> = Vec::new();
    while results.len() < expected.len() {
        let msg = read_message(reader).unwrap();
        results.push(msg);
    }

    println!(
        "expect_messages:\n  results: {:#?},\n  expected: {:#?}",
        results,
        expected
    );
    assert_eq!(results.len(), expected.len());
    for (found, expected) in results.iter().zip(expected.iter()) {
        let values: serde_json::Value = serde_json::from_str(found).unwrap();
        assert!(
            values
                .get("jsonrpc")
                .expect("Missing jsonrpc field")
                .as_str()
                .unwrap() == "2.0",
            "Bad jsonrpc field"
        );
        if let Some(id) = expected.id {
            assert_eq!(
                values
                    .get("id")
                    .expect("Missing id field")
                    .as_u64()
                    .unwrap(),
                id,
                "Unexpected id"
            );
        }
        for c in expected.contains.iter() {
            found
                .find(c)
                .expect(&format!("Could not find `{}` in `{}`", c, found));
        }
    }
}

pub struct RlsHandle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl RlsHandle {
    pub fn new(mut child: Child) -> RlsHandle {
        let stdin = mem::replace(&mut child.stdin, None).unwrap();
        let stdout = mem::replace(&mut child.stdout, None).unwrap();
        let stdout = BufReader::new(stdout);

        RlsHandle {
            child,
            stdin,
            stdout,
        }
    }

    pub fn send_string(&mut self, s: &str) -> io::Result<usize> {
        let full_msg = format!("Content-Length: {}\r\n\r\n{}", s.len(), s);
        self.stdin.write(full_msg.as_bytes())
    }
    pub fn send(&mut self, j: serde_json::Value) -> io::Result<usize> {
        self.send_string(&j.to_string())
    }
    pub fn notify(&mut self, method: &str, params: Option<serde_json::Value>) -> io::Result<usize> {
        let message = if let Some(params) = params {
            json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "method": method,
            })
        };

        self.send(message)
    }
    pub fn request(&mut self, id: u64, method: &str, params: Option<serde_json::Value>) -> io::Result<usize> {
        let message = if let Some(params) = params {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
            })
        };

        self.send(message)
    }
    pub fn shutdown_exit(&mut self) {
        self.request(99999, "shutdown", None).unwrap();

        self.expect_messages(&[
            &ExpectedMessage::new(Some(99999)),
        ]);

        self.notify("exit", None).unwrap();

        let ecode = self.child.wait()
            .expect("failed to wait on child rls process");

        assert!(ecode.success());
    }

    pub fn expect_messages(&mut self, expected: &[&ExpectedMessage]) {
        expect_messages(&mut self.stdout, expected);
    }
}

#[derive(PartialEq,Clone)]
struct FileBuilder {
    path: PathBuf,
    body: String
}

impl FileBuilder {
    pub fn new(path: PathBuf, body: &str) -> FileBuilder {
        FileBuilder { path: path, body: body.to_string() }
    }

    fn mk(&self) {
        self.dirname().mkdir_p();

        let mut file = fs::File::create(&self.path).unwrap_or_else(|e| {
            panic!("could not create file {}: {}", self.path.display(), e)
        });

        file.write_all(self.body.as_bytes()).unwrap();
    }

    fn dirname(&self) -> &Path {
        self.path.parent().unwrap()
    }
}

#[derive(PartialEq,Clone)]
pub struct Project{
    root: PathBuf,
}

#[must_use]
#[derive(PartialEq,Clone)]
pub struct ProjectBuilder {
    name: String,
    root: Project,
    files: Vec<FileBuilder>,
}

impl ProjectBuilder {
    pub fn new(name: &str, root: PathBuf) -> ProjectBuilder {
        ProjectBuilder {
            name: name.to_string(),
            root: Project{ root },
            files: vec![],
        }
    }

    pub fn file<B: AsRef<Path>>(mut self, path: B,
                                body: &str) -> Self {
        self._file(path.as_ref(), body);
        self
    }

    fn _file(&mut self, path: &Path, body: &str) {
        self.files.push(FileBuilder::new(self.root.root.join(path), body));
    }

    pub fn build(self) -> Project {
        // First, clean the directory if it already exists
        self.rm_root();

        // Create the empty directory
        self.root.root.mkdir_p();

        for file in self.files.iter() {
            file.mk();
        }

        self.root
    }

    fn rm_root(&self) {
        self.root.root.rm_rf()
    }
}

impl Project {
    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn rls(&self) -> Command {
        let mut cmd = Command::new(rls_exe());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .current_dir(self.root());
        cmd
    }
}

// Generates a project layout
pub fn project(name: &str) -> ProjectBuilder {
    ProjectBuilder::new(name, paths::root().join(name))
}

// Path to cargo executables
pub fn target_conf_dir() -> PathBuf {
    let mut path = env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path
}

pub fn rls_exe() -> PathBuf {
    target_conf_dir().join(format!("rls{}", env::consts::EXE_SUFFIX))
}

#[allow(dead_code)]
pub fn main_file(println: &str, deps: &[&str]) -> String {
    let mut buf = String::new();

    for dep in deps.iter() {
        buf.push_str(&format!("extern crate {};\n", dep));
    }

    buf.push_str("fn main() { println!(");
    buf.push_str(&println);
    buf.push_str("); }\n");

    buf.to_string()
}

pub fn basic_bin_manifest(name: &str) -> String {
    format!(r#"
        [package]
        name = "{}"
        version = "0.5.0"
        authors = ["wycats@example.com"]
        [[bin]]
        name = "{}"
    "#, name, name)
}

#[allow(dead_code)]
pub fn basic_lib_manifest(name: &str) -> String {
    format!(r#"
        [package]
        name = "{}"
        version = "0.5.0"
        authors = ["wycats@example.com"]
        [lib]
        name = "{}"
    "#, name, name)
}
