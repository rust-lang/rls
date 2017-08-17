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

use std::sync::{Arc, Mutex};
use env_logger;
use serde_json;

use analysis;
use config::{Config, Inferrable};
use server::{self as ls_server, ServerMessage, Request, Method};
use jsonrpc_core;
use vfs;

use self::harness::{expect_messages, ExpectedMessage, init_env, mock_server, mock_server_with_config, RecordOutput, src};

use url::Url;
use ls_types::*;
use lsp_data::InitializationOptions;
use std::path::Path;

impl ServerMessage {
    pub fn initialize(id: usize, root_path: Option<String>) -> ServerMessage {
         Self::initialize_with_opts(id, root_path, None)
    }

    pub fn initialize_with_opts(id: usize, root_path: Option<String>, initialization_options: Option<InitializationOptions>) -> ServerMessage {
        let init_opts = initialization_options
                            .map(|val| serde_json::to_value(val).unwrap());

        ServerMessage::Request(Request {
            id: id,
            method: Method::Initialize(InitializeParams {
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
            })
        })
    }

    pub fn request(id: usize, method: Method) -> ServerMessage {
        ServerMessage::Request(Request { id: id, method: method })
    }
}

#[test]
fn test_goto_def() {
    let (mut cache, _tc) = init_env("goto_def");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(11, Method::GotoDefinition(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 22, "world"))
        })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    // TODO structural checking of result, rather than looking for a string - src(&source_file_path, 12, "world")
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#""start":{"line":20,"character":8}"#)]);
}

#[test]
fn test_hover() {
    let (mut cache, _tc) = init_env("hover");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(11, Method::Hover(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 22, "world"))
        })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#"[{"language":"rust","value":"&str"}]"#)]);
}

#[test]
fn test_find_all_refs() {
    let (mut cache, _tc) = init_env("find_all_refs");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::References(ReferenceParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 10, "Bar")),
            context: ReferenceContext { include_declaration: true }
        })),
    ];

    let mut config = Config::default();
    config.cfg_test = true;
    let (mut server, results) = mock_server_with_config(messages, config);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
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
    let (mut cache, _tc) = init_env("find_all_refs_no_cfg_test");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::References(ReferenceParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 10, "Bar")),
            context: ReferenceContext { include_declaration: true }
        })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":9,"character":7},"end":{"line":9,"character":10}}"#)
                                                                     .expect_contains(r#"{"start":{"line":22,"character":15},"end":{"line":22,"character":18}}"#)]);
}

#[test]
fn test_borrow_error() {
    let (cache, _tc) = init_env("borrow_error");

    let root_path = cache.abs_path(Path::new("."));
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned()))
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains(r#""message":"cannot borrow `x` as mutable more than once at a time""#),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_highlight() {
    let (mut cache, _tc) = init_env("highlight");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::DocumentHighlight(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url),
            position: cache.mk_ls_position(src(&source_file_path, 22, "world"))
        })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":20,"character":8},"end":{"line":20,"character":13}}"#)
                                                                     .expect_contains(r#"{"start":{"line":21,"character":27},"end":{"line":21,"character":32}}"#),]);
}

#[test]
fn test_rename() {
    let (mut cache, _tc) = init_env("rename");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::Rename(RenameParams {
            text_document: text_doc,
            position: cache.mk_ls_position(src(&source_file_path, 22, "world")),
            new_name: "foo".to_owned()
        })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
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
    let (cache, _tc) = init_env("reformat");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::Formatting(DocumentFormattingParams {
            text_document: text_doc,
            options: FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                properties: ::std::collections::HashMap::new(),
            },
        })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":0,"character":0},"end":{"line":12,"character":0}}"#)
                                            .expect_contains(r#"newText":"// Copyright 2017 The Rust Project Developers. See the COPYRIGHT\n// file at the top-level directory of this distribution and at\n// http://rust-lang.org/COPYRIGHT.\n//\n// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or\n// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license\n// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your\n// option. This file may not be copied, modified, or distributed\n// except according to those terms.\n\npub mod foo;\npub fn main() {\n    let world = \"world\";\n    println!(\"Hello, {}!\", world);\n}"#)]);
}

