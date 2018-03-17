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
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::RefUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, Thread};

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
    pub analysis_queue: Arc<AnalysisQueue>,
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
    pub fn handle(self, result: BuildResult) {
        match result {
            BuildResult::Success(cwd, messages, new_analysis, _) => {
                trace!("build - Success");
                self.notifier.notify_begin_diagnostics();

                // Emit appropriate diagnostics using the ones from build.
                self.handle_messages(&cwd, &messages);
                let analysis_queue = self.analysis_queue.clone();

                let job = Job::new(self, new_analysis, cwd);
                analysis_queue.enqueue(job);
            }
            BuildResult::Squashed => {
                trace!("build - Squashed");
                self.active_build_count.fetch_sub(1, Ordering::SeqCst);
            }
            BuildResult::Err(cause, cmd) => {
                trace!("build - Error {} when running {:?}", cause, cmd);
                self.notifier.notify_begin_diagnostics();
                if !self.shown_cargo_error.swap(true, Ordering::SeqCst) {
                    // It's not a good idea to make a long message here, the output in
                    // VSCode is one single line, and it's important to capture the
                    // root cause.
                    self.notifier.notify_error_diagnostics(cause);
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
                main: (diagnostic, suggestions),
                secondaries,
            }) = parse_diagnostics(msg, group as u64)
            {
                let entry = results.entry(cwd.join(file_path)).or_insert_with(Vec::new);

                entry.push((diagnostic, suggestions));
                for (secondary, suggestions) in secondaries {
                    entry.push((secondary, suggestions));
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
                .reload_from_analysis(
                    analysis,
                    &self.project_path,
                    cwd,
                    &::blacklist::CRATE_BLACKLIST,
                )
                .unwrap();
        } else {
            self.analysis
                .reload_from_analysis(analysis, &self.project_path, cwd, &[])
                .unwrap();
        }
    }

    fn finalise(mut self) {
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

// Queue up analysis tasks and execute them on the same thread (this is slower
// than executing in parallel, but allows us to skip indexing tasks).
pub struct AnalysisQueue {
    // The cwd of the previous build.
    // !!! Do not take this lock without holding the lock on `queue` !!!
    cur_cwd: Mutex<Option<PathBuf>>,
    queue: Arc<Mutex<Vec<QueuedJob>>>,
    // Handle to the worker thread where we handle analysis tasks.
    worker_thread: Thread,
}

impl AnalysisQueue {
    // Create a new queue and start the worker thread.
    pub fn init() -> AnalysisQueue {
        let queue = Arc::new(Mutex::new(Vec::new()));
        let queue_clone = queue.clone();
        let worker_thread = thread::spawn(move || AnalysisQueue::run_worker_thread(queue_clone))
            .thread()
            .clone();

        AnalysisQueue {
            cur_cwd: Mutex::new(None),
            queue,
            worker_thread,
        }
    }

    fn enqueue(&self, job: Job) {
        trace!("enqueue job");

        {
            let mut queue = self.queue.lock().unwrap();
            let mut cur_cwd = self.cur_cwd.lock().unwrap();
            *cur_cwd = Some(job.cwd.clone());

            // Remove any analysis jobs which this job obsoletes.
            trace!("Pre-prune queue len: {}", queue.len());
            if let Some(hash) = job.hash {
                queue.drain_filter(|j| match *j {
                    QueuedJob::Job(ref j) if j.hash == Some(hash) => true,
                    _ => false,
                }).for_each(|j| j.unwrap_job().handler.finalise())
            }
            trace!("Post-prune queue len: {}", queue.len());

            queue.push(QueuedJob::Job(job));
        }

        self.worker_thread.unpark();
    }

    fn run_worker_thread(queue: Arc<Mutex<Vec<QueuedJob>>>) {
        loop {
            let job = {
                let mut queue = queue.lock().unwrap();
                if queue.is_empty() {
                    None
                } else {
                    Some(queue.remove(0))
                }
            };
            match job {
                Some(QueuedJob::Terminate) => return,
                Some(QueuedJob::Job(job)) => job.process(),
                None => thread::park(),
            }
        }
    }
}

impl RefUnwindSafe for AnalysisQueue {}

impl Drop for AnalysisQueue {
    fn drop(&mut self) {
        let _ = self.queue.lock().map(|mut q| q.push(QueuedJob::Terminate));
    }
}

enum QueuedJob {
    Job(Job),
    Terminate,
}

impl QueuedJob {
    fn unwrap_job(self) -> Job {
        match self {
            QueuedJob::Job(job) => job,
            QueuedJob::Terminate => panic!("Expected Job"),
        }
    }
}

// An analysis task to be queued and executed by `AnalysisQueue`.
struct Job {
    handler: PostBuildHandler,
    analysis: Vec<Analysis>,
    cwd: PathBuf,
    hash: Option<u64>,
}

impl Job {
    fn new(handler: PostBuildHandler, analysis: Vec<Analysis>, cwd: PathBuf) -> Job {
        // We make a hash from all the crate paths in analysis.
        let hash = analysis
            .iter()
            .map(|a| a.prelude.as_ref().map(|p| &*p.crate_root))
            .collect::<Option<Vec<&str>>>()
            .map(|v| {
                let mut hasher = DefaultHasher::new();
                Hash::hash_slice(&v, &mut hasher);
                hasher.finish()
            });

        Job {
            handler,
            analysis,
            cwd,
            hash,
        }
    }

    fn process(self) {
        // Reload the analysis data.
        trace!(
            "reload analysis: {:?} {:?}",
            self.handler.project_path,
            self.cwd
        );
        if self.analysis.is_empty() {
            self.handler.reload_analysis_from_disk(&self.cwd);
        } else {
            self.handler
                .reload_analysis_from_memory(&self.cwd, self.analysis);
        }

        self.handler.finalise();
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
    main: (Diagnostic, Vec<Suggestion>),
    secondaries: Vec<(Diagnostic, Vec<Suggestion>)>,
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
    let (first_primary_span_index, first_primary_span) = message
        .spans
        .iter()
        .enumerate()
        .find(|s| s.1.is_primary)
        .unwrap();
    let rls_span = first_primary_span.rls_span().zero_indexed();
    let suggestions = make_suggestions(&message, &rls_span.file);

    let mut source = "rustc";
    let diagnostic = {
        let mut primary_message = diagnostic_msg.clone();
        if let Some(ref primary_label) = first_primary_span.label {
            if primary_label.trim() != primary_message.trim() {
                primary_message.push_str(&format!("\n\n{}", primary_label));
            }
        }

        if let Some(notes) = format_notes(&message.children, first_primary_span) {
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
            },
        }
    };

    // For a compiler error that has secondary spans (e.g. borrow error showing
    // both borrow and error spans) we emit additional diagnostics. These don't
    // include notes and are of an `Information` severity.
    let secondaries = message
        .spans
        .iter()
        .enumerate()
        .filter(|x| x.0 != first_primary_span_index)
        .map(|(_, secondary_span)| {
            let mut secondary_message = if secondary_span.is_within(first_primary_span) {
                String::new()
            } else {
                diagnostic_msg.clone()
            };

            let mut suggestion = secondary_span
                .suggested_replacement
                .as_ref()
                .map(|s| span_suggestion(secondary_span, s));

            if let Some(ref secondary_label) = secondary_span.label {
                let label_suggestion = label_suggestion(secondary_span, secondary_label);
                if suggestion.is_none() && label_suggestion.is_some() {
                    suggestion = label_suggestion;
                } else {
                    secondary_message.push_str(&format!("\n\n{}", secondary_label));
                }
            }
            let severity = Some(if secondary_span.is_primary {
                severity(&message.level)
            } else {
                DiagnosticSeverity::Information
            });
            let rls_span = secondary_span.rls_span().zero_indexed();

            let diag = Diagnostic {
                range: ls_util::rls_to_range(rls_span.range),
                severity,
                code: Some(NumberOrString::String(match message.code {
                    Some(ref c) => c.code.clone(),
                    None => String::new(),
                })),
                source: Some(source.to_owned()),
                message: secondary_message.trim().to_owned(),
                group: Some(group),
            };
            (diag, suggestion.map(|s| vec![s]).unwrap_or_default())
        })
        .collect();

    Some(FileDiagnostic {
        file_path: rls_span.file,
        main: (diagnostic, suggestions),
        secondaries,
    })
}

fn format_notes(children: &[CompilerMessage], primary: &DiagnosticSpan) -> Option<String> {
    if !children.is_empty() {
        let mut notes = String::new();
        for &CompilerMessage {
            ref message,
            ref level,
            ref spans,
            ..
        } in children
        {
            macro_rules! add_message_to_notes {
                ($msg: expr) => {{
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
                }};
            }

            if spans.is_empty() {
                add_message_to_notes!(message);
            } else if spans.len() == 1 && spans[0].is_within(primary) {
                add_message_to_notes!(message);
                if let Some(ref suggested) = spans[0].suggested_replacement {
                    notes.push_str(&format!(": `{}`", suggested));
                }
            }
        }

        if notes.is_empty() {
            None
        } else {
            Some(notes.trim().to_string())
        }
    } else {
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

fn make_suggestions(message: &CompilerMessage, file: &Path) -> Vec<Suggestion> {
    let mut suggestions = vec![];
    for sp in message.children.iter().flat_map(|msg| &msg.spans) {
        let span = sp.rls_span().zero_indexed();
        if span.file == file {
            if let Some(ref s) = sp.suggested_replacement {
                let range = ls_util::rls_to_range(span.range);
                let action = if range.start == range.end {
                    "Add"
                } else {
                    "Change to"
                };
                let label = if message
                    .spans
                    .iter()
                    .filter(|s| s.is_primary)
                    .map(|s| s.rls_span().zero_indexed())
                    .any(|s| s.range.row_start == span.range.row_start)
                {
                    // on the same line as diagnostic
                    format!("{} `{}`", action, s)
                } else {
                    format!("Line {}: {} `{}`", range.start.line + 1, action, s)
                };
                let suggestion = Suggestion {
                    new_text: s.clone(),
                    range,
                    label,
                };
                suggestions.push(suggestion);
            }
        }
    }
    suggestions
}

fn span_suggestion(span: &DiagnosticSpan, suggested: &str) -> Suggestion {
    let zspan = span.rls_span().zero_indexed();
    let range = ls_util::rls_to_range(zspan.range);
    let action = if range.start == range.end {
        "Add"
    } else {
        "Change to"
    };
    let label = format!("{} `{}`", action, suggested);
    Suggestion {
        new_text: suggested.to_string(),
        range,
        label,
    }
}

fn label_suggestion(span: &DiagnosticSpan, label: &str) -> Option<Suggestion> {
    let suggest_label = "consider changing this to `";
    if label.starts_with(suggest_label) && label.ends_with('`') {
        let suggested_replacement = &label[suggest_label.len()..label.len() - 1];
        return Some(span_suggestion(span, suggested_replacement));
    }
    None
}

trait IsWithin {
    /// Returns whether `other` is considered within `self`
    /// note: a thing should be 'within' itself
    fn is_within(&self, other: &Self) -> bool;
}
impl<T: PartialOrd<T>> IsWithin for ::std::ops::Range<T> {
    fn is_within(&self, other: &Self) -> bool {
        self.start >= other.start && self.start <= other.end && self.end <= other.end
            && self.end >= other.start
    }
}
impl IsWithin for DiagnosticSpan {
    fn is_within(&self, other: &Self) -> bool {
        let DiagnosticSpan {
            line_start,
            line_end,
            column_start,
            column_end,
            ..
        } = *self;
        (line_start..line_end + 1).is_within(&(other.line_start..other.line_end + 1))
            && (column_start..column_end + 1).is_within(&(other.column_start..other.column_end + 1))
    }
}

/// Tests for formatted messages from the compilers json output
/// run cargo with `--message-format=json` to generate the json for new tests and add .json
/// message files to '../../test_data/compiler_message/'
#[cfg(test)]
mod diagnostic_message_test {
    use super::*;

    pub(super) fn parse_compiler_message(compiler_message: &str) -> FileDiagnostic {
        let _ = ::env_logger::try_init();
        parse_diagnostics(compiler_message, 0).expect("failed to parse compiler message")
    }

    pub(super) trait FileDiagnosticTestExt {
        /// Returns (primary message, secondary messages)
        fn to_messages(&self) -> (String, Vec<String>);

        /// Returns all primary & secondary suggestions
        fn all_suggestions(&self) -> Vec<&Suggestion>;
    }

    impl FileDiagnosticTestExt for FileDiagnostic {
        fn to_messages(&self) -> (String, Vec<String>) {
            (
                self.main.0.message.clone(),
                self.secondaries
                    .iter()
                    .map(|d| d.0.message.clone())
                    .collect(),
            )
        }

        fn all_suggestions(&self) -> Vec<&Suggestion> {
            self.main
                .1
                .iter()
                .chain(self.secondaries.iter().flat_map(|s| &s.1))
                .collect()
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
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/use-after-move.json"
        ));

        assert_eq!(diag.main.0.source, Some("rustc".into()));
        for source in diag.secondaries.iter().map(|d| d.0.source.as_ref()) {
            assert_eq!(source, Some(&"rustc".into()));
        }

        let (msg, others) = diag.to_messages();
        assert_eq!(
            msg,
            "use of moved value: `s`\n\n\
            value used here after move\n\n\
            note: move occurs because `s` has type `std::string::String`, which does not implement the `Copy` trait"
        );

        assert_eq!(
            others,
            vec![
                "use of moved value: `s`\n\n\
                 value moved here",
            ]
        );
    }

    /// ```
    /// fn type_annotations_needed() {
    ///     let v = Vec::new();
    /// }
    /// ```
    #[test]
    fn message_type_annotations_needed() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/type-annotations-needed.json"
        )).to_messages();
        assert_eq!(
            msg,
            "type annotations needed\n\n\
             cannot infer type for `T`",
        );

        assert_eq!(
            others,
            vec![
                "type annotations needed\n\n\
                 consider giving `v` a type",
            ]
        );
    }

    /// ```
    /// fn mismatched_types() -> usize {
    ///     123_i32
    /// }
    /// ```
    #[test]
    fn message_mismatched_types() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/mismatched-types.json"
        )).to_messages();
        assert_eq!(
            msg,
            "mismatched types\n\n\
             expected usize, found i32",
        );

        assert_eq!(
            others,
            vec![
                "mismatched types\n\n\
                 expected `usize` because of return type",
            ]
        );
    }

    /// ```
    /// fn not_mut() {
    ///     let string = String::new();
    ///     let _s1 = &mut string;
    /// }
    /// ```
    #[test]
    fn message_not_mutable() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/not-mut.json"
        )).to_messages();
        assert_eq!(
            msg,
            "cannot borrow immutable local variable `string` as mutable\n\n\
             cannot borrow mutably",
        );

        // note: consider message becomes a suggetion
        assert_eq!(
            others,
            vec!["cannot borrow immutable local variable `string` as mutable"]
        );
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
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/consider-borrowing.json"
        )).to_messages();
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
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/move-out-of-borrow.json"
        )).to_messages();
        assert_eq!(msg, "cannot move out of borrowed content");

        assert_eq!(
            others,
            vec![
                "hint: to prevent move, use `ref string` or `ref mut string`",
            ]
        );
    }

    /// ```
    /// use std::{f64, u64, u8 as Foo};
    /// ```
    #[test]
    fn message_unused_use() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/unused-use.json"
        )).to_messages();
        assert_eq!(
            msg,
            "unused imports: `f64`, `u64`, `u8 as Foo`\n\n\
             note: #[warn(unused_imports)] on by default"
        );

        // 2 more warnings for the other two imports
        assert_eq!(
            others,
            vec![
                "unused imports: `f64`, `u64`, `u8 as Foo`",
                "unused imports: `f64`, `u64`, `u8 as Foo`",
            ]
        );
    }

    #[test]
    fn message_cannot_find_type() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/cannot-find-type.json"
        )).to_messages();
        assert_eq!(
            msg,
            "cannot find type `HashSet` in this scope\n\n\
             not found in this scope"
        );

        assert!(others.is_empty(), "{:?}", others);
    }

    /// ```
    /// let _s = 1 / 1;
    /// ```
    #[test]
    fn message_clippy_identity_op() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/clippy-identity-op.json"
        ));

        assert_eq!(diag.main.0.source, Some("clippy".into()));
        for source in diag.secondaries.iter().map(|d| d.0.source.as_ref()) {
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
             help: for further information visit "
                .to_owned() + link
        );

        assert!(others.is_empty(), "{:?}", others);
    }
}

