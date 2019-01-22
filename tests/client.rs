use std::path::Path;

use futures::future::Future;
use lsp_types::{*, request::*, notification::*};
use serde_json::json;

use crate::support::{basic_bin_manifest, fixtures_dir};
use crate::support::project_builder::{project, ProjectBuilder};

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

#[test]
fn client_changing_workspace_lib_retains_diagnostics() {
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
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let lib = rls.future_diagnostics("library/src/lib.rs");
    let bin = rls.future_diagnostics("binary/src/main.rs");
    let (lib, bin) = rls.block_on(lib.join(bin)).unwrap();

    assert!(lib.diagnostics.iter().any(|m| m.message.contains("unused variable: `test_val`")));
    assert!(lib.diagnostics.iter().any(|m| m.message.contains("unused variable: `unused`")));
    assert!(bin.diagnostics[0].message.contains("unused variable: `val`"));

    rls.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
        content_changes: vec![TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 1,
                    character: 38,
                },
                end: Position {
                    line: 1,
                    character: 41
                }
            }),
            range_length: Some(3),
            text: "u64".to_string(),
        }],
        text_document: VersionedTextDocumentIdentifier {
            uri: Url::from_file_path(p.root().join("library/src/lib.rs")).unwrap(),
            version: Some(0),
        }
    });

    let lib = rls.future_diagnostics("library/src/lib.rs");
    let bin = rls.future_diagnostics("binary/src/main.rs");
    let (lib, bin) = rls.block_on(lib.join(bin)).unwrap();

    // lib unit tests have compile errors
    assert!(lib.diagnostics.iter().any(|m| m.message.contains("unused variable: `unused`")));
    assert!(lib.diagnostics.iter().any(|m| m.message.contains("expected u32, found u64")));
    // bin depending on lib picks up type mismatch
    assert!(bin.diagnostics[0].message.contains("mismatched types\n\nexpected u32, found u64"));

    rls.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
        content_changes: vec![TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 1,
                    character: 38,
                },
                end: Position {
                    line: 1,
                    character: 41
                }
            }),
            range_length: Some(3),
            text: "u32".to_string(),
        }],
        text_document: VersionedTextDocumentIdentifier {
            uri: Url::from_file_path(p.root().join("library/src/lib.rs")).unwrap(),
            version: Some(1),
        }
    });

    let lib = rls.future_diagnostics("library/src/lib.rs");
    let bin = rls.future_diagnostics("binary/src/main.rs");
    let (lib, bin) = rls.block_on(lib.join(bin)).unwrap();

    assert!(lib.diagnostics.iter().any(|m| m.message.contains("unused variable: `test_val`")));
    assert!(lib.diagnostics.iter().any(|m| m.message.contains("unused variable: `unused`")));
    assert!(bin.diagnostics[0].message.contains("unused variable: `val`"));

    rls.shutdown();
}

#[test]
fn client_implicit_workspace_pick_up_lib_changes() {
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
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let bin = rls.future_diagnostics("src/main.rs");
    let bin = rls.block_on(bin).unwrap();
    assert!(bin.diagnostics[0].message.contains("unused variable: `val`"));

    rls.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
        content_changes: vec![TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 1,
                    character: 23,
                },
                end: Position {
                    line: 1,
                    character: 26
                }
            }),
            range_length: Some(3),
            text: "bar".to_string(),
        }],
        text_document: VersionedTextDocumentIdentifier {
            uri: Url::from_file_path(p.root().join("inner/src/lib.rs")).unwrap(),
            version: Some(0),
        }
    });

    // bin depending on lib picks up type mismatch
    let bin = rls.future_diagnostics("src/main.rs");
    let bin = rls.block_on(bin).unwrap();
    assert!(bin.diagnostics[0].message.contains("cannot find function `foo`"));

    rls.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
        content_changes: vec![TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 1,
                    character: 23,
                },
                end: Position {
                    line: 1,
                    character: 26
                }
            }),
            range_length: Some(3),
            text: "foo".to_string(),
        }],
        text_document: VersionedTextDocumentIdentifier {
            uri: Url::from_file_path(p.root().join("inner/src/lib.rs")).unwrap(),
            version: Some(1),
        }
    });

    let bin = rls.future_diagnostics("src/main.rs");
    let bin = rls.block_on(bin).unwrap();
    assert!(bin.diagnostics[0].message.contains("unused variable: `val`"));

    rls.shutdown();
}

