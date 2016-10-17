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
use vfs::{Vfs, Change};
use racer::core::complete_from_file;
use racer::core;
use serde_json;

use build::*;
use lsp_data::*;
use ide::VscodeKind;
use ls_server::{Output, Logger};

use std::collections::HashMap;
use std::panic;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct ActionHandler {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    build_queue: Arc<BuildQueue>,
    current_project: Mutex<Option<String>>,
    previous_build_results: Mutex<HashMap<String, Vec<Diagnostic>>>,
    logger: Arc<Logger>,
}

impl ActionHandler {
    pub fn new(analysis: Arc<AnalysisHost>,
           vfs: Arc<Vfs>,
           build_queue: Arc<BuildQueue>,
           logger: Arc<Logger>) -> ActionHandler {
        ActionHandler {
            analysis: analysis,
            vfs: vfs,
            build_queue: build_queue,
            current_project: Mutex::new(None),
            previous_build_results: Mutex::new(HashMap::new()),
            logger: logger,
        }
    }

    pub fn init(&self, root_path: String, out: &Output) {
        {
            let mut results = self.previous_build_results.lock().unwrap();
            results.clear();
        }
        {
            let mut current_project = self.current_project.lock().unwrap();
            *current_project = Some(root_path.clone());
        }
        self.build(&root_path, BuildPriority::Normal, out);
    }

    pub fn build(&self, project_path: &str, priority: BuildPriority, out: &Output) {
        out.notify("rustDocument/diagnosticsBegin");

        self.logger.log(&format!("\nBUILDING {}\n", project_path));
        let result = self.build_queue.request_build(project_path, priority);
        match result {
            BuildResult::Success(ref x) | BuildResult::Failure(ref x) => {
                self.logger.log(&format!("\nBUILDING - Success\n"));
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
                                range: Range::from_span(&method.spans[0]),
                                severity: if method.level == "error" { 1 } else { 2 },
                                code: match method.code {
                                    Some(c) => c.code.clone(),
                                    None => String::new(),
                                },
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
                            self.logger.log(&format!("<<ERROR>> {:?}", e));
                            self.logger.log(&format!("<<FROM>> {}", msg));
                        }
                    }
                }

                let mut notifications = vec![];
                {
                    // These notifications will include empty sets of errors for files
                    // which had errors, but now don't. This instructs the IDE to clear
                    // errors for those files.
                    let results = self.previous_build_results.lock().unwrap();
                    for k in results.keys() {
                        notifications.push(NotificationMessage {
                            jsonrpc: "2.0".into(),
                            method: "textDocument/publishDiagnostics".to_string(),
                            params: PublishDiagnosticsParams {
                                uri: "file://".to_string() +
                                        project_path + "/" +
                                        k,
                                diagnostics: results.get(k).unwrap().clone()
                            }
                        });
                    }
                }

                // TODO we don't send an OK notification if there were no errors
                for notification in notifications {
                    // FIXME(43) factor out the notification mechanism.
                    let output = serde_json::to_string(&notification).unwrap();
                    out.response(output);
                }

                out.notify("rustDocument/diagnosticsEnd");

