// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// Utilities and infrastructure for testing. Tests in this module test the
// testing infrastructure *not* the RLS.

mod harness;

use analysis;
use actions::requests;
use config::{Config, Inferrable};
use server::{self as ls_server, Request, ShutdownRequest, NoParams};
use jsonrpc_core;
use vfs;

use self::harness::{Environment, expect_messages, ExpectedMessage, RecordOutput, src};

use ls_types::*;
use lsp_data::InitializationOptions;

use env_logger;
use serde_json;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::{Arc, Mutex};
use url::Url;

pub fn initialize<'a>(id: usize, root_path: Option<String>) -> Request<'a, ls_server::InitializeRequest> {
     initialize_with_opts(id, root_path, None)
}

pub fn initialize_with_opts<'a>(id: usize, root_path: Option<String>, initialization_options: Option<InitializationOptions>) -> Request<'a, ls_server::InitializeRequest> {
    let init_opts = initialization_options.map(|val| serde_json::to_value(val).unwrap());
    let params = InitializeParams {
        process_id: None,
        root_path,
        root_uri: None,
        initialization_options: init_opts,
        capabilities: ClientCapabilities {
            workspace: None,
            text_document: None,
            experimental: None,
        },
        trace: TraceOption::Off,
    };
    Request {
        id,
        params,
        _action: PhantomData,
    }
}

pub fn request<'a, T: ls_server::RequestAction<'a>>(id: usize, params: T::Params) -> Request<'a, T> {
    Request {
        id,
        params,
        _action: PhantomData,
    }
}

#[test]
fn test_shutdown() {
    let mut env = Environment::new("common");

    let root_path = env.cache.abs_path(Path::new("."));

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<ShutdownRequest>(1, NoParams).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[&ExpectedMessage::new(Some(1))]);
}

#[test]
fn test_goto_def() {
    let mut env = Environment::new("common");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::Definition>(11, TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: env.cache.mk_ls_position(src(&source_file_path, 22, "world"))
        }).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    // TODO structural checking of result, rather than looking for a string - src(&source_file_path, 12, "world")
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#""start":{"line":20,"character":8}"#)]);
}

#[test]
fn test_hover() {
    let mut env = Environment::new("common");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::Hover>(11, TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: env.cache.mk_ls_position(src(&source_file_path, 22, "world"))
        }).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#"[{"language":"rust","value":"&str"}]"#)]);
}

