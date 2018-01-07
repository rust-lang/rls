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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use build::BuildResult;
use lsp_data::{ls_util, PublishDiagnosticsParams};
use actions::progress::DiagnosticsNotifier;

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
    pub shown_cargo_error: Arc<AtomicBool>,
    pub active_build_count: Arc<AtomicUsize>,
    pub notifier: Box<DiagnosticsNotifier>,
    pub blocked_threads: Vec<thread::Thread>,
}


impl PostBuildHandler {
    pub fn handle(mut self, result: BuildResult) {

        match result {
            BuildResult::Success(cwd, messages, new_analysis, _) => {
                thread::spawn(move || {
                    trace!("build - Success");
                    self.notifier.notify_begin_diagnostics();

                    // Emit appropriate diagnostics using the ones from build.
                    self.handle_messages(&cwd, &messages);

                    // Reload the analysis data.
                    trace!("reload analysis: {:?} {:?}", self.project_path, cwd);
                    if new_analysis.is_empty() {
                        self.reload_analysis_from_disk(&cwd);
                    } else {
                        self.reload_analysis_from_memory(&cwd, new_analysis);
                    }

                    // the end message must be dispatched before waking up
                    // the blocked threads, or we might see "done":true message
                    // first in the next action invocation.
                    self.notifier.notify_end_diagnostics();

                    // Wake up any threads blocked on this analysis.
                    for t in self.blocked_threads.drain(..) {
                        t.unpark();
                    }

                    self.shown_cargo_error.store(false, Ordering::SeqCst);
                    self.active_build_count.fetch_sub(1, Ordering::SeqCst);
                });
            }
            BuildResult::Squashed => {
                trace!("build - Squashed");
                self.active_build_count.fetch_sub(1, Ordering::SeqCst);
            }
            BuildResult::Err(cause, cmd) => {
                trace!("build - Error {} when running {:?}", cause, cmd);
                self.notifier.notify_begin_diagnostics();
                if !self.shown_cargo_error.swap(true, Ordering::SeqCst) {
                    let msg = format!("There was an error trying to build, RLS features will be limited: {}", cause);
                    self.notifier.notify_error_diagnostics(&msg);
                }
                self.notifier.notify_end_diagnostics();
                self.active_build_count.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    fn handle_messages(&self, cwd: &Path, messages: &[String]) {
        // These notifications will include empty sets of errors for files
        // which had errors, but now don't. This instructs the IDE to clear
        // errors for those files.
        let mut results = self.previous_build_results.lock().unwrap();
        // We must not clear the hashmap, just the values in each list.
        // This allows us to save allocated before memory.
        for v in &mut results.values_mut() {
            v.clear();
        }

        for (group, msg) in messages.iter().enumerate() {
            if let Some(FileDiagnostic {
                file_path,
                diagnostic,
                secondaries,
                suggestions,
            }) = parse_diagnostics(msg, group as u64) {
                let entry = results
                    .entry(cwd.join(file_path))
                    .or_insert_with(Vec::new);

                entry.push((diagnostic, suggestions));
                for secondary in secondaries {
                    entry.push((secondary, vec![]));
                }
            }
        }

        self.emit_notifications(&results);
    }

    fn reload_analysis_from_disk(&self, cwd: &Path) {
        if self.use_black_list {
            self.analysis
                .reload_with_blacklist(&self.project_path, cwd, &::blacklist::CRATE_BLACKLIST)
                .unwrap();
        } else {
            self.analysis.reload(&self.project_path, cwd).unwrap();
        }
    }

    fn reload_analysis_from_memory(&self, cwd: &Path, analysis: Vec<Analysis>) {
        if self.use_black_list {
            self.analysis
                .reload_from_analysis(analysis, &self.project_path, cwd, &::blacklist::CRATE_BLACKLIST)
                .unwrap();
        } else {
            self.analysis
                .reload_from_analysis(analysis, &self.project_path, cwd, &[])
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

            self.notifier.notify_publish_diagnostics(params);
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
    secondaries: Vec<Diagnostic>,
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

fn parse_diagnostics(message: &str, group: u64) -> Option<FileDiagnostic> {
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

    let diagnostic_msg = message.message.clone();
    let primary_span = message.spans.iter().find(|s| s.is_primary).unwrap();
    let rls_span = primary_span.rls_span().zero_indexed();
    let suggestions = make_suggestions(&message.children, &rls_span.file);

    let mut source = "rustc";
    let diagnostic = {
        let mut primary_message = diagnostic_msg.clone();
        if let Some(ref primary_label) = primary_span.label {
            if primary_label.trim() != primary_message.trim() {
                primary_message.push_str(&format!("\n\n{}", primary_label));
            }
        }

        if let Some(notes) = format_notes(&message.children, primary_span) {
            primary_message.push_str(&format!("\n\n{}", notes));
        }

        // A diagnostic source is quite likely to be clippy if it contains
        // the further information link to the rust-clippy project.
        if primary_message.contains("rust-clippy") {
            source = "clippy"
        }

        Diagnostic {
            range: ls_util::rls_to_range(rls_span.range),
            severity: Some(severity(&message.level)),
            code: Some(NumberOrString::String(match message.code {
                Some(ref c) => c.code.clone(),
                None => String::new(),
            })),
            source: Some(source.to_owned()),
            message: primary_message.trim().to_owned(),
            group: if message.spans.iter().any(|x| !x.is_primary) {
                Some(group)
            } else {
                None
            }
        }
    };

    // For a compiler error that has secondary spans (e.g. borrow error showing
    // both borrow and error spans) we emit additional diagnostics. These don't
    // include notes and are of an `Information` severity.
    let secondaries = message
    .spans
    .iter()
    .filter(|x| !x.is_primary)
    .map(|secondary_span| {
        let mut secondary_message = if secondary_span.is_within(primary_span) {
            String::new()
        }
        else {
            diagnostic_msg.clone()
        };

        if let Some(ref secondary_label) = secondary_span.label {
            secondary_message.push_str(&format!("\n\n{}", secondary_label));
        }
        let rls_span = secondary_span.rls_span().zero_indexed();

        Diagnostic {
            range: ls_util::rls_to_range(rls_span.range),
            severity: Some(DiagnosticSeverity::Information),
            code: Some(NumberOrString::String(match message.code {
                Some(ref c) => c.code.clone(),
                None => String::new(),
            })),
            source: Some(source.to_owned()),
            message: secondary_message.trim().to_owned(),
            group: Some(group),
        }
    }).collect();

    Some(FileDiagnostic {
        file_path: rls_span.file,
        diagnostic,
        secondaries,
        suggestions,
    })
}

fn format_notes(children: &[CompilerMessage], primary: &DiagnosticSpan) -> Option<String> {
    if !children.is_empty() {
        let mut notes = String::new();
        for &CompilerMessage { ref message, ref level, ref spans, .. } in children {

            macro_rules! add_message_to_notes {
                ($msg:expr) => {{
                    let mut lines = message.lines();
                    notes.push_str(&format!("\n{}: {}", level, lines.next().unwrap()));
                    for line in lines {
                        notes.push_str(&format!(
                            "\n{:indent$}{line}",
                            "",
                            indent = level.len() + 2,
                            line = line,
                        ));
                    }
                }}
            }

            if spans.is_empty() {
                add_message_to_notes!(message);
            }
            else if spans.len() == 1 && spans[0].is_within(primary) {
                add_message_to_notes!(message);
                if let Some(ref suggested) = spans[0].suggested_replacement {
                    notes.push_str(&format!(": `{}`", suggested));
                }
            }
        }

        if notes.is_empty() { None } else { Some(notes.trim().to_string()) }
    }
    else {
        None
    }
}

fn severity(level: &str) -> DiagnosticSeverity {
    if level == "error" {
        DiagnosticSeverity::Error
    } else {
        DiagnosticSeverity::Warning
    }
}

fn make_suggestions(children: &[CompilerMessage], file: &Path) -> Vec<Suggestion> {
    let mut suggestions = vec![];
    for c in children {
        for sp in &c.spans {
            let span = sp.rls_span().zero_indexed();
            if span.file == file {
                if let Some(ref s) = sp.suggested_replacement {
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

trait IsWithin {
    /// Returns whether `other` is considered within `self`
    /// note: a thing should be 'within' itself
    fn is_within(&self, other: &Self) -> bool;
}
impl<T: PartialOrd<T>> IsWithin for ::std::ops::Range<T> {
    fn is_within(&self, other: &Self) -> bool {
        self.start >= other.start &&
            self.start <= other.end &&
            self.end <= other.end &&
            self.end >= other.start
    }
}
impl IsWithin for DiagnosticSpan {
    fn is_within(&self, other: &Self) -> bool {
        let DiagnosticSpan { line_start, line_end, column_start, column_end, .. } = *self;
        (line_start..line_end+1).is_within(&(other.line_start..other.line_end+1)) &&
            (column_start..column_end+1).is_within(&(other.column_start..other.column_end+1))
    }
}

/// Tests for formatted messages from the compilers json output
/// run cargo with `--message-format=json` to generate the json for new tests and add .json
/// message files to '../../test_data/compiler_message/'
#[cfg(test)]
mod diagnostic_message_test {
    use super::*;

    fn parse_compiler_message(compiler_message: &str) -> FileDiagnostic {
        let _ = ::env_logger::try_init();
        parse_diagnostics(compiler_message, 0)
            .expect("failed to parse compiler message")
    }

    trait FileDiagnosticTestExt {
        /// Returns (primary message, secondary messages)
        fn to_messages(&self) -> (String, Vec<String>);
    }

    impl FileDiagnosticTestExt for FileDiagnostic {
        fn to_messages(&self) -> (String, Vec<String>) {
            (
                self.diagnostic.message.clone(),
                self.secondaries.iter().map(|d| d.message.clone()).collect()
            )
        }
    }

    /// ```
    /// fn use_after_move() {
    ///     let s = String::new();
    ///     ::std::mem::drop(s);
    ///     ::std::mem::drop(s);
    /// }
    /// ```
    #[test]
    fn message_use_after_move() {
        let diag = parse_compiler_message(
            include_str!("../../test_data/compiler_message/use-after-move.json")
        );

        assert_eq!(diag.diagnostic.source, Some("rustc".into()));
        for source in diag.secondaries.iter().map(|d| d.source.as_ref()) {
            assert_eq!(source, Some(&"rustc".into()));
        }

        let (msg, others) = diag.to_messages();
        assert_eq!(
            msg,
            "use of moved value: `s`\n\n\
            value used here after move\n\n\
            note: move occurs because `s` has type `std::string::String`, which does not implement the `Copy` trait"
        );

        assert_eq!(others, vec![
            "use of moved value: `s`\n\n\
            value moved here"
        ]);
    }

    /// ```
    /// fn type_annotations_needed() {
    ///     let v = Vec::new();
    /// }
    /// ```
    #[test]
    fn message_type_annotations_needed() {
        let (msg, others) = parse_compiler_message(
            include_str!("../../test_data/compiler_message/type-annotations-needed.json")
        ).to_messages();
        assert_eq!(
            msg,
            "type annotations needed\n\n\
            cannot infer type for `T`",
        );

        assert_eq!(others, vec![
            "type annotations needed\n\n\
            consider giving `v` a type"
        ]);
    }

    /// ```
    /// fn mismatched_types() -> usize {
    ///     123_i32
    /// }
    /// ```
    #[test]
    fn message_mismatched_types() {
        let (msg, others) = parse_compiler_message(
            include_str!("../../test_data/compiler_message/mismatched-types.json")
        ).to_messages();
        assert_eq!(
            msg,
            "mismatched types\n\n\
            expected usize, found i32",
        );

        assert_eq!(others, vec![
            "mismatched types\n\n\
            expected `usize` because of return type"
        ]);
    }

    /// ```
    /// fn not_mut() {
    ///     let string = String::new();
    ///     let _s1 = &mut string;
    /// }
    /// ```
    #[test]
    fn message_not_mutable() {
        let (msg, others) = parse_compiler_message(
            include_str!("../../test_data/compiler_message/not-mut.json")
        ).to_messages();
        assert_eq!(
            msg,
            "cannot borrow immutable local variable `string` as mutable\n\n\
            cannot borrow mutably",
        );

        assert_eq!(others, vec![
            "cannot borrow immutable local variable `string` as mutable\n\n\
            consider changing this to `mut string`"
        ]);
    }

    /// ```
    /// fn consider_borrow() {
    ///     fn takes_ref(s: &str) {}
    ///     let string = String::new();
    ///     takes_ref(string);
    /// }
    /// ```
    #[test]
    fn message_consider_borrowing() {
        let (msg, others) = parse_compiler_message(
            include_str!("../../test_data/compiler_message/consider-borrowing.json")
        ).to_messages();
        assert_eq!(
            msg,
            r#"mismatched types

expected &str, found struct `std::string::String`

note: expected type `&str`
         found type `std::string::String`
help: consider borrowing here: `&string`"#,
        );

        assert!(others.is_empty(), "{:?}", others);
    }

    /// ```
    /// fn move_out_of_borrow() {
    ///     match &Some(String::new()) {
    ///         &Some(string) => takes_borrow(&string),
    ///         &None => {},
    ///     }
    /// }
    /// ```
    #[test]
    fn message_move_out_of_borrow() {
        let (msg, others) = parse_compiler_message(
            include_str!("../../test_data/compiler_message/move-out-of-borrow.json")
        ).to_messages();
        assert_eq!(msg, "cannot move out of borrowed content");

        assert_eq!(others, vec!["hint: to prevent move, use `ref string` or `ref mut string`"]);
    }

    /// ```
    /// use std::borrow::Cow;
    /// ```
    #[test]
    fn message_unused_use() {
        let (msg, others) = parse_compiler_message(
            include_str!("../../test_data/compiler_message/unused-use.json")
        ).to_messages();
        assert_eq!(msg, "unused import: `std::borrow::Cow`\n\n\
                         note: #[warn(unused_imports)] on by default");

        assert!(others.is_empty(), "{:?}", others);
    }

    #[test]
    fn message_cannot_find_type() {
        let (msg, others) = parse_compiler_message(
            include_str!("../../test_data/compiler_message/cannot-find-type.json")
        ).to_messages();
        assert_eq!(msg, "cannot find type `HashSet` in this scope\n\n\
                         not found in this scope");

        assert!(others.is_empty(), "{:?}", others);
    }

    /// ```
    /// let _s = 1 / 1;
    /// ```
    #[test]
    fn message_clippy_identity_op() {
        let diag = parse_compiler_message(
            include_str!("../../test_data/compiler_message/clippy-identity-op.json")
        );

        assert_eq!(diag.diagnostic.source, Some("clippy".into()));
        for source in diag.secondaries.iter().map(|d| d.source.as_ref()) {
            assert_eq!(source, Some(&"clippy".into()));
        }

        let (msg, others) = diag.to_messages();
        println!("\n---message---\n{}\n---", msg);

        let link = {
            let link_index = msg.find("https://rust-lang-nursery.github.io/rust-clippy/")
                .expect("no clippy link found in message");
            &msg[link_index..]
        };

        assert_eq!(
            msg,
            "the operation is ineffective. Consider reducing it to `1`\n\n\
             note: #[warn(identity_op)] implied by #[warn(clippy)]\n\
             help: for further information visit ".to_owned() + link
         );

        assert!(others.is_empty(), "{:?}", others);
    }
}
