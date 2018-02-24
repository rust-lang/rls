// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Actions that the RLS can perform: responding to requests, watching files,
//! etc.

use analysis::AnalysisHost;
use vfs::Vfs;
#[cfg(feature = "rustfmt")]
use config::FmtConfig;
use config::Config;
use serde_json;
use url::Url;
use span;
use Span;

use actions::post_build::{BuildResults, PostBuildHandler};
use actions::progress::{BuildProgressNotifier, BuildDiagnosticsNotifier};
use build::*;
use lsp_data;
use lsp_data::*;
use server::Output;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::thread;


// TODO: Support non-`file` URI schemes in VFS. We're currently ignoring them because
// we don't want to crash the RLS in case a client opens a file under different URI scheme
// like with git:/ or perforce:/ (Probably even http:/? We currently don't support remote schemes).
macro_rules! ignore_non_file_uri {
    ($expr: expr, $uri: expr, $log_name: expr) => {
        $expr.map_err(|_| {
            trace!("{}: Non-`file` URI scheme, ignoring: {:?}", $log_name, $uri);
            ()
        })
    };
}

macro_rules! parse_file_path {
    ($uri: expr, $log_name: expr) => {
        ignore_non_file_uri!(parse_file_path($uri), $uri, $log_name)
    }
}

pub mod work_pool;
pub mod post_build;
pub mod requests;
pub mod notifications;
pub mod progress;

/// Persistent context shared across all requests and notifications.
pub enum ActionContext {
    /// Context after server initialization.
    Init(InitActionContext),
    /// Context before initialization.
    Uninit(UninitActionContext),
}

impl ActionContext {
    /// Construct a new, uninitialized context.
    pub fn new(
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>,
    ) -> ActionContext {
        ActionContext::Uninit(UninitActionContext::new(analysis, vfs, config))
    }

    /// Initialize this context. Panics if it has already been initialized.
    pub fn init<O: Output>(
        &mut self,
        current_project: PathBuf,
        init_options: &InitializationOptions,
        client_capabilities: lsp_data::ClientCapabilities,
        out: &O,
    ) {
        let ctx = match *self {
            ActionContext::Uninit(ref uninit) => {
                let ctx = InitActionContext::new(
                    uninit.analysis.clone(),
                    uninit.vfs.clone(),
                    uninit.config.clone(),
                    client_capabilities,
                    current_project,
                );
                ctx.init(init_options, out);
                ctx
            }
            ActionContext::Init(_) => panic!("ActionContext already initialized"),
        };
        *self = ActionContext::Init(ctx);
    }

    /// Returns an initialiased wrapped context, or panics if not initialised.
    pub fn inited(&self) -> InitActionContext {
        match *self {
            ActionContext::Uninit(_) => panic!("ActionContext not initialized"),
            ActionContext::Init(ref ctx) => ctx.clone(),
        }
    }
}

/// Persistent context shared across all requests and actions after the RLS has
/// been initialized.
#[derive(Clone)]
pub struct InitActionContext {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,

    current_project: PathBuf,

    previous_build_results: Arc<Mutex<BuildResults>>,
    build_queue: BuildQueue,
    // Keep a record of builds/post-build tasks currently in flight so that
    // mutating actions can block until the data is ready.
    active_build_count: Arc<AtomicUsize>,
    // Whether we've shown an error message from Cargo since the last successful
    // build.
    shown_cargo_error: Arc<AtomicBool>,
    // Set to true when a potentially mutating request is received. Set to false
    // if a change arrives. We can thus tell if the RLS has been quiescent while
    // waiting to mutate the client state.
    pub quiescent: Arc<AtomicBool>,

    prev_changes: Arc<Mutex<HashMap<PathBuf, u64>>>,

    config: Arc<Mutex<Config>>,
    client_capabilities: Arc<lsp_data::ClientCapabilities>,
    /// Whether the server is performing cleanup (after having received
    /// 'shutdown' request), just before final 'exit' request.
    pub shut_down: Arc<AtomicBool>,
}

/// Persistent context shared across all requests and actions before the RLS has
/// been initialized.
pub struct UninitActionContext {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    config: Arc<Mutex<Config>>,
}

