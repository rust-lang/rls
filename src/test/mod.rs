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

#[test]
fn test_simple_goto_def() {
    init_env();
    let mut cache = types::Cache::new("hello");
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
fn init_env() {
    const FAIL_MSG: &'static str = "Error initialising environment";

    let mut cwd = env::current_dir().expect(FAIL_MSG);
    cwd.push("test_data");
    // FIXME, really we should cd into the working directory, not its parent, and work from there.
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
