// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

mod compiler_message_parsing;

use cargo::CargoResult;
use cargo::util::important_paths;
use cargo::core::{Shell, Workspace};

use analysis::{AnalysisHost};
use url::Url;
use vfs::{Vfs, Change, FileContents};
use racer;
use rustfmt::{Input as FmtInput, format_input};
use rustfmt::file_lines::{Range as RustfmtRange, FileLines};
use config::{Config, FmtConfig, Inferrable};
use serde::Deserialize;
use serde::de::Error;
use serde_json;
use span;
use Span;

use build::*;
use CRATE_BLACKLIST;
use lsp_data::*;
use server::{ResponseData, Output, Ack};
use jsonrpc_core::types::ErrorCode;

use std::collections::HashMap;
use std::panic;
use std::path::{Path, PathBuf};
use std::io::sink;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use self::compiler_message_parsing::{FileDiagnostic, ParseError, Suggestion};

// TODO: Support non-`file` URI schemes in VFS. We're currently ignoring them because
// we don't want to crash the RLS in case a client opens a file under different URI scheme
// like with git:/ or perforce:/ (Probably even http:/? We currently don't support remote schemes).
macro_rules! ignore_non_file_uri {
    ($expr: expr, $uri: expr, $log_name: expr) => {
        match $expr {
            Err(UrlFileParseError::InvalidScheme) => {
                trace!("{}: Non-`file` URI scheme, ignoring: {:?}", $log_name, $uri);
                return;
            },
            result @ _ => result.unwrap(),
        }
    };
}

macro_rules! parse_file_path {
    ($uri: expr, $log_name: expr) => {
        ignore_non_file_uri!(parse_file_path($uri), $uri, $log_name)
    }
}

type BuildResults = HashMap<PathBuf, Vec<(Diagnostic, Vec<Suggestion>)>>;

pub struct ActionHandler {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    build_queue: BuildQueue,
    current_project: PathBuf,
    previous_build_results: Arc<Mutex<BuildResults>>,
    config: Arc<Mutex<Config>>,
    fmt_config: FmtConfig,
}

