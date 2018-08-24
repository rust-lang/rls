// Copyright 2018 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Contains data and logic for executing builds specified externally (rather
//! than executing and intercepting Cargo calls).
//!
//! Provides deserialization structs for the build plan format as it is output
//! by `cargo build --build-plan` and means to execute that plan as part of the
//! RLS build to retrieve diagnostics and analysis data.
//!
//! Additionally, we allow to build the analysis data with an external command,
//! which should return a list of save-analysis JSON files to be reloaded by RLS.
//! From these we construct an internal build plan that is used to rebuild
//! the project incrementally ourselves.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::build::BuildResult;
use crate::build::plan::{BuildKey, BuildGraph, JobQueue, WorkStatus};

use cargo::util::{process, ProcessBuilder};
use log::trace;
use rls_data::{Analysis, CompilationOptions};
use serde_derive::Deserialize;

fn cmd_line_to_command<S: AsRef<str>>(cmd_line: &S, cwd: &Path) -> Result<Command, ()> {
    let cmd_line = cmd_line.as_ref();
    let (cmd, args) = {
        let mut words = cmd_line.split_whitespace();
        let cmd = words.next().ok_or(())?;
        (cmd, words)
    };

    let mut cmd = Command::new(cmd);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(cmd)
}

/// Performs a build using an external command and interprets the results.
/// The command should output on stdout a list of save-analysis .json files
/// to be reloaded by the RLS.
/// Note: This is *very* experimental and preliminary - this can viewed as
/// an experimentation until a more complete solution emerges.
pub(super) fn build_with_external_cmd<S: AsRef<str>>(
    cmd_line: S,
    build_dir: PathBuf,
) -> (BuildResult, Result<ExternalPlan, ()>) {
    let cmd_line = cmd_line.as_ref();

    let mut cmd = match cmd_line_to_command(&cmd_line, &build_dir) {
        Ok(cmd) => cmd,
        Err(_) => {
            let err_msg = format!("Couldn't treat {} as command", cmd_line);
            return (BuildResult::Err(err_msg, Some(cmd_line.to_owned())), Err(()));
        }
    };

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(io) => {
            let err_msg = format!("Couldn't execute: {} ({:?})", cmd_line, io.kind());
            return (BuildResult::Err(err_msg, Some(cmd_line.to_owned())), Err(()));
        }
    };

    let reader = std::io::BufReader::new(child.stdout.unwrap());

    let files = reader.lines().filter_map(|res| res.ok())
        .map(PathBuf::from)
        // Relative paths are relative to build command, not RLS itself (cwd may be different)
        .map(|path| if !path.is_absolute() { build_dir.join(path) } else { path });

    let analyses = match read_analysis_files(files) {
        Ok(analyses) => analyses,
        Err(cause) => {
            let err_msg = format!("Couldn't read analysis data: {}", cause);
            return (BuildResult::Err(err_msg, Some(cmd_line.to_owned())), Err(()));
        }
    };

    let plan = plan_from_analysis(&analyses, &build_dir);
    (BuildResult::Success(build_dir, vec![], analyses, false), plan)
}

/// Reads and deserializes given save-analysis JSON files into corresponding
/// `rls_data::Analysis` for each file. If an error is encountered, a `String`
/// with the error message is returned.
fn read_analysis_files<I>(files: I) -> Result<Vec<Analysis>, String>
where
    I: Iterator,
    I::Item: AsRef<Path>,
{
    let mut analyses = Vec::new();

    for path in files {
        trace!(
            "external::read_analysis_files: Attempt to read `{}`",
            path.as_ref().display()
        );

        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|e| e.to_string())?;

        let data = rustc_serialize::json::decode(&contents).map_err(|e| e.to_string())?;
        analyses.push(data);
    }

    Ok(analyses)
}