                self.logger.log(&format!("reload analysis: {}", project_path));
                self.analysis.reload(&project_path).unwrap();
            }
            BuildResult::Squashed => {
                self.logger.log(&format!("\nBUILDING - Squashed\n"));
                out.notify("rustDocument/diagnosticsEnd");
            },
            BuildResult::Err => {
                // TODO why are we erroring out?
                self.logger.log(&format!("\nBUILDING - Error\n"));
                out.notify("rustDocument/diagnosticsEnd");
            },
        }
    }

    pub fn on_change(&self, change: ChangeParams, out: &Output) {
        let fname: String = change.textDocument.uri.chars().skip("file://".len()).collect();
        let changes: Vec<Change> = change.contentChanges.iter().map(move |i| {
            Change {
                span: i.range.to_span(fname.clone()),
                text: i.text.clone()
            }
        }).collect();
        self.vfs.on_changes(&changes).unwrap();

        self.logger.log(&format!("CHANGES: {:?}", changes));

        let current_project = {
            let current_project = self.current_project.lock().unwrap();
            current_project.clone()
        };
        match current_project {
            Some(ref current_project) => self.build(&current_project, BuildPriority::Normal, out),
            None => self.logger.log("No project path"),
        }
    }

    pub fn symbols(&self, id: usize, doc: DocumentSymbolParams, out: &Output) {
        let t = thread::current();
        let file_name: String = doc.textDocument.uri.chars().skip("file://".len()).collect();
        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let symbols = analysis.symbols(&file_name).unwrap_or(vec![]);
            t.unpark();

            symbols.into_iter().map(|s| {
                SymbolInformation {
                    name: s.name,
                    kind: VscodeKind::from(s.kind) as u32,
                    location: Location::from_span(&s.span),
                }
            }).collect()
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().unwrap_or(vec![]);
        out.success(id, serde_json::to_string(&result).unwrap());
    }

    pub fn complete(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        fn adjust_vscode_pos_for_racer(mut source: Position) -> Position {
            source.line += 1;
            source
        }

        let vfs: &Vfs = &self.vfs;

        let pos = adjust_vscode_pos_for_racer(params.position);
        let fname: String = params.textDocument.uri.chars().skip("file://".len()).collect();
        let file_path = &Path::new(&fname);

        let result: Vec<CompletionItem> = panic::catch_unwind(move || {
            let cache = core::FileCache::new();
            let session = core::Session::from_path(&cache, file_path, file_path);
            for (path, txt) in vfs.get_cached_files() {
                session.cache_file_contents(&path, txt);
            }

            let src = session.load_file(file_path);

            let pos = session.load_file(file_path).coords_to_point(pos.line, pos.character).unwrap();
            let results = complete_from_file(&src.code, file_path, pos, &session);

            results.map(|comp| CompletionItem {
                label: comp.matchstr.clone(),
                detail: comp.contextstr.clone(),
            }).collect()
        }).unwrap_or(vec![]);

        out.success(id, serde_json::to_string(&result).unwrap());
    }

    pub fn rename(&self, id: usize, params: RenameParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(params.textDocument, params.position);
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]);

        let mut edits: HashMap<String, Vec<TextEdit>> = HashMap::new();

        for item in result.iter() {
            let loc = Location::from_span(&item);
            edits.entry(loc.uri.clone()).or_insert(vec![]);
            edits.get_mut(&loc.uri).unwrap().push(TextEdit {
                range: loc.range.clone(),
                newText: params.newName.clone(),
            });
        }

        out.success(id, serde_json::to_string(&WorkspaceEdit { changes: edits }).unwrap());
    }

    pub fn find_all_refs(&self, id: usize, params: ReferenceParams, out: &Output) {
        let t = thread::current();
        let span = self.convert_pos_to_span(params.textDocument, params.position);
        let analysis = self.analysis.clone();

        let rustw_handle = thread::spawn(move || {
            let result = analysis.find_all_refs(&span);
            t.unpark();

            result
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join().ok().and_then(|t| t.ok()).unwrap_or(vec![]);
        let refs: Vec<Location> = result.iter().map(|item| {
            Location::from_span(&item)
        }).collect();

        out.success(id, serde_json::to_string(&refs).unwrap());
    }

    pub fn goto_def(&self, id: usize, params: TextDocumentPositionParams, out: &Output) {
        // Save-analysis thread.
        let t = thread::current();
        let span = self.convert_pos_to_span(params.textDocument, params.position);
        let analysis = self.analysis.clone();
        let results = thread::spawn(move || {
            let result = if let Ok(s) = analysis.goto_def(&span) {
                vec![Location::from_span(&s)]
            } else {
                vec![]
            };

            t.unpark();

            result
        });
        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let results = results.join();
        match results {
            Ok(r) => {
                self.logger.log(&format!("\nGOING TO: {:?}\n", r));
                out.success(id, serde_json::to_string(&r).unwrap());
            }
            Err(e) => {
                self.logger.log(&format!("\nERROR IN GOTODEF: {:?}\n", e));
                out.failure(id, "GotoDef failed to complete successfully");
            }
        };
    }

    pub fn hover(&self, id: usize, params: HoverParams, out: &Output) {
        let t = thread::current();
        self.logger.log(&format!("CREATING SPAN"));
        let span = self.convert_pos_to_span(params.textDocument, params.position);

        self.logger.log(&format!("\nHovering span: {:?}\n", span));

        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let ty = analysis.show_type(&span).unwrap_or(String::new());
            let docs = analysis.docs(&span).unwrap_or(String::new());
            let doc_url = analysis.doc_url(&span).unwrap_or(String::new());
            t.unpark();

            let mut contents = vec![];
            if !docs.is_empty() {
                contents.push(MarkedString { language: "markdown".into(), value: docs });
            }
            if !doc_url.is_empty() {
                contents.push(MarkedString { language: "url".into(), value: doc_url });
            }
            if !ty.is_empty() {
                contents.push(MarkedString { language: "rust".into(), value: ty });
            }
            HoverSuccessContents {
                contents: contents
            }
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join();
        match result {
            Ok(r) => {
                out.success(id, serde_json::to_string(&r).unwrap());
            }
            Err(_) => {
                out.failure(id, "Hover failed to complete successfully");
            }
        }
    }

    fn convert_pos_to_span(&self, doc: Document, pos: Position) -> Span {
        let fname: String = doc.uri.chars().skip("file://".len()).collect();
        self.logger.log(&format!("\nWorking on: {:?} {:?}", fname, pos));
        let line = self.vfs.load_line(Path::new(&fname), pos.line);
        self.logger.log(&format!("\nGOT LINE: {:?}", line));
        let start_pos = {
            let mut tmp = Position { line: pos.line, character: 1 };
            for (i, c) in line.clone().unwrap().chars().enumerate() {
                if !(c.is_alphanumeric() || c == '_') {
                    tmp.character = i + 1;
                }
                if i == pos.character {
                    break;
                }
            }
            tmp
        };

        let end_pos = {
            let mut tmp = Position { line: pos.line, character: pos.character };
            for (i, c) in line.unwrap().chars().skip(pos.character).enumerate() {
                if !(c.is_alphanumeric() || c == '_') {
                    break;
                }
                tmp.character = i + pos.character + 1;
            }
            tmp
        };

        Span {
            file_name: fname,
            line_start: start_pos.line,
            column_start: start_pos.character,
            line_end: end_pos.line,
            column_end: end_pos.character,
        }
    }
}
