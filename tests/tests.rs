use serde_json::{self, json, Value as JsonValue};

use std::io::Write;

use rls::actions::requests;
use rls::lsp_data::request::Request as _;

use self::support::harness::compare_json;
use self::support::project_builder::{project, ProjectBuilder};
use self::support::{fixtures_dir, rls_timeout};

#[allow(dead_code)]
mod support;

/// Tests correct positioning of a toml parse error, use of `==` instead of `=`.
#[test]
fn cmd_invalid_toml_manifest() {
    let project = project("invalid_toml")
        .file(
            "Cargo.toml",
            r#"[package]
            name = "probably_valid"
            version == "0.1.0"
            authors = ["alexheretic@gmail.com"]
            "#,
        )
        .file(
            "src/main.rs",
            r#"
                fn main() {
                    println!("Hello world!");
                }
            "#,
        )
        .build();
    let root_path = project.root();
    let mut rls = project.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let publish = rls
        .wait_until_done_indexing(rls_timeout())
        .to_json_messages()
        .rfind(|m| m["method"] == "textDocument/publishDiagnostics")
        .expect("No publishDiagnostics");

    let uri = publish["params"]["uri"].as_str().expect("uri");
    assert!(uri.ends_with("invalid_toml/Cargo.toml"));

    let diags = &publish["params"]["diagnostics"];
    assert_eq!(diags.as_array().unwrap().len(), 1);
    assert_eq!(diags[0]["severity"], 1);
    assert!(diags[0]["message"]
        .as_str()
        .unwrap()
        .contains("failed to parse manifest"));
    assert_eq!(
        diags[0]["range"],
        json!({ "start": { "line": 2, "character": 21 }, "end": { "line": 2, "character": 22 }})
    );

    rls.shutdown(rls_timeout());
}

/// Tests correct file highlighting of workspace member manifest with invalid path dependency.
#[test]
fn cmd_invalid_member_toml_manifest() {
    let project = project("invalid_member_toml")
        .file(
            "Cargo.toml",
            r#"[package]
            name = "root_is_fine"
            version = "0.1.0"
            authors = ["alexheretic@gmail.com"]

            [dependencies]
            member_a = { path = "member_a" }

            [workspace]
            "#,
        )
        .file("src/main.rs", "fn main() {}")
        .file(
            "member_a/Cargo.toml",
            r#"[package]
            name = "member_a"
            version = "0.0.3"
            authors = ["alexheretic@gmail.com"]

            [dependencies]
            dodgy_member = { path = "dodgy_member" }
            "#,
        )
        .file("member_a/src/lib.rs", "fn ma() {}")
        .file(
            "member_a/dodgy_member/Cargo.toml",
            r#"[package]
            name = "dodgy_member"
            version = "0.5.0"
            authors = ["alexheretic@gmail.com"]

            [dependencies]
            nosuch = { path = "not-exist" }
            "#,
        )
        .file("member_a/dodgy_member/src/lib.rs", "fn dm() {}")
        .build();
    let root_path = project.root();
    let mut rls = project.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let publish = rls
        .wait_until_done_indexing(rls_timeout())
        .to_json_messages()
        .rfind(|m| m["method"] == "textDocument/publishDiagnostics")
        .expect("No publishDiagnostics");

    let uri = publish["params"]["uri"].as_str().expect("uri");
    assert!(uri.ends_with("invalid_member_toml/member_a/dodgy_member/Cargo.toml"));

    let diags = &publish["params"]["diagnostics"];
    assert_eq!(diags.as_array().unwrap().len(), 1);
    assert_eq!(diags[0]["severity"], 1);
    assert!(diags[0]["message"]
        .as_str()
        .unwrap()
        .contains("failed to read"));

    rls.shutdown(rls_timeout());
}

