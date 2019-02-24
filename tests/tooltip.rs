use rls::actions::hover::tooltip;
use rls::actions::{ActionContext, InitActionContext};
use rls::config;
use rls::lsp_data::MarkedString;
use rls::lsp_data::{ClientCapabilities, InitializationOptions};
use rls::lsp_data::{Position, TextDocumentIdentifier, TextDocumentPositionParams};
use rls::server::{Output, RequestId};
use rls_analysis as analysis;
use rls_vfs::Vfs;
use serde_derive::{Deserialize, Serialize};
use serde_json as json;
use url::Url;

use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub fn fixtures_dir() -> &'static Path {
    Path::new(env!("FIXTURES_DIR"))
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct Test {
    /// Relative to the project _source_ dir (e.g. relative to $FIXTURES_DIR/hover/src)
    pub file: String,
    /// One-based line number
    pub line: u64,
    /// One-based column number
    pub col: u64,
}

impl Test {
    fn load_result(&self, dir: &Path) -> Result<TestResult, String> {
        let path = self.path(dir);
        let file = fs::File::open(path.clone())
            .map_err(|e| format!("failed to open hover test result: {:?} ({:?})", path, e))?;
        let result: Result<TestResult, String> = json::from_reader(file)
            .map_err(|e| format!("failed to deserialize hover test result: {:?} ({:?})", path, e));
        result
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct TestResult {
    test: Test,
    data: Result<Vec<MarkedString>, String>,
}

impl TestResult {
    fn save(&self, result_dir: &Path) -> Result<(), String> {
        let path = self.test.path(result_dir);
        let data = json::to_string_pretty(&self)
            .map_err(|e| format!("failed to serialize hover test result: {:?} ({:?})", path, e))?;
        fs::write(&path, data)
            .map_err(|e| format!("failed to save hover test result: {:?} ({:?})", path, e))
    }

    /// Returns true if data is equal to `other` relaxed so that
    /// `MarkedString::String` in `other` need only start with self's.
    fn has_same_data_start(&self, other: &Self) -> bool {
        match (&self.data, &other.data) {
            (Ok(data), Ok(them)) if data.len() == them.len() => data
                .iter()
                .zip(them.iter())
                .map(|(us, them)| match (us, them) {
                    (MarkedString::String(us), MarkedString::String(them)) => them.starts_with(us),
                    _ => us == them,
                })
                .all(|r| r),
            _ => false,
        }
    }
}

impl Test {
    pub fn new(file: &str, line: u64, col: u64) -> Test {
        Test { file: file.into(), line, col }
    }

    fn path(&self, result_dir: &Path) -> PathBuf {
        result_dir.join(format!("{}.{:04}_{:03}.json", self.file, self.line, self.col))
    }

    fn run(&self, project_dir: &Path, ctx: &InitActionContext) -> TestResult {
        let url = Url::from_file_path(project_dir.join("src").join(&self.file)).expect(&self.file);
        let doc_id = TextDocumentIdentifier::new(url);
        let position = Position::new(self.line - 1u64, self.col - 1u64);
        let params = TextDocumentPositionParams::new(doc_id, position);
        let result = tooltip(&ctx, &params)
            .map_err(|e| format!("tooltip error: {:?}", e))
            .map(|v| v.contents);

        TestResult { test: self.clone(), data: result }
    }
}

#[derive(PartialEq, Eq)]
pub struct TestFailure {
    /// The test case, indicating file, line, and column
    pub test: Test,
    /// The location of the loaded result input.
    pub expect_file: PathBuf,
    /// The location of the saved result output.
    pub actual_file: PathBuf,
    /// The expected outcome. The outer `Result` relates to errors while
    /// loading saved data. The inner `Result` is the saved output from
    /// `hover::tooltip`.
    pub expect_data: Result<Result<Vec<MarkedString>, String>, String>,
    /// The current output from `hover::tooltip`. The inner `Result`
    /// is the output from `hover::tooltip`.
    pub actual_data: Result<Result<Vec<MarkedString>, String>, ()>,
}

impl fmt::Debug for TestFailure {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("TestFailure")
            .field("test", &self.test)
            .field("expect_file", &self.expect_file)
            .field("actual_file", &self.actual_file)
            .field("expect_data", &self.expect_data)
            .field("actual_data", &self.actual_data)
            .finish()?;

        let expected = format!("{:#?}", self.expect_data);
        let actual = format!("{:#?}", self.actual_data);
        write!(fmt, "-diff: {}", difference::Changeset::new(&expected, &actual, ""))
    }
}

#[derive(Clone, Default)]
pub struct LineOutput {
    req_id: Arc<Mutex<u64>>,
    lines: Arc<Mutex<Vec<String>>>,
}

impl LineOutput {
    /// Clears and returns the recorded output lines
    pub fn reset(&self) -> Vec<String> {
        let mut lines = self.lines.lock().unwrap();
        let mut swapped = Vec::new();
        ::std::mem::swap(&mut *lines, &mut swapped);
        swapped
    }
}

impl Output for LineOutput {
    fn response(&self, output: String) {
        self.lines.lock().unwrap().push(output);
    }