impl ActionHandler {
    pub fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               config: Arc<Mutex<Config>>,
               current_project: PathBuf) -> ActionHandler {
        let build_queue = BuildQueue::new(vfs.clone(), config.clone());
        let fmt_config = FmtConfig::from(&current_project);
        ActionHandler {
            analysis,
            vfs: vfs.clone(),
            build_queue,
            current_project,
            previous_build_results: Arc::new(Mutex::new(HashMap::new())),
            config,
            fmt_config,
        }
    }

    pub fn init<O: Output>(&self, init_options: InitializationOptions, out: O) {
        trace!("init: {:?}", init_options);

        let project_dir = self.current_project.clone();
        let config = self.config.clone();
        // Spawn another thread since we're shelling out to Cargo and this can
        // cause a non-trivial amount of time due to disk access
        thread::spawn(move || {
            let mut config = config.lock().unwrap();
            if let Err(e)  = infer_config_defaults(&project_dir, &mut *config) {
                debug!("Encountered an error while trying to infer config \
                    defaults: {:?}", e);
            }

        });

        if !init_options.omit_init_build {
            self.build_current_project(BuildPriority::Cargo, out);
        }
    }

    // Respond to the `initialized` notification. We take this opportunity to
    // dynamically register some options.
    pub fn initialized<O: Output>(&self,out: O) {
        const WATCH_ID: &'static str = "rls-watch";
        // TODO we should watch for workspace Cargo.tomls too
        let pattern = format!("{}/Cargo{{.toml,.lock}}", self.current_project.to_str().unwrap());
        let target_pattern = format!("{}/target", self.current_project.to_str().unwrap());
        // For target, we only watch if it gets deleted.
        let options = json!({
            "watchers": [{ "globPattern": pattern }, { "globPattern": target_pattern, "kind": 4 }]
        });
        let output = serde_json::to_string(
            &RequestMessage::new(out.provide_id(),
                                 NOTIFICATION__RegisterCapability.to_owned(),
                                 RegistrationParams { registrations: vec![Registration { id: WATCH_ID.to_owned(), method: NOTIFICATION__DidChangeWatchedFiles.to_owned(), register_options: options } ]})
        ).unwrap();
        out.response(output);
    }

    pub fn build<O: Output>(&self, project_path: &Path, priority: BuildPriority, out: O) {
        fn clear_build_results(results: &mut BuildResults) {
            // We must not clear the hashmap, just the values in each list.
            // This allows us to save allocated before memory.
            for v in &mut results.values_mut() {
                v.clear();
            }
        }

        fn parse_compiler_messages(messages: &[String], results: &mut BuildResults) {
            for msg in messages {
                match compiler_message_parsing::parse(msg) {
                    Ok(FileDiagnostic { file_path, diagnostic, suggestions }) => {
                        results.entry(file_path).or_insert_with(Vec::new).push((diagnostic, suggestions));
                    }
                    Err(ParseError::JsonError(e)) => {
                        debug!("build error {:?}", e);
                        debug!("from {}", msg);
                    }
                    Err(ParseError::NoSpans) => {}
                }
            }
        }

        fn convert_build_results_to_notifications(build_results: &BuildResults, show_warnings: bool)
            -> Vec<NotificationMessage<PublishDiagnosticsParams>>
        {
            let cwd = ::std::env::current_dir().unwrap();

            build_results
                .iter()
                .map(|(path, diagnostics)| {
                    let method = "textDocument/publishDiagnostics".to_string();

                    let params = PublishDiagnosticsParams {
                        uri: Url::from_file_path(cwd.join(path)).unwrap(),
                        diagnostics: diagnostics.iter()
                            .filter_map(|&(ref d, _)| {
                                let d = d.clone();
                                if show_warnings || d.severity != Some(DiagnosticSeverity::Warning) {
                                    Some(d)
                                } else {
                                    None
                                }
                            })
                            .collect(),
                    };

                    NotificationMessage::new(method, params)
                })
                .collect()
        }

        let analysis = self.analysis.clone();
        let previous_build_results = self.previous_build_results.clone();
        let project_path_clone = project_path.to_owned();
        let out = out.clone();
        let (show_warnings, use_black_list) = {
            let config = self.config.lock().unwrap();
            (config.show_warnings, config.use_crate_blacklist)
        };

        // We use `rustDocument` document here since these notifications are
        // custom to the RLS and not part of the LS protocol.
        out.notify("rustDocument/diagnosticsBegin");
        self.build_queue.request_build(project_path, priority, move |result| {
            match result {
                BuildResult::Success(messages, new_analysis) |
                BuildResult::Failure(messages, new_analysis) => {
                    thread::spawn(move || {
                        trace!("build - Success");

                        // These notifications will include empty sets of errors for files
                        // which had errors, but now don't. This instructs the IDE to clear
                        // errors for those files.
                        let notifications = {
                            let mut results = previous_build_results.lock().unwrap();
                            clear_build_results(&mut results);
                            parse_compiler_messages(&messages, &mut results);
                            convert_build_results_to_notifications(&results, show_warnings)
                        };

                        for notification in notifications {
                            // FIXME(43) factor out the notification mechanism.
                            let output = serde_json::to_string(&notification).unwrap();
                            out.response(output);
                        }

                        debug!("reload analysis: {:?}", project_path_clone);
                        let cwd = ::std::env::current_dir().unwrap();
                        if new_analysis.is_empty() {
                            if use_black_list {
                                analysis.reload_with_blacklist(&project_path_clone, &cwd, &CRATE_BLACKLIST).unwrap();
                            } else {
                                analysis.reload(&project_path_clone, &cwd).unwrap();
                            }
                        } else {
                            for data in new_analysis.into_iter() {
                                if use_black_list {
                                    analysis.reload_from_analysis(data, &project_path_clone, &cwd, &CRATE_BLACKLIST).unwrap();
                                } else {
                                    analysis.reload_from_analysis(data, &project_path_clone, &cwd, &[]).unwrap();
                                }
                            }
                        }

                        out.notify("rustDocument/diagnosticsEnd");
                    });
                }
                BuildResult::Squashed => {
                    trace!("build - Squashed");
                    out.notify("rustDocument/diagnosticsEnd");
                },
                BuildResult::Err => {
                    trace!("build - Error");
                    out.notify("rustDocument/diagnosticsEnd");
                },
            }
        });
    }

    pub fn find_impls<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
        let t = thread::current();
        let file_path = parse_file_path!(&params.text_document.uri, "find_impls");
        let span = self.convert_pos_to_span(file_path, params.position);
        let type_id = self.analysis.id(&span).expect("Analysis: Getting typeid from span");
        let analysis = self.analysis.clone();

        let handle = thread::spawn(move || {
            let result = analysis.find_impls(type_id).map(|spans| {
                spans.into_iter().map(|x| ls_util::rls_to_location(&x)).collect()
            });
            t.unpark();
            result
        });
        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = handle.join();
        trace!("find_impls: {:?}", result);
        match result {
            Ok(Ok(r)) => out.success(id, ResponseData::Locations(r)),
            _ => out.failure_message(id, ErrorCode::InternalError, "Find Implementations failed to complete successfully"),
        }
    }

    pub fn on_open<O: Output>(&self, open: DidOpenTextDocumentParams, _out: O) {
        trace!("on_open: {:?}", open.text_document.uri);
        let file_path = parse_file_path!(&open.text_document.uri, "on_open");

        self.vfs.set_file(&file_path, &open.text_document.text);
    }

    pub fn on_change<O: Output>(&self, change: DidChangeTextDocumentParams, out: O) {
        trace!("on_change: {:?}, thread: {:?}", change, thread::current().id());

        let file_path = parse_file_path!(&change.text_document.uri, "on_change");

        let changes: Vec<Change> = change.content_changes.iter().map(move |i| {
            if let Some(range) = i.range {
                let range = ls_util::range_to_rls(range);
                Change::ReplaceText {
                    span: Span::from_range(range, file_path.clone()),
                    len: i.range_length,
                    text: i.text.clone()
                }
            } else {
                Change::AddFile {
                    file: file_path.clone(),
                    text: i.text.clone(),
                }
            }
        }).collect();
        self.vfs.on_changes(&changes).expect("error committing to VFS");

        if !self.config.lock().unwrap().build_on_save {
            self.build_current_project(BuildPriority::Normal, out);
        }
    }

    pub fn on_cargo_change<O: Output>(&self, out: O) {
        trace!("on_cargo_change: thread: {:?}", thread::current().id());
        self.build_current_project(BuildPriority::Cargo, out);
    }

    pub fn on_save<O: Output>(&self, save: DidSaveTextDocumentParams, out: O) {
        let file_path = parse_file_path!(&save.text_document.uri, "on_save");

        self.vfs.file_saved(&file_path).unwrap();

        if self.config.lock().unwrap().build_on_save {
            self.build_current_project(BuildPriority::Normal, out);
        }
    }

    fn build_current_project<O: Output>(&self, priority: BuildPriority, out: O) {
        self.build(&self.current_project, priority, out);
    }

    pub fn symbols<O: Output>(&self, id: usize, doc: DocumentSymbolParams, out: O) {
        let t = thread::current();
        let file_path = parse_file_path!(&doc.text_document.uri, "symbols");

        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let symbols = analysis.symbols(&file_path).unwrap_or_else(|_| vec![]);
            t.unpark();

            symbols.into_iter().map(|s| {
                SymbolInformation {
                    name: s.name,
                    kind: source_kind_from_def_kind(s.kind),
                    location: ls_util::rls_to_location(&s.span),
                    container_name: None // FIXME: more info could be added here
                }
            }).collect()
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().unwrap_or_else(|_| vec![]);
        out.success(id, ResponseData::SymbolInfo(result));
    }

    pub fn complete<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
        let vfs = self.vfs.clone();
        let file_path = parse_file_path!(&params.text_document.uri, "complete");

        let result: Vec<CompletionItem> = panic::catch_unwind(move || {
            let cache = racer::FileCache::new(vfs);
            let session = racer::Session::new(&cache);

            let location = pos_to_racer_location(params.position);
            let results = racer::complete_from_file(file_path, location, &session);

            results.map(|comp| completion_item_from_racer_match(comp)).collect()
        }).unwrap_or_else(|_| vec![]);

        out.success(id, ResponseData::CompletionItems(result));
    }

    pub fn rename<O: Output>(&self, id: usize, params: RenameParams, out: O) {
        let t = thread::current();
        let file_path = parse_file_path!(&params.text_document.uri, "rename");
        let span = self.convert_pos_to_span(file_path, params.position);

        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            macro_rules! unwrap_or_empty {
                ($e: expr) => {
                    match $e {
                        Ok(e) => e,
                        Err(_) => {
                            t.unpark();
                            return vec![];
                        }
                    }
                }
            }

            let id = unwrap_or_empty!(analysis.crate_local_id(&span));
            let def = unwrap_or_empty!(analysis.get_def(id));
            if def.name == "self" || def.name == "Self" {
                t.unpark();
                return vec![];
            }

            let result = analysis.find_all_refs(&span, true);

            t.unpark();
            unwrap_or_empty!(result)
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().unwrap_or_else(|_| Vec::new());

        let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();

        for item in result.iter() {
            let loc = ls_util::rls_to_location(item);
            edits.entry(loc.uri).or_insert_with(Vec::new).push(TextEdit {
                range: loc.range,
                new_text: params.new_name.clone(),
            });
        }

        out.success(id, ResponseData::WorkspaceEdit(WorkspaceEdit { changes: edits }));
    }

    pub fn highlight<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
        let t = thread::current();
        let file_path = parse_file_path!(&params.text_document.uri, "highlight");
        let span = self.convert_pos_to_span(file_path, params.position);
        let analysis = self.analysis.clone();

        let handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span, true);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = handle.join().ok().and_then(|t| t.ok()).unwrap_or_else(Vec::new);
        let refs: Vec<_> = result.iter().map(|span| DocumentHighlight {
            range: ls_util::rls_to_range(span.range),
            kind: Some(DocumentHighlightKind::Text),
        }).collect();

        out.success(id, ResponseData::Highlights(refs));
    }

    pub fn find_all_refs<O: Output>(&self, id: usize, params: ReferenceParams, out: O) {
        let t = thread::current();
        let file_path = parse_file_path!(&params.text_document.uri, "find_all_refs");
        let span = self.convert_pos_to_span(file_path, params.position);
        let analysis = self.analysis.clone();

        let handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span, params.context.include_declaration);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = handle.join().ok().and_then(|t| t.ok()).unwrap_or_else(Vec::new);
        let refs: Vec<_> = result.iter().map(|item| ls_util::rls_to_location(item)).collect();

        out.success(id, ResponseData::Locations(refs));
    }

    pub fn goto_def<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
        // Save-analysis thread.
        let t = thread::current();
        let file_path = parse_file_path!(&params.text_document.uri, "goto_def");
        let span = self.convert_pos_to_span(file_path.clone(), params.position);
        let analysis = self.analysis.clone();
        let vfs = self.vfs.clone();

        let compiler_handle = thread::spawn(move || {
            let result = analysis.goto_def(&span);

            t.unpark();

            result
        });

        // Racer thread.
        let racer_handle = if self.config.lock().unwrap().goto_def_racer_fallback {
            Some(thread::spawn(move || {

                let cache = racer::FileCache::new(vfs);
                let session = racer::Session::new(&cache);
                let location = pos_to_racer_location(params.position);

                racer::find_definition(file_path, location, &session)
                    .and_then(location_from_racer_match)
            }))
        } else {
            None
        };

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let compiler_result = compiler_handle.join();
        match compiler_result {
            Ok(Ok(r)) => {
                let result = vec![ls_util::rls_to_location(&r)];
                trace!("goto_def (compiler): {:?}", result);
                out.success(id, ResponseData::Locations(result));
            }
            _ => {
                match racer_handle {
                    Some(racer_handle) => match racer_handle.join() {
                        Ok(Some(r)) => {
                            trace!("goto_def (Racer): {:?}", r);
                            out.success(id, ResponseData::Locations(vec![r]));
                        }
                        Ok(None) => {
                            trace!("goto_def (Racer): None");
                            out.success(id, ResponseData::Locations(vec![]));
                        }
                        _ => {
                            debug!("Error in Racer");
                            out.success(id, ResponseData::Locations(vec![]));
                        }
                    },
                    None => out.success(id, ResponseData::Locations(vec![])),
                }
            }
        }
    }

    pub fn hover<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
        let t = thread::current();
        let file_path = parse_file_path!(&params.text_document.uri, "hover");
        let span = self.convert_pos_to_span(file_path, params.position);

        trace!("hover: {:?}", span);

        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let ty = analysis.show_type(&span).unwrap_or_else(|_| String::new());
            let docs = analysis.docs(&span).unwrap_or_else(|_| String::new());
            let doc_url = analysis.doc_url(&span).unwrap_or_else(|_| String::new());
            t.unpark();

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
            Hover {
                contents: contents,
                range: None, // TODO: maybe add?
            }
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join();
        match result {
            Ok(r) => {
                out.success(id, ResponseData::HoverSuccess(r));
            }
            Err(_) => {
                let r = Hover {
                    contents: vec![],
                    range: None,
                };
                out.success(id, ResponseData::HoverSuccess(r));
            }
        }
    }

    pub fn execute_command<O: Output>(&self, id: usize, params: ExecuteCommandParams, out: O) {
        match &*params.command {
            "rls.applySuggestion" => {
                let location = serde_json::from_value(params.arguments[0].clone()).expect("Bad argument");
                let new_text = serde_json::from_value(params.arguments[1].clone()).expect("Bad argument");
                self.apply_suggestion(id, location, new_text, out)
            }
            c => {
                debug!("Unknown command: {}", c);
                out.failure_message(id, ErrorCode::MethodNotFound, "Unknown command");
            }
        }
    }

    pub fn apply_suggestion<O: Output>(&self, id: usize, location: Location, new_text: String, out: O) {
        trace!("apply_suggestion {:?} {}", location, new_text);
        // FIXME should handle the response
        let output = serde_json::to_string(
            &RequestMessage::new(out.provide_id(),
                                 "workspace/applyEdit".to_owned(),
                                 ApplyWorkspaceEditParams { edit: make_workspace_edit(location, new_text) })
        ).unwrap();
        out.response(output);
        out.success(id, ResponseData::Ack(Ack));
    }

    pub fn code_action<O: Output>(&self, id: usize, params: CodeActionParams, out: O) {
        trace!("code_action {:?}", params);

        let file_path = parse_file_path!(&params.text_document.uri, "code_action");

        match self.previous_build_results.lock().unwrap().get(&file_path) {
            Some(ref diagnostics) => {
                let suggestions = diagnostics.iter().filter(|&&(ref d, _)| d.range == params.range).flat_map(|&(_, ref ss)| ss.iter());
                let mut cmds = vec![];
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
                    cmds.push(cmd);
                }

                out.success(id, ResponseData::Commands(cmds));
            }
            None => {
                out.success(id, ResponseData::Commands(vec![]));
                return;
            }
        }
    }

    pub fn deglob<O: Output>(&self, id: usize, location: Location, out: O) {
        let t = thread::current();
        let span = ls_util::location_to_rls(location.clone());
        let mut span = ignore_non_file_uri!(span, &location.uri, "deglob");

        trace!("deglob {:?}", span);

        // Start by checking that the user has selected a glob import.
        if span.range.start() == span.range.end() {
            // search for a glob in the line
            let vfs = self.vfs.clone();
            let line = match vfs.load_line(&span.file, span.range.row_start) {
                Ok(l) => l,
                Err(_) => {
                    out.failure_message(id, ErrorCode::InvalidParams, "Could not retrieve line from VFS.");
                    return;
                }
            };

            // search for exactly one "::*;" in the line. This should work fine for formatted text, but
            // multiple use statements could be in the same line, then it is not possible to find which
            // one to deglob.
            let matches: Vec<_> = line.char_indices().filter(|&(_, chr)| chr == '*').collect();
            if matches.len() == 0 {
                out.failure_message(id, ErrorCode::InvalidParams, "No glob in selection.");
                return;
            } else if matches.len() > 1 {
                out.failure_message(id, ErrorCode::InvalidParams, "Multiple globs in selection.");
                return;
            }
            let index = matches[0].0 as u32;
            span.range.col_start = span::Column::new_zero_indexed(index);
            span.range.col_end = span::Column::new_zero_indexed(index+1);
        }

        // Save-analysis exports the deglobbed version of a glob import as its type string.
        let vfs = self.vfs.clone();
        let analysis = self.analysis.clone();
        let out_clone = out.clone();
        let span_ = span.clone();
        let rustw_handle = thread::spawn(move || {
            match vfs.load_span(span_.clone()) {
                Ok(ref s) if s != "*" => {
                    out_clone.failure_message(id, ErrorCode::InvalidParams, "Not a glob");
                    t.unpark();
                    return Err("Not a glob");
                }
                Err(e) => {
                    debug!("Deglob failed: {:?}", e);
                    out_clone.failure_message(id, ErrorCode::InternalError, "Couldn't open file");
                    t.unpark();
                    return Err("Couldn't open file");
                }
                _ => {}
            }

            let ty = analysis.show_type(&span_);
            t.unpark();

            ty.map_err(|_| {
                out_clone.failure_message(id, ErrorCode::InternalError, "Couldn't get info from analysis");
                "Couldn't get info from analysis"
            })
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join();
        let mut deglob_str = match result {
            Ok(Ok(s)) => s,
            _ => {
                return;
            }
        };

        // Handle multiple imports.
        if deglob_str.contains(',') {
            deglob_str = format!("{{{}}}", deglob_str);
        }

        // Send a workspace edit to make the actual change.
        // FIXME should handle the response
        let output = serde_json::to_string(
            &RequestMessage::new(out.provide_id(),
                                 "workspace/applyEdit".to_owned(),
                                 ApplyWorkspaceEditParams { edit: make_workspace_edit(ls_util::rls_to_location(&span), deglob_str) })
        ).unwrap();
        out.response(output);

        // Nothing to actually send in the response.
        out.success(id, ResponseData::Ack(Ack));
    }

    pub fn reformat<O: Output>(&self, id: usize, doc: TextDocumentIdentifier, selection: Option<Range>, out: O, opts: &FormattingOptions) {
        trace!("Reformat: {} {:?} {:?} {} {}", id, doc, selection, opts.tab_size, opts.insert_spaces);
        let path = parse_file_path!(&doc.uri, "reformat");

        let input = match self.vfs.load_file(&path) {
            Ok(FileContents::Text(s)) => FmtInput::Text(s),
            Ok(_) => {
                debug!("Reformat failed, found binary file");
                out.failure_message(id, ErrorCode::InternalError, "Reformat failed to complete successfully");
                return;
            }
            Err(e) => {
                debug!("Reformat failed: {:?}", e);
                out.failure_message(id, ErrorCode::InternalError, "Reformat failed to complete successfully");
                return;
            }
        };

        let range_whole_file = ls_util::range_from_vfs_file(&self.vfs, &path);
        let mut config = self.fmt_config.get_rustfmt_config().clone();
        if !config.was_set().hard_tabs() {
            config.set().hard_tabs(!opts.insert_spaces);
        }
        if !config.was_set().tab_spaces() {
            config.set().tab_spaces(opts.tab_size as usize);
        }

        if let Some(r) = selection {
            let range_of_rls = ls_util::range_to_rls(r).one_indexed();
            let range = RustfmtRange::new(range_of_rls.row_start.0 as usize, range_of_rls.row_end.0 as usize);
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
                    let result = [TextEdit {
                        range: range_whole_file,
                        new_text: text,
                    }];
                    out.success(id, ResponseData::TextEdit(result));
                } else {
                    debug!("reformat: format_input failed: has errors, summary = {:?}", summary);

                    out.failure_message(id, ErrorCode::InternalError, "Reformat failed to complete successfully");
                }
            }
            Err(e) => {
                debug!("Reformat failed: {:?}", e);
                out.failure_message(id, ErrorCode::InternalError, "Reformat failed to complete successfully");
            }
        }
    }

    pub fn on_change_config<O: Output>(&self, params: DidChangeConfigurationParams, out: O) {
        trace!("config change: {:?}", params.settings);
        let config = params.settings.get("rust")
                         .ok_or(serde_json::Error::missing_field("rust"))
                         .and_then(|value| Config::deserialize(value));

        let new_config = match config {
            Ok(mut value) => {
                value.normalise();
                value
            }
            Err(err) => {
                debug!("Received unactionable config: {:?} (error: {:?})", params.settings, err);
                return;
            }
        };

        let unstable_features = new_config.unstable_features;

        {
            let mut config = self.config.lock().unwrap();

            // User may specify null (to be inferred) options, in which case
            // we schedule further inference on a separate thread not to block
            // the main thread
            let needs_inference = new_config.needs_inference();
            // In case of null options, we provide default values for now
            config.update(new_config);
            trace!("Updated config: {:?}", *config);

            if needs_inference {
                let project_dir = self.current_project.clone();
                let config = self.config.clone();
                // Will lock and access Config just outside the current scope
                thread::spawn(move || {
                    let mut config = config.lock().unwrap();
                    if let Err(e)  = infer_config_defaults(&project_dir, &mut *config) {
                        debug!("Encountered an error while trying to infer config \
                            defaults: {:?}", e);
                    }
                });
            }
        }
        // We do a clean build so that if we've changed any relevant options
        // for Cargo, we'll notice them. But if nothing relevant changes
        // then we don't do unnecessary building (i.e., we don't delete
        // artifacts on disk).
        self.build_current_project(BuildPriority::Cargo, out.clone());

        const RANGE_FORMATTING_ID: &'static str = "rls-range-formatting";
        // FIXME should handle the response
        if unstable_features {
            let output = serde_json::to_string(
                &RequestMessage::new(out.provide_id(),
                                        NOTIFICATION__RegisterCapability.to_owned(),
                                        RegistrationParams { registrations: vec![Registration { id: RANGE_FORMATTING_ID.to_owned(), method: REQUEST__RangeFormatting.to_owned(), register_options: serde_json::Value::Null }] })
            ).unwrap();
            out.response(output);
        } else {
            let output = serde_json::to_string(
                &RequestMessage::new(out.provide_id(),
                                        NOTIFICATION__UnregisterCapability.to_owned(),
                                        UnregistrationParams { unregisterations: vec![Unregistration { id: RANGE_FORMATTING_ID.to_owned(), method: REQUEST__RangeFormatting.to_owned() }] })
            ).unwrap();
            out.response(output);
        }
    }

    fn convert_pos_to_span(&self, file_path: PathBuf, pos: Position) -> Span {
        trace!("convert_pos_to_span: {:?} {:?}", file_path, pos);

        let pos = ls_util::position_to_rls(pos);
        let line = self.vfs.load_line(&file_path, pos.row).unwrap();
        trace!("line: `{}`", line);

        let start_pos = {
            let mut col = 0;
            for (i, c) in line.chars().enumerate() {
                if !(c.is_alphanumeric() || c == '_') {
                    col = i + 1;
                }
                if i == pos.col.0 as usize {
                    break;
                }
            }
            trace!("start: {}", col);
            span::Position::new(pos.row, span::Column::new_zero_indexed(col as u32))
        };

        let end_pos = {
            let mut col = pos.col.0 as usize;
            for c in line.chars().skip(col) {
                if !(c.is_alphanumeric() || c == '_') {
                    break;
                }
                col += 1;
            }
            trace!("end: {}", col);
            span::Position::new(pos.row, span::Column::new_zero_indexed(col as u32))
        };

        Span::from_positions(start_pos, end_pos, file_path)
    }
}

