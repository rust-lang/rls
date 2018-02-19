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
use data;
use url::Url;
#[cfg(feature = "rustfmt")]
use vfs::FileContents;
use racer;
#[cfg(feature = "rustfmt")]
use rustfmt::{FileName, format_input, Input as FmtInput};
#[cfg(feature = "rustfmt")]
use rustfmt::file_lines::{FileLines, Range as RustfmtRange};
use serde_json;
use span;

use actions::work_pool;
use actions::work_pool::WorkDescription;
use lsp_data;
use lsp_data::*;
use server;
use server::{Ack, Output, Request, RequestAction, ResponseError};
use jsonrpc_core::types::ErrorCode;

use lsp_data::request::ApplyWorkspaceEdit;
pub use lsp_data::request::{
    WorkspaceSymbol,
    DocumentSymbol as Symbols,
    HoverRequest as Hover,
    GotoDefinition as Definition,
    References,
    Completion,
    DocumentHighlightRequest as DocumentHighlight,
    Rename,
    ExecuteCommand,
    CodeActionRequest as CodeAction,
    Formatting,
    RangeFormatting,
    ResolveCompletionItem as ResolveCompletion,
};
pub use lsp_data::FindImpls;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;


/// Represent the result of a deglob action for a single wildcard import.
///
/// The `location` is the position of the wildcard.
/// `new_text` is the text which should replace the wildcard.
#[derive(Debug, Deserialize, Serialize)]
pub struct DeglobResult {
    /// Location of the "*" character in a wildcard import
    pub location: Location,
    /// Replacement text
    pub new_text: String,
}

impl RequestAction for WorkspaceSymbol {
    type Response = Vec<SymbolInformation>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let analysis = ctx.analysis;

        let defs = analysis.matching_defs(&params.query).unwrap_or_else(|_| vec![]);

        Ok(
            defs.into_iter()
                .map(|d| {
                    SymbolInformation {
                        name: d.name,
                        kind: source_kind_from_def_kind(d.kind),
                        location: ls_util::rls_to_location(&d.span),
                        container_name: d.parent
                            .and_then(|id| analysis.get_def(id).ok())
                            .map(|parent| parent.name),
                    }
                })
                .collect(),
        )
    }
}

impl RequestAction for Symbols {
    type Response = Vec<SymbolInformation>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let file_path = parse_file_path!(&params.text_document.uri, "symbols")?;

        let symbols = ctx.analysis.symbols(&file_path).unwrap_or_else(|_| vec![]);

        Ok(
            symbols
                .into_iter()
                .map(|s| {
                    SymbolInformation {
                        name: s.name,
                        kind: source_kind_from_def_kind(s.kind),
                        location: ls_util::rls_to_location(&s.span),
                        container_name: None, // FIXME: more info could be added here
                    }
                })
                .collect(),
        )
    }
}

impl RequestAction for Hover {
    type Response = lsp_data::Hover;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(lsp_data::Hover {
            contents: HoverContents::Array(vec![]),
            range: None,
        })
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let file_path = parse_file_path!(&params.text_document.uri, "hover")?;
        let span = ctx.convert_pos_to_span(file_path, params.position);

        trace!("hover: {:?}", span);

        let analysis = ctx.analysis;
        let ty = analysis.show_type(&span).unwrap_or_else(|_| String::new());
        let docs = analysis.docs(&span).unwrap_or_else(|_| String::new());
        let doc_url = analysis.doc_url(&span).unwrap_or_else(|_| String::new());

        let mut contents = vec![];
        if !docs.is_empty() {
            contents.push(MarkedString::from_markdown(docs));
        }
        if !doc_url.is_empty() {
            contents.push(MarkedString::from_markdown(doc_url));
        }
        if !ty.is_empty() {
            contents.push(MarkedString::from_language_code("rust".into(), ty));
        }
        Ok(lsp_data::Hover {
            contents: HoverContents::Array(contents),
            range: None, // TODO: maybe add?
        })
    }
}

impl RequestAction for FindImpls {
    type Response = Vec<Location>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let file_path = parse_file_path!(&params.text_document.uri, "find_impls")?;
        let span = ctx.convert_pos_to_span(file_path, params.position);
        let analysis = ctx.analysis;

        let type_id = analysis.id(&span).map_err(|_| ResponseError::Empty)?;
        let result = analysis.find_impls(type_id).map(|spans| {
            spans
                .into_iter()
                .map(|x| ls_util::rls_to_location(&x))
                .collect()
        });