#[test]
fn client_test_complete_self_crate_name() {
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
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let diag = rls.wait_for_diagnostics();
    assert!(diag.diagnostics[0].message.contains("expected identifier"));

    // Sometimes RLS is not ready immediately for completion
    let mut detail = None;
    for id in 100..103 {
        let response = rls.request::<Completion>(id, CompletionParams {
            context: Some(CompletionContext {
                trigger_character: Some(":".to_string()),
                trigger_kind: CompletionTriggerKind::TriggerCharacter,
            }),
            position: Position::new(2, 32),
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(p.root().join("library/tests/test.rs")).unwrap(),
            }
        });

        let items = match response {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(CompletionList { items, ..})) => items,
            _ => Vec::new(),
        };

        if let Some(item) = items.get(0) {
            detail = item.detail.clone();
            break;
        }
    }
    // Make sure we get the completion at least once right
    assert_eq!(detail.as_ref().unwrap(), "pub fn function() -> usize");

    rls.shutdown();
}

#[test]
fn client_completion_suggests_arguments_in_statements() {
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
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, lsp_types::InitializeParams {
        process_id: None,
        root_uri: None,
        root_path: Some(root_path.display().to_string()),
        initialization_options: None,
        capabilities: lsp_types::ClientCapabilities {
            workspace: None,
            text_document: Some(TextDocumentClientCapabilities {
                completion: Some(CompletionCapability {
                    completion_item: Some(CompletionItemCapability {
                        snippet_support: Some(true),
                        ..CompletionItemCapability::default()
                    }),
                    ..CompletionCapability::default()
                }),
                ..TextDocumentClientCapabilities::default()
            }),
            experimental: None,
        },
        trace: None,
        workspace_folders: None,
    });

    let diag = rls.wait_for_diagnostics();
    assert!(diag.diagnostics[0].message.contains("expected one of"));

    // Sometimes RLS is not ready immediately for completion
    let mut insert_text = None;
    for id in 100..103 {
        let response = rls.request::<Completion>(id, CompletionParams {
            context: Some(CompletionContext {
                trigger_character: Some("f".to_string()),
                trigger_kind: CompletionTriggerKind::TriggerCharacter,
            }),
            position: Position::new(3, 41),
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(p.root().join("library/tests/test.rs")).unwrap(),
            }
        });

        let items = match response {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(CompletionList { items, ..})) => items,
            _ => Vec::new(),
        };

        if let Some(item) = items.get(0) {
            insert_text = item.insert_text.clone();
            break;
        }
    }
    // Make sure we get the completion at least once right
    assert_eq!(insert_text.as_ref().unwrap(), "function()");
}

#[test]
fn client_use_statement_completion_doesnt_suggest_arguments() {
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

    let root_path = p.root();
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let diag = rls.wait_for_diagnostics();
    assert!(diag.diagnostics[0].message.contains("expected identifier"));

    // Sometimes RLS is not ready immediately for completion
    let mut insert_text = None;
    for id in 100..103 {
        let response = rls.request::<Completion>(id, CompletionParams {
            context: Some(CompletionContext {
                trigger_character: Some(":".to_string()),
                trigger_kind: CompletionTriggerKind::TriggerCharacter,
            }),
            position: Position::new(2, 32),
            text_document: TextDocumentIdentifier {
                uri: Url::from_file_path(p.root().join("library/tests/test.rs")).unwrap(),
            }
        });

        let items = match response {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(CompletionList { items, ..})) => items,
            _ => Vec::new(),
        };

        if let Some(item) = items.get(0) {
            insert_text = item.insert_text.clone();
            break;
        }
    }
    // Make sure we get the completion at least once right
    assert_eq!(insert_text.as_ref().unwrap(), "function");
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
fn client_dependency_typo_and_fix() {
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

    let p = project("dependency_typo")
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
    let root_path = p.root();
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let diag = rls.wait_for_diagnostics();
    assert_eq!(diag.diagnostics.len(), 1);
    assert_eq!(diag.diagnostics[0].severity, Some(DiagnosticSeverity::Error));
    assert!(diag.diagnostics[0].message.contains("no matching package named `version-check`"));

    let change_manifest = |contents: &str| {
        std::fs::write(root_path.join("Cargo.toml"), contents).unwrap();
    };

    // fix naming typo, we now expect a version error diagnostic
    change_manifest(&manifest_with_dependency(r#"version_check = "0.5555""#));
    rls.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![
            FileEvent {
                uri: Url::from_file_path(p.root().join("Cargo.toml")).unwrap(),
                typ: FileChangeType::Changed
            }
        ]
    });

    let diag = rls.wait_for_diagnostics();
    assert_eq!(diag.diagnostics.len(), 1);
    assert_eq!(diag.diagnostics[0].severity, Some(DiagnosticSeverity::Error));
    assert!(diag.diagnostics[0].message.contains("^0.5555"));

    // Fix version issue so no error diagnostics occur.
    // This is kinda slow as cargo will compile the dependency, though I
    // chose version_check to minimise this as it is a very small dependency.
    change_manifest(&manifest_with_dependency(r#"version_check = "0.1""#));
    rls.notify::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
        changes: vec![
            FileEvent {
                uri: Url::from_file_path(p.root().join("Cargo.toml")).unwrap(),
                typ: FileChangeType::Changed
            }
        ]
    });

    let diag = rls.wait_for_diagnostics();
    assert_eq!(diag.diagnostics.iter().find(|d| d.severity == Some(DiagnosticSeverity::Error)), None);

    rls.shutdown();
}

