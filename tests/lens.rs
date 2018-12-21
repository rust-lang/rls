use rls::actions::requests;

use languageserver_types::request::Request as _;

use self::support::{fixtures_dir, rls_timeout};
use self::support::harness::compare_json;
use self::support::project_builder::ProjectBuilder;
use serde_json;
use serde_json::json;

#[allow(dead_code)]
mod support;

#[test]
fn cmd_lens_run() {
    let p = ProjectBuilder::try_from_fixture(fixtures_dir().join("lens_run"))
        .unwrap()
        .build();
    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {},
            "initializationOptions": { "cmdRun": true }
        })),
    )
    .unwrap();

    let json: Vec<_> = rls
        .wait_until_done_indexing(rls_timeout())
        .to_json_messages()
        .collect();
    assert!(json.len() >= 7);

    let request_id = 1;
    rls.request(
        request_id,
        requests::CodeLensRequest::METHOD,
        Some(json!({
            "textDocument": {
                "uri": format!("file://{}/src/main.rs", root_path.display()),
                "version": 1
            }
        })),
    )
    .unwrap();

    let json = rls.wait_until_json_id(request_id, rls_timeout());

    compare_json(
        &json["result"],
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
    );

    rls.shutdown(rls_timeout());
}