fn plan_from_analysis(analysis: &[Analysis], build_dir: &Path) -> Result<ExternalPlan, ()> {
    let indices: HashMap<_, usize> = analysis
        .iter()
        .enumerate()
        .map(|(idx, a)| (a.prelude.as_ref().unwrap().crate_id.disambiguator, idx))
        .collect();

    let invocations: Vec<RawInvocation> = analysis.into_iter()
        .map(|a| {
            let CompilationOptions { ref directory, ref program, ref arguments, ref output } =
                a.compilation.as_ref().ok_or(())?;

            let deps: Vec<usize> = a.prelude.as_ref().unwrap()
                .external_crates
                .iter()
                .filter_map(|c| indices.get(&c.id.disambiguator))
                .cloned()
                .collect();

            let cwd = match directory.is_relative() {
                true => build_dir.join(directory),
                false => directory.to_owned(),
            };

            Ok(RawInvocation {
                deps,
                outputs: vec![output.clone()],
                program: program.clone(),
                args: arguments.clone(),
                env: Default::default(),
                links: Default::default(),
                cwd: Some(cwd)

            })
        })
        .collect::<Result<Vec<RawInvocation>, ()>>()?;

    ExternalPlan::try_from_raw(RawPlan { invocations })
}

#[derive(Debug, Deserialize)]
/// Build plan as emitted by `cargo build --build-plan -Zunstable-options`
crate struct RawPlan {
    crate invocations: Vec<RawInvocation>,
}

#[derive(Debug, Deserialize)]
crate struct RawInvocation {
    crate deps: Vec<usize>,
    crate outputs: Vec<PathBuf>,
    #[serde(default)]
    crate links: BTreeMap<PathBuf, PathBuf>,
    crate program: String,
    crate args: Vec<String>,
    crate env: BTreeMap<String, String>,
    #[serde(default)]
    crate cwd: Option<PathBuf>,
}

#[derive(Clone, Debug)]
crate struct Invocation {
    deps: Vec<usize>, // FIXME: Use arena and store refs instead for ergonomics
    outputs: Vec<PathBuf>,
    links: BTreeMap<PathBuf, PathBuf>,
    command: ProcessBuilder,
    // Parsed data
    src_path: Option<PathBuf>,
}

/// Safe build plan type, invocation dependencies are guaranteed to be inside
/// the plan.
#[derive(Debug, Default)]
crate struct ExternalPlan {
    units: HashMap<u64, Invocation>,
    deps: HashMap<u64, HashSet<u64>>,
    rev_deps: HashMap<u64, HashSet<u64>>,
}

impl BuildKey for Invocation {
    type Key = u64;

    // Invocation key is the hash of the program, its arguments and environment.
    fn key(&self) -> u64 {
        let mut hash = DefaultHasher::new();

        self.command.get_program().hash(&mut hash);
        let /*mut*/ args = self.command.get_args().to_owned();
        // args.sort(); // TODO: Parse 2-part args (e.g. ["--extern", "a=b"])
        args.hash(&mut hash);
        let mut envs: Vec<_> = self.command.get_envs().iter().collect();
        envs.sort();
        envs.hash(&mut hash);

        hash.finish()
    }
}

impl From<RawInvocation> for Invocation {
    fn from(raw: RawInvocation) -> Invocation {
        let mut command = process(&raw.program);
        command.args(&raw.args);
        for (k, v) in &raw.env {
            command.env(&k, v);
        }
        if let Some(cwd) = &raw.cwd {
            command.cwd(cwd);
        }

        Invocation {
            deps: raw.deps.to_owned(),
            outputs: raw.outputs.to_owned(),
            links: raw.links.to_owned(),
            src_path: guess_rustc_src_path(&command),
            command,
        }
    }
}

impl ExternalPlan {
    crate fn new() -> ExternalPlan {
        Default::default()
    }

