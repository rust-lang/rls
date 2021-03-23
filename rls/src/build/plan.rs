//! Specified a notion of a build graph, which ultimately can be queried as to
//! what work is required (either a list of `rustc` invocations or a rebuild
//! request) for a given set of dirty files.
//! Currently, there are 2 types of build plans:
//! * Cargo - used when we run Cargo in-process and intercept it
//! * External - dependency graph between invocations

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use cargo_util::ProcessBuilder;
use log::trace;
use serde::{Deserialize, Serialize};

use crate::actions::progress::ProgressUpdate;
use crate::build::cargo_plan::CargoPlan;
use crate::build::external::ExternalPlan;
use crate::build::{BuildResult, Internals, PackageArg};

pub(crate) trait BuildKey {
    type Key: Eq + Hash;
    fn key(&self) -> Self::Key;
}

pub(crate) trait BuildGraph {
    type Unit: BuildKey;

    fn units(&self) -> Vec<&Self::Unit>;
    fn get(&self, key: <Self::Unit as BuildKey>::Key) -> Option<&Self::Unit>;
    fn get_mut(&mut self, key: <Self::Unit as BuildKey>::Key) -> Option<&mut Self::Unit>;
    fn deps(&self, key: <Self::Unit as BuildKey>::Key) -> Vec<&Self::Unit>;

    fn add<T: Into<Self::Unit>>(&mut self, unit: T, deps: Vec<T>);

    fn dirties<T: AsRef<Path>>(&self, files: &[T]) -> Vec<&Self::Unit>;
    /// For a given set of select dirty units, returns a set of all the
    /// dependencies that has to be rebuilt transitively.
    fn dirties_transitive<T: AsRef<Path>>(&self, files: &[T]) -> Vec<&Self::Unit>;
    /// Returns a topological ordering of units with regards to reverse
    /// dependencies.
    /// The output is a stack of units that can be linearly rebuilt, starting
    /// from the last element.
    fn topological_sort(&self, units: Vec<&Self::Unit>) -> Vec<&Self::Unit>;
    fn prepare_work<T: AsRef<Path> + std::fmt::Debug>(&self, files: &[T]) -> WorkStatus;
}

#[derive(Debug)]
pub(crate) enum WorkStatus {
    NeedsCargo(PackageArg),
    Execute(JobQueue),
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum BuildPlan {
    External(ExternalPlan),
    Cargo(CargoPlan),
}

impl BuildPlan {
    pub fn new() -> BuildPlan {
        BuildPlan::Cargo(Default::default())
    }