        trace!("find_impls: {:?}", result);

        result.map_err(|_| {
            ResponseError::Message(
                ErrorCode::InternalError,
                "Find Implementations failed to complete successfully".into(),
            )
        })
    }
}

impl RequestAction for Definition {
    type Response = Vec<Location>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        // Save-analysis thread.
        let file_path = parse_file_path!(&params.text_document.uri, "goto_def")?;
        let span = ctx.convert_pos_to_span(file_path.clone(), params.position);
        let analysis = ctx.analysis;
        let vfs = ctx.vfs;

        // If configured start racer concurrently and fallback to racer result
        let racer_receiver = {
            if ctx.config.lock().unwrap().goto_def_racer_fallback {
                Some(work_pool::receive_from_thread(move || {
                    let cache = racer::FileCache::new(vfs);
                    let session = racer::Session::new(&cache);
                    let location = pos_to_racer_location(params.position);

                    racer::find_definition(file_path, location, &session)
                        .and_then(|rm| location_from_racer_match(&rm))
                }, WorkDescription("textDocument/definition-racer")))
            } else {
                None
            }
        };

        match analysis.goto_def(&span) {
            Ok(out) => {
                let result = vec![ls_util::rls_to_location(&out)];
                trace!("goto_def (compiler): {:?}", result);
                Ok(result)
            }
            _ => match racer_receiver {
                Some(receiver) => match receiver.recv() {
                    Ok(Some(r)) => {
                        trace!("goto_def (Racer): {:?}", r);
                        Ok(vec![r])
                    }
                    Ok(None) => {
                        trace!("goto_def (Racer): None");
                        Ok(vec![])
                    }
                    _ => Self::fallback_response(),
                },
                _ => Self::fallback_response(),
            },
        }
    }
}

impl RequestAction for References {
    type Response = Vec<Location>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let file_path = parse_file_path!(&params.text_document.uri, "find_all_refs")?;
        let span = ctx.convert_pos_to_span(file_path, params.position);

        let result = match ctx.analysis
            .find_all_refs(&span, params.context.include_declaration, false)
        {
            Ok(t) => t,
            _ => vec![],
        };

        Ok(
            result
                .iter()
                .map(|item| ls_util::rls_to_location(item))
                .collect(),
        )
    }
}

impl RequestAction for Completion {
    type Response = Vec<CompletionItem>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let vfs = ctx.vfs;
        let file_path = parse_file_path!(&params.text_document.uri, "complete")?;

        let cache = racer::FileCache::new(vfs);
        let session = racer::Session::new(&cache);

        let location = pos_to_racer_location(params.position);
        let results = racer::complete_from_file(file_path, location, &session);

        let code_completion_has_snippet_support =
            ctx.client_capabilities.code_completion_has_snippet_support;

        Ok(
            results
                .map(|comp| {
                    let mut item = completion_item_from_racer_match(&comp);
                    if code_completion_has_snippet_support {
                        let snippet = racer::snippet_for_match(&comp, &session);
                        if !snippet.is_empty() {
                            item.insert_text = Some(snippet);
                            item.insert_text_format = Some(InsertTextFormat::Snippet);
                        }
                    }
                    item
                })
                .collect(),
        )
    }
}

impl RequestAction for DocumentHighlight {
    type Response = Vec<lsp_data::DocumentHighlight>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let file_path = parse_file_path!(&params.text_document.uri, "highlight")?;
        let span = ctx.convert_pos_to_span(file_path.clone(), params.position);

        let result = ctx.analysis
            .find_all_refs(&span, true, false)
            .unwrap_or_else(|_| vec![]);

        Ok(
            result
                .iter()
                .filter_map(|span| {
                    if span.file == file_path {
                        Some(lsp_data::DocumentHighlight {
                            range: ls_util::rls_to_range(span.range),
                            kind: Some(DocumentHighlightKind::Text),
                        })
                    } else {
                        None
                    }
                })
                .collect()
        )
    }
}

impl RequestAction for Rename {
    type Response = WorkspaceEdit;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(WorkspaceEdit {
            changes: None,
            document_changes: None,
        })
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        ctx.quiescent.store(true, Ordering::SeqCst);
        // We're going to mutate based on our data so we should block until the
        // data is ready.
        ctx.block_on_build();