    crate fn with_units(units: Vec<Invocation>) -> ExternalPlan {
        let mut plan = ExternalPlan::new();
        for unit in &units {
            for &dep in &unit.deps {
                plan.add_dep(unit.key(), units[dep].key());
            }
        }

        ExternalPlan {
            units: units.into_iter().map(|u| (u.key(), u)).collect(),
            ..plan
        }
    }

    #[rustfmt::skip]
    fn add_dep(&mut self, key: u64, dep: u64) {
        self.deps.entry(key).or_insert_with(HashSet::new).insert(dep);
        self.rev_deps.entry(dep).or_insert_with(HashSet::new).insert(key);
    }

    crate fn try_from_raw(raw: RawPlan) -> Result<ExternalPlan, ()> {
        // Sanity check, each dependency (index) has to be inside the build plan
        if raw
            .invocations
            .iter()
            .flat_map(|inv| &inv.deps)
            .any(|idx| raw.invocations.get(*idx).is_none())
        {
            return Err(());
        }

        let units: Vec<Invocation> = raw.invocations.into_iter().map(|x| x.into()).collect();

        Ok(ExternalPlan::with_units(units))
    }
}

impl BuildGraph for ExternalPlan {
    type Unit = Invocation;

    fn units(&self) -> Vec<&Self::Unit> {
        self.units.values().collect()
    }

    fn get(&self, key: u64) -> Option<&Self::Unit> {
        self.units.get(&key)
    }

    fn get_mut(&mut self, key: u64) -> Option<&mut Self::Unit> {
        self.units.get_mut(&key)
    }

    fn deps(&self, key: u64) -> Vec<&Self::Unit> {
        self.deps
            .get(&key)
            .map(|d| d.iter().map(|d| &self.units[d]).collect())
            .unwrap_or_default()
    }

    fn add<T>(&mut self, unit: T, deps: Vec<T>)
    where
        T: Into<Self::Unit>
    {
        let unit = unit.into();

        for dep in deps.into_iter().map(|d| d.into()) {
            self.add_dep(unit.key(), dep.key());

            self.units.entry(dep.key()).or_insert(dep);
        }

        self.rev_deps.entry(unit.key()).or_insert_with(HashSet::new);
        self.units.entry(unit.key()).or_insert(unit);
    }

    // FIXME: Change associating files with units by their path but rather
    // include file inputs in the build plan or call rustc with --emit=dep-info
    fn dirties<T: AsRef<Path>>(&self, modified: &[T]) -> Vec<&Self::Unit> {
        let mut results = HashSet::<u64>::new();

        for modified in modified.iter().map(|x| x.as_ref()) {
            // We associate a dirty file with a
            // package by finding longest (most specified) path prefix.
            let matching_prefix_components = |a: &Path, b: &Path| -> usize {
                assert!(a.is_absolute() && b.is_absolute());
                a.components()
                    .zip(b.components())
                    .take_while(|&(x, y)| x == y)
                    .count()
            };
            // Since a package can correspond to many units (e.g. compiled
            // as a regular binary or a test harness for unit tests), we
            // collect every unit having the longest path prefix.
            let matching_units: Vec<(&_, usize)> = self.units.values()
                // For `rustc dir/some.rs` we'll consider every changed files
                // under dir/ as relevant
                .map(|unit| (unit, unit.src_path.as_ref().and_then(|src| src.parent())))
                .filter_map(|(unit, src)| src.map(|src| (unit, src)))
                // Discard units that are in a different directory subtree
                .filter_map(|(unit, src)| {
                    let matching = matching_prefix_components(modified, &src);
                    if matching >= src.components().count() {
                        Some((unit, matching))
                    } else {
                        None
                    }
                })
                .collect();

            // Changing files in the same directory might affect multiple units
            // (e.g. multiple crate binaries, their unit test harness), so
            // treat all of them as dirty.
            if let Some(max_prefix) = matching_units.iter().map(|(_, p)| p).max() {
                let dirty_keys = matching_units
                    .iter()
                    .filter(|(_, prefix)| prefix == max_prefix)
                    .map(|(unit, _)| unit.key());

                results.extend(dirty_keys);
            }
        }

        results.iter().map(|key| &self.units[key]).collect()
    }

