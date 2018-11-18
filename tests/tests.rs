// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#[macro_use]
extern crate serde_json;

mod support;

use self::support::{basic_bin_manifest, project};
use crate::support::RlsStdout;
use std::io::Write;
use std::time::Duration;

/// Returns a timeout for waiting for rls stdout messages
///
/// Env var `RLS_TEST_WAIT_FOR_AGES` allows super long waiting for CI
fn rls_timeout() -> Duration {
    Duration::from_secs(if std::env::var("RLS_TEST_WAIT_FOR_AGES").is_ok() {
        300
    } else {
        15
    })
}

#[test]
fn cmd_test_infer_bin() {
    let p = project("simple_workspace")
        .file("Cargo.toml", &basic_bin_manifest("foo"))
        .file(
            "src/main.rs",
            r#"
                struct UnusedBin;
                fn main() {
                    println!("Hello world!");
                }
            "#,
        )
        .build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let json: Vec<_> = rls
        .wait_until_done_indexing(rls_timeout())
        .to_json_messages()
        .filter(|json| json["method"] != "window/progress")
        .collect();

    assert!(json.len() > 1);

    assert!(json[0]["result"]["capabilities"].is_object());

    assert_eq!(json[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(json[1]["params"]["diagnostics"][0]["code"], "dead_code");

    rls.shutdown(rls_timeout());
}

/// Test includes window/progress regression testing
#[test]
fn cmd_test_simple_workspace() {
    let p = project("simple_workspace")
        .file(
            "Cargo.toml",
            r#"
                [workspace]
                members = [
                "member_lib",
                "member_bin",
                ]
            "#,
        )
        .file(
            "Cargo.lock",
            r#"
                [root]
                name = "member_lib"
                version = "0.1.0"

                [[package]]
                name = "member_bin"
                version = "0.1.0"
                dependencies = [
                "member_lib 0.1.0",
                ]
            "#,
        )
        .file(
            "member_bin/Cargo.toml",
            r#"
                [package]
                name = "member_bin"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
                member_lib = { path = "../member_lib" }
            "#,
        )
        .file(
            "member_bin/src/main.rs",
            r#"
                extern crate member_lib;

                fn main() {
                    let a = member_lib::MemberLibStruct;
                }
            "#,
        )
        .file(
            "member_lib/Cargo.toml",
            r#"
                [package]
                name = "member_lib"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
            "#,
        )
        .file(
            "member_lib/src/lib.rs",
            r#"
                pub struct MemberLibStruct;

                struct Unused;

                #[cfg(test)]
                mod tests {
                    #[test]
                    fn it_works() {
                    }
                }
            "#,
        )
        .build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let json: Vec<_> = rls
        .wait_until_done_indexing(rls_timeout())
        .to_json_messages()
        .collect();
    assert!(json.len() >= 11);

    assert!(json[0]["result"]["capabilities"].is_object());

    assert_eq!(json[1]["method"], "window/progress");
    assert_eq!(json[1]["params"]["title"], "Building");
    assert_eq!(json[1]["params"].get("message"), None);

    // order of member_lib/member_bin is undefined
    for json in &json[2..6] {
        assert_eq!(json["method"], "window/progress");
        assert_eq!(json["params"]["title"], "Building");
        assert!(json["params"]["message"]
            .as_str()
            .unwrap()
            .starts_with("member_"));
    }

    assert_eq!(json[6]["method"], "window/progress");
    assert_eq!(json[6]["params"]["done"], true);
    assert_eq!(json[6]["params"]["title"], "Building");

    assert_eq!(json[7]["method"], "window/progress");
    assert_eq!(json[7]["params"]["title"], "Indexing");

    assert_eq!(json[8]["method"], "textDocument/publishDiagnostics");

    assert_eq!(json[9]["method"], "textDocument/publishDiagnostics");

    assert_eq!(json[10]["method"], "window/progress");
    assert_eq!(json[10]["params"]["done"], true);
    assert_eq!(json[10]["params"]["title"], "Indexing");

    let json = rls
        .shutdown(rls_timeout())
        .to_json_messages()
        .nth(11)
        .expect("No shutdown response received");

    assert_eq!(json["id"], 99999);
}

#[test]
fn cmd_changing_workspace_lib_retains_bin_diagnostics() {
    let p = project("simple_workspace")
        .file(
            "Cargo.toml",
            r#"
                [workspace]
                members = [
                "library",
                "binary",
                ]
            "#,
        )
        .file(
            "library/Cargo.toml",
            r#"
                [package]
                name = "library"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#,
        )
        .file(
            "library/src/lib.rs",
            r#"
                pub fn fetch_u32() -> u32 {
                    let unused = ();
                    42
                }
                #[cfg(test)]
                mod test {
                    #[test]
                    fn my_test() {
                        let test_val: u32 = super::fetch_u32();
                    }
                }
            "#,
        )
        .file(
            "binary/Cargo.toml",
            r#"
                [package]
                name = "binary"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
                library = { path = "../library" }
            "#,
        )
        .file(
            "binary/src/main.rs",
            r#"
                extern crate library;

                fn main() {
                    let val: u32 = library::fetch_u32();
                }
            "#,
        )
        .build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let to_publish_messages = |stdout: &RlsStdout| {
        stdout
            .to_json_messages()
            .filter(|json| json["method"] == "textDocument/publishDiagnostics")
    };
    let rfind_diagnostics_with_uri = |stdout, uri_end| {
        to_publish_messages(stdout)
            .rfind(|json| json["params"]["uri"].as_str().unwrap().ends_with(uri_end))
            .unwrap()
    };

    let stdout = rls.wait_until_done_indexing(rls_timeout());

    let lib_diagnostic = rfind_diagnostics_with_uri(&stdout, "library/src/lib.rs");
    assert_eq!(
        lib_diagnostic["params"]["diagnostics"][0]["code"],
        "unused_variables"
    );
    let bin_diagnostic = rfind_diagnostics_with_uri(&stdout, "binary/src/main.rs");
    assert_eq!(
        bin_diagnostic["params"]["diagnostics"][0]["code"],
        "unused_variables"
    );

    rls.notify(
        "textDocument/didChange",
        Some(json!({
                "contentChanges": [
                    {
                        "range": {
                            "start": {
                                "line": 1,
                                "character": 38,
                            },
                            "end": {
                                "line": 1,
                                "character": 41,
                            }
                        },
                        "rangeLength": 3,
                        "text": "u64"
                    }
                ],
                "textDocument": {
                    "uri": format!("file://{}/library/src/lib.rs", root_path.as_path().display()),
                    "version": 0
                }
            })),
    )
    .unwrap();

    let stdout = rls.wait_until_done_indexing_n(2, rls_timeout());

    // lib unit tests have compile errors
    let lib_diagnostic = rfind_diagnostics_with_uri(&stdout, "library/src/lib.rs");
    let error_diagnostic = lib_diagnostic["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "E0308")
        .expect("expected lib error diagnostic");
    assert!(error_diagnostic["message"]
        .as_str()
        .unwrap()
        .contains("expected u32, found u64"));

    // bin depending on lib picks up type mismatch
    let bin_diagnostic = rfind_diagnostics_with_uri(&stdout, "binary/src/main.rs");
    let error_diagnostic = bin_diagnostic["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "E0308")
        .expect("expected bin error diagnostic");
    assert!(error_diagnostic["message"]
        .as_str()
        .unwrap()
        .contains("expected u32, found u64"));

    rls.notify(
        "textDocument/didChange",
        Some(json!({
                "contentChanges": [
                    {
                        "range": {
                            "start": {
                                "line": 1,
                                "character": 38,
                            },
                            "end": {
                                "line": 1,
                                "character": 41,
                            }
                        },
                        "rangeLength": 3,
                        "text": "u32"
                    }
                ],
                "textDocument": {
                    "uri": format!("file://{}/library/src/lib.rs", root_path.as_path().display()),
                    "version": 1
                }
            })),
    )
    .unwrap();

    let stdout = rls.wait_until_done_indexing_n(3, rls_timeout());
    let lib_diagnostic = rfind_diagnostics_with_uri(&stdout, "library/src/lib.rs");
    assert_eq!(
        lib_diagnostic["params"]["diagnostics"][0]["code"],
        "unused_variables"
    );
    let bin_diagnostic = rfind_diagnostics_with_uri(&stdout, "binary/src/main.rs");
    assert_eq!(
        bin_diagnostic["params"]["diagnostics"][0]["code"],
        "unused_variables"
    );

    rls.shutdown(rls_timeout());
}

#[test]
fn cmd_implicit_workspace_pick_up_lib_changes() {
    let p = project("simple_workspace")
        .file(
            "Cargo.toml",
            r#"
                [package]
                name = "binary"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]

                [dependencies]
                inner = { path = "inner" }
            "#,
        )
        .file(
            "src/main.rs",
            r#"
                extern crate inner;

                fn main() {
                    let val = inner::foo();
                }
            "#,
        )
        .file(
            "inner/Cargo.toml",
            r#"
                [package]
                name = "inner"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#,
        )
        .file(
            "inner/src/lib.rs",
            r#"
                pub fn foo() -> u32 { 42 }
            "#,
        )
        .build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let to_publish_messages = |stdout: &RlsStdout| {
        stdout
            .to_json_messages()
            .filter(|json| json["method"] == "textDocument/publishDiagnostics")
    };
    let rfind_diagnostics_with_uri = |stdout, uri_end| {
        to_publish_messages(stdout)
            .rfind(|json| json["params"]["uri"].as_str().unwrap().ends_with(uri_end))
            .unwrap()
    };

    let stdout = rls.wait_until_done_indexing(rls_timeout());

    let bin_diagnostic = rfind_diagnostics_with_uri(&stdout, "src/main.rs");
    assert_eq!(
        bin_diagnostic["params"]["diagnostics"][0]["code"],
        "unused_variables"
    );

    rls.notify(
        "textDocument/didChange",
        Some(json!({
                "contentChanges": [
                    {
                        "range": {
                            "start": {
                                "line": 1,
                                "character": 23,
                            },
                            "end": {
                                "line": 1,
                                "character": 26,
                            }
                        },
                        "rangeLength": 3,
                        "text": "bar"
                    }
                ],
                "textDocument": {
                    "uri": format!("file://{}/inner/src/lib.rs", root_path.as_path().display()),
                    "version": 0
                }
            })),
    )
    .unwrap();

    let stdout = rls.wait_until_done_indexing_n(2, rls_timeout());

    // bin depending on lib picks up type mismatch
    let bin_diagnostic = rfind_diagnostics_with_uri(&stdout, "src/main.rs");
    let error_diagnostic = bin_diagnostic["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "E0425")
        .expect("expected bin error diagnostic");
    assert!(error_diagnostic["message"]
        .as_str()
        .unwrap()
        .contains("cannot find function `foo` in module `inner`"));

    rls.notify(
        "textDocument/didChange",
        Some(json!({
                "contentChanges": [
                    {
                        "range": {
                            "start": {
                                "line": 1,
                                "character": 23,
                            },
                            "end": {
                                "line": 1,
                                "character": 26,
                            }
                        },
                        "rangeLength": 3,
                        "text": "foo"
                    }
                ],
                "textDocument": {
                    "uri": format!("file://{}/inner/src/lib.rs", root_path.as_path().display()),
                    "version": 1
                }
            })),
    )
    .unwrap();

    let stdout = rls.wait_until_done_indexing_n(3, rls_timeout());
    let bin_diagnostic = rfind_diagnostics_with_uri(&stdout, "src/main.rs");
    assert_eq!(
        bin_diagnostic["params"]["diagnostics"][0]["code"],
        "unused_variables"
    );

    rls.shutdown(rls_timeout());
}

#[test]
fn cmd_test_complete_self_crate_name() {
    let p = project("ws_with_test_dir")
        .file(
            "Cargo.toml",
            r#"
                [workspace]
                members = ["library"]
            "#,
        )
        .file(
            "library/Cargo.toml",
            r#"
                [package]
                name = "library"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#,
        )
        .file(
            "library/src/lib.rs",
            r#"
                pub fn function() -> usize { 5 }
            "#,
        )
        .file(
            "library/tests/test.rs",
            r#"
                   extern crate library;
                   use library::~
            "#,
        )
        .build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let stdout = rls.wait_until_done_indexing(rls_timeout());

    let json: Vec<_> = stdout
        .to_json_messages()
        .filter(|json| json["method"] != "window/progress")
        .collect();
    assert!(json.len() > 1);

    assert!(json[0]["result"]["capabilities"].is_object());

    assert_eq!(json[1]["method"], "textDocument/publishDiagnostics");
    assert!(json[1]["params"]["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .contains("expected identifier"));

    let mut json = serde_json::Value::Null;
    for i in 0..3 {
        let request_id = 100 + i;

        rls.request(
            request_id,
            "textDocument/completion",
            Some(json!({
                "context": {
                    "triggerCharacter": ":",
                    "triggerKind": 2
                },
                "position": {
                    "character": 32,
                    "line": 2
                },
                "textDocument": {
                    "uri": format!("file://{}/library/tests/test.rs", root_path.as_path().display()),
                    "version": 1
                }
            })),
        )
        .unwrap();

        json = rls.wait_until_json_id(request_id, rls_timeout());

        if !json["result"].as_array().unwrap().is_empty() {
            // retry completion message, rls not ready?
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
    };
    assert_eq!(json["result"][0]["detail"], "pub fn function() -> usize");

    rls.shutdown(rls_timeout());
}

#[test]
fn test_completion_suggests_arguments_in_statements() {
    let p = project("ws_with_test_dir")
        .file(
            "Cargo.toml",
            r#"
                [workspace]
                members = ["library"]
            "#,
        )
        .file(
            "library/Cargo.toml",
            r#"
                [package]
                name = "library"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#,
        )
        .file(
            "library/src/lib.rs",
            r#"
                pub fn function() -> usize { 5 }
            "#,
        )
        .file(
            "library/tests/test.rs",
            r#"
                   extern crate library;
                   fn magic() {
                       let a = library::f~
                   }
            "#,
        )
        .build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {
                "textDocument": {
                    "completion": {
                        "completionItem": {
                            "snippetSupport": true
                        }
                    }
                }
            }
        })),
    )
    .unwrap();

    let mut json = serde_json::Value::Null;
    for i in 0..3 {
        let request_id = 100 + i;

        rls.request(
            request_id,
            "textDocument/completion",
            Some(json!({
                "context": {
                    "triggerCharacter": "f",
                    "triggerKind": 2
                },
                "position": {
                    "character": 41,
                    "line": 3
                },
                "textDocument": {
                    "uri": format!("file://{}/library/tests/test.rs", root_path.as_path().display()),
                    "version": 1
                }
            })),
        )
        .unwrap();

        json = rls.wait_until_json_id(request_id, rls_timeout());

        if json["result"].as_array().unwrap().is_empty() {
            // retry completion message, rls not ready?
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
    }
    assert_eq!(json["result"][0]["insertText"], "function()");

    rls.shutdown(rls_timeout());
}

#[test]
fn test_use_statement_completion_doesnt_suggest_arguments() {
    let p = project("ws_with_test_dir")
        .file(
            "Cargo.toml",
            r#"
                [workspace]
                members = ["library"]
            "#,
        )
        .file(
            "library/Cargo.toml",
            r#"
                [package]
                name = "library"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#,
        )
        .file(
            "library/src/lib.rs",
            r#"
                pub fn function() -> usize { 5 }
            "#,
        )
        .file(
            "library/tests/test.rs",
            r#"
                   extern crate library;
                   use library::~;
            "#,
        )
        .build();

    //32, 2
    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    )
    .unwrap();

    let mut json = serde_json::Value::Null;
    for i in 0..3 {
        let request_id = 100 + i;

        rls.request(
            request_id,
            "textDocument/completion",
            Some(json!({
                "context": {
                    "triggerCharacter": ":",
                    "triggerKind": 2
                },
                "position": {
                    "character": 32,
                    "line": 2
                },
                "textDocument": {
                    "uri": format!("file://{}/library/tests/test.rs", root_path.as_path().display()),
                    "version": 1
                }
            })),
        )
        .unwrap();

        json = rls.wait_until_json_id(request_id, rls_timeout());

        if json["result"].as_array().unwrap().is_empty() {
            // retry completion message, rls not ready?
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
    }
    assert_eq!(json["result"][0]["insertText"], "function");

    rls.shutdown(rls_timeout());
}

/// Test simulates typing in a dependency wrongly in a couple of ways before finally getting it
/// right. Rls should provide Cargo.toml diagnostics.
///
/// ```
/// [dependencies]
/// version-check = "0.5555"
/// ```
///
/// * Firstly "version-check" doesn't exist, it should be "version_check"
/// * Secondly version 0.5555 of "version_check" doesn't exist.
#[test]
fn cmd_dependency_typo_and_fix() {
    let manifest_with_dependency = |dep: &str| {
        format!(
            r#"
            [package]
            name = "dependency_typo"
            version = "0.1.0"
            authors = ["alexheretic@gmail.com"]

            [dependencies]
            {}
        "#,
            dep
        )
    };

    let project = project("dependency_typo")
        .file(
            "Cargo.toml",
            &manifest_with_dependency(r#"version-check = "0.5555""#),
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

    let diags = &publish["params"]["diagnostics"];
    assert_eq!(diags.as_array().unwrap().len(), 1);
    assert!(diags[0]["message"]
        .as_str()
        .unwrap()
        .contains("no matching package named `version-check`"));
    assert_eq!(diags[0]["severity"], 1);

    let change_manifest = |contents: &str| {
        let mut manifest = std::fs::OpenOptions::new()
            .write(true)
            .open(root_path.join("Cargo.toml"))
            .unwrap();

        manifest.set_len(0).unwrap();
        write!(manifest, "{}", contents,).unwrap();
    };

    // fix naming typo, we now expect a version error diagnostic
    change_manifest(&manifest_with_dependency(r#"version_check = "0.5555""#));
    rls.request(
        1,
        "workspace/didChangeWatchedFiles",
        Some(json!({
            "changes": [{
                "uri": format!("file://{}/Cargo.toml", root_path.as_path().display()),
                "type": 2
            }],
        })),
    )
    .unwrap();

    let publish = rls
        .wait_until_done_indexing_n(2, rls_timeout())
        .to_json_messages()
        .rfind(|m| m["method"] == "textDocument/publishDiagnostics")
        .expect("No publishDiagnostics");

    let diags = &publish["params"]["diagnostics"];
    assert_eq!(diags.as_array().unwrap().len(), 1);
    assert!(diags[0]["message"].as_str().unwrap().contains("^0.5555"));
    assert_eq!(diags[0]["severity"], 1);

    // Fix version issue so no error diagnostics occur.
    // This is kinda slow as cargo will compile the dependency, though I
    // chose version_check to minimise this as it is a very small dependency.
    change_manifest(&manifest_with_dependency(r#"version_check = "0.1""#));
    rls.request(
        2,
        "workspace/didChangeWatchedFiles",
        Some(json!({
            "changes": [{
                "uri": format!("file://{}/Cargo.toml", root_path.as_path().display()),
                "type": 2
            }],
        })),
    )
    .unwrap();

    let publish = rls
        .wait_until_done_indexing_n(3, rls_timeout())
        .to_json_messages()
        .rfind(|m| m["method"] == "textDocument/publishDiagnostics")
        .expect("No publishDiagnostics");

    let diags = &publish["params"]["diagnostics"];

    assert_eq!(
        diags
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["severity"] == 1),
        None
    );

    rls.shutdown(rls_timeout());
}

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
                "uri": format!("file://{}/src/unrelated.rs", root_path.as_path().display()),
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
                "uri": format!("file://{}/src/main.rs", root_path.as_path().display()),
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
        .collect();
    // Actual formatting isn't important - what is, is that the buffer isn't
    // malformed and code stays semantically equivalent.
    assert_eq!(new_text, vec!["/* 😢😢😢😢😢😢😢 */\nfn main() {}\n"]);

    rls.shutdown(rls_timeout());
}
