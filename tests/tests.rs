use serde_json::{self, json, Value as JsonValue};

use std::io::Write;

use rls::actions::requests;
use rls::lsp_data::request::Request as _;

use self::support::harness::compare_json;
use self::support::project_builder::{project, ProjectBuilder};
use self::support::{fixtures_dir, rls_timeout};

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

#[test]
fn test_find_definitions() {
    const SRC: &str = r#"
        struct Foo {
        }

        impl Foo {
            fn new() {
            }
        }

        fn main() {
            Foo::new();
        }
    "#;

    let p = project("simple_workspace")
        .file("Cargo.toml", &basic_bin_manifest("bar"))
        .file("src/main.rs", SRC)
        .build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {},
            "initializationOptions": {
                "settings": {
                    "rust": {
                        "racer_completion": false
                    }
                }
            }
        })),
    )
    .unwrap();

    rls.wait_until_done_indexing(rls_timeout());

    let uri = format!("file://{}/src/main.rs", root_path.display());

    let mut results = vec![];
    let mut request_id = 1;
    for (line_index, line) in SRC.lines().enumerate() {
        for i in 0..line.len() {
            rls.request(
                request_id,
                "textDocument/definition",
                Some(json!({
                    "position": {
                        "character": i,
                        "line": line_index
                    },
                    "textDocument": {
                        "uri": uri,
                        "version": 1
                    }
                })),
            )
            .unwrap();

            let json = rls.wait_until_json_id(request_id, rls_timeout());
            let result = json["result"].as_array().unwrap();

            request_id += 1;

            if result.is_empty() {
                continue;
            }

            results.push((
                line_index,
                i,
                result
                    .iter()
                    .map(|definition| definition["range"].clone())
                    .collect::<Vec<_>>(),
            ));
        }
    }

    rls.shutdown(rls_timeout());

    // Foo
    let foo_definition: JsonValue = json!({
        "start": {
            "line": 1,
            "character": 15,
        },
        "end": {
            "line": 1,
            "character": 18,
        }
    });

    // Foo::new
    let foo_new_definition: JsonValue = json!({
        "start": {
            "line": 5,
            "character": 15,
        },
        "end": {
            "line": 5,
            "character": 18,
        }
    });


    // main
    let main_definition: JsonValue = json!({
        "start": {
            "line": 9,
            "character": 11,
        },
        "end": {
            "line": 9,
            "character": 15,
        }
    });

    let expected = [
        // struct Foo
        (1, 15, vec![foo_definition.clone()]),
        (1, 16, vec![foo_definition.clone()]),
        (1, 17, vec![foo_definition.clone()]),
        (1, 18, vec![foo_definition.clone()]),
        // impl Foo
        (4, 13, vec![foo_definition.clone()]),
        (4, 14, vec![foo_definition.clone()]),
        (4, 15, vec![foo_definition.clone()]),
        (4, 16, vec![foo_definition.clone()]),

        // fn new
        (5, 15, vec![foo_new_definition.clone()]),
        (5, 16, vec![foo_new_definition.clone()]),
        (5, 17, vec![foo_new_definition.clone()]),
        (5, 18, vec![foo_new_definition.clone()]),

        // fn main
        (9, 11, vec![main_definition.clone()]),
        (9, 12, vec![main_definition.clone()]),
        (9, 13, vec![main_definition.clone()]),
        (9, 14, vec![main_definition.clone()]),
        (9, 15, vec![main_definition.clone()]),

        // Foo::new()
        (10, 12, vec![foo_definition.clone()]),
        (10, 13, vec![foo_definition.clone()]),
        (10, 14, vec![foo_definition.clone()]),
        (10, 15, vec![foo_definition.clone()]),
        (10, 17, vec![foo_new_definition.clone()]),
        (10, 18, vec![foo_new_definition.clone()]),
        (10, 19, vec![foo_new_definition.clone()]),
        (10, 20, vec![foo_new_definition.clone()]),
    ];

    if results.len() != expected.len() {
        panic!(
            "Got different amount of completions than expected: {} vs. {}: {:#?}",
            results.len(),
            expected.len(),
            results
        )
    }

    for (i, (actual, expected)) in results.iter().zip(expected.iter()).enumerate() {
        if actual != expected {
            panic!(
                "Found different definition at index {}. Got {:#?}, expected {:#?}",
                i,
                actual,
                expected
            )
        }
    }
}
