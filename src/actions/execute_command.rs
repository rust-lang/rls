// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Requests that the RLS can respond to.

use actions::InitActionContext;
use serde_json;
use jsonrpc_core::ErrorCode;
use lsp_data::*;
use server;
use server::{Ack, Action, Output, RequestAction, ResponseError};
use actions::requests::DeglobResult;



use std::collections::HashMap;

/// Execute a command within the workspace.
///
/// These are *not* shell commands, but commands given by the client and
/// performed by the RLS.
///
/// Currently support "rls.applySuggestion", "rls.deglobImports".
pub struct ExecuteCommand;

///
#[derive(Debug)]
pub enum ExecuteCommandResponse {
    /// Response/client request containing workspace edits.
    ApplyEdit(ApplyWorkspaceEditParams),
}


impl server::Response for ExecuteCommandResponse {
    fn send<O: Output>(&self, id: usize, out: &O) {
        // FIXME should handle the client's responses
        match *self {
            ExecuteCommandResponse::ApplyEdit(ref params) => {
                let output = serde_json::to_string(&RequestMessage::new(
                    out.provide_id(),
                    "workspace/applyEdit".to_owned(),
                    ApplyWorkspaceEditParams {
                        edit: params.edit.clone(),
                    },
                )).unwrap();
                out.response(output);
            }
        }

        // The formal request response is a simple ACK, though the objective
        // is the preceeding client requests.
        Ack.send(id, out);
    }
}

impl Action for ExecuteCommand {
    type Params = ExecuteCommandParams;
    const METHOD: &'static str = "workspace/executeCommand";
}

impl RequestAction for ExecuteCommand {
    type Response = ExecuteCommandResponse;

    fn new() -> Self {
        ExecuteCommand
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Empty)
    }

    fn handle(
        &mut self,
        _: InitActionContext,
        params: ExecuteCommandParams,
    ) -> Result<Self::Response, ResponseError> {
        match &*params.command {
            "rls.applySuggestion" => {
                apply_suggestion(params.arguments).map(ExecuteCommandResponse::ApplyEdit)
            }
            "rls.deglobImports" => {
                apply_deglobs(params.arguments).map(ExecuteCommandResponse::ApplyEdit)
            }
            c => {
                debug!("Unknown command: {}", c);
                Err(ResponseError::Message(
                    ErrorCode::MethodNotFound,
                    "Unknown command".to_owned(),
                ))
            }
        }
    }
}

fn apply_suggestion(
    args: Vec<serde_json::Value>,
) -> Result<ApplyWorkspaceEditParams, ResponseError> {
    let location = serde_json::from_value(args[0].clone()).expect("Bad argument");
    let new_text = serde_json::from_value(args[1].clone()).expect("Bad argument");

    trace!("apply_suggestion {:?} {}", location, new_text);
    Ok(ApplyWorkspaceEditParams {
        edit: make_workspace_edit(location, new_text),
    })
}

fn apply_deglobs(args: Vec<serde_json::Value>) -> Result<ApplyWorkspaceEditParams, ResponseError> {
    let deglob_results: Vec<DeglobResult> = args.into_iter()
        .map(|res| serde_json::from_value(res).expect("Bad argument"))
        .collect();

    trace!("apply_deglob {:?}", deglob_results);

    assert!(!deglob_results.is_empty());
    let uri = deglob_results[0].location.uri.clone();

    let text_edits: Vec<_> = deglob_results
        .into_iter()
        .map(|res| {
            TextEdit {
                range: res.location.range,
                new_text: res.new_text,
            }
        })
        .collect();
    let mut edit = WorkspaceEdit {
        changes: HashMap::new(),
    };
    // all deglob results will share the same URI
    edit.changes.insert(uri, text_edits);

    Ok(ApplyWorkspaceEditParams { edit })
}