fn infer_config_defaults(project_dir: &Path, config: &mut Config) -> CargoResult<()> {
    // Note that this may not be equal build_dir when inside a workspace member
    let manifest_path = important_paths::find_root_manifest_for_wd(None, project_dir)?;
    trace!("root manifest_path: {:?}", &manifest_path);

    // Cargo constructs relative paths from the manifest dir, so we have to pop "Cargo.toml"
    let manifest_dir = manifest_path.parent().unwrap();
    let shell = Shell::from_write(Box::new(sink()));
    let cargo_config = make_cargo_config(manifest_dir, shell);

    let ws = Workspace::new(&manifest_path, &cargo_config)?;

    // Auto-detect --lib/--bin switch if working under single package mode
    // or under workspace mode with `analyze_package` specified
    let package = match config.workspace_mode {
        true => {
            let package_name = match config.analyze_package {
                // No package specified, nothing to do
                None => { return Ok(()); },
                Some(ref package) => package,
            };

            ws.members()
              .find(move |x| x.name() == package_name)
              .ok_or(
                  format!("Couldn't find specified `{}` package via \
                      `analyze_package` in the workspace", package_name)
              )?
        },
        false => ws.current()?,
    };

    trace!("infer_config_defaults: Auto-detected `{}` package", package.name());

    let targets = package.targets();
    let (lib, bin) = if targets.iter().any(|x| x.is_lib()) {
        (true, None)
    } else {
        let mut bins = targets.iter().filter(|x| x.is_bin());
        // No `lib` detected, but also can't find any `bin` target - there's
        // no sensible target here, so just Err out
        let first = bins.nth(0)
            .ok_or("No `bin` or `lib` targets in the package")?;

        let mut bins = targets.iter().filter(|x| x.is_bin());
        let target = match bins.find(|x| x.src_path().ends_with("main.rs")) {
            Some(main_bin) => main_bin,
            None => first,
        };

        (false, Some(target.name().to_owned()))
    };

    trace!("infer_config_defaults: build_lib: {:?}, build_bin: {:?}", lib, bin);

    // Unless crate target is explicitly specified, mark the values as
    // inferred, so they're not simply ovewritten on config change without
    // any specified value
    let (lib, bin) = match (&config.build_lib, &config.build_bin) {
        (&Inferrable::Specified(true), _) => (lib, None),
        (_, &Inferrable::Specified(Some(_))) => (false, bin),
        _ => (lib, bin),
    };

    config.build_lib.infer(lib);
    config.build_bin.infer(bin);

    Ok(())
}

fn racer_coord(line: span::Row<span::OneIndexed>,
               column: span::Column<span::ZeroIndexed>)
               -> racer::Coordinate {
    racer::Coordinate {
        line: line.0 as usize,
        column: column.0 as usize,
    }
}

fn from_racer_coord(coord: racer::Coordinate) -> (span::Row<span::OneIndexed>,span::Column<span::ZeroIndexed>) {
    (span::Row::new_one_indexed(coord.line as u32), span::Column::new_zero_indexed(coord.column as u32))
}

fn pos_to_racer_location(pos: Position) -> racer::Location {
    let pos = ls_util::position_to_rls(pos);
    racer::Location::Coords(racer_coord(pos.row.one_indexed(), pos.col))
}

fn location_from_racer_match(mtch: racer::Match) -> Option<Location> {
    let source_path = &mtch.filepath;

    mtch.coords.map(|coord| {
        let (row, col) = from_racer_coord(coord);
        let loc = span::Location::new(row.zero_indexed(), col, source_path);
        ls_util::rls_location_to_location(&loc)
    })
}
