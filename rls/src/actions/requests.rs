//! Requests that the RLS can respond to.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;

use itertools::Itertools;
use jsonrpc_core::types::ErrorCode;
use log::{debug, trace, warn};
use rls_analysis::SymbolQuery;
use rls_data as data;
use rls_span as span;
use rls_vfs::FileContents;
use rustfmt_nightly::{Edition as RustfmtEdition, FileLines, FileName, Range as RustfmtRange};
use serde_derive::{Deserialize, Serialize};
use url::Url;

use crate::actions::hover;
use crate::actions::run::collect_run_actions;
use crate::actions::InitActionContext;
use crate::build::Edition;
use crate::lsp_data;
use crate::lsp_data::request::ApplyWorkspaceEdit;
pub use crate::lsp_data::request::{
    CodeActionRequest as CodeAction, CodeLensRequest, Completion,
    DocumentHighlightRequest as DocumentHighlight, DocumentSymbolRequest as Symbols,
    ExecuteCommand, Formatting, GotoDefinition as Definition, GotoImplementation as Implementation,
    HoverRequest as Hover, RangeFormatting, References, Rename,
    ResolveCompletionItem as ResolveCompletion, WorkspaceSymbol,
};
use crate::lsp_data::*;
use crate::server;
use crate::server::{Ack, Output, Request, RequestAction, ResponseError, ResponseWithMessage};

/// The result of a deglob action for a single wildcard import.
///
/// The `location` is the position of the wildcard.
/// `new_text` is the text which should replace the wildcard.
#[derive(Debug, Deserialize, Serialize)]
pub struct DeglobResult {
    /// The `Location` of the "*" character in a wildcard import.
    pub location: Location,
    /// The replacement text.
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
        let query = SymbolQuery::subsequence(&params.query).limit(512);
        let defs = analysis.query_defs(query).unwrap_or_else(|_| vec![]);

        Ok(defs
            .into_iter()
            // Sometimes analysis will return duplicate symbols
            // for the same location, fix that up.
            .unique_by(|d| (d.span.clone(), d.name.clone()))
            .map(|d| SymbolInformation {
                name: d.name,
                kind: source_kind_from_def_kind(d.kind),
                location: ls_util::rls_to_location(&d.span),
                container_name: d
                    .parent
                    .and_then(|id| analysis.get_def(id).ok())
                    .map(|parent| parent.name),
                deprecated: None,
            })
            .collect())
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
        let analysis = ctx.analysis;

        let file_path = parse_file_path!(&params.text_document.uri, "symbols")?;

        let symbols = analysis.symbols(&file_path).unwrap_or_else(|_| vec![]);

        Ok(symbols
            .into_iter()
            .filter(|s| !s.name.is_empty()) // HACK: VS Code chokes on empty names
            .filter(|s| {
                let range = ls_util::rls_to_range(s.span.range);
                range.start != range.end
            })
            .map(|s| SymbolInformation {
                name: s.name,
                kind: source_kind_from_def_kind(s.kind),
                location: ls_util::rls_to_location(&s.span),
                container_name: s
                    .parent
                    .and_then(|id| analysis.get_def(id).ok())
                    .map(|parent| parent.name),
                deprecated: None,
            })
            .collect())
    }
}

impl RequestAction for Hover {
    type Response = lsp_data::Hover;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(lsp_data::Hover { contents: HoverContents::Array(vec![]), range: None })
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let tooltip = hover::tooltip(&ctx, &params)?;

        Ok(lsp_data::Hover {
            contents: HoverContents::Array(tooltip.contents),
            range: Some(ls_util::rls_to_range(tooltip.range)),
        })
    }
}

impl RequestAction for Implementation {
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
        let result = analysis
            .find_impls(type_id)
            .map(|spans| spans.into_iter().map(|x| ls_util::rls_to_location(&x)).collect());

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