    fn dirties_transitive<T: AsRef<Path>>(&self, files: &[T]) -> Vec<&Self::Unit> {
        let mut results = HashSet::new();

        let mut stack = self.dirties(files);

        while let Some(key) = stack.pop().map(|u| u.key()) {
            if results.insert(key) {
                if let Some(rdeps) = self.rev_deps.get(&key) {
                    for rdep in rdeps {
                        stack.push(&self.units[rdep]);
                    }
                }
            }
        }

        results.into_iter().map(|key| &self.units[&key]).collect()
    }

    fn topological_sort(&self, units: Vec<&Self::Unit>) -> Vec<&Self::Unit> {
        let dirties: HashSet<_> = units.into_iter().map(|u| u.key()).collect();

        let mut visited: HashSet<_> = HashSet::new();
        let mut output = vec![];

        for k in dirties {
            if !visited.contains(&k) {
                dfs(k, &self.rev_deps, &mut visited, &mut output);
            }
        }

        return output.iter().map(|key| &self.units[key]).collect();

        // Process graph depth-first recursively. A node needs to be pushed
        // after processing every other before to ensure topological ordering.
        fn dfs(
            unit: u64,
            graph: &HashMap<u64, HashSet<u64>>,
            visited: &mut HashSet<u64>,
            output: &mut Vec<u64>,
        ) {
            if visited.insert(unit) {
                for &neighbour in graph.get(&unit).iter().flat_map(|&edges| edges) {
                    dfs(neighbour, graph, visited, output);
                }
                output.push(unit);
            }
        }
    }

    fn prepare_work<T: AsRef<Path>>(&self, files: &[T]) -> WorkStatus {
        let dirties = self.dirties_transitive(files);
        let topo = self.topological_sort(dirties);

        let cmds = topo.into_iter().map(|unit| unit.command.clone()).collect();

        WorkStatus::Execute(JobQueue::with_commands(cmds))
    }
}

