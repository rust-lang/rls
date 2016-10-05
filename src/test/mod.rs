// Utilities and infrastructure for testing. Tests in this module test the
// testing infrastructure *not* the RLS. 

mod types;

use std::env;
use std::sync::Arc;

use actions::Provider;
use analysis;
use build;
use ide::Output;
use server;
use vfs;

use self::types::src;

use serde_json;
use std::path::PathBuf;

#[test]
fn test_simple_goto_def() {
    let _cr = CwdRestorer::new();

    init_env("hello");
    let mut cache = types::Cache::new(".");
    mock_server(|server| {
        // Build.
        assert_non_empty(&server.handle_action("/on_save", &cache.mk_save_input("src/main.rs")));

        // Goto def.
        let output = server.handle_action("/goto_def", &cache.mk_input(src("src/main.rs", 3, "world")));
        assert_output(&mut cache, &output, src("src/main.rs", 2, "world"), Provider::Compiler);
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
        let output = server.handle_action("/goto_def", &cache.mk_input(src("src/main.rs", 3, "world")));
        assert_output(&mut cache, &output, src("src/main.rs", 2, "world"), Provider::Compiler);
    });
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

// Initialise the environment for a test.
fn init_env(project_dir: &str) {
    let mut cwd = env::current_dir().expect(FAIL_MSG);
    cwd.push("test_data");
    cwd.push(project_dir);
    env::set_current_dir(cwd).expect(FAIL_MSG);
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