        if let Ok(out) = ctx.analysis.goto_def(&span) {
            let result = vec![ls_util::rls_to_location(&out)];
            trace!("goto_def (compiler): {:?}", result);
            Ok(result)
        } else {
            let racer_enabled = {
                let config = ctx.config.lock().unwrap();
                config.racer_completion
            };
            if racer_enabled {
                let cache = ctx.racer_cache();
                let session = ctx.racer_session(&cache);
                let location = pos_to_racer_location(params.position);

                let r = racer::find_definition(file_path, location, &session)
                    .and_then(|rm| location_from_racer_match(&rm))
                    .map(|l| vec![l])
                    .unwrap_or_default();

                trace!("goto_def (Racer): {:?}", r);
                Ok(r)
            } else {
                Self::fallback_response()
            }
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
        let file_path =
            parse_file_path!(&params.text_document_position.text_document.uri, "find_all_refs")?;
        let span = ctx.convert_pos_to_span(file_path, params.text_document_position.position);

        let result =
            match ctx.analysis.find_all_refs(&span, params.context.include_declaration, false) {
                Ok(t) => t,
                _ => vec![],
            };

        Ok(result.iter().map(|item| ls_util::rls_to_location(item)).collect())
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
        if !ctx.config.lock().unwrap().racer_completion {
            return Self::fallback_response();
        }

        let file_path =
            parse_file_path!(&params.text_document_position.text_document.uri, "complete")?;

        let cache = ctx.racer_cache();
        let session = ctx.racer_session(&cache);

        let location = pos_to_racer_location(params.text_document_position.position);
        let results = racer::complete_from_file(&file_path, location, &session);
        let is_use_stmt = racer::is_use_stmt(&file_path, location, &session);

        let code_completion_has_snippet_support =
            ctx.client_capabilities.code_completion_has_snippet_support;

        Ok(results
            .map(|comp| {
                let mut item = completion_item_from_racer_match(&comp);
                if is_use_stmt && comp.mtype.is_function() {
                    item.insert_text = Some(comp.matchstr);
                } else if code_completion_has_snippet_support {
                    let snippet = racer::snippet_for_match(&comp, &session);
                    if !snippet.is_empty() {
                        item.insert_text = Some(snippet);
                        item.insert_text_format = Some(InsertTextFormat::Snippet);
                    }
                }
                item
            })
            .collect())
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

        let result = ctx.analysis.find_all_refs(&span, true, false).unwrap_or_else(|_| vec![]);

        Ok(result
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
            .collect())
    }
}

impl RequestAction for Rename {
    type Response = ResponseWithMessage<WorkspaceEdit>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Ok(ResponseWithMessage::Response(WorkspaceEdit { changes: None, document_changes: None }))
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        ctx.quiescent.store(true, Ordering::SeqCst);
        // We're going to mutate based on our data so we should block until the
        // data is ready.
        ctx.block_on_build();

        let file_path =
            parse_file_path!(&params.text_document_position.text_document.uri, "rename")?;
        let span = ctx.convert_pos_to_span(file_path, params.text_document_position.position);

        let analysis = ctx.analysis;

        macro_rules! unwrap_or_fallback {
            ($e: expr, $msg: expr) => {
                match $e {
                    Ok(e) => e,
                    Err(_) => {
                        return Ok(ResponseWithMessage::Warn($msg.to_owned()));
                    }
                }
            };
        }

        let id = unwrap_or_fallback!(
            analysis.crate_local_id(&span),
            "Rename failed: no information for symbol"
        );
        let def =
            unwrap_or_fallback!(analysis.get_def(id), "Rename failed: no definition for symbol");
        if def.name == "self" || def.name == "Self"
            // FIXME(#578)
            || def.kind == data::DefKind::Mod
        {
            return Ok(ResponseWithMessage::Warn(format!(
                "Rename failed: cannot rename {}",
                if def.kind == data::DefKind::Mod { "modules" } else { &def.name }
            )));
        }

        let result = unwrap_or_fallback!(
            analysis.find_all_refs(&span, true, true),
            "Rename failed: error finding references"
        );

        if result.is_empty() {
            return Ok(ResponseWithMessage::Warn(
                "Rename failed: RLS found nothing to rename - possibly due to multiple defs"
                    .to_owned(),
            ));
        }

        let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();

        for item in &result {
            let loc = ls_util::rls_to_location(item);
            edits
                .entry(loc.uri)
                .or_insert_with(Vec::new)
                .push(TextEdit { range: loc.range, new_text: params.new_name.clone() });
        }

        if !ctx.quiescent.load(Ordering::SeqCst) {
            return Ok(ResponseWithMessage::Warn(
                "Rename failed: RLS busy, please retry".to_owned(),
            ));
        }

