
// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate cargo;
#[macro_use]
extern crate serde_json;

use std::time::Duration;

mod support;
use support::{ExpectedMessage, RlsHandle, basic_bin_manifest, project, timeout};

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
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Build""#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("foo"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Diagnostics""#),
            ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedBin`"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
        ]);

        rls.shutdown_exit();
    });
}

#[test]
fn cmd_test_simple_workspace() {
    timeout(Duration::from_secs(300), ||{
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

        // This is the expected behavior is workspace_mode is on by default
        rls.expect_messages(&[
            ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Build""#),
            // order of member_lib/member_bin is undefined
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("member_"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains("member_"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#"title":"Diagnostics""#),
            ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
            ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
            ExpectedMessage::new(None).expect_contains("progress").expect_contains(r#""done":true"#),
        ]);

        rls.shutdown_exit();
    });
}
