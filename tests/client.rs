use std::path::Path;

use crate::support::basic_bin_manifest;
use crate::support::project_builder::project;

use lsp_types::{*, request::*};

#[allow(dead_code)]
mod support;

fn initialize_params(root_path: &Path) -> InitializeParams {
    lsp_types::InitializeParams {
        process_id: None,
        root_uri: None,
        root_path: Some(root_path.display().to_string()),
        initialization_options: None,
        capabilities: lsp_types::ClientCapabilities {
            workspace: None,
            text_document: None,
            experimental: None,
        },
        trace: None,
        workspace_folders: None,
    }
}

#[test]
fn client_test_infer_bin() {
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
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let diag = rls.wait_for_diagnostics();
    assert!(diag.diagnostics[0].message.contains("struct is never constructed: `UnusedBin`"));

    rls.wait_for_indexing();
    assert!(rls.messages().iter().filter(|msg| msg["method"] != "window/progress").count() > 1);

    rls.shutdown();
}

/// Test includes window/progress regression testing
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

    rls.request::<Initialize>(0, initialize_params(root_path));

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
