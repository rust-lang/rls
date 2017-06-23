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

use analysis::{AnalysisHost};
use url::Url;
use vfs::{Vfs, Change, FileContents};
use racer;
use rustfmt::{Input as FmtInput, format_input};
use rustfmt::file_lines::{Range as RustfmtRange, FileLines};
use config::FmtConfig;
use serde_json;
use span;
use Span;

use build::*;
use lsp_data::*;
use server::{ResponseData, Output, Ack};

use std::collections::HashMap;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use self::compiler_message_parsing::{FileDiagnostic, ParseError, Suggestion};

type BuildResults = HashMap<PathBuf, Vec<(Diagnostic, Vec<Suggestion>)>>;

pub struct ActionHandler {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    build_queue: Arc<BuildQueue>,
    current_project: Mutex<Option<PathBuf>>,
    previous_build_results: Arc<Mutex<BuildResults>>,
    fmt_config: Mutex<FmtConfig>,
}

impl ActionHandler {
    pub fn new(analysis: Arc<AnalysisHost>,
           vfs: Arc<Vfs>,
           build_queue: Arc<BuildQueue>) -> ActionHandler {
        ActionHandler {
            analysis,
            vfs: vfs.clone(),
            build_queue,
            current_project: Mutex::new(None),
            previous_build_results: Arc::new(Mutex::new(HashMap::new())),
            fmt_config: Mutex::new(FmtConfig::default()),
        }
    }

    pub fn init<O: Output>(&self, root_path: PathBuf, out: O) {
        {
            let mut results = self.previous_build_results.lock().unwrap();
            results.clear();
        }
        {
            let mut current_project = self.current_project.lock().unwrap();
            if current_project
                   .as_ref()
                   .map_or(true, |existing| *existing != root_path) {
                let new_path = root_path.clone();
                {
                    let mut config = self.fmt_config.lock().unwrap();
                    *config = FmtConfig::from(&new_path);
                }
                *current_project = Some(new_path);
            }
        }
        self.build(&root_path, BuildPriority::Immediate, out);
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

        fn convert_build_results_to_notifications(build_results: &BuildResults)
            -> Vec<NotificationMessage<PublishDiagnosticsParams>>
        {
            let cwd = ::std::env::current_dir().unwrap();

            build_results
                .iter()
                .map(|(path, diagnostics)| {
                    let method = "textDocument/publishDiagnostics".to_string();

                    let params = PublishDiagnosticsParams {
                        uri: Url::from_file_path(cwd.join(path)).unwrap(),
                        diagnostics: diagnostics.iter().map(|&(ref d, _)| d.clone()).collect(),
                    };

                    NotificationMessage::new(method, params)
                })
                .collect()
        }

        let build_queue = self.build_queue.clone();
        let analysis = self.analysis.clone();
        let previous_build_results = self.previous_build_results.clone();
        let project_path = project_path.to_owned();
        let out = out.clone();
        thread::spawn(move || {
            // We use `rustDocument` document here since these notifications are
            // custom to the RLS and not part of the LS protocol.
            out.notify("rustDocument/diagnosticsBegin");
            // let start_time = ::std::time::Instant::now();

            debug!("build {:?}", project_path);
            let result = build_queue.request_build(&project_path, priority);
            match result {
                BuildResult::Success(messages, new_analysis) | BuildResult::Failure(messages, new_analysis) => {
                    // eprintln!("built {:?}", start_time.elapsed());
                    debug!("build - Success");

                    // These notifications will include empty sets of errors for files
                    // which had errors, but now don't. This instructs the IDE to clear
                    // errors for those files.
                    let notifications = {
                        let mut results = previous_build_results.lock().unwrap();
                        clear_build_results(&mut results);
                        parse_compiler_messages(&messages, &mut results);
                        convert_build_results_to_notifications(&results)
                    };

                    for notification in notifications {
                        // FIXME(43) factor out the notification mechanism.
                        let output = serde_json::to_string(&notification).unwrap();
                        out.response(output);
                    }

                    debug!("reload analysis: {:?}", project_path);
                    let cwd = ::std::env::current_dir().unwrap();
                    // eprintln!("start analysis {:?}", start_time.elapsed());
                    if let Some(new_analysis) = new_analysis {
                        analysis.reload_from_analysis(new_analysis, &project_path, &cwd, false).unwrap();
                    } else {
                        analysis.reload(&project_path, &cwd, false).unwrap();
                    }
                    // eprintln!("finished analysis {:?}", start_time.elapsed());

                    out.notify("rustDocument/diagnosticsEnd");
                }
                BuildResult::Squashed => {
                    debug!("build - Squashed");
                    out.notify("rustDocument/diagnosticsEnd");
                },
                BuildResult::Err => {
                    debug!("build - Error");
                    out.notify("rustDocument/diagnosticsEnd");
                },
            }
        });
    }

    pub fn on_open<O: Output>(&self, open: DidOpenTextDocumentParams, _out: O) {
        let fname = parse_file_path(&open.text_document.uri).unwrap();
        self.vfs.set_file(fname.as_path(), &open.text_document.text);

        trace!("on_open: {:?}", fname);
    }

    pub fn on_change<O: Output>(&self, change: DidChangeTextDocumentParams, out: O) {
        trace!("on_change: {:?}, thread: {}", change, unsafe { ::std::mem::transmute::<_, u64>(thread::current().id()) });
        let fname = parse_file_path(&change.text_document.uri).unwrap();
        let changes: Vec<Change> = change.content_changes.iter().map(move |i| {
            if let Some(range) = i.range {
                let range = ls_util::range_to_rls(range);
                Change::ReplaceText {
                    span: Span::from_range(range, fname.clone()),
                    len: i.range_length,
                    text: i.text.clone()
                }
            } else {
                Change::AddFile {
                    file: fname.clone(),
                    text: i.text.clone(),
                }
            }
        }).collect();
        self.vfs.on_changes(&changes).expect("error committing to VFS");

        self.build_current_project(BuildPriority::Normal, out);
    }