    pub fn as_cargo_mut(&mut self) -> Option<&mut CargoPlan> {
        match self {
            BuildPlan::Cargo(plan) => Some(plan),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct JobQueue(Vec<ProcessBuilder>);

/// Returns an immediately next argument to the one specified in a given
/// ProcessBuilder (or `None` if the searched or the next argument could not be found).
///
/// This is useful for returning values for arguments of `--key <value>` format.
/// For example, if `[.., "--crate-name", "rls", ...]` arguments are specified,
/// then proc_arg(prc, "--crate-name") returns Some(&OsStr::new("rls"));
fn proc_argument_value<T: AsRef<OsStr>>(prc: &ProcessBuilder, key: T) -> Option<&std::ffi::OsStr> {
    let args = prc.get_args();
    let (idx, _) = args.iter().enumerate().find(|(_, arg)| arg.as_os_str() == key.as_ref())?;

    Some(args.get(idx + 1)?.as_os_str())
}

impl JobQueue {
    pub(crate) fn with_commands(jobs: Vec<ProcessBuilder>) -> JobQueue {
        JobQueue(jobs)
    }

    pub(crate) fn dequeue(&mut self) -> Option<ProcessBuilder> {
        self.0.pop()
    }

    /// Performs a rustc build using cached compiler invocations.
    pub(super) fn execute(
        mut self,
        internals: &Internals,
        progress_sender: Sender<ProgressUpdate>,
    ) -> BuildResult {
        // TODO: In case of an empty job queue we shouldn't be here, since the
        // returned results will replace currently held diagnostics/analyses.
        // Either allow to return a BuildResult::Squashed here or just delegate
        // to Cargo (which we do currently) in `prepare_work`
        assert!(!self.0.is_empty());

        let mut compiler_messages = vec![];
        let mut analyses = vec![];
        let mut input_files = HashMap::<_, HashSet<_>>::new();
        let (build_dir, mut cwd) = {
            let comp_cx = internals.compilation_cx.lock().unwrap();
            (comp_cx.build_dir.clone().expect("no build directory"), comp_cx.cwd.clone())
        };

        // Go through cached compiler invocations sequentially, collecting each
        // invocation's compiler messages for diagnostics and analysis data
        while let Some(job) = self.dequeue() {
            trace!("Executing: {:#?}", job);
            let mut args: Vec<_> = job
                .get_args()
                .iter()
                .cloned()
                .map(|x| x.into_string().expect("cannot stringify job args"))
                .collect();

            let program =
                job.get_program().clone().into_string().expect("cannot stringify job program");
            args.insert(0, program.clone());

            // Needed to parse rustc diagnostics
            if args.iter().find(|x| x.as_str() == "--error-format=json").is_none() {
                args.push("--error-format=json".to_owned());
            }

            if args.iter().find(|x| x.as_str() == "--sysroot").is_none() {
                let sysroot = super::rustc::current_sysroot()
                    .expect("need to specify SYSROOT env var or use rustup or multirust");

                let config = internals.config.lock().unwrap();
                if config.sysroot.is_none() {
                    args.push("--sysroot".to_owned());
                    args.push(sysroot);
                }
            }

            // Send a window/progress notification.
            {
                let crate_name = proc_argument_value(&job, "--crate-name").and_then(OsStr::to_str);
                let update = match crate_name {
                    Some(name) => {
                        let cfg_test = job.get_args().iter().any(|arg| arg == "--test");
                        ProgressUpdate::Message(if cfg_test {
                            format!("{} cfg(test)", name)
                        } else {
                            name.to_owned()
                        })
                    }
                    None => {
                        // divide by zero is avoided by earlier assert!
                        let percentage = compiler_messages.len() as f64 / self.0.len() as f64;
                        ProgressUpdate::Percentage(percentage)
                    }
                };

                progress_sender.send(update).expect("Failed to send progress update");
            }

            match super::rustc::rustc(
                &internals.vfs,
                &args,
                job.get_envs(),
                job.get_cwd().or_else(|| cwd.as_deref()),
                &build_dir,
                Arc::clone(&internals.config),
                &internals.env_lock.as_facade(),
            ) {
                BuildResult::Success(c, mut messages, mut analysis, files, success) => {
                    compiler_messages.append(&mut messages);
                    analyses.append(&mut analysis);
                    for (file, inputs) in files {
                        input_files.entry(file).or_default().extend(inputs);
                    }

                    cwd = Some(c);

                    // This compilation failed, but the build as a whole does not
                    // need to error out.
                    if !success {
                        return BuildResult::Success(
                            cwd.unwrap(),
                            compiler_messages,
                            analyses,
                            input_files,
                            false,
                        );
                    }
                }
                BuildResult::Err(cause, _) => {
                    let cmd = format!("{} {}", program, args.join(" "));
                    return BuildResult::Err(cause, Some(cmd));
                }
                _ => {}
            }
        }

        BuildResult::Success(
            cwd.unwrap_or_else(|| PathBuf::from(".")),
            compiler_messages,
            analyses,
            input_files,
            true,
        )
    }
}

/// Build system-agnostic, basic compilation unit
#[derive(PartialEq, Eq, Hash, Debug, Clone, Deserialize, Serialize)]
pub struct Crate {
    pub name: String,
    pub src_path: Option<PathBuf>,
    pub edition: Edition,
    /// From rustc; mainly used to group other properties used to disambiguate a
    /// given compilation unit.
    pub disambiguator: (u64, u64),
}

// Temporary, until Edition from rustfmt is available
#[derive(PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Copy, Clone, Deserialize, Serialize)]
pub enum Edition {
    Edition2015,
    Edition2018,
    Edition2021,
}

impl Default for Edition {
    fn default() -> Edition {
        Edition::Edition2015
    }
}

impl std::convert::TryFrom<&str> for Edition {
    type Error = &'static str;

    fn try_from(val: &str) -> Result<Self, Self::Error> {
        Ok(match val {
            "2015" => Edition::Edition2015,
            "2018" => Edition::Edition2018,
            "2021" => Edition::Edition2021,
            _ => return Err("unknown"),
        })
    }
}