fn guess_rustc_src_path(cmd: &ProcessBuilder) -> Option<PathBuf> {
    if !Path::new(cmd.get_program()).ends_with("rustc") {
        return None;
    }

    let file = cmd
        .get_args()
        .iter()
        .find(|&a| Path::new(a).extension().map(|e| e == "rs").unwrap_or(false))?;
    let file_path = PathBuf::from(file);

    Some(match (cmd.get_cwd(), file_path.is_absolute()) {
        (_, true) => file_path,
        (Some(cwd), _) => cwd.join(file_path),
        // TODO: is cwd correct here?
        (None, _) => std::env::current_dir().ok()?.join(file_path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Sorted {
        fn sorted(self) -> Self;
    }

    impl<T: Ord> Sorted for Vec<T> {
        fn sorted(mut self: Self) -> Self {
            self.sort();
            self
        }
    }

    /// Helper struct that prints sorted unit source directories in a given plan.
    #[derive(Debug)]
    struct SrcPaths<'a>(Vec<&'a PathBuf>);
    impl<'a> SrcPaths<'a> {
        fn from(plan: &ExternalPlan) -> SrcPaths<'_> {
            SrcPaths(
                plan.units()
                    .iter()
                    .filter_map(|u| u.src_path.as_ref())
                    .collect(),
            )
        }
    }

    fn paths<'a>(invocations: &Vec<&'a Invocation>) -> Vec<&'a str> {
        invocations
            .iter()
            .filter_map(|d| d.src_path.as_ref())
            .map(|p| p.to_str().unwrap())
            .collect()
    }

    #[test]
    fn dirty_units_path_heuristics() {
        let plan = r#"{"invocations": [
            { "deps": [],  "program": "rustc", "args": ["--crate-name", "build_script_build", "/my/repo/build.rs"], "env": {}, "outputs": [] },
            { "deps": [0], "program": "rustc", "args": ["--crate-name", "repo", "/my/repo/src/lib.rs"], "env": {}, "outputs": [] }
        ]}"#;
        let plan = serde_json::from_str::<RawPlan>(&plan).unwrap();
        let plan = ExternalPlan::try_from_raw(plan).unwrap();

        eprintln!("src_paths: {:#?}", &SrcPaths::from(&plan));

        let dirties = |file: &str| -> Vec<&str> {
            plan.dirties(&[file])
                .iter()
                .filter_map(|d| d.src_path.as_ref())
                .map(|p| p.to_str().unwrap())
                .collect()
        };

        assert_eq!(dirties("/my/dummy.rs"), Vec::<&str>::new());
        assert_eq!(dirties("/my/repo/dummy.rs"), vec!["/my/repo/build.rs"]);
        assert_eq!(dirties("/my/repo/src/c.rs"), vec!["/my/repo/src/lib.rs"]);
        assert_eq!(dirties("/my/repo/src/a/b.rs"), vec!["/my/repo/src/lib.rs"]);
    }

    #[test]
    fn dirties_transitive() {
        let plan = r#"{"invocations": [
            { "deps": [],  "program": "rustc", "args": ["--crate-name", "build_script_build", "/my/repo/build.rs"], "env": {}, "outputs": [] },
            { "deps": [0], "program": "rustc", "args": ["--crate-name", "repo", "/my/repo/src/lib.rs"], "env": {}, "outputs": [] }
        ]}"#;
        let plan = serde_json::from_str::<RawPlan>(&plan).unwrap();
        let plan = ExternalPlan::try_from_raw(plan).unwrap();

        eprintln!("src_paths: {:#?}", &SrcPaths::from(&plan));
        eprintln!("plan: {:?}", &plan);

        assert_eq!(
            paths(&plan.dirties(&["/my/repo/src/a/b.rs"])),
            vec!["/my/repo/src/lib.rs"]
        );

        assert_eq!(
            paths(&plan.dirties_transitive(&["/my/repo/file.rs"])).sorted(),
            vec!["/my/repo/build.rs", "/my/repo/src/lib.rs"].sorted(),
        );
        assert_eq!(
            paths(&plan.dirties_transitive(&["/my/repo/src/file.rs"])).sorted(),
            vec!["/my/repo/src/lib.rs"].sorted(),
        );
    }

    #[test]
    fn topological_sort() {
        let plan = r#"{"invocations": [
            { "deps": [],  "program": "rustc", "args": ["--crate-name", "build_script_build", "/my/repo/build.rs"], "env": {}, "outputs": [] },
            { "deps": [0], "program": "rustc", "args": ["--crate-name", "repo", "/my/repo/src/lib.rs"], "env": {}, "outputs": [] }
        ]}"#;
        let plan = serde_json::from_str::<RawPlan>(&plan).unwrap();
        let plan = ExternalPlan::try_from_raw(plan).unwrap();

        eprintln!("src_paths: {:#?}", &SrcPaths::from(&plan));
        eprintln!("plan: {:?}", &plan);

        let units_to_rebuild = plan.dirties_transitive(&["/my/repo/file.rs"]);
        assert_eq!(
            paths(&units_to_rebuild).sorted(),
            vec!["/my/repo/build.rs", "/my/repo/src/lib.rs"].sorted(),
        );

        // TODO: Test on non-trivial input, use Iterator::position if
        // nondeterminate order wrt hashing is a problem
        // Jobs that have to run first are *last* in the topological sorting here
        let topo_units = plan.topological_sort(units_to_rebuild);
        assert_eq!(
            paths(&topo_units),
            vec!["/my/repo/src/lib.rs", "/my/repo/build.rs"],
        )
    }
}
