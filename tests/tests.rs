#![feature(tool_lints)]

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

use crate::support::RlsStdout;
use std::time::Duration;

mod support;
use self::support::{basic_bin_manifest, project};

const RLS_TIMEOUT: Duration = Duration::from_secs(30);

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
        ).build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    ).unwrap();

    let json: Vec<_> = rls
        .wait_until_done_indexing(RLS_TIMEOUT)
        .to_json_messages()
        .filter(|json| json["method"] != "window/progress")
        .collect();

    assert!(json.len() > 1);

    assert!(json[0]["result"]["capabilities"].is_object());

    assert_eq!(json[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(json[1]["params"]["diagnostics"][0]["code"], "dead_code");

    rls.shutdown(RLS_TIMEOUT);
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
        ).file(
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
        ).file(
            "member_bin/Cargo.toml",
            r#"
                [package]
                name = "member_bin"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
                member_lib = { path = "../member_lib" }
            "#,
        ).file(
            "member_bin/src/main.rs",
            r#"
                extern crate member_lib;

                fn main() {
                    let a = member_lib::MemberLibStruct;
                }
            "#,
        ).file(
            "member_lib/Cargo.toml",
            r#"
                [package]
                name = "member_lib"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
            "#,
        ).file(
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
        ).build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    ).unwrap();

    let json: Vec<_> = rls
        .wait_until_done_indexing(RLS_TIMEOUT)
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
        assert!(
            json["params"]["message"]
                .as_str()
                .unwrap()
                .starts_with("member_")
        );
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

    let json: Vec<_> = rls.shutdown(RLS_TIMEOUT).to_json_messages().collect();

    assert_eq!(json[11]["id"], 99999);
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
        ).file(
            "library/Cargo.toml",
            r#"
                [package]
                name = "library"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#,
        ).file(
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
        ).file(
            "binary/Cargo.toml",
            r#"
                [package]
                name = "binary"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
                library = { path = "../library" }
            "#,
        ).file(
            "binary/src/main.rs",
            r#"
                extern crate library;

                fn main() {
                    let val: u32 = library::fetch_u32();
                }
            "#,
        ).build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    ).unwrap();

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

    let stdout = rls.wait_until_done_indexing(RLS_TIMEOUT);

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
    ).unwrap();

    let stdout = rls.wait_until_done_indexing_n(2, RLS_TIMEOUT);

    // lib unit tests have compile errors
    let lib_diagnostic = rfind_diagnostics_with_uri(&stdout, "library/src/lib.rs");
    let error_diagnostic = lib_diagnostic["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "E0308")
        .expect("expected lib error diagnostic");
    assert!(
        error_diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("expected u32, found u64")
    );

    // bin depending on lib picks up type mismatch
    let bin_diagnostic = rfind_diagnostics_with_uri(&stdout, "binary/src/main.rs");
    let error_diagnostic = bin_diagnostic["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "E0308")
        .expect("expected bin error diagnostic");
    assert!(
        error_diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("expected u32, found u64")
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
                        "text": "u32"
                    }
                ],
                "textDocument": {
                    "uri": format!("file://{}/library/src/lib.rs", root_path.as_path().display()),
                    "version": 1
                }
            })),
    ).unwrap();

    let stdout = rls.wait_until_done_indexing_n(3, RLS_TIMEOUT);
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

    rls.shutdown(RLS_TIMEOUT);
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
        ).file(
            "library/Cargo.toml",
            r#"
                [package]
                name = "library"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#,
        ).file(
            "library/src/lib.rs",
            r#"
                pub fn function() -> usize { 5 }
            "#,
        ).file(
            "library/tests/test.rs",
            r#"
                   extern crate library;
                   use library::~
            "#,
        ).build();

    let root_path = p.root();
    let mut rls = p.spawn_rls();

    rls.request(
        0,
        "initialize",
        Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        })),
    ).unwrap();

    let stdout = rls.wait_until_done_indexing(RLS_TIMEOUT);

    let json: Vec<_> = stdout
        .to_json_messages()
        .filter(|json| json["method"] != "window/progress")
        .collect();
    assert!(json.len() > 1);

    assert!(json[0]["result"]["capabilities"].is_object());

    assert_eq!(json[1]["method"], "textDocument/publishDiagnostics");
    assert!(
        json[1]["params"]["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("expected identifier")
    );

    rls.request(
        0,
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
    ).unwrap();

    let stdout = rls.wait_until(
        |stdout| {
            stdout
                .to_json_messages()
                .any(|json| json["result"][0]["detail"].is_string())
        },
        RLS_TIMEOUT,
    );

    let json = stdout
        .to_json_messages()
        .rfind(|json| json["result"].is_array())
        .unwrap();

    assert_eq!(json["result"][0]["detail"], "pub fn function() -> usize");

    rls.shutdown(RLS_TIMEOUT);
}
