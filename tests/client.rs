use serde_json::{self, json};

use self::support::project_builder::project;

#[allow(dead_code)]
mod support;

#[test]
fn client_test_simple_workspace() {
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
    let mut rls = p.spawn_rls_async();

    rls.send(json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "rootPath": root_path,
            "capabilities": {}
        }
    }));

    rls.wait_for_indexing();

    // Check if we built member_lib and member_bin + their cfg(test) variants
    let count = rls.messages()
        .iter()
        .filter(|msg| msg["method"] == "window/progress")
        .filter(|msg| msg["params"]["title"] == "Building")
        .filter(|msg| msg["params"]["message"].as_str().map(|x| x.starts_with("member_")).unwrap_or(false))
        .count();
    assert_eq!(count, 4);

    rls.shutdown();
}