#[test]
fn cmd_invalid_member_dependency_resolution() {
    let project = project("invalid_member_resolution")
        .file(
            "Cargo.toml",
            r#"[package]
            name = "root_is_fine"
            version = "0.1.0"
            authors = ["alexheretic@gmail.com"]

            [dependencies]
            member_a = { path = "member_a" }

            [workspace]
            "#,
        )
        .file("src/main.rs", "fn main() {}")
        .file(
            "member_a/Cargo.toml",
            r#"[package]
            name = "member_a"
            version = "0.0.5"
            authors = ["alexheretic@gmail.com"]

            [dependencies]
            dodgy_member = { path = "dodgy_member" }
            "#,
        )
        .file("member_a/src/lib.rs", "fn ma() {}")
        .file(
            "member_a/dodgy_member/Cargo.toml",
            r#"[package]
            name = "dodgy_member"
            version = "0.6.0"
            authors = ["alexheretic@gmail.com"]

            [dependencies]
            nosuchdep123 = "1.2.4"
            "#,
        )
        .file("member_a/dodgy_member/src/lib.rs", "fn dm() {}")
        .build();
    let root_path = project.root();
    let mut rls = project.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let publish = rls
        .wait_until_done_indexing(rls_timeout())
        .to_json_messages()
        .rfind(|m| m["method"] == "textDocument/publishDiagnostics")
        .expect("No publishDiagnostics");

    let uri = publish["params"]["uri"].as_str().expect("uri");
    assert!(uri.ends_with("invalid_member_resolution/member_a/dodgy_member/Cargo.toml"));

    let diags = &publish["params"]["diagnostics"];
    assert_eq!(diags.as_array().unwrap().len(), 1);
    assert_eq!(diags[0]["severity"], 1);
    assert!(diags[0]["message"]
        .as_str()
        .unwrap()
        .contains("no matching package named `nosuchdep123`"));

    rls.shutdown(rls_timeout());
}

#[test]
fn cmd_handle_utf16_unit_text_edits() {
    let project = project("cmd_handle_utf16_unit_text_edits")
        .file(
            "Cargo.toml",
            r#"[package]
            name = "cmd_handle_utf16_unit_text_edits"
            version = "0.1.0"
            authors = ["example@example.com"]
            "#,
        )
        .file("src/main.rs", "fn main() {}")
        .file("src/unrelated.rs", "😢")
        .build();
    let root_path = project.root();
    let mut rls = project.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    rls.wait_until_done_indexing(rls_timeout());

    rls.notify(
        "textDocument/didChange",
        Some(json!(
        {"textDocument": {
                "uri": format!("file://{}/src/unrelated.rs", root_path.display()),
                "version": 1
            },
            // "😢" -> ""
            "contentChanges": [
                {
                    "range": {
                        "start": {
                            "line":0,
                            "character":0
                        },
                        "end":{
                            "line":0,
                            "character":2
                        }
                    },
                    "rangeLength":2,
                    "text":""
                }
            ]
        }))
    ).unwrap();

    rls.shutdown(rls_timeout());
}

/// Ensures that wide characters do not prevent RLS from calculating correct
/// 'whole file' LSP range.
#[test]
fn cmd_format_utf16_range() {
    let project = project("cmd_format_utf16_range")
        .file(
            "Cargo.toml",
            r#"[package]
            name = "cmd_format_utf16_range"
            version = "0.1.0"
            authors = ["example@example.com"]
            "#,
        )
        .file("src/main.rs", "/* 😢😢😢😢😢😢😢 */ fn main() { }")
        .build();
    let root_path = project.root();
    let mut rls = project.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    rls.wait_until_done_indexing(rls_timeout());

    let request_id = 66;
    rls.request(
        request_id,
        "textDocument/formatting",
        Some(json!(
        {
            "textDocument": {
                "uri": format!("file://{}/src/main.rs", root_path.display()),
                "version": 1
            },
            "options": {
                "tabSize": 4,
                "insertSpaces": true
            }
        }))
    ).unwrap();

    let json = rls.wait_until_json_id(request_id, rls_timeout());
    eprintln!("{:#?}", json);

    let result = json["result"].as_array().unwrap();
    let new_text: Vec<_> = result
        .iter()
        .map(|o| o["newText"].as_str().unwrap())
        .map(|text| text.replace('\r', ""))
        .collect();
    // Actual formatting isn't important - what is, is that the buffer isn't
    // malformed and code stays semantically equivalent.
    assert_eq!(new_text, vec!["/* 😢😢😢😢😢😢😢 */\nfn main() {}\n"]);

    rls.shutdown(rls_timeout());
}

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