        let file_path = parse_file_path!(&params.text_document.uri, "rename")?;
        let span = ctx.convert_pos_to_span(file_path, params.position);

        let analysis = ctx.analysis;

        macro_rules! unwrap_or_fallback {
            ($e: expr) => {
                match $e {
                    Ok(e) => e,
                    Err(_) => {
                        return Self::fallback_response();
                    }
                }
            }
        }

        let id = unwrap_or_fallback!(analysis.crate_local_id(&span));
        let def = unwrap_or_fallback!(analysis.get_def(id));
        if def.name == "self" || def.name == "Self"
            // FIXME(#578)
            || def.kind == data::DefKind::Mod
        {
            return Self::fallback_response();
        }

        let result = unwrap_or_fallback!(analysis.find_all_refs(&span, true, true));

        let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();

        for item in &result {
            let loc = ls_util::rls_to_location(item);
            edits
                .entry(loc.uri)
                .or_insert_with(Vec::new)
                .push(TextEdit {
                    range: loc.range,
                    new_text: params.new_name.clone(),
                });
        }

        if !ctx.quiescent.load(Ordering::SeqCst) {
            return Self::fallback_response();
        }

        Ok(WorkspaceEdit { changes: Some(edits), document_changes: None })
    }
}

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
                let id = out.provide_id() as usize;
                let params = ApplyWorkspaceEditParams {
                        edit: params.edit.clone(),
                };

                let request = Request::<ApplyWorkspaceEdit>::new(id, params);
                out.request(request);
            }
        }

        // The formal request response is a simple ACK, though the objective
        // is the preceeding client requests.
        Ack.send(id, out);
    }
}