/// Tests correct positioning of a toml parse error, use of `==` instead of `=`.
#[test]
fn client_invalid_toml_manifest() {
    let p = project("invalid_toml")
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
    let root_path = p.root();
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let diag: PublishDiagnosticsParams = rls.wait_for_diagnostics();

    assert!(diag.uri.as_str().ends_with("invalid_toml/Cargo.toml"));
    assert_eq!(diag.diagnostics.len(), 1);
    assert_eq!(diag.diagnostics[0].severity, Some(DiagnosticSeverity::Error));
    assert!(diag.diagnostics[0].message.contains("failed to parse manifest"));

    assert_eq!(diag.diagnostics[0].range, Range {
        start: Position { line: 2, character: 21 },
        end: Position { line: 2, character: 22 },
    });

    rls.shutdown();
}

/// Tests correct file highlighting of workspace member manifest with invalid path dependency.
#[test]
fn client_invalid_member_toml_manifest() {
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
    let mut rls = project.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let diag: PublishDiagnosticsParams = rls.wait_for_diagnostics();

    assert!(diag.uri.as_str().ends_with("invalid_member_toml/member_a/dodgy_member/Cargo.toml"));

    assert_eq!(diag.diagnostics.len(), 1);
    assert_eq!(diag.diagnostics[0].severity, Some(DiagnosticSeverity::Error));
    assert!(diag.diagnostics[0].message.contains("failed to read"));

    rls.shutdown();
}

#[test]
fn client_invalid_member_dependency_resolution() {
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
    let mut rls = project.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    let diag: PublishDiagnosticsParams = rls.wait_for_diagnostics();

    assert!(diag.uri.as_str().ends_with("invalid_member_resolution/member_a/dodgy_member/Cargo.toml"));

    assert_eq!(diag.diagnostics.len(), 1);
    assert_eq!(diag.diagnostics[0].severity, Some(DiagnosticSeverity::Error));
    assert!(diag.diagnostics[0].message.contains("no matching package named `nosuchdep123`"));

    rls.shutdown();
}

#[test]
fn client_handle_utf16_unit_text_edits() {
    let p = project("client_handle_utf16_unit_text_edits")
        .file(
            "Cargo.toml",
            r#"[package]
            name = "client_handle_utf16_unit_text_edits"
            version = "0.1.0"
            authors = ["example@example.com"]
            "#,
        )
        .file("src/main.rs", "fn main() {}")
        .file("src/some.rs", "😢")
        .build();
    let root_path = p.root();
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    rls.wait_for_indexing();

    rls.notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: Url::from_file_path(p.root().join("src/some.rs")).unwrap(),
            version: Some(0),
        },
        // "😢" -> ""
        content_changes: vec![TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 2
                }
            }),
            range_length: Some(2),
            text: "".to_string(),
        }]
    });

    rls.shutdown();
}

