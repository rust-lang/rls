// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use analysis::{AnalysisHost, Span};
use hyper::Url;
use vfs::{Vfs, Change};
use racer::core::{self, find_definition, complete_from_file};
use rustfmt::{Input as FmtInput, format_input};
use rustfmt::config::{self, WriteMode};
use serde_json;

use build::*;
use lsp_data::*;
use server::{ResponseData, Output};

use std::collections::HashMap;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct ActionHandler {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    build_queue: Arc<BuildQueue>,
    current_project: Mutex<Option<PathBuf>>,
    previous_build_results: Mutex<HashMap<PathBuf, Vec<Diagnostic>>>,
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

    pub fn init(&self, root_path: Option<PathBuf>, out: &Output) {
        {
            let mut results = self.previous_build_results.lock().unwrap();
            results.clear();
        }
        let root_path = match root_path {
            Some(some) => some, 
            None => return
        };
        {
            let mut current_project = self.current_project.lock().unwrap();
            *current_project = Some(root_path.clone());
        }
        self.build(&root_path, BuildPriority::Immediate, out);
    }

    pub fn build(&self, project_path: &Path, priority: BuildPriority, out: &Output) {
        out.notify("rustDocument/diagnosticsBegin");

        debug!("build {:?}", project_path);
        let result = self.build_queue.request_build(project_path, priority);
        match result {
            BuildResult::Success(ref x) | BuildResult::Failure(ref x) => {
                debug!("build - Success");
                {
                    let mut results = self.previous_build_results.lock().unwrap();
                    // We must not clear the hashmap, just the values in each list.
                    for v in &mut results.values_mut() {
                        v.clear();
                    }
                }
                for msg in x.iter() {
                    match serde_json::from_str::<CompilerMessage>(&msg) {
                        Ok(method) => {
                            if method.spans.is_empty() {
                                continue;
                            }
                            let mut diag = Diagnostic {
                                range: ls_util::range_from_span(&method.spans[0]),
                                severity: Some(if method.level == "error" {
                                    DiagnosticSeverity::Error
                                } else {
                                    DiagnosticSeverity::Warning
                                }),
                                code: Some(NumberOrString::String(match method.code {
                                    Some(c) => c.code.clone(),
                                    None => String::new(),
                                })),
                                source: Some("rustc".into()),
                                message: method.message.clone(),
                            };

                            //adjust diagnostic range for LSP
                            diag.range.start.line -= 1;
                            diag.range.start.character -= 1;
                            diag.range.end.line -= 1;
                            diag.range.end.character -= 1;

                            {
                                let mut results = self.previous_build_results.lock().unwrap();
                                results.entry(method.spans[0].file_name.clone()).or_insert(vec![]).push(diag);
                            }
                        }
                        Err(e) => {
                            debug!("build error {:?}", e);
                            debug!("from {}", msg);
                        }
                    }
                }

                let mut notifications = vec![];
                {
                    // These notifications will include empty sets of errors for files
                    // which had errors, but now don't. This instructs the IDE to clear
                    // errors for those files.
                    let results = self.previous_build_results.lock().unwrap();
                    for (k, v) in results.iter() {
                        notifications.push(NotificationMessage::new(
                            "textDocument/publishDiagnostics".to_string(),
                            PublishDiagnosticsParams::new(
                                Url::from_file_path(project_path.join(k)).unwrap(),
                                v.clone(),
                            )
                        ));
                    }
                }

                // TODO we don't send an OK notification if there were no errors
                for notification in notifications {
                    // FIXME(43) factor out the notification mechanism.
                    let output = serde_json::to_string(&notification).unwrap();
                    out.response(output);
                }

                out.notify("rustDocument/diagnosticsEnd");

                trace!("reload analysis: {:?}", project_path);
                self.analysis.reload(&project_path, false).unwrap();
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

    pub fn on_change(&self, change: DidChangeTextDocumentParams, out: &Output) {
        let fname: PathBuf = parse_file_path(&change.text_document.uri).unwrap();
        let changes: Vec<Change> = change.content_changes.iter().map(move |i| {
            let range = match i.range {
                Some(some) => { some } 
                None => {
                    // In this case the range is considered to be the whole document,
                    // as specified by LSP
                    
                    // FIXME: to do, endpos must be the end of the document, this is not correct
                    let end_pos = Position::new(0, 0);
                    Range{ start : Position::new(0, 0), end : end_pos }
                }
            };
            Change {
                span: ls_util::range_to_span(range, fname.clone()),
                text: i.text.clone()
            }
        }).collect();
        self.vfs.on_changes(&changes).unwrap();

        trace!("on_change: {:?}", changes);

        self.build_current_project(out);
    }

    fn build_current_project(&self, out: &Output) {
        let current_project = {
            let current_project = self.current_project.lock().unwrap();
            current_project.clone()
        };
        match current_project {
            Some(ref current_project) => self.build(&current_project, BuildPriority::Normal, out),
            None => debug!("build_current_project - no project path"),
        }
    }

    pub fn symbols(&self, id: usize, doc: DocumentSymbolParams, out: &Output) {
        let t = thread::current();
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let file_name = parse_file_path(&doc.text_document.uri).unwrap();
            let symbols = analysis.symbols(&file_name).unwrap_or(vec![]);
            t.unpark();

            symbols.into_iter().map(|s| {
                SymbolInformation {
                    name: s.name,
                    kind: source_kind_from_def_kind(s.kind),
                    location: ls_util::location_from_span(&s.span),
                    container_name: None // TODO: more info could be added here
                }
            }).collect()
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().unwrap_or(vec![]);
        out.success(id, ResponseData::SymbolInfo(result));
    }

    pub fn complete(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        let vfs: &Vfs = &self.vfs;
        let result: Vec<CompletionItem> = panic::catch_unwind(move || {
            let pos = adjust_vscode_pos_for_racer(params.position);
            let file_path = &parse_file_path(&params.text_document.uri).unwrap();

            let cache = core::FileCache::new();
            let session = core::Session::from_path(&cache, file_path, file_path);
            for (path, txt) in vfs.get_cached_files() {
                session.cache_file_contents(&path, txt);
            }

            let src = session.load_file(file_path);

            let pos = session.load_file(file_path).coords_to_point(to_usize(pos.line), to_usize(pos.character)).unwrap();
            let results = complete_from_file(&src.code, file_path, pos, &session);

            results.map(|comp| CompletionItem::new_simple(
                comp.matchstr.clone(),
                comp.contextstr.clone(),
            )).collect()
        }).unwrap_or(vec![]);

        out.success(id, ResponseData::CompletionItems(result));
    }

    pub fn rename(&self, id: usize, params: RenameParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, &params.position);
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span, true);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]);

        let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();

        for item in result.iter() {
            let loc = ls_util::location_from_span(&item);
            edits.entry(loc.uri).or_insert(vec![]).push(TextEdit {
                range: loc.range,
                new_text: params.new_name.clone(),
            });
        }

        out.success(id, ResponseData::WorkspaceEdit(WorkspaceEdit { changes: edits }));
    }

    pub fn highlight(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, &params.position);
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span, true);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]);
        let refs: Vec<_> = result.iter().map(|item| DocumentHighlight {
            range: ls_util::range_from_span(&item),
            kind: Some(DocumentHighlightKind::Text),
        }).collect();

        out.success(id, ResponseData::Highlights(refs));
    }

    pub fn find_all_refs(&self, id: usize, params: ReferenceParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, &params.position);
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span, params.context.include_declaration);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]);
        let refs: Vec<_> = result.iter().map(|item| ls_util::location_from_span(&item)).collect();

        out.success(id, ResponseData::Locations(refs));
    }

    pub fn goto_def(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        // Save-analysis thread.
        let t = thread::current();
        let span = self.convert_pos_to_span(&params.text_document, &params.position);
        let analysis = self.analysis.clone();
        let vfs = self.vfs.clone();

        let compiler_handle = thread::spawn(move || {
            let result = analysis.goto_def(&span);

            t.unpark();

            result
        });

        // Racer thread.
        let racer_handle = thread::spawn(move || {
            let pos = adjust_vscode_pos_for_racer(params.position);
            let file_path = &parse_file_path(&params.text_document.uri).unwrap();

            let cache = core::FileCache::new();
            let session = core::Session::from_path(&cache, file_path, file_path);
            for (path, txt) in vfs.get_cached_files() {
                session.cache_file_contents(&path, txt);
            }

            let src = session.load_file(file_path);

            find_definition(&src.code,
                            file_path,
                            src.coords_to_point(to_usize(pos.line), to_usize(pos.character)).unwrap(),
                            &session)
                .and_then(|mtch| {
                    let source_path = &mtch.filepath;
                    if mtch.point != 0 {
                        let (line, col) = session.load_file(source_path)
                                                 .point_to_coords(mtch.point)
                                                 .unwrap();
                        Some(ls_util::location_from_position(source_path,
                                                     adjust_racer_line_for_vscode(line),
                                                     col))
                    } else {
                        None
                    }
                })
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let compiler_result = compiler_handle.join();
        match compiler_result {
            Ok(Ok(r)) => {
                let result = vec![ls_util::location_from_span(&r)];
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
        let span = self.convert_pos_to_span(&params.text_document, &params.position);

        trace!("hover: {:?}", span);

        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let ty = analysis.show_type(&span).unwrap_or(String::new());
            let docs = analysis.docs(&span).unwrap_or(String::new());
            let doc_url = analysis.doc_url(&span).unwrap_or(String::new());
            t.unpark();

            let mut contents = vec![];
            if !docs.is_empty() {
                contents.push(MarkedString::from_markdown(docs.into()));
            }
            if !doc_url.is_empty() {
                contents.push(MarkedString::from_language_code("url".into(), doc_url.into()));
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
            Ok(_) => {
                // Note that we don't need to keep the VFS up to date, the client
                // echos back the change to us.
                let text = String::from_utf8(buf).unwrap();
                let result = [TextEdit {
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(from_usize(text.lines().count()), 0),
                    },
                    new_text: text,
                }];
                out.success(id, ResponseData::TextEdit(result))
            }
            Err(e) => {
                debug!("Reformat failed: {:?}", e);
                out.failure(id, "Reformat failed to complete successfully")
            }
        }
    }

    fn convert_pos_to_span(&self, doc: &TextDocumentIdentifier, pos: &Position) -> Span {
        let fname = parse_file_path(&doc.uri).unwrap();
        trace!("convert_pos_to_span: {:?} {:?}", fname, pos);
        let line = self.vfs.load_line(&fname, to_usize(pos.line));
        let start_pos = {
            let mut tmp = Position::new(pos.line, 1);
            for (i, c) in line.clone().unwrap().chars().enumerate() {
                if !(c.is_alphanumeric() || c == '_') {
                    tmp.character = from_usize(i + 1);
                }
                if from_usize(i) == pos.character {
                    break;
                }
            }
            tmp
        };

        let end_pos = {
            let mut tmp = Position::new(pos.line, pos.character);
            for (i, c) in line.unwrap().chars().skip(to_usize(pos.character)).enumerate() {
                if !(c.is_alphanumeric() || c == '_') {
                    break;
                }
                tmp.character = from_usize(i) + pos.character + 1;
            }
            tmp
        };

        Span {
            file_name: fname.to_owned(),
            line_start: to_usize(start_pos.line),
            column_start: to_usize(start_pos.character),
            line_end: to_usize(end_pos.line),
            column_end: to_usize(end_pos.character),
        }
    }
}

fn adjust_vscode_pos_for_racer(mut source: Position) -> Position {
    source.line += 1;
    source
}

fn adjust_racer_line_for_vscode(mut line: usize) -> usize {
    if line > 0 {
        line -= 1;
    }
    line
}
