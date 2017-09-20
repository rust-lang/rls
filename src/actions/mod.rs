// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use analysis::AnalysisHost;
use vfs::Vfs;
use config::{Config, FmtConfig};
use span;
use Span;

use actions::post_build::{BuildResults, PostBuildHandler};
use build::*;
use lsp_data::*;
use server::Output;

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


pub enum ActionContext {
    Init(InitActionContext),
    Uninit(UninitActionContext),
}

impl ActionContext {
    pub fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               config: Arc<Mutex<Config>>) -> ActionContext {
        ActionContext::Uninit(UninitActionContext::new(analysis, vfs, config))
    }

    pub fn init<O: Output>(&mut self, current_project: PathBuf, init_options: &InitializationOptions, out: O) {
        let ctx = match *self {
            ActionContext::Uninit(ref uninit) => {
                let ctx = InitActionContext::new(uninit.analysis.clone(), uninit.vfs.clone(), uninit.config.clone(), current_project);
                ctx.init(init_options, out);
                ctx
            }
            ActionContext::Init(_) => panic!("ActionContext already initialized"),
        };
        *self = ActionContext::Init(ctx);
    }

    fn inited(&self) -> &InitActionContext {
        match *self {
            ActionContext::Uninit(_) => panic!("ActionContext not initialized"),
            ActionContext::Init(ref ctx) => ctx,
        }
    }
}

pub struct InitActionContext {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,

    current_project: PathBuf,

    previous_build_results: Arc<Mutex<BuildResults>>,
    build_queue: BuildQueue,

    config: Arc<Mutex<Config>>,
    fmt_config: FmtConfig,
}

pub struct UninitActionContext {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    config: Arc<Mutex<Config>>,
}

impl UninitActionContext {
    fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               config: Arc<Mutex<Config>>) -> UninitActionContext {
        UninitActionContext {
            analysis,
            vfs,
            config,
        }
    }

}

impl InitActionContext {
    fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               config: Arc<Mutex<Config>>,
               current_project: PathBuf) -> InitActionContext {
        let build_queue = BuildQueue::new(vfs.clone(), config.clone());
        let fmt_config = FmtConfig::from(&current_project);
        InitActionContext {
            analysis,
            vfs,
            config,
            current_project,
            previous_build_results: Arc::new(Mutex::new(HashMap::new())),
            build_queue,
            fmt_config,
        }
    }

    fn init<O: Output>(&self, init_options: &InitializationOptions, out: O) {
        let current_project = self.current_project.clone();
        let config = self.config.clone();
        // Spawn another thread since we're shelling out to Cargo and this can
        // cause a non-trivial amount of time due to disk access
        thread::spawn(move || {
            let mut config = config.lock().unwrap();
            if let Err(e)  = config.infer_defaults(&current_project) {
                debug!("Encountered an error while trying to infer config defaults: {:?}", e);
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

        out.notify(NotificationMessage::new(
            NOTIFICATION_BUILD_BEGIN,
            None,
        ));
        self.build_queue.request_build(project_path, priority, move |result| {
            pbh.handle(result)
        });
    }

    fn build_current_project<O: Output>(&self, priority: BuildPriority, out: O) {
        self.build(&self.current_project, priority, out);
    }

    fn convert_pos_to_span(&self, file_path: PathBuf, pos: Position) -> Span {
        trace!("convert_pos_to_span: {:?} {:?}", file_path, pos);

        let pos = ls_util::position_to_rls(pos);
        let line = self.vfs.load_line(&file_path, pos.row).unwrap();
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

        Span::from_positions(start_pos, end_pos, file_path)
    }
}