impl RequestAction for ExecuteCommand {
    type Response = ExecuteCommandResponse;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Empty)
    }

    /// Currently supports "rls.applySuggestion", "rls.deglobImports".
    fn handle(
        ctx: InitActionContext,
        params: ExecuteCommandParams,
    ) -> Result<Self::Response, ResponseError> {
        match &*params.command {
            "rls.applySuggestion" => {
                apply_suggestion(&params.arguments).map(ExecuteCommandResponse::ApplyEdit)
            }
            "rls.deglobImports" => {
                apply_deglobs(params.arguments, &ctx).map(ExecuteCommandResponse::ApplyEdit)
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
    args: &[serde_json::Value],
) -> Result<ApplyWorkspaceEditParams, ResponseError> {
    let location = serde_json::from_value(args[0].clone()).expect("Bad argument");
    let new_text = serde_json::from_value(args[1].clone()).expect("Bad argument");

    trace!("apply_suggestion {:?} {}", location, new_text);
    Ok(ApplyWorkspaceEditParams {
        edit: make_workspace_edit(location, new_text),
    })
}

fn apply_deglobs(args: Vec<serde_json::Value>, ctx: &InitActionContext) -> Result<ApplyWorkspaceEditParams, ResponseError> {
    ctx.quiescent.store(true, Ordering::SeqCst);
    let deglob_results: Vec<DeglobResult> = args.into_iter()
        .map(|res| serde_json::from_value(res).expect("Bad argument"))
        .collect();

    trace!("apply_deglobs {:?}", deglob_results);

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
    // all deglob results will share the same URI
    let changes: HashMap<_, _> = vec![(uri, text_edits)]
        .into_iter()
        .collect();

    let edit = WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
    };

    if !ctx.quiescent.load(Ordering::SeqCst) {
        return Err(ResponseError::Empty);
    }
    Ok(ApplyWorkspaceEditParams { edit })
}

/// Create `CodeActions` for fixes suggested by the compiler
/// the results are appended to `code_actions_result`
fn make_suggestion_fix_actions(
    params: &<CodeAction as lsp_data::request::Request>::Params,
    file_path: &Path,
    ctx: &InitActionContext,
    code_actions_result: &mut <CodeAction as RequestAction>::Response,
) {
    // search for compiler suggestions
    if let Some(diagnostics) = ctx.previous_build_results.lock().unwrap().get(file_path) {
        let suggestions = diagnostics
            .iter()
            .filter(|&&(ref d, _)| d.range.overlaps(&params.range))
            .flat_map(|&(_, ref ss)| ss.iter());
        for s in suggestions {
            let span = Location {
                uri: params.text_document.uri.clone(),
                range: s.range,
            };
            let span = serde_json::to_value(&span).unwrap();
            let new_text = serde_json::to_value(&s.new_text).unwrap();
            let cmd = Command {
                title: s.label.clone(),
                command: "rls.applySuggestion".to_owned(),
                arguments: Some(vec![span, new_text]),
            };
            code_actions_result.push(cmd);
        }
    }
}

/// Create `CodeActions` for performing deglobbing when a wildcard import is found
/// the results are appended to `code_actions_result`
fn make_deglob_actions(
    params: &<CodeAction as lsp_data::request::Request>::Params,
    file_path: &Path,
    ctx: &InitActionContext,
    code_actions_result: &mut <CodeAction as RequestAction>::Response,
) {
    // search for a glob in the line
    if let Ok(line) = ctx.vfs
        .load_line(file_path, ls_util::range_to_rls(params.range).row_start)
    {
        let span = Location::new(params.text_document.uri.clone(), params.range);

        // for all indices which are a `*`
        // check if we can deglob them
        // this handles badly formated text containing multiple "use"s in one line
        let deglob_results: Vec<_> = line.char_indices()
            .filter(|&(_, chr)| chr == '*')
            .filter_map(|(index, _)| {
                // map the indices to `Span`s
                let mut span = ls_util::location_to_rls(&span).unwrap();
                span.range.col_start = span::Column::new_zero_indexed(index as u32);
                span.range.col_end = span::Column::new_zero_indexed(index as u32 + 1);

                // load the deglob type information
                ctx.analysis.show_type(&span)
                // remove all errors
                .ok()
                .map(|ty| (ty, span))
            })
            .map(|(mut deglob_str, span)| {
                // Handle multiple imports from one *
                if deglob_str.contains(',') {
                    deglob_str = format!("{{{}}}", deglob_str);
                }

                // build result
                let deglob_result = DeglobResult {
                    location: ls_util::rls_to_location(&span),
                    new_text: deglob_str,
                };

                // convert to json
                serde_json::to_value(&deglob_result).unwrap()
            })
            .collect();

        if !deglob_results.is_empty() {
            // extend result list
            let cmd = Command {
                title: format!(
                    "Deglob Import{}",
                    if deglob_results.len() > 1 { "s" } else { "" }
                ),
                command: "rls.deglobImports".to_owned(),
                arguments: Some(deglob_results),
            };
            code_actions_result.push(cmd);
        }
    };
}

impl RequestAction for CodeAction {
    type Response = Vec<Command>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        trace!("code_action {:?}", params);

        let file_path = parse_file_path!(&params.text_document.uri, "code_action")?;

        let mut cmds = vec![];
        if ctx.build_ready() {
            make_suggestion_fix_actions(&params, &file_path, &ctx, &mut cmds);
        }
        if ctx.analysis_ready() {
            make_deglob_actions(&params, &file_path, &ctx, &mut cmds);
        }
        Ok(cmds)
    }
}

impl RequestAction for Formatting {
    type Response = [TextEdit; 1];

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Message(
            ErrorCode::InternalError,
            "Reformat failed to complete successfully".into(),
        ))
    }

    #[cfg(feature = "rustfmt")]
    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        reformat(&params.text_document, None, &params.options, &ctx)
    }

    #[cfg(not(feature = "rustfmt"))]
    fn handle(_: InitActionContext, _: Self::Params) -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Message(
            ErrorCode::InternalError,
            "rustfmt was not distributed with this rls release".into(),
        ))
    }
}

impl RequestAction for RangeFormatting {
    type Response = [TextEdit; 1];

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Message(
            ErrorCode::InternalError,
            "Reformat failed to complete successfully".into(),
        ))
    }

    #[cfg(feature = "rustfmt")]
    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        reformat(
            &params.text_document,
            Some(params.range),
            &params.options,
            &ctx,
        )
    }
    #[cfg(not(feature = "rustfmt"))]
    fn handle(_: InitActionContext, _: Self::Params) -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Message(
            ErrorCode::InternalError,
            "rustfmt was not distributed with this rls release".into(),
        ))
    }
}

