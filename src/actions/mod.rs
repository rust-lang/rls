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
use config::{Config, FmtConfig};
use serde_json;
use url::Url;
use span;
use Span;

use actions::post_build::{BuildResults, PostBuildHandler};
use actions::notifications::BeginBuild;
use build::*;
use lsp_data::*;
use server::{Output, Notification, NoParams};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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

mod post_build;
pub mod requests;
pub mod notifications;

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
        out: O,
    ) {
        let ctx = match *self {
            ActionContext::Uninit(ref uninit) => {
                let ctx = InitActionContext::new(
                    uninit.analysis.clone(),
                    uninit.vfs.clone(),
                    uninit.config.clone(),
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

    config: Arc<Mutex<Config>>,
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
        }
    }

    fn fmt_config(&self) -> FmtConfig {
        FmtConfig::from(&self.current_project)
    }

    fn init<O: Output>(&self, init_options: &InitializationOptions, out: O) {
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

    fn build<O: Output>(&self, project_path: &Path, priority: BuildPriority, out: O) {
        let pbh = {
            let config = self.config.lock().unwrap();
            PostBuildHandler {
                analysis: self.analysis.clone(),
                previous_build_results: self.previous_build_results.clone(),
                project_path: project_path.to_owned(),
                out: out.clone(),
                show_warnings: config.show_warnings,
                use_black_list: config.use_crate_blacklist,
            }
        };

        out.notify(Notification::<BeginBuild>::new(NoParams {}));
        self.build_queue
            .request_build(project_path, priority, move |result| pbh.handle(result));
    }

    fn build_current_project<O: Output>(&self, priority: BuildPriority, out: O) {
        self.build(&self.current_project, priority, out);
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
