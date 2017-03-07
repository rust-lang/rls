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
mod lsp_extensions;

use analysis::{AnalysisHost};
use hyper::Url;
use vfs::{Vfs, Change};
use racer;
use rustfmt::{Input as FmtInput, format_input};
use rustfmt::config::{self, WriteMode};
use serde_json;
use span;
use Span;

use build::*;
use lsp_data::*;
use server::{ResponseData, Output};

use std::collections::HashMap;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use self::lsp_extensions::{PublishRustDiagnosticsParams, RustDiagnostic};
use self::compiler_message_parsing::{FileDiagnostic, ParseError};

type BuildResults = HashMap<PathBuf, Vec<RustDiagnostic>>;

pub struct ActionHandler {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    build_queue: Arc<BuildQueue>,
    current_project: Mutex<Option<PathBuf>>,
    previous_build_results: Mutex<BuildResults>,
}

impl ActionHandler {
    pub fn new(analysis: Arc<AnalysisHost>,
           vfs: Arc<Vfs>,
           build_queue: Arc<BuildQueue>) -> ActionHandler {
        ActionHandler {
            analysis: analysis,
            vfs: vfs,
            build_queue: build_queue,
            current_project: Mutex::new(None),
            previous_build_results: Mutex::new(HashMap::new()),
        }
    }

    pub fn init(&self, root_path: PathBuf, out: &Output) {
        {
            let mut results = self.previous_build_results.lock().unwrap();
            results.clear();
        }
        {
            let mut current_project = self.current_project.lock().unwrap();
            *current_project = Some(root_path.clone());
        }
        self.build(&root_path, BuildPriority::Immediate, out);
    }

