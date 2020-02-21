#![allow(clippy::expect_fun_call)]

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rls::config::{Config, Inferrable};
use rls::server as ls_server;
use rls_analysis::{AnalysisHost, Target};
use rls_vfs::Vfs;
use walkdir::WalkDir;

use super::fixtures_dir;

pub(crate) struct Environment {
    pub(crate) config: Option<Config>,
    pub(crate) cache: Cache,
    pub(crate) target_path: PathBuf,
}

impl Environment {
    pub(crate) fn generate_from_fixture(fixture_dir: impl AsRef<Path>) -> Self {
        let _ = env_logger::try_init();
        if env::var("RUSTC").is_err() {
            env::set_var("RUSTC", "rustc");
        }

        let fixture_dir = fixtures_dir().join(fixture_dir.as_ref());
        let scratchpad_dir = build_scratchpad_from_fixture(fixture_dir)
            .expect("Can't copy fixture files to scratchpad");

        let target_dir = scratchpad_dir.join("target");

        let mut config = Config::default();
        config.target_dir = Inferrable::Specified(Some(target_dir.clone()));
        config.unstable_features = true;

        let cache = Cache::new(scratchpad_dir);

        Self { config: Some(config), cache, target_path: target_dir }
    }
}

impl Environment {
    pub(crate) fn with_config<F>(&mut self, f: F)
    where
        F: FnOnce(&mut Config),
    {
        let config = self.config.as_mut().unwrap();
        f(config);
    }

    // Initialize and run the internals of an LS protocol RLS server.
    pub(crate) fn mock_server(
        &mut self,
        messages: Vec<String>,
    ) -> (ls_server::LsService<RecordOutput>, LsResultList, Arc<Mutex<Config>>) {
        let analysis = Arc::new(AnalysisHost::new(Target::Debug));
        let vfs = Arc::new(Vfs::new());
        let config = Arc::new(Mutex::new(self.config.take().unwrap()));
        let reader = Box::new(MockMsgReader::new(messages));
        let output = RecordOutput::new();
        let results = output.output.clone();
        (
            ls_server::LsService::new(analysis, vfs, Arc::clone(&config), reader, output),
            results,
            config,
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

pub fn build_scratchpad_from_fixture(fixture_dir: impl AsRef<Path>) -> io::Result<PathBuf> {
    let fixture_dir = fixture_dir.as_ref();

    let dirname = fixture_dir
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No filename"))?;

    // FIXME: For now persist the path; ideally we should clean up after every test
    let genroot = tempfile::tempdir()?.into_path().join(dirname);
    // Recursively copy read-only fixture files to freshly generated scratchpad
    for entry in WalkDir::new(fixture_dir).into_iter() {
        let entry = entry?;
        let src = entry.path();

        let relative = src.strip_prefix(fixture_dir).unwrap();
        let dst = genroot.join(relative);

        if std::fs::metadata(src)?.is_dir() {
            std::fs::create_dir(dst)?;
        } else {
            std::fs::copy(src, dst)?;
        }
    }

    Ok(genroot)
}

struct MockMsgReader {
    messages: Vec<String>,
    cur: AtomicUsize,
}

impl MockMsgReader {
    fn new(messages: Vec<String>) -> MockMsgReader {
        MockMsgReader { messages, cur: AtomicUsize::new(0) }
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
pub(crate) struct RecordOutput {
    pub(crate) output: LsResultList,
    output_id: Arc<Mutex<u64>>,
}

impl RecordOutput {
    pub(crate) fn new() -> RecordOutput {
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
pub(crate) struct ExpectedMessage {
    id: Option<u64>,
    contains: Vec<String>,
}

impl ExpectedMessage {
    pub(crate) fn new(id: Option<u64>) -> ExpectedMessage {
        ExpectedMessage { id, contains: vec![] }
    }

    pub(crate) fn expect_contains(&mut self, s: &str) -> &mut ExpectedMessage {
        self.contains.push(s.to_owned());
        self
    }
}

/// This function checks for messages with a series of constraints (expecrations)
/// to appear in the buffer, removing valid messages and returning when encountering
/// some that didn't meet the expectation
pub(crate) fn expect_series(
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
pub(crate) fn expect_message(
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
        None => return Err("No message found!".into()),
    };

    let values: serde_json::Value = serde_json::from_str(&found).unwrap();
    if values.get("jsonrpc").expect("Missing jsonrpc field").as_str().unwrap() != "2.0" {
        return Err("Bad jsonrpc field".into());
    }

    if let Some(id) = expected.id {
        if values.get("id").expect("Missing id field").as_u64().unwrap() != id {
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

pub(crate) fn compare_json(actual: &serde_json::Value, expected: &str) {
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
pub(crate) struct Src<'a> {
    pub(crate) file_name: &'a Path,
    // 1 indexed
    pub(crate) line: usize,
    pub(crate) name: &'a str,
}

pub(crate) fn src<'a>(file_name: &'a Path, line: usize, name: &'a str) -> Src<'a> {
    Src { file_name, line, name }
}

pub(crate) struct Cache {
    base_path: PathBuf,
    files: HashMap<PathBuf, Vec<String>>,
}

impl Cache {
    fn new(base_path: PathBuf) -> Cache {
        Cache { base_path, files: HashMap::new() }
    }

    pub(crate) fn mk_ls_position(&mut self, src: Src<'_>) -> lsp_types::Position {
        let line = self.get_line(src);
        let col = line.find(src.name).expect(&format!("Line does not contain name {}", src.name));
        lsp_types::Position::new((src.line - 1) as u64, char_of_byte_index(&line, col) as u64)
    }

    /// Create a range covering the initial position on the line
    ///
    /// The line number uses a 0-based index.
    pub(crate) fn mk_ls_range_from_line(&mut self, line: u64) -> lsp_types::Range {
        lsp_types::Range::new(lsp_types::Position::new(line, 0), lsp_types::Position::new(line, 0))
    }

    pub(crate) fn abs_path(&self, file_name: &Path) -> PathBuf {
        let result =
            self.base_path.join(file_name).canonicalize().expect("Couldn't canonicalise path");
        if cfg!(windows) {
            // FIXME: If the \\?\ prefix is not stripped from the canonical path, the HTTP server tests fail. Why?
            let result_string = result.to_str().expect("Path contains non-utf8 characters.");
            PathBuf::from(&result_string[r"\\?\".len()..])
        } else {
            result
        }
    }

    fn get_line(&mut self, src: Src<'_>) -> String {
        let base_path = &self.base_path;
        let lines = self.files.entry(src.file_name.to_owned()).or_insert_with(|| {
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
