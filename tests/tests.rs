
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

use std::time::Duration;

mod support;
use self::support::{ExpectedMessage, RlsHandle, basic_bin_manifest, project, timeout};

const TIME_LIMIT_SECS: u64 = 300;

#[test]
fn cmd_test_infer_bin() {
    timeout(Duration::from_secs(TIME_LIMIT_SECS), ||{
        let p = project("simple_workspace")
            .file("Cargo.toml", &basic_bin_manifest("foo"))
            .file("src/main.rs", r#"
                struct UnusedBin;
                fn main() {
                    println!("Hello world!");
                }
            "#)
            .build();

        let root_path = p.root();
        let rls_child = p.rls().spawn().unwrap();
        let mut rls = RlsHandle::new(rls_child);

        rls.request(0, "initialize", Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        }))).unwrap();

        rls.expect_messages(&[
            ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("foo"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("foo"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Indexing""#),
            ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedBin`"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
        ]);

        rls.shutdown_exit();
    });
}

#[test]
fn cmd_test_simple_workspace() {
    timeout(Duration::from_secs(TIME_LIMIT_SECS), ||{
        let p = project("simple_workspace")
            .file("Cargo.toml", r#"
                [workspace]
                members = [
                "member_lib",
                "member_bin",
                ]
            "#)
            .file("Cargo.lock", r#"
                [root]
                name = "member_lib"
                version = "0.1.0"

                [[package]]
                name = "member_bin"
                version = "0.1.0"
                dependencies = [
                "member_lib 0.1.0",
                ]
            "#)
            .file("member_bin/Cargo.toml", r#"
                [package]
                name = "member_bin"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
                member_lib = { path = "../member_lib" }
            "#)
            .file("member_bin/src/main.rs", r#"
                extern crate member_lib;

                fn main() {
                    let a = member_lib::MemberLibStruct;
                }
            "#)
            .file("member_lib/Cargo.toml", r#"
                [package]
                name = "member_lib"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
            "#)
            .file("member_lib/src/lib.rs", r#"
                pub struct MemberLibStruct;

                struct Unused;

                #[cfg(test)]
                mod tests {
                    #[test]
                    fn it_works() {
                    }
                }
            "#)
            .build();

        let root_path = p.root();
        let rls_child = p.rls().spawn().unwrap();
        let mut rls = RlsHandle::new(rls_child);

        rls.request(0, "initialize", Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        }))).unwrap();

        rls.expect_messages(&[
            ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            // order of member_lib/member_bin is undefined
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("member_"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("member_"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("member_"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("member_"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Indexing""#),
            ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
            ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
        ]);

        rls.shutdown_exit();
    });
}

#[test]
fn changing_workspace_lib_retains_bin_diagnostics() {
    timeout(Duration::from_secs(TIME_LIMIT_SECS), ||{
        let p = project("simple_workspace")
            .file("Cargo.toml", r#"
                [workspace]
                members = [
                "library",
                "binary",
                ]
            "#)
            .file("library/Cargo.toml", r#"
                [package]
                name = "library"
                version = "0.1.0"
                authors = ["Example <rls@example.com>"]
            "#)
            .file("library/src/lib.rs", r#"
                pub fn fetch_u32() -> u32 {
                    let unused = ();
                    42
                }
            "#)
            .file("binary/Cargo.toml", r#"
                [package]
                name = "binary"
                version = "0.1.0"
                authors = ["Igor Matuszewski <Xanewok@gmail.com>"]

                [dependencies]
                library = { path = "../library" }
            "#)
            .file("binary/src/main.rs", r#"
                extern crate library;

                fn main() {
                    let val: u32 = library::fetch_u32();
                }
            "#)
            .build();

        let root_path = p.root();
        let rls_child = p.rls().spawn().unwrap();
        let mut rls = RlsHandle::new(rls_child);

        rls.request(0, "initialize", Some(json!({
            "rootPath": root_path,
            "capabilities": {}
        }))).unwrap();

        rls.expect_messages(&[
            ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Indexing""#),
        ]);
        rls.expect_messages_unordered(&[
            ExpectedMessage::new(None).expect_contains("publishDiagnostics").expect_contains("library/src/lib.rs")
                .expect_contains("unused variable: `unused`"),
            ExpectedMessage::new(None).expect_contains("publishDiagnostics").expect_contains("binary/src/main.rs")
                .expect_contains("unused variable: `val`"),
        ]);
        rls.expect_messages(&[
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
        ]);

        rls.notify("textDocument/didChange", Some(json!({
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
                    "text": "invalid_return_type"
                }
            ],
            "textDocument": {
                "uri": format!("file://{}/library/src/lib.rs", root_path.as_path().display()),
                "version": 0
            }
        }))).unwrap();

        rls.expect_messages(&[
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#).expect_contains(r#""done":true"#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Indexing""#),
        ]);
        rls.expect_messages_unordered(&[
            ExpectedMessage::new(None).expect_contains("publishDiagnostics").expect_contains("library/src/lib.rs")
                .expect_contains("cannot find type `invalid_return_type` in this scope"),
            ExpectedMessage::new(None).expect_contains("publishDiagnostics").expect_contains("binary/src/main.rs")
                .expect_contains("unused variable: `val`"),
        ]);
        rls.expect_messages(&[
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Indexing""#).expect_contains(r#""done":true"#),
        ]);

        rls.notify("textDocument/didChange", Some(json!({
            "contentChanges": [
                {
                    "range": {
                        "start": {
                            "line": 1,
                            "character": 38,
                        },
                        "end": {
                            "line": 1,
                            "character": 57,
                        }
                    },
                    "rangeLength": 19,
                    "text": "u32"
                }
            ],
            "textDocument": {
                "uri": format!("file://{}/library/src/lib.rs", root_path.as_path().display()),
                "version": 1
            }
        }))).unwrap();

        rls.expect_messages(&[
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Building""#).expect_contains(r#""done":true"#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Indexing""#),
        ]);
        rls.expect_messages_unordered(&[
            ExpectedMessage::new(None).expect_contains("publishDiagnostics").expect_contains("library/src/lib.rs")
                .expect_contains("unused variable: `unused`"),
            ExpectedMessage::new(None).expect_contains("publishDiagnostics").expect_contains("binary/src/main.rs")
                .expect_contains("unused variable: `val`"),
        ]);
        rls.expect_messages(&[
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Indexing""#).expect_contains(r#""done":true"#),
        ]);

        rls.shutdown_exit();
    });
}