        Ok(ResponseWithMessage::Response(WorkspaceEdit {
            changes: Some(edits),
            document_changes: None,
        }))
    }
}

#[derive(Debug)]
pub enum ExecuteCommandResponse {
    /// Response/client request containing workspace edits.
    ApplyEdit(ApplyWorkspaceEditParams),
}

impl server::Response for ExecuteCommandResponse {
    fn send<O: Output>(self, id: server::RequestId, out: &O) {
        // FIXME should handle the client's responses
        match self {
            ExecuteCommandResponse::ApplyEdit(ref params) => {
                let id = out.provide_id();
                let params = ApplyWorkspaceEditParams { edit: params.edit.clone() };

                let request = Request::<ApplyWorkspaceEdit>::new(id, params);
                out.request(request);
            }
        }

        // The formal request response is a simple ACK, though the objective
        // is the preceding client requests.
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
        if params.command.starts_with("rls.applySuggestion") {
            apply_suggestion(&params.arguments).map(ExecuteCommandResponse::ApplyEdit)
        } else if params.command.starts_with("rls.deglobImports") {
            apply_deglobs(params.arguments, &ctx).map(ExecuteCommandResponse::ApplyEdit)
        } else {
            debug!("Unknown command: {}", params.command);
            Err(ResponseError::Message(ErrorCode::MethodNotFound, "Unknown command".to_owned()))
        }
    }
}

fn apply_suggestion(args: &[serde_json::Value]) -> Result<ApplyWorkspaceEditParams, ResponseError> {
    let location = serde_json::from_value(args[0].clone()).expect("Bad argument");
    let new_text = serde_json::from_value(args[1].clone()).expect("Bad argument");

    trace!("apply_suggestion {:?} {}", location, new_text);
    Ok(ApplyWorkspaceEditParams { edit: make_workspace_edit(location, new_text) })
}

fn apply_deglobs(
    args: Vec<serde_json::Value>,
    ctx: &InitActionContext,
) -> Result<ApplyWorkspaceEditParams, ResponseError> {
    ctx.quiescent.store(true, Ordering::SeqCst);
    let deglob_results: Vec<DeglobResult> =
        args.into_iter().map(|res| serde_json::from_value(res).expect("Bad argument")).collect();

    trace!("apply_deglobs {:?}", deglob_results);

    assert!(!deglob_results.is_empty());
    let uri = deglob_results[0].location.uri.clone();

    let text_edits: Vec<_> = deglob_results
        .into_iter()
        .map(|res| TextEdit { range: res.location.range, new_text: res.new_text })
        .collect();
    // all deglob results will share the same URI
    let changes: HashMap<_, _> = vec![(uri, text_edits)].into_iter().collect();

    let edit = WorkspaceEdit { changes: Some(changes), document_changes: None };

    if !ctx.quiescent.load(Ordering::SeqCst) {
        return Err(ResponseError::Empty);
    }
    Ok(ApplyWorkspaceEditParams { edit })
}

/// Creates `CodeAction`s for fixes suggested by the compiler.
/// The results are appended to `code_actions_result`.
fn make_suggestion_fix_actions(
    params: &<CodeAction as lsp_data::request::Request>::Params,
    file_path: &Path,
    ctx: &InitActionContext,
    code_actions_result: &mut <CodeAction as RequestAction>::Response,
) {
    // Search for compiler suggestions.
    if let Some(results) = ctx.previous_build_results.lock().unwrap().get(file_path) {
        let suggestions = results
            .iter()
            .filter(|(diag, _)| diag.range.overlaps(&params.range))
            .flat_map(|(_, suggestions)| suggestions);
        for s in suggestions {
            let span = Location { uri: params.text_document.uri.clone(), range: s.range };
            let span = serde_json::to_value(&span).unwrap();
            let new_text = serde_json::to_value(&s.new_text).unwrap();
            let cmd = Command {
                title: s.label.clone(),
                command: format!("rls.applySuggestion-{}", ctx.pid),
                arguments: Some(vec![span, new_text]),
            };
            code_actions_result.push(cmd);
        }
    }
}