#[cfg(feature = "rustfmt")]
fn reformat(
    doc: &TextDocumentIdentifier,
    selection: Option<Range>,
    opts: &FormattingOptions,
    ctx: &InitActionContext,
) -> Result<[TextEdit; 1], ResponseError> {
    ctx.quiescent.store(true, Ordering::SeqCst);
    trace!(
        "Reformat: {:?} {:?} {} {}",
        doc,
        selection,
        opts.tab_size,
        opts.insert_spaces
    );
    let path = parse_file_path!(&doc.uri, "reformat")?;

    let input = match ctx.vfs.load_file(&path) {
        Ok(FileContents::Text(s)) => FmtInput::Text(s),
        Ok(_) => {
            debug!("Reformat failed, found binary file");
            return Err(ResponseError::Message(
                ErrorCode::InternalError,
                "Reformat failed to complete successfully".into(),
            ));
        }
        Err(e) => {
            debug!("Reformat failed: {:?}", e);
            return Err(ResponseError::Message(
                ErrorCode::InternalError,
                "Reformat failed to complete successfully".into(),
            ));
        }
    };

    let range_whole_file = ls_util::range_from_vfs_file(&ctx.vfs, &path);
    let mut config = ctx.fmt_config().get_rustfmt_config().clone();
    if !config.was_set().hard_tabs() {
        config.set().hard_tabs(!opts.insert_spaces);
    }
    if !config.was_set().tab_spaces() {
        config.set().tab_spaces(opts.tab_size as usize);
    }

    if let Some(r) = selection {
        let range_of_rls = ls_util::range_to_rls(r).one_indexed();
        let range = RustfmtRange::new(
            range_of_rls.row_start.0 as usize,
            range_of_rls.row_end.0 as usize,
        );
        let mut ranges = HashMap::new();
        ranges.insert(FileName::Custom("stdin".to_owned()), vec![range]);
        let file_lines = FileLines::from_ranges(ranges);
        config.set().file_lines(file_lines);
    };

    let mut buf = Vec::<u8>::new();
    match format_input(input, &config, Some(&mut buf)) {
        Ok((summary, ..)) => {
            // format_input returns Ok even if there are any errors, i.e., parsing errors.
            if !summary.has_operational_errors() && !summary.has_parsing_errors() {
                // Note that we don't need to update the VFS, the client
                // echos back the change to us.
                let text = String::from_utf8(buf).unwrap();

                if !ctx.quiescent.load(Ordering::SeqCst) {
                    return Err(ResponseError::Message(
                        ErrorCode::InternalError,
                        "Reformat failed to complete successfully".into(),
                    ))
                }

                // If Rustfmt returns range of text that changed,
                // we will be able to pass only range of changed text to the client.
                Ok([
                    TextEdit {
                        range: range_whole_file,
                        new_text: text,
                    },
                ])
            } else {
                debug!(
                    "reformat: format_input failed: has errors, summary = {:?}",
                    summary
                );

                Err(ResponseError::Message(
                    ErrorCode::InternalError,
                    "Reformat failed to complete successfully".into(),
                ))
            }
        }
        Err(e) => {
            debug!("Reformat failed: {:?}", e);

            Err(ResponseError::Message(
                ErrorCode::InternalError,
                "Reformat failed to complete successfully".into(),
            ))
        }
    }
}

impl RequestAction for ResolveCompletion {
    type Response = CompletionItem;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Empty)
    }

    fn handle(_: InitActionContext, params: Self::Params) -> Result<Self::Response, ResponseError> {
        // currently, we safely ignore this as a pass-through since we fully handle
        // textDocument/completion.  In the future, we may want to use this method as a
        // way to more lazily fill out completion information
        Ok(params)
    }
}

fn racer_coord(
    line: span::Row<span::OneIndexed>,
    column: span::Column<span::ZeroIndexed>,
) -> racer::Coordinate {
    racer::Coordinate {
        line: line.0 as usize,
        column: column.0 as usize,
    }
}

fn from_racer_coord(
    coord: racer::Coordinate,
) -> (span::Row<span::OneIndexed>, span::Column<span::ZeroIndexed>) {
    (
        span::Row::new_one_indexed(coord.line as u32),
        span::Column::new_zero_indexed(coord.column as u32),
    )
}

fn pos_to_racer_location(pos: Position) -> racer::Location {
    let pos = ls_util::position_to_rls(pos);
    racer::Location::Coords(racer_coord(pos.row.one_indexed(), pos.col))
}

fn location_from_racer_match(a_match: &racer::Match) -> Option<Location> {
    let source_path = &a_match.filepath;

    a_match.coords.map(|coord| {
        let (row, col) = from_racer_coord(coord);
        let loc = span::Location::new(row.zero_indexed(), col, source_path);
        ls_util::rls_location_to_location(&loc)
    })
}