    fn provide_id(&self) -> RequestId {
        let mut id = self.req_id.lock().unwrap();
        *id += 1;
        RequestId::Num(*id as u64)
    }
}

pub struct TooltipTestHarness {
    ctx: InitActionContext,
    project_dir: PathBuf,
    _working_dir: tempfile::TempDir,
}

impl TooltipTestHarness {
    /// Creates a new `TooltipTestHarness`. The `project_dir` must contain
    /// a valid rust project with a `Cargo.toml`.
    pub fn new<O: Output>(
        project_dir: PathBuf,
        output: &O,
        racer_fallback_completion: bool,
    ) -> TooltipTestHarness {
        let _ = env_logger::try_init();

        // Prevent the hover test project build from trying to use the rls test
        // binary as a rustc shim. See RlsExecutor::exec for more information.
        if env::var("RUSTC").is_err() {
            env::set_var("RUSTC", "rustc");
        }

        let client_caps = ClientCapabilities {
            code_completion_has_snippet_support: true,
            related_information_support: true,
        };

        let _working_dir = tempfile::tempdir().expect("Couldn't create tempdir");
        let target_dir = _working_dir.path().to_owned();

        let config = config::Config {
            target_dir: config::Inferrable::Specified(Some(target_dir)),
            racer_completion: racer_fallback_completion,
            // FIXME(#1195): This led to spurious failures on macOS.
            // Possibly because regular build and #[cfg(test)] did race or
            // rls-analysis didn't lower them properly?
            all_targets: false,
            ..Default::default()
        };

        let config = Arc::new(Mutex::new(config));
        let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
        let vfs = Arc::new(Vfs::new());

        let ctx = {
            let mut ctx = ActionContext::new(analysis, vfs, config);
            ctx.init(project_dir.clone(), InitializationOptions::default(), client_caps, output)
                .unwrap();
            ctx.inited().unwrap()
        };

        ctx.block_on_build();

        TooltipTestHarness { ctx, project_dir, _working_dir }
    }