#[test]
fn test_workspace_symbol() {
    let mut env = Environment::new("workspace_symbol");

    let root_path = env.cache.abs_path(Path::new("."));

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::WorkspaceSymbol>(42, WorkspaceSymbolParams {
            query: "nem".to_owned(),
        }).to_string(),
    ];

    env.with_config(|c| c.cfg_test = true);
    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);

    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#""id":42"#)
                                                                     // in main.rs
                                                                     .expect_contains(r#"main.rs"#)
                                                                     .expect_contains(r#""name":"nemo""#)
                                                                     .expect_contains(r#""kind":12"#)
                                                                     .expect_contains(r#""range":{"start":{"line":11,"character":11},"end":{"line":11,"character":15}}"#)
                                                                     .expect_contains(r#""containerName":"x""#)

                                                                     // in foo.rs
                                                                     .expect_contains(r#"foo.rs"#)
                                                                     .expect_contains(r#""name":"nemo""#)
                                                                     .expect_contains(r#""kind":2"#)
                                                                     .expect_contains(r#""range":{"start":{"line":0,"character":4},"end":{"line":0,"character":8}}"#)
                                                                     .expect_contains(r#""containerName":"foo""#)]);
}

#[test]
fn test_find_all_refs() {
    let mut env = Environment::new("common");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::References>(42, ReferenceParams {
            text_document: TextDocumentIdentifier::new(url),
            position: env.cache.mk_ls_position(src(&source_file_path, 10, "Bar")),
            context: ReferenceContext { include_declaration: true }
        }).to_string(),
    ];

    env.with_config(|c| c.cfg_test = true);
    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":9,"character":7},"end":{"line":9,"character":10}}"#)
                                                                     .expect_contains(r#"{"start":{"line":15,"character":14},"end":{"line":15,"character":17}}"#)
                                                                     .expect_contains(r#"{"start":{"line":23,"character":15},"end":{"line":23,"character":18}}"#)]);
}

#[test]
fn test_find_all_refs_no_cfg_test() {
    let mut env = Environment::new("find_all_refs_no_cfg_test");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::References>(42, ReferenceParams {
            text_document: TextDocumentIdentifier::new(url),
            position: env.cache.mk_ls_position(src(&source_file_path, 10, "Bar")),
            context: ReferenceContext { include_declaration: true }
        }).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":9,"character":7},"end":{"line":9,"character":10}}"#)
                                                                     .expect_contains(r#"{"start":{"line":22,"character":15},"end":{"line":22,"character":18}}"#)]);
}

#[test]
fn test_borrow_error() {
    let mut env = Environment::new("borrow_error");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains(r#""message":"cannot borrow `x` as mutable more than once at a time""#),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_highlight() {
    let mut env = Environment::new("common");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::DocumentHighlight>(42, TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: env.cache.mk_ls_position(src(&source_file_path, 22, "world"))
        }).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":20,"character":8},"end":{"line":20,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":21,"character":27},"end":{"line":21,"character":32}}"#),]);
}

#[test]
fn test_rename() {
    let mut env = Environment::new("common");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::Rename>(42, RenameParams {
            text_document: text_doc,
            position: env.cache.mk_ls_position(src(&source_file_path, 22, "world")),
            new_name: "foo".to_owned()
        }).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":20,"character":8},"end":{"line":20,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":21,"character":27},"end":{"line":21,"character":32}}"#)
                                                                     .expect_contains(r#"{"changes""#),]);
}

#[test]
fn test_reformat() {
    let mut env = Environment::new("reformat");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::Formatting>(42, DocumentFormattingParams {
            text_document: text_doc,
            options: FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                properties: ::std::collections::HashMap::new(),
            },
        }).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":0,"character":0},"end":{"line":12,"character":0}}"#)
                                            .expect_contains(r#"newText":"// Copyright 2017 The Rust Project Developers. See the COPYRIGHT\n// file at the top-level directory of this distribution and at\n// http://rust-lang.org/COPYRIGHT.\n//\n// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or\n// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license\n// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your\n// option. This file may not be copied, modified, or distributed\n// except according to those terms.\n\npub mod foo;\npub fn main() {\n    let world = \"world\";\n    println!(\"Hello, {}!\", world);\n}"#)]);
}

#[test]
fn test_reformat_with_range() {
    let mut env = Environment::new("reformat_with_range");
    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::RangeFormatting>(42, DocumentRangeFormattingParams {
            text_document: text_doc,
            range: Range {
                start: Position { line: 12, character: 0 },
                end: Position { line: 13, character: 0 },
            },
            options: FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                properties: ::std::collections::HashMap::new(),
            },
        }).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);

    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":0,"character":0},"end":{"line":15,"character":5}}"#)
                                            .expect_contains(r#"newText":"// Copyright 2017 The Rust Project Developers. See the COPYRIGHT\n// file at the top-level directory of this distribution and at\n// http://rust-lang.org/COPYRIGHT.\n//\n// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or\n// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license\n// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your\n// option. This file may not be copied, modified, or distributed\n// except according to those terms.\n\npub fn main() {\n    let world1 = \"world\";\n    println!(\"Hello, {}!\", world1);\n    let world2 = \"world\";\n    println!(\"Hello, {}!\", world2);\n    let world3 = \"world\";\n    println!(\"Hello, {}!\", world3);\n}\n"#)]);
}

#[test]
fn test_multiple_binaries() {
    let mut env = Environment::new("multiple_bins");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    env.with_config(|c| c.build_bin = Inferrable::Specified(Some("bin2".to_owned())));
    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("unused variable: `bin_name2`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

// FIXME Requires rust-src component, which would break Rust CI
// #[test]
// fn test_completion() {
//     let mut env = Environment::new("common");

//     let source_file_path = Path::new("src").join("main.rs");

//     let root_path = env.cache.abs_path(Path::new("."));
//     let url = Url::from_file_path(env.cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
//     let text_doc = TextDocumentIdentifier::new(url);

//     let messages = vec![
//         initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
//         request::<requests::Completion>(11, TextDocumentPositionParams {
//             text_document: text_doc.clone(),
//             position: env.cache.mk_ls_position(src(&source_file_path, 22, "rld"))
//         }).to_string(),
//         request::<requests::Completion>(22, TextDocumentPositionParams {
//             text_document: text_doc.clone(),
//             position: env.cache.mk_ls_position(src(&source_file_path, 25, "x)"))
//         }).to_string(),
//     ];

//     let (mut server, results) = env.mock_server(messages);
//     // Initialize and build.
//     assert_eq!(ls_server::LsService::handle_message(&mut server),
//                ls_server::ServerStateChange::Continue);
//     expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
//                                        ExpectedMessage::new(None).expect_contains("beginBuild"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

//     assert_eq!(ls_server::LsService::handle_message(&mut server),
//                ls_server::ServerStateChange::Continue);
//     expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#"[{"label":"world","kind":6,"detail":"let world = \"world\";"}]"#)]);

//     assert_eq!(ls_server::LsService::handle_message(&mut server),
//                ls_server::ServerStateChange::Continue);
//     expect_messages(results.clone(), &[ExpectedMessage::new(Some(22)).expect_contains(r#"{"label":"x","kind":5,"detail":"u64"#)]);
// }

#[test]
fn test_bin_lib_project() {
    let mut env = Environment::new("bin_lib");

    let root_path = env.cache.abs_path(Path::new("."));

    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
    ];

    env.with_config(|c| {
        c.cfg_test = true;
        c.build_bin = Inferrable::Specified(Some("bin_lib".into()));
    });
    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

// FIXME(#524) timing issues when run concurrently with `test_bin_lib_project`
// #[test]
// fn test_bin_lib_project_no_cfg_test() {
//     let mut env = Environment::new("bin_lib");

//     let root_path = env.cache.abs_path(Path::new("."));

//     let messages = vec![
//         initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
//     ];

//     env.with_config(|c| {
//         c.build_lib = Inferrable::Specified(false);
//         c.build_bin = Inferrable::Specified(Some("bin_lib".into()));
//     });
//     let (mut server, results) = env.mock_server(messages);
//     // Initialize and build.
//     assert_eq!(ls_server::LsService::handle_message(&mut server),
//                ls_server::ServerStateChange::Continue);
//     expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
//                                        ExpectedMessage::new(None).expect_contains("beginBuild"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
//                                        ExpectedMessage::new(None).expect_contains("cannot find struct, variant or union type `LibCfgTestStruct` in module `bin_lib`"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
// }

// FIXME(#455) reinstate this test
// #[test]
// fn test_simple_workspace() {
//     let mut env = Environment::new("simple_workspace");

//     let root_path = env.cache.abs_path(Path::new("."));

//     let messages = vec![
//         initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
//     ];

//     env.with_config(|c| c.workspace_mode = true);
//     let (mut server, results) = env.mock_server(messages);
//     // Initialize and build.
//     assert_eq!(ls_server::LsService::handle_message(&mut server),
//                ls_server::ServerStateChange::Continue);
//     expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
//                                        ExpectedMessage::new(None).expect_contains("beginBuild"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
//                                        // TODO: Ideally we should check for message contents for different crates/targets,
//                                        // however order of received messages is non-deterministic and this
//                                        // would require implementing something like `or_expect_contains`
//                                        ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
//                                        ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
// }

#[test]
fn test_infer_lib() {
    let mut env = Environment::new("infer_lib");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedLib`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_infer_bin() {
    let mut env = Environment::new("infer_bin");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedBin`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_infer_custom_bin() {
    let mut env = Environment::new("infer_custom_bin");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedCustomBin`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_omit_init_build() {
    let mut env = Environment::new("common");

    let root_path = env.cache.abs_path(Path::new("."));
    let root_path = root_path.as_os_str().to_str().map(|x| x.to_owned());
    let init_options = Some(InitializationOptions { omit_init_build: true });
    let initialize = initialize_with_opts(0, root_path, init_options);

    let messages = vec![initialize.to_string()];

    let (mut server, results) = env.mock_server(messages);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities")]);
}


#[test]
fn test_parse_error_on_malformed_input() {
    let _ = env_logger::init();
    struct NoneMsgReader;

    impl ls_server::MessageReader for NoneMsgReader {
        fn read_message(&self) -> Option<String> { None }
    }

    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let reader = Box::new(NoneMsgReader);
    let output = RecordOutput::new();
    let results = output.output.clone();
    let mut server = ls_server::LsService::new(analysis, vfs, Arc::new(Mutex::new(Config::default())), reader, output);

    let result = ls_server::LsService::handle_message(&mut server);
    assert_eq!(result,
               ls_server::ServerStateChange::Break);

    let error = results.lock().unwrap()
        .pop().expect("no error response");

    let failure: jsonrpc_core::Failure = serde_json::from_str(&error)
        .expect("Couldn't parse json failure response");

    assert!(failure.error.code == jsonrpc_core::ErrorCode::ParseError);
}

#[test]
fn test_find_impls() {
    let mut env = Environment::new("find_impls");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = env.cache.abs_path(Path::new("."));
    let url = Url::from_file_path(env.cache.abs_path(&source_file_path))
        .expect("couldn't convert file path to URL");

    // This test contains code for testing implementations of `Eq`. However, `rust-analysis` is not
    // installed on Travis making rls-analysis fail why retrieving the typeid. Installing
    // `rust-analysis` is also not an option, because this makes other test timeout.
    // e.g., https://travis-ci.org/rust-lang-nursery/rls/jobs/265339002

    let messages = vec![
        initialize(0,root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string(),
        request::<requests::FindImpls>(1, TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url.clone()),
            position: env.cache.mk_ls_position(src(&source_file_path, 13, "Bar"))
        }).to_string(),
        request::<requests::FindImpls>(2, TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url.clone()),
            position: env.cache.mk_ls_position(src(&source_file_path, 16, "Super"))
        }).to_string(),
        // Does not work on Travis
        // request::<requests::FindImpls>(3, TextDocumentPositionParams {
        //     text_document: TextDocumentIdentifier::new(url),
        //     position: env.cache.mk_ls_position(src(&source_file_path, 20, "Eq"))
        // })).to_string(),
    ];

    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(),
                    &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                      ExpectedMessage::new(None).expect_contains("beginBuild"),
                      ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                      ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    // TODO structural checking of result, rather than looking for a string - src(&source_file_path, 12, "world")
    expect_messages(results.clone(), &[
        ExpectedMessage::new(Some(1))
            .expect_contains(r#""range":{"start":{"line":18,"character":15},"end":{"line":18,"character":18}}"#)
            .expect_contains(r#""range":{"start":{"line":19,"character":12},"end":{"line":19,"character":15}}"#)
    ]);
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[
        ExpectedMessage::new(Some(2))
            .expect_contains(r#""range":{"start":{"line":18,"character":15},"end":{"line":18,"character":18}}"#)
            .expect_contains(r#""range":{"start":{"line":22,"character":15},"end":{"line":22,"character":18}}"#)
    ]);
    // Does not work on Travis
    // assert_eq!(ls_server::LsService::handle_message(&mut server),
    //            ls_server::ServerStateChange::Continue);
    // expect_messages(results.clone(), &[
    //     // TODO assert that only one position is returned
    //     ExpectedMessage::new(Some(3))
    //         .expect_contains(r#""range":{"start":{"line":19,"character":12},"end":{"line":19,"character":15}}"#)
    // ]);
}

#[test]
fn test_features() {
    let mut env = Environment::new("features");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    env.with_config(|c| c.features = vec!["foo".to_owned()]);
    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains(r#""message":"cannot find struct, variant or union type `Bar` in this scope""#),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_all_features() {
    let mut env = Environment::new("features");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    env.with_config(|c| c.all_features = true);
    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_no_default_features() {
    let mut env = Environment::new("features");

    let root_path = env.cache.abs_path(Path::new("."));
    let messages = vec![
        initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
    ];

    env.with_config(|c| {
        c.no_default_features = true;
        c.features = vec!["foo".to_owned(), "bar".to_owned()]
    });
    let (mut server, results) = env.mock_server(messages);
    // Initialize and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("beginBuild"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains(r#""message":"cannot find struct, variant or union type `Baz` in this scope""#),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

// #[test]
// fn test_handle_utf8_directory() {
//     let mut env = Environment::new("unicødë");
//
//     let root_path = env.cache.abs_path(Path::new("."));
//     let root_url = Url::from_directory_path(&root_path).unwrap();
//     let messages = vec![
//         initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())).to_string()
//     ];
//
//     let (mut server, results) = env.mock_server(messages);
//     // Initialize and build.
//     assert_eq!(ls_server::LsService::handle_message(&mut server),
//                ls_server::ServerStateChange::Continue);
//     expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
//                                        ExpectedMessage::new(None).expect_contains("beginBuild"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
//                                        ExpectedMessage::new(None)
//                                            .expect_contains(root_url.path())
//                                            .expect_contains("struct is never used: `Unused`"),
//                                        ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
// }
