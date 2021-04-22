use serde_json::{self, json};

use std::env;
use std::io::{self, Read, Write};
use std::mem;
use std::panic;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::str;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub mod client;
pub mod harness;
pub mod paths;
pub mod project_builder;

/// Returns a path to directory containing test fixtures.
pub fn fixtures_dir() -> &'static Path {
    Path::new(env!("FIXTURES_DIR"))
}

/// Returns a timeout for waiting for rls stdout messages
///
/// Env var `RLS_TEST_WAIT_FOR_AGES` allows super long waiting for CI
pub fn rls_timeout() -> Duration {
    Duration::from_secs(if std::env::var("RLS_TEST_WAIT_FOR_AGES").is_ok() { 300 } else { 30 })
}

/// Parse valid LSP stdout into a list of json messages
pub fn parse_messages(stdout: &str) -> Vec<String> {
    let mut messages = vec![];
    let mut next_message_len: usize = 0;

    for line in stdout.lines().filter(|l| !l.is_empty()) {
        if let Some(msg) = line.get(..next_message_len).filter(|s| !s.is_empty()) {
            messages.push(msg.to_owned());
        }
        next_message_len = line
            .get(next_message_len + "Content-Length: ".len()..)
            .and_then(|s| match s.trim().parse() {
                Ok(s) => Some(s),
                Err(err) => panic!("Unexpected Content-Length {:?}: {}", s.trim(), err),
            })
            .unwrap_or(0);
    }

    messages
}

pub struct RlsHandle {
    child: Child,
    stdin: ChildStdin,
    /// stdout from rls along with the last write instant
    stdout: Arc<Mutex<(String, Instant)>>,
}

impl RlsHandle {
    pub fn new(mut child: Child) -> RlsHandle {
        let stdin = mem::replace(&mut child.stdin, None).unwrap();
        let child_stdout = mem::replace(&mut child.stdout, None).unwrap();
        let stdout = Arc::new(Mutex::new((String::new(), Instant::now())));
        let processed_stdout = Arc::clone(&stdout);

        thread::spawn(move || {
            let mut rls_stdout = child_stdout;

            let mut buf = vec![0; 1024];
            loop {
                let read = rls_stdout.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                buf.truncate(read);

                buf = match String::from_utf8(buf) {
                    Ok(s) => {
                        let mut guard = processed_stdout.lock().unwrap();
                        guard.0.push_str(&s);
                        guard.1 = Instant::now();
                        vec![0; 1024]
                    }
                    Err(e) => {
                        let mut vec = e.into_bytes();
                        vec.reserve(1024);
                        vec
                    }
                }
            }
        });

        RlsHandle { child, stdin, stdout }
    }

    pub fn send_string(&mut self, s: &str) -> io::Result<usize> {
        let full_msg = format!("Content-Length: {}\r\n\r\n{}", s.len(), s);
        self.stdin.write(full_msg.as_bytes())
    }
    pub fn send(&mut self, j: &serde_json::Value) -> io::Result<usize> {
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

        self.send(&message)
    }
    pub fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> io::Result<usize> {
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

        self.send(&message)
    }