    /// Execute a series of tooltip tests. The test results will be saved in `save_dir`.
    /// Each test will attempt to load a previous result from the `load_dir` and compare
    /// the results. If a matching file can't be found or the compared data mismatches,
    /// the test case fails. The output file names are derived from the source filename,
    /// line number, and column. The execution will return an `Err` if either the save or
    /// load directories do not exist nor could be created.
    pub fn run_tests(
        &self,
        tests: &[Test],
        load_dir: PathBuf,
        save_dir: PathBuf,
    ) -> Result<Vec<TestFailure>, String> {
        fs::create_dir_all(&load_dir).map_err(|e| {
            format!("load_dir does not exist and could not be created: {:?} ({:?})", load_dir, e)
        })?;
        fs::create_dir_all(&save_dir).map_err(|e| {
            format!("save_dir does not exist and could not be created: {:?} ({:?})", save_dir, e)
        })?;

        let results: Vec<TestResult> = tests
            .iter()
            .map(|test| {
                let result = test.run(&self.project_dir, &self.ctx);
                result.save(&save_dir).unwrap();
                result
            })
            .collect();

        let failures: Vec<TestFailure> = results
            .into_iter()
            .map(|actual_result: TestResult| match actual_result.test.load_result(&load_dir) {
                Ok(expect_result) => {
                    if actual_result.test != expect_result.test {
                        let e = format!("Mismatched test: {:?}", expect_result.test);
                        Some((Err(e), actual_result))
                    } else if expect_result.has_same_data_start(&actual_result) {
                        None
                    } else {
                        Some((Ok(expect_result), actual_result))
                    }
                }
                Err(e) => Some((Err(e), actual_result)),
            })
            .filter_map(|failed_result| failed_result)
            .map(|(result, actual_result)| {
                let load_file = actual_result.test.path(&load_dir);
                let save_file = actual_result.test.path(&save_dir);

                TestFailure {
                    test: actual_result.test,
                    expect_data: result.map(|x| x.data),
                    expect_file: load_file,
                    actual_data: Ok(actual_result.data),
                    actual_file: save_file,
                }
            })
            .collect();

        Ok(failures)
    }
}

impl Drop for TooltipTestHarness {
    fn drop(&mut self) {
        self.ctx.wait_for_concurrent_jobs();
    }
}

enum RacerFallback {
    Yes,
    No,
}

impl From<RacerFallback> for bool {
    fn from(arg: RacerFallback) -> bool {
        match arg {
            RacerFallback::Yes => true,
            RacerFallback::No => false,
        }
    }
}

fn run_tooltip_tests(
    tests: &[Test],
    proj_dir: PathBuf,
    racer_completion: RacerFallback,
) -> Result<(), Box<dyn std::error::Error>> {
    let out = LineOutput::default();

    let save_dir_guard = tempfile::tempdir().unwrap();
    let save_dir = save_dir_guard.path().to_owned();
    let load_dir = proj_dir.join("save_data");

    let harness = TooltipTestHarness::new(proj_dir, &out, racer_completion.into());

    out.reset();

    let failures = harness.run_tests(tests, load_dir, save_dir)?;

    if failures.is_empty() {
        Ok(())
    } else {
        eprintln!("{}\n\n", out.reset().join("\n"));
        eprintln!("Failures (\x1b[91mexpected\x1b[92mactual\x1b[0m): {:#?}\n\n", failures);
        Err(format!("{} of {} tooltip tests failed", failures.len(), tests.len()).into())
    }
}

#[test]
#[ignore] // FIXME: For now these hang in Rust CI, fix me and reenable later
fn test_tooltip() -> Result<(), Box<dyn std::error::Error>> {
    let _ = env_logger::try_init();

    let tests = vec![
        Test::new("test_tooltip_01.rs", 3, 11),
        Test::new("test_tooltip_01.rs", 5, 7),
        Test::new("test_tooltip_01.rs", 7, 7),
        Test::new("test_tooltip_01.rs", 11, 13),
        Test::new("test_tooltip_01.rs", 13, 9),
        Test::new("test_tooltip_01.rs", 13, 16),
        Test::new("test_tooltip_01.rs", 15, 8),
        Test::new("test_tooltip_01.rs", 17, 8),
        Test::new("test_tooltip_01.rs", 17, 8),
        Test::new("test_tooltip_01.rs", 20, 11),
        Test::new("test_tooltip_01.rs", 22, 10),
        Test::new("test_tooltip_01.rs", 22, 19),
        Test::new("test_tooltip_01.rs", 22, 26),
        Test::new("test_tooltip_01.rs", 22, 35),
        Test::new("test_tooltip_01.rs", 22, 49),
        Test::new("test_tooltip_01.rs", 23, 11),
        Test::new("test_tooltip_01.rs", 24, 16),
        Test::new("test_tooltip_01.rs", 24, 23),
        Test::new("test_tooltip_01.rs", 25, 16),
        Test::new("test_tooltip_01.rs", 25, 23),
        Test::new("test_tooltip_01.rs", 26, 16),
        Test::new("test_tooltip_01.rs", 26, 23),
        Test::new("test_tooltip_01.rs", 32, 15),
        Test::new("test_tooltip_01.rs", 46, 6),
        Test::new("test_tooltip_01.rs", 56, 6),
        Test::new("test_tooltip_01.rs", 57, 30),
        Test::new("test_tooltip_01.rs", 58, 11),
        Test::new("test_tooltip_01.rs", 58, 26),
        Test::new("test_tooltip_01.rs", 65, 10),
        Test::new("test_tooltip_01.rs", 75, 14),
        Test::new("test_tooltip_01.rs", 75, 50),
        Test::new("test_tooltip_01.rs", 75, 54),
        Test::new("test_tooltip_01.rs", 76, 7),
        Test::new("test_tooltip_01.rs", 76, 10),
        Test::new("test_tooltip_01.rs", 77, 20),
        Test::new("test_tooltip_01.rs", 78, 18),
        Test::new("test_tooltip_01.rs", 83, 11),
        Test::new("test_tooltip_01.rs", 85, 25),
        Test::new("test_tooltip_01.rs", 99, 21),
        Test::new("test_tooltip_01.rs", 103, 21),
        Test::new("test_tooltip_mod.rs", 12, 14),
        Test::new("test_tooltip_mod_use.rs", 1, 14),
        Test::new("test_tooltip_mod_use.rs", 2, 14),
        Test::new("test_tooltip_mod_use.rs", 2, 25),
        Test::new("test_tooltip_mod_use.rs", 3, 28),
    ];

    run_tooltip_tests(&tests, fixtures_dir().join("hover"), RacerFallback::No)
}

#[test]
#[ignore] // FIXME: For now these hang in Rust CI, fix me and reenable later
fn test_tooltip_racer() -> Result<(), Box<dyn std::error::Error>> {
    let _ = env_logger::try_init();

    let tests = vec![
        Test::new("test_tooltip_01.rs", 70, 11),
        Test::new("test_tooltip_01.rs", 83, 18),
        Test::new("test_tooltip_mod_use_external.rs", 1, 7),
        Test::new("test_tooltip_mod_use_external.rs", 2, 7),
        Test::new("test_tooltip_mod_use_external.rs", 2, 12),
    ];

    run_tooltip_tests(&tests, fixtures_dir().join("hover"), RacerFallback::Yes)
}

/// Note: This test is ignored as it doesn't work in the rust-lang/rust repo.
/// It is enabled on CI.
/// Run with `cargo test test_tooltip_std -- --ignored`
#[test]
#[ignore]
fn test_tooltip_std() -> Result<(), Box<dyn std::error::Error>> {
    let _ = env_logger::try_init();

    let tests = vec![
        Test::new("test_tooltip_std.rs", 8, 15),
        Test::new("test_tooltip_std.rs", 8, 27),
        Test::new("test_tooltip_std.rs", 9, 7),
        Test::new("test_tooltip_std.rs", 9, 12),
        Test::new("test_tooltip_std.rs", 10, 12),
        Test::new("test_tooltip_std.rs", 10, 20),
        Test::new("test_tooltip_std.rs", 11, 25),
        Test::new("test_tooltip_std.rs", 12, 33),
        Test::new("test_tooltip_std.rs", 13, 11),
        Test::new("test_tooltip_std.rs", 13, 18),
        Test::new("test_tooltip_std.rs", 14, 24),
        Test::new("test_tooltip_std.rs", 15, 17),
        Test::new("test_tooltip_std.rs", 15, 25),
    ];

    run_tooltip_tests(&tests, fixtures_dir().join("hover"), RacerFallback::No)
}

/// Note: This test is ignored as it doesn't work in the rust-lang/rust repo.
/// It is enabled on CI.
/// Run with `cargo test test_tooltip_std -- --ignored`
#[test]
#[ignore]
fn test_tooltip_std_racer() -> Result<(), Box<dyn std::error::Error>> {
    let _ = env_logger::try_init();

    let tests = vec![
        // these test std stuff
        Test::new("test_tooltip_mod_use_external.rs", 4, 12),
        Test::new("test_tooltip_mod_use_external.rs", 5, 12),
    ];

    run_tooltip_tests(&tests, fixtures_dir().join("hover"), RacerFallback::Yes)
}