#[test]
fn test_reformat_with_range() {
    let (cache, _tc) = init_env("reformat_with_range");
    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(42, Method::RangeFormatting(DocumentRangeFormattingParams {
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
        })),
    ];

    let (mut server, results) = mock_server(messages);

    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(42)).expect_contains(r#"{"start":{"line":0,"character":0},"end":{"line":15,"character":5}}"#)
                                            .expect_contains(r#"newText":"// Copyright 2017 The Rust Project Developers. See the COPYRIGHT\n// file at the top-level directory of this distribution and at\n// http://rust-lang.org/COPYRIGHT.\n//\n// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or\n// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license\n// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your\n// option. This file may not be copied, modified, or distributed\n// except according to those terms.\n\npub fn main() {\n    let world1 = \"world\";\n    println!(\"Hello, {}!\", world1);\n    let world2 = \"world\";\n    println!(\"Hello, {}!\", world2);\nlet world3 = \"world\"; println!(\"Hello, {}!\", world3);\n}\n"#)]);
}

#[test]
fn test_multiple_binaries() {
    let (cache, _tc) = init_env("multiple_bins");

    let root_path = cache.abs_path(Path::new("."));
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned()))
    ];

    let mut config = Config::default();
    config.build_bin = Inferrable::Specified(Some("bin2".to_owned()));
    let (mut server, results) = mock_server_with_config(messages, config);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("unused variable: `bin_name2`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_completion() {
    let (mut cache, _tc) = init_env("completion");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");
    let text_doc = TextDocumentIdentifier::new(url);

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(11, Method::Completion(TextDocumentPositionParams {
            text_document: text_doc.clone(),
            position: cache.mk_ls_position(src(&source_file_path, 22, "rld"))
        })),
        ServerMessage::request(22, Method::Completion(TextDocumentPositionParams {
            text_document: text_doc.clone(),
            position: cache.mk_ls_position(src(&source_file_path, 25, "x)"))
        })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(11)).expect_contains(r#"[{"label":"world","kind":6,"detail":"let world = \"world\";"}]"#)]);

    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(22)).expect_contains(r#"{"label":"x","kind":5,"detail":"u64"#)]);
}

#[test]
fn test_bin_lib_project() {
    let (cache, _tc) = init_env("bin_lib");

    let root_path = cache.abs_path(Path::new("."));

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
    ];

    let mut config = Config::default();
    config.cfg_test = true;
    config.build_bin = Inferrable::Specified(Some("bin_lib".into()));
    let (mut server, results) = mock_server_with_config(messages, config);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_bin_lib_project_no_cfg_test() {
    let (cache, _tc) = init_env("bin_lib_no_cfg_test");

    let root_path = cache.abs_path(Path::new("."));

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
    ];

    let mut config = Config::default();
    config.build_lib = Inferrable::Specified(false);
    config.build_bin = Inferrable::Specified(Some("bin_lib_no_cfg_test".into()));
    let (mut server, results) = mock_server_with_config(messages, config);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("cannot find struct, variant or union type `LibCfgTestStruct` in module `bin_lib_no_cfg_test`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_simple_workspace() {
    let (cache, _tc) = init_env("simple_workspace");

    let root_path = cache.abs_path(Path::new("."));

    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
    ];

    let mut config = Config::default();
    config.workspace_mode = true;
    let (mut server, results) = mock_server_with_config(messages, config);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       // TODO: Ideally we should check for message contents for different crates/targets,
                                       // however order of received messages is non-deterministic and this
                                       // would require implementing something like `or_expect_contains`
                                       ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
                                       ExpectedMessage::new(None).expect_contains("publishDiagnostics"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_infer_lib() {
    let (cache, _tc) = init_env("infer_lib");

    let root_path = cache.abs_path(Path::new("."));
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned()))
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedLib`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_infer_bin() {
    let (cache, _tc) = init_env("infer_bin");

    let root_path = cache.abs_path(Path::new("."));
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned()))
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedBin`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_infer_custom_bin() {
    let (cache, _tc) = init_env("infer_custom_bin");

    let root_path = cache.abs_path(Path::new("."));
    let messages = vec![
        ServerMessage::initialize(0, root_path.as_os_str().to_str().map(|x| x.to_owned()))
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(), &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsBegin"),
                                       ExpectedMessage::new(None).expect_contains("struct is never used: `UnusedCustomBin`"),
                                       ExpectedMessage::new(None).expect_contains("diagnosticsEnd")]);
}

#[test]
fn test_omit_init_build() {
    let (cache, _tc) = init_env("omit_init_build");

    let root_path = cache.abs_path(Path::new("."));
    let root_path = root_path.as_os_str().to_str().map(|x| x.to_owned());
    let init_options = Some(InitializationOptions { omit_init_build: true });
    let initialize = ServerMessage::initialize_with_opts(0, root_path, init_options);

    let messages = vec![initialize];

    let (mut server, results) = mock_server(messages);

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
    let (mut cache, _tc) = init_env("find_impls");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path))
        .expect("couldn't convert file path to URL");

    // This test contains code for testing implementations of `Eq`. However, `rust-analysis` is not
    // installed on Travis making rls-analysis fail why retrieving the typeid. Installing
    // `rust-analysis` is also not an option, because this makes other test timeout.
    // e.g., https://travis-ci.org/rust-lang-nursery/rls/jobs/265339002

    let messages = vec![
        ServerMessage::initialize(0,root_path.as_os_str().to_str().map(|x| x.to_owned())),
        ServerMessage::request(1, Method::FindImpls(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url.clone()),
            position: cache.mk_ls_position(src(&source_file_path, 13, "Bar"))
        })),
        ServerMessage::request(2, Method::FindImpls(TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(url.clone()),
            position: cache.mk_ls_position(src(&source_file_path, 16, "Super"))
        })),
        // Does not work on Travis
        // ServerMessage::request(3, Method::FindImpls(TextDocumentPositionParams {
        //     text_document: TextDocumentIdentifier::new(url),
        //     position: cache.mk_ls_position(src(&source_file_path, 20, "Eq"))
        // })),
    ];

    let (mut server, results) = mock_server(messages);
    // Initialise and build.
    assert_eq!(ls_server::LsService::handle_message(&mut server),
               ls_server::ServerStateChange::Continue);
    expect_messages(results.clone(),
                    &[ExpectedMessage::new(Some(0)).expect_contains("capabilities"),
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