/// Creates `CodeAction`s for performing deglobbing when a wildcard import is found.
/// The results are appended to `code_actions_result`.
fn make_deglob_actions(
    params: &<CodeAction as lsp_data::request::Request>::Params,
    file_path: &Path,
    ctx: &InitActionContext,
    code_actions_result: &mut <CodeAction as RequestAction>::Response,
) {
    // Search for a glob in the line.
    if let Ok(line) = ctx.vfs.load_line(file_path, ls_util::range_to_rls(params.range).row_start) {
        let span = Location::new(params.text_document.uri.clone(), params.range);

        // For all indices that are a `*`, check if we can deglob them.
        // This handles badly-formatted text containing multiple `use`s in one line.
        let deglob_results: Vec<_> = line
            .char_indices()
            .filter(|&(_, chr)| chr == '*')
            .filter_map(|(index, _)| {
                // Map the indices to `Span`s.
                let mut span = ls_util::location_to_rls(&span).unwrap();
                span.range.col_start = span::Column::new_zero_indexed(index as u32);
                span.range.col_end = span::Column::new_zero_indexed(index as u32 + 1);

                // Load the deglob type information.
                ctx.analysis.show_type(&span).ok().map(|ty| (ty, span))
            })
            .map(|(mut deglob_str, span)| {
                // Handle multiple imports from one `*`.
                if deglob_str.contains(',') || deglob_str.is_empty() {
                    deglob_str = format!("{{{}}}", sort_deglob_str(&deglob_str));
                }

                // Build result.
                let deglob_result = DeglobResult {
                    location: ls_util::rls_to_location(&span),
                    new_text: deglob_str,
                };

                // Convert to json
                serde_json::to_value(&deglob_result).unwrap()
            })
            .collect();

        if !deglob_results.is_empty() {
            // extend result list
            let cmd = Command {
                title: format!("Deglob import{}", if deglob_results.len() > 1 { "s" } else { "" }),
                command: format!("rls.deglobImports-{}", ctx.pid),
                arguments: Some(deglob_results),
            };
            code_actions_result.push(cmd);
        }
    };
}

// Ideally we'd use Rustfmt for this, but reparsing is a bit of a pain.
fn sort_deglob_str(s: &str) -> String {
    let mut substrings = s.split(',').map(str::trim).collect::<Vec<_>>();
    substrings.sort_by(|a, b| {
        use std::cmp::Ordering;

        // Algorithm taken from rustfmt (`rustfmt/src/imports.rs`).

        let is_upper_snake_case =
            |s: &str| s.chars().all(|c| c.is_uppercase() || c == '_' || c.is_numeric());

        // snake_case < CamelCase < UPPER_SNAKE_CASE
        if a.starts_with(char::is_uppercase) && b.starts_with(char::is_lowercase) {
            return Ordering::Greater;
        }
        if a.starts_with(char::is_lowercase) && b.starts_with(char::is_uppercase) {
            return Ordering::Less;
        }
        if is_upper_snake_case(a) && !is_upper_snake_case(b) {
            return Ordering::Greater;
        }
        if !is_upper_snake_case(a) && is_upper_snake_case(b) {
            return Ordering::Less;
        }
        a.cmp(b)
    });
    substrings.join(", ")
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
    type Response = Vec<TextEdit>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Message(
            ErrorCode::InternalError,
            "Reformat failed to complete successfully".into(),
        ))
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        reformat(&params.text_document, None, &params.options, &ctx)
    }
}

impl RequestAction for RangeFormatting {
    type Response = Vec<TextEdit>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Message(
            ErrorCode::InternalError,
            "Reformat failed to complete successfully".into(),
        ))
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        reformat(&params.text_document, Some(params.range), &params.options, &ctx)
    }
}