    /// Blocks until at least `count` messages have appearing in stdout.
    ///
    /// Panics if the timeout has been exceeded from call time **and** exceeded
    /// from the last rls-stdout write instant.
    pub fn wait_until<P>(&self, stdout_predicate: P, timeout: Duration) -> RlsStdout
    where
        P: Fn(&RlsStdout) -> bool,
    {
        let start = Instant::now();
        let mut stdout_len = 0;
        loop {
            let stdout = self.stdout();
            if stdout.out.len() != stdout_len {
                if stdout_predicate(&stdout) {
                    break stdout;
                }
                stdout_len = stdout.out.len();
            }

            assert!(
                self.within_timeout(start, timeout),
                "Timed out waiting {:?} for predicate, last rls-stdout write {:.1?} ago",
                timeout,
                stdout.last_write.elapsed(),
            );

            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_until_done_indexing(&self, timeout: Duration) -> RlsStdout {
        self.wait_until_done_indexing_n(1, timeout)
    }

    pub fn wait_until_done_indexing_n(&self, n: usize, timeout: Duration) -> RlsStdout {
        self.wait_until(
            |stdout| {
                stdout
                    .to_json_messages()
                    .filter(|json| {
                        json["params"]["title"] == "Indexing"
                            && json["params"]["done"].as_bool().unwrap_or(false)
                    })
                    .count()
                    >= n
            },
            timeout,
        )
    }

    /// Blocks until a json message has `json["id"] == id`.
    ///
    /// Returns the json message.
    pub fn wait_until_json_id(&self, id: u64, timeout: Duration) -> serde_json::Value {
        self.wait_until(|stdout| stdout.to_json_messages().any(|json| json["id"] == id), timeout)
            .to_json_messages()
            .rfind(|json| json["id"] == id)
            .unwrap()
    }

    pub fn stdout(&self) -> RlsStdout {
        let stdout = self.stdout.lock().unwrap();
        RlsStdout { out: stdout.0.clone(), last_write: stdout.1 }
    }

    /// Sends shutdown messages, assets successful exit of process and returns stdout
    pub fn shutdown(&mut self, timeout: Duration) -> RlsStdout {
        self.request(99999, "shutdown", None).unwrap();
        self.notify("exit", None).unwrap();

        let start = Instant::now();

        while self.within_timeout(start, timeout) {
            if let Some(ecode) = self.child.try_wait().expect("failed to wait on child rls process")
            {
                assert!(ecode.success(), "rls exit code {}", ecode);
                // wait for stdout thread to finish to avoid races
                while Arc::strong_count(&self.stdout) > 1 {
                    assert!(self.within_timeout(start, timeout));
                    thread::yield_now();
                }
                return self.stdout();
            }
        }
        panic!("Timed out shutting down rls");
    }

    /// Uses the `call_start` or last stdout write instant, whichever is later,
    /// to measure if the timeout has been passed.
    ///
    /// Also uses `timeout * 10` from the `call_start` as an absolute limit.
    fn within_timeout(&self, call_start: Instant, timeout: Duration) -> bool {
        let call_elapsed = call_start.elapsed();
        let stdout_elapsed = self.stdout.lock().unwrap().1.elapsed();

        call_elapsed.min(stdout_elapsed) < timeout && call_elapsed < timeout * 10
    }
}

impl Drop for RlsHandle {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!("---rls-stdout---\n{}\n---------------", self.stdout.lock().unwrap().0);
        }

        let _ = self.child.kill();
    }
}

#[derive(Debug, Clone)]
pub struct RlsStdout {
    out: String,
    last_write: Instant,
}

impl RlsStdout {
    /// Parse into a list of string messages.
    ///
    /// The last one should be the shutdown response.
    pub fn to_string_messages(&self) -> Vec<String> {
        parse_messages(&self.out)
    }
    /// Parse into json values.
    ///
    /// The last one should be the shutdown response.
    pub fn to_json_messages(
        &self,
    ) -> impl Iterator<Item = serde_json::Value> + DoubleEndedIterator {
        self.to_string_messages()
            .into_iter()
            .map(|msg| serde_json::from_str(&msg).unwrap_or(serde_json::Value::Null))
    }
}

impl project_builder::Project {
    pub fn spawn_rls(&self) -> RlsHandle {
        RlsHandle::new(
            Command::new(rls_exe())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .current_dir(self.root())
                .spawn()
                .unwrap(),
        )
    }
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
    buf.push_str(println);
    buf.push_str("); }\n");

    buf
}

pub fn basic_bin_manifest(name: &str) -> String {
    format!(
        r#"
        [package]
        name = "{}"
        version = "0.5.0"
        authors = ["wycats@example.com"]
        [[bin]]
        name = "{}"
    "#,
        name, name
    )
}

#[allow(dead_code)]
pub fn basic_lib_manifest(name: &str) -> String {
    format!(
        r#"
        [package]
        name = "{}"
        version = "0.5.0"
        authors = ["wycats@example.com"]
        [lib]
        name = "{}"
    "#,
        name, name
    )
}
