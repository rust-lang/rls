// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use url::Url;
use super::*;
use std::str::FromStr;

// TODO do we need these?
#[allow(non_upper_case_globals)]
pub const REQUEST__Deglob: &'static str = "rustWorkspace/deglob";

#[allow(non_upper_case_globals)]
pub const REQUEST__FindImpls: &'static str = "rustDocument/implementations";


#[test]
fn server_message_get_method_name() {
    let test_url = Url::from_str("http://testurl").expect("Couldn't parse test URI");

    let request_shut = ServerMessage::request(1, Method::Shutdown);
    assert_eq!(request_shut.get_method_name(), "shutdown");

    let request_init = ServerMessage::initialize(1, None);
    assert_eq!(request_init.get_method_name(), "initialize");

    let request_hover = ServerMessage::request(1, Method::Hover(TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: test_url.clone() },
        position: Position { line: 0, character: 0 },
    }));
    assert_eq!(request_hover.get_method_name(), "textDocument/hover");


    let request_resolve = ServerMessage::request(1, Method::ResolveCompletionItem(
        CompletionItem::new_simple("label".to_owned(), "detail".to_owned())
    ));
    assert_eq!(request_resolve.get_method_name(), "completionItem/resolve");

    let notif_exit = ServerMessage::Notification(Notification::Exit);
    assert_eq!(notif_exit.get_method_name(), "exit");

    let notif_change = ServerMessage::Notification(Notification::DidChangeTextDocument(DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier { uri: test_url.clone(), version: 1 },
        content_changes: vec![],
    }));
    assert_eq!(notif_change.get_method_name(), "textDocument/didChange");

    let notif_cancel = ServerMessage::Notification(Notification::Cancel(CancelParams {
        id: NumberOrString::Number(1)
    }));
    assert_eq!(notif_cancel.get_method_name(), "$/cancelRequest");
}

#[test]
fn server_message_to_str() {
    let request = ServerMessage::request(1, Method::Shutdown);
    let request_json: serde_json::Value = serde_json::from_str(&request.to_message_str()).unwrap();
    let expected_json = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": request.get_method_name()
    });
    assert_eq!(request_json, expected_json);

    //println!("{0}", request_json);

    let test_url = Url::from_str("http://testurl").expect("Couldn't parse test URI");
    let request = ServerMessage::request(2, Method::Hover(TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: test_url.clone() },
        position: Position { line: 0, character: 0 },
    }));
    let request_json: serde_json::Value = serde_json::from_str(&request.to_message_str()).unwrap();
    assert_eq!(request_json.get("jsonrpc").unwrap().as_str().unwrap(), "2.0");
    assert_eq!(request_json.get("id").unwrap().as_i64().unwrap(), 2);
    assert_eq!(request_json.get("method").unwrap().as_str().unwrap(), "textDocument/hover");
    let request_params = request_json.get("params").unwrap();
    let expected_params = json!({
        "textDocument": TextDocumentIdentifier::new(test_url.clone()),
        "position": Position {line: 0, character: 0 }
    });
    assert_eq!(request_params, &expected_params);
}
