use std::path::Path;

use serde_json;
use url::Url;
use languageserver_types::{TextDocumentIdentifier, CodeLensParams};

use crate::{
    server as ls_server,
    actions::requests,
    lsp_data::InitializationOptions,
    test::{
        request, initialize_with_opts,
        harness::{expect_message, expect_fuzzy, compare_json, Environment, ExpectedMessage},
    },
};

#[test]
fn test_lens_run() {
    let mut env = Environment::new("lens_run");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let root_path = root_path.as_os_str().to_str().map(|x| x.to_owned());
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path))
        .expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url.clone());
    let messages = vec![
        initialize_with_opts(
            0,
            root_path,
            Some(InitializationOptions {
                omit_init_build: false,
                cmd_run: true,
            }),
        ).to_string(),
        request::<requests::CodeLensRequest>(
            100,
            CodeLensParams {
                text_document: text_doc.clone(),
            },
        ).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(
        ls_server::LsService::handle_message(&mut server),
        ls_server::ServerStateChange::Continue
    );
    expect_message(
        &mut server,
        results.clone(),
        &ExpectedMessage::new(Some(0))
            .expect_contains(r#""codeLensProvider":{"resolveProvider":false}"#),
    );

    expect_fuzzy(&mut server, results.clone(), vec!["progress"]);

    assert_eq!(
        ls_server::LsService::handle_message(&mut server),
        ls_server::ServerStateChange::Continue
    );
    server.wait_for_concurrent_jobs();
    let result: serde_json::Value = serde_json::from_str(&results.lock().unwrap().remove(0)).unwrap();
    compare_json(
        result.get("result").unwrap(),
        r#"[{
            "command": {
              "command": "rls.run",
              "title": "Run test",
              "arguments": [{
                  "args": [ "test", "--", "--nocapture", "test_foo" ],
                  "binary": "cargo",
                  "env": { "RUST_BACKTRACE": "short" }
              }]
            },
            "range": {
              "start": { "character": 3, "line": 14 },
              "end": { "character": 11, "line": 14 }
            }
        }]"#,
    )
}
