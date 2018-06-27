use std::{
    path::Path,
};

use url::Url;
use serde_json;
use ls_types::{
    TextDocumentIdentifier, CodeLensParams
};

use ::{
    server as ls_server,
    actions::requests,
};
use super::{
    Environment, expect_messages, request, ExpectedMessage, initialize_with_opts, InitializationOptions
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
        initialize_with_opts(0, root_path, Some(InitializationOptions {
            omit_init_build: false,
            cmd_run: true,
        })).to_string(),
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
    expect_messages(
        results.clone(),
        &[
            ExpectedMessage::new(Some(0)).expect_contains("rls.deglobImports-"),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress"),
        ],
    );

    assert_eq!(
        ls_server::LsService::handle_message(&mut server),
        ls_server::ServerStateChange::Continue
    );
    wait_for_n_results!(1, results);
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
        }]"#
    )
}

fn compare_json(actual: &serde_json::Value, expected: &str) {
    let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
    if actual != &expected {
        panic!(
            "JSON differs\nExpected:\n{}\nActual:\n{}\n",
            serde_json::to_string_pretty(&expected).unwrap(),
            serde_json::to_string_pretty(actual).unwrap(),
        );
    }
}