fn reformat(
    doc: &TextDocumentIdentifier,
    selection: Option<Range>,
    opts: &FormattingOptions,
    ctx: &InitActionContext,
) -> Result<Vec<TextEdit>, ResponseError> {
    ctx.quiescent.store(true, Ordering::SeqCst);
    trace!("Reformat: {:?} {:?} {} {}", doc, selection, opts.tab_size, opts.insert_spaces);
    let path = parse_file_path!(&doc.uri, "reformat")?;

    let input = match ctx.vfs.load_file(&path) {
        Ok(FileContents::Text(s)) => s,
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

    let mut config = ctx.fmt_config().get_rustfmt_config().clone();
    if !config.was_set().hard_tabs() {
        config.set().hard_tabs(!opts.insert_spaces);
    }
    if !config.was_set().tab_spaces() {
        config.set().tab_spaces(opts.tab_size as usize);
    }
    if !config.was_set().edition() {
        match ctx.file_edition(path.clone()) {
            Some(edition) => {
                let edition = match edition {
                    Edition::Edition2015 => RustfmtEdition::Edition2015,
                    Edition::Edition2018 => RustfmtEdition::Edition2018,
                    Edition::Edition2021 => RustfmtEdition::Edition2021,
                };
                config.set().edition(edition);
                trace!("Detected edition {:?} for file `{}`", edition, path.display());
            }
            None => {
                warn!("Reformat failed: ambiguous edition for `{}`", path.display());

                return Err(ResponseError::Message(
                    ErrorCode::InternalError,
                    "Reformat failed to complete successfully".into(),
                ));
            }
        }
    }

    if let Some(r) = selection {
        let range_of_rls = ls_util::range_to_rls(r).one_indexed();
        let range =
            RustfmtRange::new(range_of_rls.row_start.0 as usize, range_of_rls.row_end.0 as usize);
        let mut ranges = HashMap::new();
        ranges.insert(FileName::Stdin, vec![range]);
        let file_lines = FileLines::from_ranges(ranges);
        config.set().file_lines(file_lines);
    };

    let text_edits = ctx
        .formatter()
        .calc_text_edits(input, config)
        .map_err(|msg| ResponseError::Message(ErrorCode::InternalError, msg.to_string()))?;

    // Note that we don't need to update the VFS, the client echos back the
    // change to us when it applies the returned `TextEdit`.

    if !ctx.quiescent.load(Ordering::SeqCst) {
        return Err(ResponseError::Message(
            ErrorCode::InternalError,
            "Reformat failed to complete successfully".into(),
        ));
    }

    Ok(text_edits)
}

impl RequestAction for ResolveCompletion {
    type Response = CompletionItem;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Empty)
    }

    fn handle(_: InitActionContext, params: Self::Params) -> Result<Self::Response, ResponseError> {
        // Currently, we safely ignore this as a pass-through since we fully handle
        // `textDocument/completion`. In the future, we may want to use this method as a
        // way to more lazily fill out completion information.
        Ok(params)
    }
}

pub(crate) fn racer_coord(
    row: span::Row<span::OneIndexed>,
    col: span::Column<span::ZeroIndexed>,
) -> racer::Coordinate {
    racer::Coordinate { row, col }
}

pub(crate) fn from_racer_coord(
    coord: racer::Coordinate,
) -> (span::Row<span::OneIndexed>, span::Column<span::ZeroIndexed>) {
    (coord.row, coord.col)
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

impl RequestAction for CodeLensRequest {
    type Response = Vec<CodeLens>;

    fn fallback_response() -> Result<Self::Response, ResponseError> {
        Err(ResponseError::Empty)
    }

    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError> {
        let mut ret = Vec::new();
        if ctx.client_supports_cmd_run {
            let file_path = parse_file_path!(&params.text_document.uri, "code_lens")?;
            for action in collect_run_actions(&ctx, &file_path) {
                let command = Command {
                    title: action.label,
                    command: "rls.run".to_string(),
                    arguments: Some(vec![serde_json::to_value(&action.cmd).unwrap()]),
                };
                let range = ls_util::rls_to_range(action.target_element);
                let lens = CodeLens { range, command: Some(command), data: None };
                ret.push(lens);
            }
        }
        Ok(ret)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_sort_deglob_str() {
        assert_eq!(sort_deglob_str(""), "");
        assert_eq!(sort_deglob_str("foo"), "foo");
        assert_eq!(sort_deglob_str("a, b"), "a, b");
        assert_eq!(sort_deglob_str("b, a"), "a, b");
        assert_eq!(sort_deglob_str("foo, bar, baz"), "bar, baz, foo");
        assert_eq!(
            sort_deglob_str("Curve, curve, ARC, bow, Bow, arc, Arc"),
            "arc, bow, curve, Arc, Bow, Curve, ARC",
        );
    }
}
