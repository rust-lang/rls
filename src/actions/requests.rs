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

use actions::{ActionContext, InitActionContext};
use data;
use url::Url;
use vfs::FileContents;
use racer;
use rustfmt::{format_input, Input as FmtInput};
use rustfmt::file_lines::{FileLines, Range as RustfmtRange};
use serde_json;
use span;
use rayon;

use lsp_data;
use lsp_data::*;
use server::{Ack, Action, BlockingRequestAction, LsState, Output, RequestAction, ResponseError};
use jsonrpc_core::types::ErrorCode;

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;

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

/// A request for information about a symbol in this workspace.
pub struct WorkspaceSymbol;

impl Action for WorkspaceSymbol {
    type Params = lsp_data::WorkspaceSymbolParams;
    const METHOD: &'static str = "workspace/symbol";
}

impl RequestAction for WorkspaceSymbol {
    type Response = Vec<SymbolInformation>;

    fn new() -> Self {
        WorkspaceSymbol
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let analysis = ctx.analysis;

        let defs = analysis.name_defs(&params.query).unwrap_or_else(|_| vec![]);

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

/// A request for a flat list of all symbols found in a given text document.
pub struct Symbols;

impl Action for Symbols {
    type Params = DocumentSymbolParams;
    const METHOD: &'static str = "textDocument/documentSymbol";
}

impl RequestAction for Symbols {
    type Response = Vec<SymbolInformation>;

    fn new() -> Self {
        Symbols
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
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

/// Handles requests for hover information at a given point.
pub struct Hover;

impl Action for Hover {
    type Params = TextDocumentPositionParams;
    const METHOD: &'static str = "textDocument/hover";
}

impl RequestAction for Hover {
    type Response = lsp_data::Hover;

    fn new() -> Self {
        Hover
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(lsp_data::Hover {
            contents: HoverContents::Array(vec![]),
            range: None,
        })
    }

    fn handle(
        &mut self,
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
            contents.push(MarkedString::from_markdown(docs.into()));
        }
        if !doc_url.is_empty() {
            contents.push(MarkedString::from_markdown(doc_url.into()));
        }
        if !ty.is_empty() {
            contents.push(MarkedString::from_language_code("rust".into(), ty.into()));
        }
        Ok(lsp_data::Hover {
            contents: HoverContents::Array(contents),
            range: None, // TODO: maybe add?
        })
    }
}

/// Find all the implementations of a given trait.
pub struct FindImpls;

impl Action for FindImpls {
    type Params = TextDocumentPositionParams;
    const METHOD: &'static str = "rustDocument/implementations";
}

impl RequestAction for FindImpls {
    type Response = Vec<Location>;

    fn new() -> Self {
        FindImpls
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
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

/// Get a list of definitions for item at the given point or identifier.
pub struct Definition;

impl Action for Definition {
    type Params = TextDocumentPositionParams;
    const METHOD: &'static str = "textDocument/definition";
}

impl RequestAction for Definition {
    type Response = Vec<Location>;

    fn new() -> Self {
        Definition
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
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
                Some(receive_from_thread(move || {
                    let cache = racer::FileCache::new(vfs);
                    let session = racer::Session::new(&cache);
                    let location = pos_to_racer_location(params.position);

                    racer::find_definition(file_path, location, &session)
                        .and_then(location_from_racer_match)
                }))
            } else {
                None
            }
        };

        match analysis.goto_def(&span) {
            Ok(out) => {
                let result = vec![ls_util::rls_to_location(&out)];
                trace!("goto_def (compiler): {:?}", result);
                return Ok(result);
            }
            _ => match racer_receiver {
                Some(receiver) => match receiver.recv() {
                    Ok(Some(r)) => {
                        trace!("goto_def (Racer): {:?}", r);
                        return Ok(vec![r]);
                    }
                    Ok(None) => {
                        trace!("goto_def (Racer): None");
                        return Ok(vec![]);
                    }
                    _ => self.fallback_response(),
                },
                _ => self.fallback_response(),
            },
        }
    }
}

/// Find references to the symbol at the given point throughout the project.
pub struct References;

impl Action for References {
    type Params = ReferenceParams;
    const METHOD: &'static str = "textDocument/references";
}

impl RequestAction for References {
    type Response = Vec<Location>;

    fn new() -> Self {
        References
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
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

/// Get a list of possible completions at the given location.
pub struct Completion;

impl Action for Completion {
    type Params = TextDocumentPositionParams;
    const METHOD: &'static str = "textDocument/completion";
}

impl RequestAction for Completion {
    type Response = Vec<CompletionItem>;

    fn new() -> Self {
        Completion
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let vfs = ctx.vfs;
        let file_path = parse_file_path!(&params.text_document.uri, "complete")?;

        let cache = racer::FileCache::new(vfs);
        let session = racer::Session::new(&cache);

        let location = pos_to_racer_location(params.position);
        let results = racer::complete_from_file(file_path, location, &session);

        Ok(
            results
                .map(|comp| {
                    let snippet = racer::snippet_for_match(&comp, &session);
                    let mut item = completion_item_from_racer_match(comp);
                    if !snippet.is_empty() {
                        item.insert_text = Some(snippet);
                        item.insert_text_format = Some(InsertTextFormat::Snippet);
                    }
                    item
                })
                .collect(),
        )
    }
}

/// Find all references to the thing at the given location within this document,
/// so they can be highlighted in the editor. In practice, this is very similar
/// to `References`.
pub struct DocumentHighlight;

impl Action for DocumentHighlight {
    type Params = TextDocumentPositionParams;
    const METHOD: &'static str = "textDocument/documentHighlight";
}

impl RequestAction for DocumentHighlight {
    type Response = Vec<lsp_data::DocumentHighlight>;

    fn new() -> Self {
        DocumentHighlight
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let file_path = parse_file_path!(&params.text_document.uri, "highlight")?;
        let span = ctx.convert_pos_to_span(file_path, params.position);

        let result = ctx.analysis
            .find_all_refs(&span, true, false)
            .unwrap_or_else(|_| vec![]);

        Ok(
            result
                .iter()
                .map(|span| {
                    lsp_data::DocumentHighlight {
                        range: ls_util::rls_to_range(span.range),
                        kind: Some(DocumentHighlightKind::Text),
                    }
                })
                .collect(),
        )
    }
}

/// Rename the given symbol within the whole project.
pub struct Rename;

impl Action for Rename {
    type Params = RenameParams;
    const METHOD: &'static str = "textDocument/rename";
}

impl RequestAction for Rename {
    type Response = WorkspaceEdit;

    fn new() -> Self {
        Rename
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(WorkspaceEdit {
            changes: HashMap::new(),
        })
    }

    fn handle(
        &mut self,
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let file_path = parse_file_path!(&params.text_document.uri, "rename")?;
        let span = ctx.convert_pos_to_span(file_path, params.position);

        let analysis = ctx.analysis;

        macro_rules! unwrap_or_fallback {
            ($e: expr) => {
                match $e {
                    Ok(e) => e,
                    Err(_) => {
                        return self.fallback_response();
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
            return self.fallback_response();
        }

        let result = unwrap_or_fallback!(analysis.find_all_refs(&span, true, true));

        let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();

        for item in result.iter() {
            let loc = ls_util::rls_to_location(item);
            edits
                .entry(loc.uri)
                .or_insert_with(Vec::new)
                .push(TextEdit {
                    range: loc.range,
                    new_text: params.new_name.clone(),
                });
        }

        Ok(WorkspaceEdit { changes: edits })
    }
}

/// Execute a command within the workspace.
///
/// These are *not* shell commands, but commands given by the client and
/// performed by the RLS.
///
/// Currently, only the "rls.applySuggestion" command is supported.
pub struct ExecuteCommand;

impl Action for ExecuteCommand {
    type Params = ExecuteCommandParams;
    const METHOD: &'static str = "workspace/executeCommand";
}

impl<'a> BlockingRequestAction<'a> for ExecuteCommand {
    type Response = Ack;

    fn new(_: &'a mut LsState) -> Self {
        ExecuteCommand
    }

    fn handle<O: Output>(
        &mut self,
        id: usize,
        params: Self::Params,
        _ctx: &mut ActionContext,
        out: O,
    ) -> Result<Self::Response, ()> {
        match &*params.command {
            "rls.applySuggestion" => {
                let location =
                    serde_json::from_value(params.arguments[0].clone()).expect("Bad argument");
                let new_text =
                    serde_json::from_value(params.arguments[1].clone()).expect("Bad argument");
                Self::apply_suggestion(id, location, new_text, out)
            }
            "rls.deglobImports" => {
                if !params.arguments.is_empty() {
                    let deglob_results: Vec<DeglobResult> = params
                        .arguments
                        .into_iter()
                        .map(|res| serde_json::from_value(res).expect("Bad argument"))
                        .collect();
                    Self::apply_deglobs(deglob_results, out)
                } else {
                    // without changes always successful
                    Ok(Ack)
                }
            }
            c => {
                debug!("Unknown command: {}", c);
                out.failure_message(id, ErrorCode::MethodNotFound, "Unknown command");
                Err(())
            }
        }
    }
}

impl ExecuteCommand {
    fn apply_suggestion<O: Output>(
        _id: usize,
        location: Location,
        new_text: String,
        out: O,
    ) -> Result<Ack, ()> {
        trace!("apply_suggestion {:?} {}", location, new_text);
        // FIXME should handle the response
        let output = serde_json::to_string(&RequestMessage::new(
            out.provide_id(),
            "workspace/applyEdit".to_owned(),
            ApplyWorkspaceEditParams {
                edit: make_workspace_edit(location, new_text),
            },
        )).unwrap();
        out.response(output);
        Ok(Ack)
    }

    fn apply_deglobs<O: Output>(deglob_results: Vec<DeglobResult>, out: O) -> Result<Ack, ()> {
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

        // FIXME should handle the response
        let output = serde_json::to_string(&RequestMessage::new(
            out.provide_id(),
            "workspace/applyEdit".to_owned(),
            ApplyWorkspaceEditParams { edit },
        )).unwrap();
        out.response(output);
        Ok(Ack)
    }
}

/// Get a list of actions that can be performed on a specific document and range
/// of text by the server.
pub struct CodeAction;

impl CodeAction {
    /// Create CodeActions for fixes suggested by the compiler
    /// the results are appended to `code_actions_result`
    fn make_suggestion_fix_actions(
        params: &<Self as Action>::Params,
        file_path: &Path,
        ctx: &InitActionContext,
        code_actions_result: &mut <Self as RequestAction>::Response,
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

    /// Create CodeActions for performing deglobbing when a wildcard import is found
    /// the results are appended to `code_actions_result`
    fn make_deglob_actions(
        params: &<Self as Action>::Params,
        file_path: &Path,
        ctx: &InitActionContext,
        code_actions_result: &mut <Self as RequestAction>::Response,
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
                    let mut span = ls_util::location_to_rls(span.clone()).unwrap();
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
}

impl Action for CodeAction {
    type Params = CodeActionParams;
    const METHOD: &'static str = "textDocument/codeAction";
}

impl RequestAction for CodeAction {
    type Response = Vec<Command>;

    fn new() -> Self {
        CodeAction
    }

    fn fallback_response(&self) -> Result<Self::Response, ResponseError> {
        Ok(vec![])
    }

    fn handle(
        &mut self,
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        trace!("code_action {:?}", params);

        let file_path = parse_file_path!(&params.text_document.uri, "code_action")?;

        let mut cmds = vec![];
        Self::make_suggestion_fix_actions(&params, &file_path, &ctx, &mut cmds);
        Self::make_deglob_actions(&params, &file_path, &ctx, &mut cmds);
        Ok(cmds)
    }
}

/// Pretty print the given document.
pub struct Formatting;

impl Action for Formatting {
    type Params = DocumentFormattingParams;
    const METHOD: &'static str = "textDocument/formatting";
}

impl<'a> BlockingRequestAction<'a> for Formatting {
    type Response = [TextEdit; 1];

    fn new(_: &'a mut LsState) -> Self {
        Formatting
    }

    fn handle<O: Output>(
        &mut self,
        id: usize,
        params: Self::Params,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<Self::Response, ()> {
        reformat(id, params.text_document, None, &params.options, ctx, out)
    }
}

/// Pretty print the source within the given location range.
pub struct RangeFormatting;

impl Action for RangeFormatting {
    type Params = DocumentRangeFormattingParams;
    const METHOD: &'static str = "textDocument/rangeFormatting";
}

impl<'a> BlockingRequestAction<'a> for RangeFormatting {
    type Response = [TextEdit; 1];

    fn new(_: &'a mut LsState) -> Self {
        RangeFormatting
    }

    fn handle<O: Output>(
        &mut self,
        id: usize,
        params: Self::Params,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<Self::Response, ()> {
        reformat(
            id,
            params.text_document,
            Some(params.range),
            &params.options,
            ctx,
            out,
        )
    }
}

fn reformat<O: Output>(
    id: usize,
    doc: TextDocumentIdentifier,
    selection: Option<Range>,
    opts: &FormattingOptions,
    ctx: &mut ActionContext,
    out: O,
) -> Result<[TextEdit; 1], ()> {
    trace!(
        "Reformat: {} {:?} {:?} {} {}",
        id,
        doc,
        selection,
        opts.tab_size,
        opts.insert_spaces
    );
    let ctx = ctx.inited();
    let path = parse_file_path!(&doc.uri, "reformat")?;

    let input = match ctx.vfs.load_file(&path) {
        Ok(FileContents::Text(s)) => FmtInput::Text(s),
        Ok(_) => {
            debug!("Reformat failed, found binary file");
            out.failure_message(
                id,
                ErrorCode::InternalError,
                "Reformat failed to complete successfully",
            );
            return Err(());
        }
        Err(e) => {
            debug!("Reformat failed: {:?}", e);
            out.failure_message(
                id,
                ErrorCode::InternalError,
                "Reformat failed to complete successfully",
            );
            return Err(());
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
        ranges.insert("stdin".to_owned(), vec![range]);
        let file_lines = FileLines::from_ranges(ranges);
        config.set().file_lines(file_lines);
    };

    let mut buf = Vec::<u8>::new();
    match format_input(input, &config, Some(&mut buf)) {
        Ok((summary, ..)) => {
            // format_input returns Ok even if there are any errors, i.e., parsing errors.
            if summary.has_no_errors() {
                // Note that we don't need to update the VFS, the client
                // echos back the change to us.
                let text = String::from_utf8(buf).unwrap();

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

                out.failure_message(
                    id,
                    ErrorCode::InternalError,
                    "Reformat failed to complete successfully",
                );
                Err(())
            }
        }
        Err(e) => {
            debug!("Reformat failed: {:?}", e);
            out.failure_message(
                id,
                ErrorCode::InternalError,
                "Reformat failed to complete successfully",
            );
            Err(())
        }
    }
}

/// Resolve additional information about the given completion item
/// suggestion. This allows completion items to be yielded as quickly as
/// possible, with more details (which are presumably more expensive to compute)
/// filled in after the initial completion's presentation.
pub struct ResolveCompletion;

impl Action for ResolveCompletion {
    type Params = CompletionItem;
    const METHOD: &'static str = "completionItem/resolve";
}

impl<'a> BlockingRequestAction<'a> for ResolveCompletion {
    type Response = CompletionItem;

    fn new(_: &'a mut LsState) -> Self {
        ResolveCompletion
    }

    fn handle<O: Output>(
        &mut self,
        _id: usize,
        params: Self::Params,
        _ctx: &mut ActionContext,
        _out: O,
    ) -> Result<Self::Response, ()> {
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

fn location_from_racer_match(a_match: racer::Match) -> Option<Location> {
    let source_path = &a_match.filepath;

    a_match.coords.map(|coord| {
        let (row, col) = from_racer_coord(coord);
        let loc = span::Location::new(row.zero_indexed(), col, source_path);
        ls_util::rls_location_to_location(&loc)
    })
}

lazy_static! {
    /// Thread pool for request execution allowing concurrent request processing.
    pub static ref WORK_POOL: rayon::ThreadPool = rayon::ThreadPool::new(
        rayon::Configuration::default()
            .thread_name(|num| format!("request-worker-{}", num))
            .panic_handler(|err| warn!("{:?}", err))
    ).unwrap();
}

/// Runs work in a new thread on the `WORK_POOL` returning a result `Receiver`
pub fn receive_from_thread<T, F>(work_fn: F) -> mpsc::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    WORK_POOL.spawn(move || {
        // an error here simply means the work took too long and the receiver has been dropped
        let _ = sender.send(work_fn());
    });
    receiver
}