    pub fn build(&self, project_path: &Path, priority: BuildPriority, out: &Output) {
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
                    Ok(FileDiagnostic { file_path, diagnostic }) => {
                        results.entry(file_path).or_insert_with(Vec::new).push(diagnostic);
                    }
                    Err(ParseError::JsonError(e)) => {
                        debug!("build error {:?}", e);
                        debug!("from {}", msg);
                    }
                    Err(ParseError::NoSpans) => {}
                }
            }
        }

        fn convert_build_results_to_notifications(build_results: &BuildResults,
                                                  project_path: &Path)
            -> Vec<NotificationMessage<PublishRustDiagnosticsParams>>
        {
            build_results
            .iter()
            .map(|(path, diagnostics)| {
                let method = "textDocument/publishDiagnostics".to_string();

                let params = PublishRustDiagnosticsParams {
                    uri: Url::from_file_path(project_path.join(path)).unwrap(),
                    diagnostics: diagnostics.clone(),
                };

                NotificationMessage::new(method, params)
            })
            .collect()
        }

        // We use `rustDocument` document here since these notifications are
        // custom to the RLS and not part of the LS protocol.
        out.notify("rustDocument/diagnosticsBegin");

        debug!("build {:?}", project_path);
        let result = self.build_queue.request_build(project_path, priority);
        match result {
            BuildResult::Success(ref x) | BuildResult::Failure(ref x) => {
                debug!("build - Success");

                // These notifications will include empty sets of errors for files
                // which had errors, but now don't. This instructs the IDE to clear
                // errors for those files.
                let notifications = {
                    let mut results = self.previous_build_results.lock().unwrap();
                    clear_build_results(&mut results);
                    parse_compiler_messages(x, &mut results);
                    convert_build_results_to_notifications(&results, project_path)
                };

                // TODO we don't send an OK notification if there were no errors
                for notification in notifications {
                    // FIXME(43) factor out the notification mechanism.
                    let output = serde_json::to_string(&notification).unwrap();
                    out.response(output);
                }

                trace!("reload analysis: {:?}", project_path);
                self.analysis.reload(project_path, false).unwrap();

                out.notify("rustDocument/diagnosticsEnd");
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
    }

    pub fn on_open(&self, open: DidOpenTextDocumentParams, out: &Output) {
        let fname = parse_file_path(&open.text_document.uri).unwrap();
        self.vfs.set_file(fname.as_path(), &open.text_document.text);

        trace!("on_open: {:?}", fname);

        self.build_current_project(BuildPriority::Normal, out);
    }

    pub fn on_change(&self, change: DidChangeTextDocumentParams, out: &Output) {
        let fname = parse_file_path(&change.text_document.uri).unwrap();
        let changes: Vec<Change> = change.content_changes.iter().map(move |i| {
            if let Some(range) = i.range {
                let range = ls_util::range_to_rls(range);
                Change::ReplaceText {
                    span: Span::from_range(range, fname.clone()),
                    text: i.text.clone()
                }
            } else {
                Change::AddFile {
                    file: fname.clone(),
                    text: i.text.clone(),
                }
            }
        }).collect();
        self.vfs.on_changes(&changes).unwrap();

        trace!("on_change: {:?}", changes);

        self.build_current_project(BuildPriority::Normal, out);
    }

    pub fn on_save(&self, save: DidSaveTextDocumentParams, out: &Output) {
        let fname = parse_file_path(&save.text_document.uri).unwrap();
        self.vfs.file_saved(&fname).unwrap();
        self.build_current_project(BuildPriority::Immediate, out);
    }

    fn build_current_project(&self, priority: BuildPriority, out: &Output) {
        let current_project = {
            let current_project = self.current_project.lock().unwrap();
            current_project.clone()
        };
        match current_project {
            Some(ref current_project) => self.build(current_project, priority, out),
            None => debug!("build_current_project - no project path"),
        }
    }

    pub fn symbols(&self, id: usize, doc: DocumentSymbolParams, out: &Output) {
        let t = thread::current();
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let file_name = parse_file_path(&doc.text_document.uri).unwrap();
            let symbols = analysis.symbols(&file_name).unwrap_or_else(|_| vec![]);
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

    pub fn complete(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        let result: Vec<CompletionItem> = panic::catch_unwind(move || {
            let file_path = &parse_file_path(&params.text_document.uri).unwrap();

            let cache = racer::FileCache::new(self.vfs.clone());
            let session = racer::Session::new(&cache);

            let location = pos_to_racer_location(params.position);
            let results = racer::complete_from_file(file_path, location, &session);

            results.map(|comp| CompletionItem::new_simple(
                comp.matchstr.clone(),
                comp.contextstr.clone(),
            )).collect()
        }).unwrap_or_else(|_| vec![]);

        out.success(id, ResponseData::CompletionItems(result));
    }

    pub fn rename(&self, id: usize, params: RenameParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, params.position);
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span, true);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or_else(Vec::new);

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

    pub fn highlight(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, params.position);
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

    pub fn find_all_refs(&self, id: usize, params: ReferenceParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, params.position);
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

    pub fn goto_def(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        // Save-analysis thread.
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, params.position);
        let analysis = self.analysis.clone();
        let vfs = self.vfs.clone();

        let compiler_handle = thread::spawn(move || {
            let result = analysis.goto_def(&span);

            t.unpark();

            result
        });

        // Racer thread.
        let racer_handle = thread::spawn(move || {
            let file_path = &parse_file_path(&params.text_document.uri).unwrap();

            let cache = racer::FileCache::new(vfs);
            let session = racer::Session::new(&cache);
            let location = pos_to_racer_location(params.position);

            racer::find_definition(file_path, location, &session)
                .and_then(location_from_racer_match)
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let compiler_result = compiler_handle.join();
        match compiler_result {
            Ok(Ok(r)) => {
                let result = vec![ls_util::rls_to_location(&r)];
                trace!("goto_def TO: {:?}", result);
                out.success(id, ResponseData::Locations(result));
            }
            _ => {
                info!("goto_def - falling back to Racer");
                match racer_handle.join() {
                    Ok(Some(r)) => {
                        trace!("goto_def: {:?}", r);
                        out.success(id, ResponseData::Locations(vec![r]));
                    }
                    _ => {
                        debug!("Error in Racer");
                        out.failure(id, "GotoDef failed to complete successfully");
                    }
                }
            }
        }
    }

    pub fn hover(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, params.position);

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
                out.failure(id, "Hover failed to complete successfully");
            }
        }
    }

    pub fn reformat(&self, id: usize, doc: TextDocumentIdentifier, out: &Output) {
        trace!("Reformat: {} {:?}", id, doc);

        let path = &parse_file_path(&doc.uri).unwrap();
        let input = match self.vfs.load_file(path) {
            Ok(s) => FmtInput::Text(s),
            Err(e) => {
                debug!("Reformat failed: {:?}", e);
                out.failure(id, "Reformat failed to complete successfully");
                return;
            }
        };

        let mut config = config::Config::default();
        config.skip_children = true;
        config.write_mode = WriteMode::Plain;

        let mut buf = Vec::<u8>::new();
        match format_input(input, &config, Some(&mut buf)) {
            Ok((summary, ..)) => {
                // format_input returns Ok even if there are any errors, i.e., parsing errors.
                if summary.has_no_errors() {
                    // Note that we don't need to keep the VFS up to date, the client
                    // echos back the change to us.
                    let range = ls_util::range_from_vfs_file(&self.vfs, path);
                    let text = String::from_utf8(buf).unwrap();
                    let result = [TextEdit {
                        range: range,
                        new_text: text,
                    }];
                    out.success(id, ResponseData::TextEdit(result))
                } else {
                    debug!("reformat: format_input failed: has errors, summary = {:?}", summary);

                    out.failure(id, "Reformat failed to complete successfully")
                }
            }
            Err(e) => {
                debug!("Reformat failed: {:?}", e);
                out.failure(id, "Reformat failed to complete successfully")
            }
        }
    }

    fn convert_pos_to_span(&self, doc: &TextDocumentIdentifier, pos: Position) -> Span {
        let fname = parse_file_path(&doc.uri).unwrap();
        trace!("convert_pos_to_span: {:?} {:?}", fname, pos);

        let pos = ls_util::position_to_rls(pos);
        let line = self.vfs.load_line(&fname, pos.row).unwrap();
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

        Span::from_positions(start_pos,
                             end_pos,
                             fname.to_owned())
    }
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