    pub fn on_save<O: Output>(&self, save: DidSaveTextDocumentParams, _out: O) {
        let fname = parse_file_path(&save.text_document.uri).unwrap();
        self.vfs.file_saved(&fname).unwrap();
    }

    fn build_current_project<O: Output>(&self, priority: BuildPriority, out: O) {
        let current_project = {
            let current_project = self.current_project.lock().unwrap();
            current_project.clone()
        };
        match current_project {
            Some(ref current_project) => self.build(current_project, priority, out),
            None => debug!("build_current_project - no project path"),
        }
    }

    pub fn symbols<O: Output>(&self, id: usize, doc: DocumentSymbolParams, out: O) {
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

    pub fn complete<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
        let result: Vec<CompletionItem> = panic::catch_unwind(move || {
            let file_path = &parse_file_path(&params.text_document.uri).unwrap();

            let cache = racer::FileCache::new(self.vfs.clone());
            let session = racer::Session::new(&cache);

            let location = pos_to_racer_location(params.position);
            let results = racer::complete_from_file(file_path, location, &session);

            results.map(|comp| completion_item_from_racer_match(comp)).collect()
        }).unwrap_or_else(|_| vec![]);

        out.success(id, ResponseData::CompletionItems(result));
    }

    pub fn rename<O: Output>(&self, id: usize, params: RenameParams, out: O) {
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

    pub fn highlight<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
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

    pub fn find_all_refs<O: Output>(&self, id: usize, params: ReferenceParams, out: O) {
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

    pub fn goto_def<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
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

    pub fn hover<O: Output>(&self, id: usize, params: TextDocumentPositionParams, out: O) {
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

    pub fn execute_command<O: Output>(&self, id: usize, params: ExecuteCommandParams, out: O) {
        match &*params.command {
            "rls.applySuggestion" => {
                let location = serde_json::from_value(params.arguments[0].clone()).expect("Bad argument");
                let new_text = serde_json::from_value(params.arguments[1].clone()).expect("Bad argument");
                self.apply_suggestion(id, location, new_text, out)
            }
            c => {
                debug!("Unknown command: {}", c);
                out.failure(id, "Unknown command");
            }
        }
    }

    pub fn apply_suggestion<O: Output>(&self, id: usize, location: Location, new_text: String, out: O) {
        trace!("apply_suggestion {:?} {}", location, new_text);
        // FIXME should handle the response
        let output = serde_json::to_string(
            &RequestMessage::new("workspace/applyEdit".to_owned(),
                                 ApplyWorkspaceEditParams { edit: make_workspace_edit(location, new_text) })
        ).unwrap();
        out.response(output);
        out.success(id, ResponseData::Ack(Ack));
    }

    pub fn code_action<O: Output>(&self, id: usize, params: CodeActionParams, out: O) {
        trace!("code_action {:?}", params);

        let path = parse_file_path(&params.text_document.uri).expect("bad url");

        match self.previous_build_results.lock().unwrap().get(&path) {
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
        let span = ls_util::location_to_rls(location.clone()).unwrap();
        trace!("deglob {:?}", span);

        // Start by checking that the user has selected a glob import.
        if span.range.start() == span.range.end() {
            out.failure(id, "Empty selection");
            return;
        }
        match self.vfs.load_span(span.clone()) {
            Ok(s) => {
                if s != "*" {
                    out.failure(id, "Not a glob");
                    return;
                }
            }
            Err(e) => {
                debug!("Deglob failed: {:?}", e);
                out.failure(id, "Couldn't open file");
                return;
            }
        }

        // Save-analysis exports the deglobbed version of a glob import as its type string.
        let analysis = self.analysis.clone();
        let rustw_handle = thread::spawn(move || {
            let ty = analysis.show_type(&span);
            t.unpark();

            ty
        });

        thread::park_timeout(Duration::from_millis(::COMPILER_TIMEOUT));

        let result = rustw_handle.join();
        let mut deglob_str = match result {
            Ok(Ok(s)) => s,
            _ => {
                out.failure(id, "Couldn't get info from analysis");
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
            &RequestMessage::new("workspace/applyEdit".to_owned(),
                                 ApplyWorkspaceEditParams { edit: make_workspace_edit(location, deglob_str) })
        ).unwrap();
        out.response(output);

        // Nothing to actually send in the response.
        out.success(id, ResponseData::Ack(Ack));
    }

    pub fn reformat<O: Output>(&self, id: usize, doc: TextDocumentIdentifier, selection: Option<Range>, out: O, opts: &FormattingOptions) {
        trace!("Reformat: {} {:?} {:?} {} {}", id, doc, selection, opts.tab_size, opts.insert_spaces);

        let path = &parse_file_path(&doc.uri).unwrap();
        let input = match self.vfs.load_file(path) {
            Ok(FileContents::Text(s)) => FmtInput::Text(s),
            Ok(_) => {
                debug!("Reformat failed, found binary file");
                out.failure(id, "Reformat failed to complete successfully");
                return;
            }
            Err(e) => {
                debug!("Reformat failed: {:?}", e);
                out.failure(id, "Reformat failed to complete successfully");
                return;
            }
        };

        let range_whole_file = ls_util::range_from_vfs_file(&self.vfs, path);
        let mut config = self.fmt_config.lock().unwrap().get_rustfmt_config().clone();
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
