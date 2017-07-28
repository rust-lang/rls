extern crate url;

mod harness;

use std::path::Path;

use url::Url;

use harness::*;
use ls_types::*;
use rls::server::{self as ls_server, Method};
use rls::lsp_data::*;

#[test]
fn test_find_all_refs() {
    let (mut cache, _tc) = init_env("find_all_refs");

    let source_file_path = Path::new("src").join("main.rs");

    let root_path = cache.abs_path(Path::new("."));
    let url = Url::from_file_path(cache.abs_path(&source_file_path)).expect("couldn't convert file path to URL");

    let messages = vec![
        make_init_msg(0, root_path.as_os_str().to_str().map(|x| x.to_owned())),
        make_request_msg(42, Method::References(ReferenceParams {
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