/// Ensures that wide characters do not prevent RLS from calculating correct
/// 'whole file' LSP range.
#[test]
fn client_format_utf16_range() {
    let p = project("client_format_utf16_range")
        .file(
            "Cargo.toml",
            r#"[package]
            name = "client_format_utf16_range"
            version = "0.1.0"
            authors = ["example@example.com"]
            "#,
        )
        .file("src/main.rs", "/* 😢😢😢😢😢😢😢 */ fn main() { }")
        .build();
    let root_path = p.root();
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, initialize_params(root_path));

    rls.wait_for_indexing();

    let result = rls.request::<Formatting>(66, DocumentFormattingParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(p.root().join("src/main.rs")).unwrap(),
        },
        options: FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            properties: Default::default(),
        }
    });

    let new_text: Vec<_> = result.unwrap()
        .iter()
        .map(|edit| edit.new_text.as_str().replace('\r', ""))
        .collect();
    // Actual formatting isn't important - what is, is that the buffer isn't
    // malformed and code stays semantically equivalent.
    assert_eq!(new_text, vec!["/* 😢😢😢😢😢😢😢 */\nfn main() {}\n"]);

    rls.shutdown();
}

#[test]
fn client_lens_run() {
    let p = ProjectBuilder::try_from_fixture(fixtures_dir().join("lens_run"))
        .unwrap()
        .build();
    let root_path = p.root();
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, lsp_types::InitializeParams {
        process_id: None,
        root_uri: None,
        root_path: Some(root_path.display().to_string()),
        initialization_options: Some(json!({ "cmdRun": true})),
        capabilities: Default::default(),
        trace: None,
        workspace_folders: None,
    });

    rls.wait_for_indexing();
    assert!(rls.messages().iter().count() >= 7);

    let lens = rls.request::<CodeLensRequest>(1, CodeLensParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(p.root().join("src/main.rs")).unwrap(),
        }
    });

    let expected = CodeLens {
        command: Some(Command {
            command: "rls.run".to_string(),
            title: "Run test".to_string(),
            arguments: Some(vec![
                json!({
                    "args": [ "test", "--", "--nocapture", "test_foo" ],
                    "binary": "cargo",
                    "env": { "RUST_BACKTRACE": "short" }
                })
            ]),
        }),
        data: None,
        range: Range {
            start: Position { line: 14, character: 3 },
            end: Position { line: 14, character: 11 }
        }
    };

    assert_eq!(lens, Some(vec![expected]));

    rls.shutdown();
}


#[test]
fn client_find_definitions() {
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
    let mut rls = p.spawn_rls_async();

    rls.request::<Initialize>(0, lsp_types::InitializeParams {
        process_id: None,
        root_uri: None,
        root_path: Some(root_path.display().to_string()),
        initialization_options: Some(json!({
            "settings": {
                "rust": {
                    "racer_completion": false
                }
            }
        })),
        capabilities: Default::default(),
        trace: None,
        workspace_folders: None,
    });

    rls.wait_for_indexing();

    let mut results = vec![];
    for (line_index, line) in SRC.lines().enumerate() {
        for i in 0..line.len() {
            let id = (line_index * 100 + i) as u64;
            let result = rls.request::<GotoDefinition>(id, TextDocumentPositionParams {
                position: Position { line: line_index as u64, character: i as u64 },
                text_document: TextDocumentIdentifier {
                    uri: Url::from_file_path(p.root().join("src/main.rs")).unwrap(),
                }
            });

            let ranges: Vec<_> = result.into_iter().flat_map(|x| match x {
                GotoDefinitionResponse::Scalar(loc) => vec![loc].into_iter(),
                GotoDefinitionResponse::Array(locs) => locs.into_iter(),
                _ => unreachable!(),
            }).map(|x| x.range).collect();

            if !ranges.is_empty() {
                results.push((line_index, i, ranges));
            }
        }
    }
    rls.shutdown();

    // Foo
    let foo_definition = Range {
        start: Position { line: 1, character: 15 },
        end: Position { line: 1, character: 18 }
    };

    // Foo::new
    let foo_new_definition = Range {
        start: Position { line: 5, character: 15 },
        end: Position { line: 5, character: 18 }
    };

    // main
    let main_definition = Range {
        start: Position { line: 9, character: 11 },
        end: Position { line: 9, character: 15 }
    };

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