impl UninitActionContext {
    fn new(
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>,
    ) -> UninitActionContext {
        UninitActionContext {
            analysis,
            vfs,
            config,
        }
    }
}

impl InitActionContext {
    fn new(
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>,
        client_capabilities: lsp_data::ClientCapabilities,
        current_project: PathBuf,
    ) -> InitActionContext {
        let build_queue = BuildQueue::new(vfs.clone(), config.clone());
        InitActionContext {
            analysis,
            vfs,
            config,
            current_project,
            previous_build_results: Arc::new(Mutex::new(HashMap::new())),
            build_queue,
            active_build_count: Arc::new(AtomicUsize::new(0)),
            shown_cargo_error: Arc::new(AtomicBool::new(false)),
            quiescent: Arc::new(AtomicBool::new(false)),
            prev_changes: Arc::new(Mutex::new(HashMap::new())),
            client_capabilities: Arc::new(client_capabilities),
            shut_down: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(feature = "rustfmt")]
    fn fmt_config(&self) -> FmtConfig {
        FmtConfig::from(&self.current_project)
    }

    fn init<O: Output>(&self, init_options: &InitializationOptions, out: &O) {
        let current_project = self.current_project.clone();
        let config = self.config.clone();
        // Spawn another thread since we're shelling out to Cargo and this can
        // cause a non-trivial amount of time due to disk access
        thread::spawn(move || {
            let mut config = config.lock().unwrap();
            if let Err(e) = config.infer_defaults(&current_project) {
                debug!(
                    "Encountered an error while trying to infer config defaults: {:?}",
                    e
                );
            }
        });

        if !init_options.omit_init_build {
            self.build_current_project(BuildPriority::Cargo, out);
        }
    }

    fn build<O: Output>(&self, project_path: &Path, priority: BuildPriority, out: &O) {

        let pbh = {
            let config = self.config.lock().unwrap();
            PostBuildHandler {
                analysis: self.analysis.clone(),
                previous_build_results: self.previous_build_results.clone(),
                project_path: project_path.to_owned(),
                show_warnings: config.show_warnings,
                shown_cargo_error: self.shown_cargo_error.clone(),
                active_build_count: self.active_build_count.clone(),
                use_black_list: config.use_crate_blacklist,
                notifier: Box::new(BuildDiagnosticsNotifier::new(out.clone())),
                blocked_threads: vec![],
            }
        };

        let notifier = Box::new(BuildProgressNotifier::new(out.clone()));

        self.active_build_count.fetch_add(1, Ordering::SeqCst);
        self.build_queue
            .request_build(project_path, priority, notifier, pbh);
    }

    fn build_current_project<O: Output>(&self, priority: BuildPriority, out: &O) {
        self.build(&self.current_project, priority, out);
    }

    /// Block until any builds and analysis tasks are complete.
    fn block_on_build(&self) {
        self.build_queue.block_on_build();
    }

    /// Returns true if there are no builds pending or in progress.
    fn build_ready(&self) -> bool {
        self.build_queue.build_ready()
    }

    /// Returns true if there are no builds or post-build (analysis) tasks pending
    /// or in progress.
    fn analysis_ready(&self) -> bool {
        self.active_build_count.load(Ordering::SeqCst) == 0
    }

    fn check_change_version(&self, file_path: &Path, version_num: u64) {
        let mut prev_changes = self.prev_changes.lock().unwrap();
        let file_path = file_path.to_owned();

        if prev_changes.contains_key(&file_path) {
            let prev_version = prev_changes[&file_path];
            assert!(version_num > prev_version, "Out of order or duplicate change");
        }

        prev_changes.insert(file_path, version_num);
    }

    fn convert_pos_to_span(&self, file_path: PathBuf, pos: Position) -> Span {
        trace!("convert_pos_to_span: {:?} {:?}", file_path, pos);

        let pos = ls_util::position_to_rls(pos);
        let line = self.vfs.load_line(&file_path, pos.row).unwrap();
        trace!("line: `{}`", line);

        let (start, end) = find_word_at_pos(&line, &pos.col);
        trace!("start: {}, end: {}", start.0, end.0);

        Span::from_positions(
            span::Position::new(pos.row, start),
            span::Position::new(pos.row, end),
            file_path,
        )
    }
}

/// Represents a text cursor between characters, pointing at the next character
/// in the buffer.
type Column = span::Column<span::ZeroIndexed>;

/// Returns a text cursor range for a found word inside `line` at which `pos`
/// text cursor points to. Resulting type represents a (`start`, `end`) range
/// between `start` and `end` cursors.
/// For example (4, 4) means an empty selection starting after first 4 characters.
fn find_word_at_pos(line: &str, pos: &Column) -> (Column, Column) {
    let col = pos.0 as usize;
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';

    let start = line.chars()
        .enumerate()
        .take(col)
        .filter(|&(_, c)| !is_ident_char(c))
        .last()
        .map(|(i, _)| i + 1)
        .unwrap_or(0) as u32;

    let end = line.chars()
        .enumerate()
        .skip(col)
        .filter(|&(_, c)| !is_ident_char(c))
        .nth(0)
        .map(|(i, _)| i)
        .unwrap_or(col) as u32;

    (
        span::Column::new_zero_indexed(start),
        span::Column::new_zero_indexed(end),
    )
}

// TODO include workspace Cargo.tomls in watchers / relevant
/// Client file-watching request / filtering logic
/// We want to watch workspace 'Cargo.toml', root 'Cargo.lock' & the root 'target' dir
pub struct FileWatch<'ctx> {
    project_str: &'ctx str,
    project_uri: String,
}

