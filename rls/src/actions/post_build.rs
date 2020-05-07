//! Post-build processing of data.

#![allow(missing_docs)]

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::panic::RefUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, Thread};

use crate::actions::diagnostics::{parse_diagnostics, Diagnostic, ParsedDiagnostics, Suggestion};
use crate::actions::progress::DiagnosticsNotifier;
use crate::build::{BuildResult, Crate};
use crate::concurrency::JobToken;
use crate::config::CrateBlacklist;
use crate::lsp_data::{PublishDiagnosticsParams, Range};

use itertools::Itertools;
use log::{trace, warn};
use lsp_types::DiagnosticSeverity;
use rls_analysis::AnalysisHost;
use rls_data::Analysis;
use url::Url;

pub type BuildResults = HashMap<PathBuf, Vec<(Diagnostic, Vec<Suggestion>)>>;

pub struct PostBuildHandler {
    pub analysis: Arc<AnalysisHost>,
    pub analysis_queue: Arc<AnalysisQueue>,
    pub previous_build_results: Arc<Mutex<BuildResults>>,
    pub file_to_crates: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
    pub project_path: PathBuf,
    pub show_warnings: bool,
    pub crate_blacklist: CrateBlacklist,
    pub related_information_support: bool,
    pub shown_cargo_error: Arc<AtomicBool>,
    pub active_build_count: Arc<AtomicUsize>,
    pub notifier: Box<dyn DiagnosticsNotifier>,
    pub blocked_threads: Vec<thread::Thread>,
    pub _token: JobToken,
}

impl PostBuildHandler {
    pub fn handle(self, result: BuildResult) {
        match result {
            BuildResult::Success(cwd, messages, new_analysis, input_files, _) => {
                trace!("build - Success");
                self.notifier.notify_begin_diagnostics();

                // Emit appropriate diagnostics using the ones from build.
                self.handle_messages(&cwd, &messages);
                let analysis_queue = Arc::clone(&self.analysis_queue);

                {
                    let mut files_to_crates = self.file_to_crates.lock().unwrap();
                    *files_to_crates = input_files;
                    trace!("Files to crates: {:#?}", files_to_crates.deref());
                }

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
                if self.shown_cargo_error.swap(true, Ordering::SeqCst) {
                    warn!("Not reporting: {}", cause);
                } else {
                    // It's not a good idea to make a long message here, the output in
                    // VSCode is one single line, and it's important to capture the
                    // root cause.
                    self.notifier.notify_error_diagnostics(cause);
                }
                self.notifier.notify_end_diagnostics();
                self.active_build_count.fetch_sub(1, Ordering::SeqCst);
            }
            BuildResult::CargoError { error, stdout, manifest_path, manifest_error_range } => {
                trace!("build - CargoError: {}, stdout: {:?}", error, stdout);
                self.notifier.notify_begin_diagnostics();

                if let Some(manifest) = manifest_path {
                    // if possible generate manifest diagnostics instead of showMessage
                    self.handle_cargo_error(manifest, manifest_error_range, &error, &stdout);
                } else if self.shown_cargo_error.swap(true, Ordering::SeqCst) {
                    warn!("Not reporting: {} {:?}", error, stdout);
                } else {
                    let stdout_msg =
                        if stdout.is_empty() { stdout } else { format!("({})", stdout) };
                    self.notifier.notify_error_diagnostics(format!("{}{}", error, stdout_msg));
                }

                self.notifier.notify_end_diagnostics();
                self.active_build_count.fetch_sub(1, Ordering::SeqCst);
            }
        }
    }

    fn handle_cargo_error(
        &self,
        manifest: PathBuf,
        manifest_error_range: Option<Range>,
        error: &anyhow::Error,
        stdout: &str,
    ) {
        use crate::lsp_data::Position;
        use std::fmt::Write;

        // These notifications will include empty sets of errors for files
        // which had errors, but now don't. This instructs the IDE to clear
        // errors for those files.
        let mut results = self.previous_build_results.lock().unwrap();
        results.values_mut().for_each(Vec::clear);

        // cover whole manifest if we haven't any better idea.
        let range = manifest_error_range
            .unwrap_or_else(|| Range { start: Position::new(0, 0), end: Position::new(9999, 0) });

        let mut message = format!("{}", error);
        for cause in error.chain().skip(1) {
            write!(message, "\n{}", cause).unwrap();
        }
        if !stdout.trim().is_empty() {
            write!(message, "\n{}", stdout).unwrap();
        }

        results.insert(
            manifest,
            vec![(
                Diagnostic {
                    range,
                    message,
                    severity: Some(DiagnosticSeverity::Error),
                    ..Diagnostic::default()
                },
                vec![],
            )],
        );

        self.emit_notifications(&results);
    }

    fn handle_messages(&self, cwd: &Path, messages: &[String]) {
        // These notifications will include empty sets of errors for files
        // which had errors, but now don't. This instructs the IDE to clear
        // errors for those files.
        let mut results = self.previous_build_results.lock().unwrap();
        // We must not clear the hashmap, just the values in each list.
        // This allows us to save allocated before memory.
        for values in &mut results.values_mut() {
            values.clear();
        }

        let file_diagnostics = messages
            .iter()
            .unique()
            .filter_map(|msg| parse_diagnostics(msg, cwd, self.related_information_support))
            .flat_map(|ParsedDiagnostics { diagnostics }| diagnostics);

        for (file_path, diagnostics) in file_diagnostics {
            results.entry(file_path).or_insert_with(Vec::new).extend(diagnostics);
        }

        self.emit_notifications(&results);
    }

    fn reload_analysis_from_disk(&self, cwd: &Path) {
        self.analysis
            .reload_with_blacklist(&self.project_path, cwd, &self.crate_blacklist.0[..])
            .unwrap();
    }

    fn reload_analysis_from_memory(&self, cwd: &Path, analysis: Vec<Analysis>) {
        self.analysis
            .reload_from_analysis(analysis, &self.project_path, cwd, &self.crate_blacklist.0[..])
            .unwrap();
    }

    fn finalize(mut self) {
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
                    .map(|(diag, _)| diag)
                    .filter(|diag| {
                        self.show_warnings || diag.severity != Some(DiagnosticSeverity::Warning)
                    })
                    .cloned()
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
        let queue = Arc::default();
        let worker_thread = thread::spawn({
            let queue = Arc::clone(&queue);
            || AnalysisQueue::run_worker_thread(queue)
        })
        .thread()
        .clone();

        AnalysisQueue { cur_cwd: Mutex::new(None), queue, worker_thread }
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
                queue
                    .drain_filter(|j| match *j {
                        QueuedJob::Job(ref j) if j.hash == Some(hash) => true,
                        _ => false,
                    })
                    .for_each(|j| j.unwrap_job().handler.finalize())
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
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(QueuedJob::Terminate);
        }
    }
}

#[allow(clippy::large_enum_variant)]
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

        Job { handler, analysis, cwd, hash }
    }

    fn process(self) {
        // Reload the analysis data.
        trace!(
            "reload analysis: {:?} {:?} {}",
            self.handler.project_path,
            self.cwd,
            self.analysis.len(),
        );
        if self.analysis.is_empty() {
            trace!("reloading from disk: {:?}", self.cwd);
            self.handler.reload_analysis_from_disk(&self.cwd);
        } else {
            trace!("reloading from memory: {:?}", self.cwd);
            self.handler.reload_analysis_from_memory(&self.cwd, self.analysis);
        }

        self.handler.finalize();
    }
}