/// Tests for creating suggestions from the compilers json output
#[cfg(test)]
mod diagnostic_suggestion_test {
    use super::*;
    use self::diagnostic_message_test::*;
    use ls_types;

    #[test]
    fn suggest_use_when_cannot_find_type() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/cannot-find-type.json"
        ));
        let suggestions = diag.all_suggestions();

        eprintln!("{:#?}", suggestions);

        let use_hash_set = suggestions
            .iter()
            .find(|s| s.new_text == "use std::collections::HashSet;\n")
            .expect("`use std::collections::HashSet` not found");

        assert_eq!(
            use_hash_set.label,
            "Line 15: Add `use std::collections::HashSet;\n`"
        );

        let expected_position = ls_types::Position {
            line: 14,
            character: 0,
        };
        assert_eq!(
            use_hash_set.range,
            Range {
                start: expected_position,
                end: expected_position,
            }
        );
    }

    #[test]
    fn suggest_mut_when_not_mut() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/not-mut.json"
        ));
        let suggestions = diag.all_suggestions();

        eprintln!("{:#?}", suggestions);

        let change_to_mut = suggestions
            .iter()
            .find(|s| s.new_text == "mut string")
            .expect("`mut string` not found");

        assert_eq!(change_to_mut.label, "Change to `mut string`");

        assert_eq!(
            change_to_mut.range,
            Range {
                start: ls_types::Position {
                    line: 132,
                    character: 12,
                },
                end: ls_types::Position {
                    line: 132,
                    character: 18,
                },
            }
        );
    }

    /// ```
    /// pub const WINDOW_PROGRESS: &'static str = "window/progress";
    /// ```
    #[test]
    fn suggest_clippy_const_static() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/clippy-const-static-lifetime.json"
        ));
        let suggestions = diag.all_suggestions();

        eprintln!("{:#?}", suggestions);

        let change_to_mut = suggestions
            .iter()
            .find(|s| s.new_text == "&str")
            .expect("`&str` not found");

        assert_eq!(change_to_mut.label, "Change to `&str`");

        assert_eq!(
            change_to_mut.range,
            Range {
                start: ls_types::Position {
                    line: 354,
                    character: 34,
                },
                end: ls_types::Position {
                    line: 354,
                    character: 46,
                },
            }
        );
    }
}
