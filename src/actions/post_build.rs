// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Post-build processing of data.

#![allow(missing_docs)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use build::BuildResult;
use lsp_data::{ls_util, PublishDiagnosticsParams};
use CRATE_BLACKLIST;
use Span;

use analysis::AnalysisHost;
use data::Analysis;
use ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};
use serde_json;
use span::compiler::DiagnosticSpan;
use url::Url;


pub type BuildResults = HashMap<PathBuf, Vec<(Diagnostic, Vec<Suggestion>)>>;

pub struct PostBuildHandler {
    pub analysis: Arc<AnalysisHost>,
    pub previous_build_results: Arc<Mutex<BuildResults>>,
    pub project_path: PathBuf,
    pub show_warnings: bool,
    pub use_black_list: bool,
    pub notifier: Box<Notifier>,
    pub blocked_threads: Vec<thread::Thread>,
}

/// Trait for communication back to the rest of the RLS (and on to the client).
// This trait only really exists to work around the object safety rules (Output
// is not object-safe).
pub trait Notifier: Send {
    fn notify_begin(&self);
    fn notify_end(&self);
    fn notify_publish(&self, PublishDiagnosticsParams);
}

impl PostBuildHandler {
    pub fn handle(mut self, result: BuildResult) {
        self.notifier.notify_begin();

        match result {
            BuildResult::Success(cwd, messages, new_analysis) => {
                thread::spawn(move || {
                    trace!("build - Success");

                    // Emit appropriate diagnostics using the ones from build.
                    self.handle_messages(&cwd, messages);

                    // Reload the analysis data.
                    debug!("reload analysis: {:?}", self.project_path);
                    if new_analysis.is_empty() {
                        self.reload_analysis_from_disk(&cwd);
                    } else {
                        self.reload_analysis_from_memory(&cwd, new_analysis);
                    }

                    // Wake up any threads blocked on this analysis.
                    for t in self.blocked_threads.drain(..) {
                        t.unpark();
                    }

                    self.notifier.notify_end();
                });
            }
            BuildResult::Squashed => {
                trace!("build - Squashed");
                self.notifier.notify_end();
            }
            BuildResult::Err => {
                trace!("build - Error");
                self.notifier.notify_end();
            }
        }
    }

    fn handle_messages(&self, cwd: &Path, messages: Vec<String>) {
        // These notifications will include empty sets of errors for files
        // which had errors, but now don't. This instructs the IDE to clear
        // errors for those files.
        let mut results = self.previous_build_results.lock().unwrap();
        // We must not clear the hashmap, just the values in each list.
        // This allows us to save allocated before memory.
        for v in &mut results.values_mut() {
            v.clear();
        }

        for msg in &messages {
            if let Some(FileDiagnostic {
                file_path,
                diagnostic,
                suggestions,
            }) = parse_diagnostics(msg) {
                results
                    .entry(cwd.join(file_path))
                    .or_insert_with(Vec::new)
                    .push((diagnostic, suggestions));
            }
        }

        self.emit_notifications(&results);
    }

    fn reload_analysis_from_disk(&self, cwd: &Path) {
        if self.use_black_list {
            self.analysis
                .reload_with_blacklist(&self.project_path, &cwd, &CRATE_BLACKLIST)
                .unwrap();
        } else {
            self.analysis.reload(&self.project_path, &cwd).unwrap();
        }
    }

    fn reload_analysis_from_memory(&self, cwd: &Path, analysis: Vec<Analysis>) {
        if self.use_black_list {
            self.analysis
                .reload_from_analysis(analysis, &self.project_path, &cwd, &CRATE_BLACKLIST)
                .unwrap();
        } else {
            self.analysis
                .reload_from_analysis(analysis, &self.project_path, &cwd, &[])
                .unwrap();
        }
    }

    fn emit_notifications(&self, build_results: &BuildResults) {
        for (path, diagnostics) in build_results {
            let params = PublishDiagnosticsParams {
                uri: Url::from_file_path(path).unwrap(),
                diagnostics: diagnostics
                    .iter()
                    .filter_map(|&(ref d, _)| {
                        if self.show_warnings || d.severity != Some(DiagnosticSeverity::Warning) {
                            Some(d.clone())
                        } else {
                            None
                        }
                    })
                    .collect(),
            };

            self.notifier.notify_publish(params);
        }
    }
}

#[derive(Debug)]
pub struct Suggestion {
    pub range: Range,
    pub new_text: String,
    pub label: String,
}

#[derive(Debug)]
struct FileDiagnostic {
    file_path: PathBuf,
    diagnostic: Diagnostic,
    suggestions: Vec<Suggestion>,
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    message: String,
    code: Option<CompilerMessageCode>,
    level: String,
    spans: Vec<DiagnosticSpan>,
    children: Vec<CompilerMessage>,
}

#[derive(Debug, Deserialize)]
struct CompilerMessageCode {
    code: String,
}

fn parse_diagnostics(message: &str) -> Option<FileDiagnostic> {
    let message = match serde_json::from_str::<CompilerMessage>(message) {
        Ok(m) => m,
        Err(e) => {
            debug!("build error {:?}", e);
            debug!("from {}", message);
            return None;
        }
    };

    if message.spans.is_empty() {
        return None;
    }

    let primary_span = primary_span(&message);
    let suggestions = make_suggestions(message.children, &primary_span.file);

    let diagnostic = Diagnostic {
        range: ls_util::rls_to_range(primary_span.range),
        severity: Some(severity(&message.level)),
        code: Some(NumberOrString::String(match message.code {
            Some(c) => c.code.clone(),
            None => String::new(),
        })),
        source: Some("rustc".into()),
        message: message.message,
    };

    Some(FileDiagnostic {
        file_path: primary_span.file,
        diagnostic: diagnostic,
        suggestions: suggestions,
    })
}

fn severity(level: &str) -> DiagnosticSeverity {
    if level == "error" {
        DiagnosticSeverity::Error
    } else {
        DiagnosticSeverity::Warning
    }
}

fn make_suggestions(children: Vec<CompilerMessage>, file: &Path) -> Vec<Suggestion> {
    let mut suggestions = vec![];
    for c in children {
        for sp in c.spans {
            let span = sp.rls_span().zero_indexed();
            if span.file == file {
                if let Some(s) = sp.suggested_replacement {
                    let suggestion = Suggestion {
                        new_text: s.clone(),
                        range: ls_util::rls_to_range(span.range),
                        label: format!("{}: `{}`", c.message, s),
                    };
                    suggestions.push(suggestion);
                }
            }
        }
    }
    suggestions
}

fn primary_span(message: &CompilerMessage) -> Span {
    let primary = message
        .spans
        .iter()
        .filter(|x| x.is_primary)
        .next()
        .unwrap()
        .clone();
    primary.rls_span().zero_indexed()
}