impl<'ctx> FileWatch<'ctx> {
    /// Construct a new `FileWatch`.
    pub fn new(ctx: &'ctx InitActionContext) -> Self {
        Self {
            project_str: ctx.current_project.to_str().unwrap(),
            project_uri: Url::from_file_path(&ctx.current_project)
                .unwrap()
                .into_string(),
        }
    }

    /// Returns json config for desired file watches
    pub fn watchers_config(&self) -> serde_json::Value {
        let pattern = format!("{}/Cargo{{.toml,.lock}}", self.project_str);
        let target_pattern = format!("{}/target", self.project_str);
        // For target, we only watch if it gets deleted.
        json!({
            "watchers": [{ "globPattern": pattern }, { "globPattern": target_pattern, "kind": 4 }]
        })
    }

    /// Returns if a file change is relevant to the files we actually wanted to watch
    // Implementation note: This is expected to be called a large number of times in a loop
    // so should be fast / avoid allocation.
    #[inline]
    pub fn is_relevant(&self, change: &FileEvent) -> bool {
        let path = change.uri.as_str();

        if !path.starts_with(&self.project_uri) {
            return false;
        }

        let local = &path[self.project_uri.len()..];

        local == "/Cargo.lock" || local == "/Cargo.toml"
            || local == "/target" && change.typ == FileChangeType::Deleted
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_find_word_at_pos() {
        fn assert_range(test_str: &'static str, range: (u32, u32)) {
            assert!(test_str.chars().filter(|c| *c == '|').count() == 1);
            let col = test_str.find('|').unwrap() as u32;
            let line = test_str.replace('|', "");
            let (start, end) = find_word_at_pos(&line, &Column::new_zero_indexed(col));
            assert_eq!(
                range,
                (start.0, end.0),
                "Assertion failed for {:?}",
                test_str
            );
        }

        assert_range("|struct Def {", (0, 6));
        assert_range("stru|ct Def {", (0, 6));
        assert_range("struct| Def {", (0, 6));

        assert_range("struct |Def {", (7, 10));
        assert_range("struct De|f {", (7, 10));
        assert_range("struct Def| {", (7, 10));

        assert_range("struct Def |{", (11, 11));

        assert_range("|span::Position<T>", (0, 4));
        assert_range(" |span::Position<T>", (1, 5));
        assert_range("sp|an::Position<T>", (0, 4));
        assert_range("span|::Position<T>", (0, 4));
        assert_range("span::|Position<T>", (6, 14));
        assert_range("span::Position|<T>", (6, 14));
        assert_range("span::Position<|T>", (15, 16));
        assert_range("span::Position<T|>", (15, 16));
    }
}
