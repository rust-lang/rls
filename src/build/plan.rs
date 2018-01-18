// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This contains a build plan that is created during the Cargo build routine
//! and stored afterwards, which can be later queried, given a list of dirty
//! files, to retrieve a queue of compiler calls to be invoked (including
//! appropriate arguments and env variables).
//! The underlying structure is a dependency graph between simplified units
//! (package id and crate target kind), as opposed to Cargo units (package with
//! a target info, including crate target kind, profile and host/target kind).
//! This will be used for a quick check recompilation and does not aim to
//! reimplement all the intricacies of Cargo.
//! The unit dependency graph in Cargo also distinguishes between compiling the
//! build script and running it and collecting the build script output to modify
//! the subsequent compilations etc. Since build script executions (not building)
//! are not exposed via `Executor` trait in Cargo, we simply coalesce every unit
//! with a same package and crate target kind (e.g. both building and running
//! build scripts).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use cargo::core::{PackageId, Profile, Target, TargetKind};
use cargo::ops::{Context, Kind, Unit};
use cargo::util::{CargoResult, ProcessBuilder};

use super::{BuildResult, Internals};

/// Main key type by which `Unit`s will be distinguished in the build plan.
pub type UnitKey = (PackageId, TargetKind);
/// Holds the information how exactly the build will be performed for a given
/// workspace with given, specified features.
pub struct Plan {
    /// Stores a full Cargo `Unit` data for a first processed unit with a given key.
    pub units: HashMap<UnitKey, OwnedUnit>,
    /// Main dependency graph between the simplified units.
    pub dep_graph: HashMap<UnitKey, HashSet<UnitKey>>,
    /// Reverse dependency graph that's used to construct a dirty compiler call queue.
    pub rev_dep_graph: HashMap<UnitKey, HashSet<UnitKey>>,
    /// Cached compiler calls used when creating a compiler call queue.
    pub compiler_jobs: HashMap<UnitKey, ProcessBuilder>,
}

impl Plan {
    pub fn new() -> Plan {
        Plan {
            units: HashMap::new(),
            dep_graph: HashMap::new(),
            rev_dep_graph: HashMap::new(),
            compiler_jobs: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        *self = Plan::new();
    }

    /// Returns whether a build plan has cached compiler invocations and dep
    /// graph so it's at all able to return a job queue via `prepare_work`.
    pub fn is_ready(&self) -> bool {
        self.compiler_jobs.is_empty() == false
    }

    /// Cache a given compiler invocation in `ProcessBuilder` for a given
    /// `PackageId` and `TargetKind` in `Target`, to be used when processing
    /// cached build plan.
    pub fn cache_compiler_job(&mut self, id: &PackageId, target: &Target, cmd: &ProcessBuilder) {
        let pkg_key = (id.clone(), target.kind().clone());
        self.compiler_jobs.insert(pkg_key, cmd.clone());
    }

    /// Emplace a given `Unit`, along with its `Unit` dependencies (recursively)
    /// into the dependency graph.
    #[allow(dead_code)]
    pub fn emplace_dep(&mut self, unit: &Unit, cx: &Context) -> CargoResult<()> {
        let null_filter = |_unit: &Unit| true;
        self.emplace_dep_with_filter(unit, cx, &null_filter)
    }

    /// Emplace a given `Unit`, along with its `Unit` dependencies (recursively)
    /// into the dependency graph as long as the passed `Unit` isn't filtered
    /// out by the `filter` closure.
    pub fn emplace_dep_with_filter<Filter>(
        &mut self,
        unit: &Unit,
        cx: &Context,
        filter: &Filter,
    ) -> CargoResult<()>
    where
        Filter: Fn(&Unit) -> bool,
    {
        if !filter(unit) {
            return Ok(());
        }

        let key = key_from_unit(unit);
        self.units.entry(key.clone()).or_insert(unit.into());
        // Process only those units, which are not yet in the dep graph.
        if self.dep_graph.get(&key).is_some() {
            return Ok(());
        }

        // Keep all the additional Unit information for a given unit (It's
        // worth remembering, that the units are only discriminated by a
        // pair of (PackageId, TargetKind), so only first occurrence will be saved.
        self.units.insert(key.clone(), unit.into());

        // Fetch and insert relevant unit dependencies to the forward dep graph.
        let units = cx.dep_targets(unit)?;
        let dep_keys: HashSet<UnitKey> = units.iter()
            // We might not want certain deps to be added transitively (e.g.
            // when creating only a sub-dep-graph, limiting the scope).
            .filter(|unit| filter(unit))
            .map(key_from_unit)
            // Units can depend on others with different Targets or Profiles
            // (e.g. different `run_custom_build`) despite having the same UnitKey.
            // We coalesce them here while creating the UnitKey dep graph.
            .filter(|dep| key != *dep)
            .collect();
        self.dep_graph.insert(key.clone(), dep_keys.clone());

        // We also need to track reverse dependencies here, as it's needed
        // to quickly construct a work sub-graph from a set of dirty units.
        self.rev_dep_graph
            .entry(key.clone())
            .or_insert(HashSet::new());
        for unit in dep_keys {
            let revs = self.rev_dep_graph.entry(unit).or_insert(HashSet::new());
            revs.insert(key.clone());
        }

        // Recursively process other remaining forward dependencies.
        for unit in units {
            self.emplace_dep_with_filter(&unit, cx, filter)?;
        }
        Ok(())
    }

    /// TODO: Improve detecting dirty crate targets for a set of dirty file paths.
    /// This uses a lousy heuristic of checking path prefix for a given crate
    /// target to determine whether a given unit (crate target) is dirty. This
    /// can easily backfire, e.g. when build script is under src/. Any change
    /// to a file under src/ would imply the build script is always dirty, so we
    /// never do work and always offload to Cargo in such case.
    /// Because of that, build scripts are checked separately and only other
    /// crate targets are checked with path prefixes.
    fn fetch_dirty_units<T: AsRef<Path> + fmt::Debug>(&self, files: &[T]) -> HashSet<UnitKey> {
        let mut result = HashSet::new();

        let build_scripts: HashMap<&Path, UnitKey> = self.units
            .iter()
            .filter(|&(&(_, ref kind), _)| *kind == TargetKind::CustomBuild)
            .map(|(key, ref unit)| (unit.target.src_path(), key.clone()))
            .collect();
        let other_targets: HashMap<UnitKey, &Path> = self.units
            .iter()
            .filter(|&(&(_, ref kind), _)| *kind != TargetKind::CustomBuild)
            .map(|(key, ref unit)| {
                (key.clone(), unit.target.src_path().parent().unwrap())
            })
            .collect();

        for modified in files {
            if let Some(unit) = build_scripts.get(modified.as_ref()) {
                result.insert(unit.clone());
            } else {
                // Not a build script, so we associate a dirty package with a
                // dirty file by finding longest (most specified) path prefix
                let unit = other_targets.iter().max_by_key(|&(_, src_dir)| {
                    modified
                        .as_ref()
                        .components()
                        .zip(src_dir.components())
                        .take_while(|&(a, b)| a == b)
                        .count()
                });
                match unit {
                    None => trace!(
                        "Modified file {:?} doesn't correspond to any package!",
                        modified
                    ),
                    Some(unit) => {
                        result.insert(unit.0.clone());
                    }
                };
            }
        }
        result
    }

    /// For a given set of select dirty units, returns a set of all the
    /// dependencies that has to be rebuilt transitively.
    fn transitive_dirty_units(&self, dirties: &HashSet<UnitKey>) -> HashSet<UnitKey> {
        let mut transitive = dirties.clone();
        // Walk through a rev dep graph using a stack of nodes to collect
        // transitively every dirty node
        let mut to_process: Vec<_> = dirties.iter().cloned().collect();
        while let Some(top) = to_process.pop() {
            if transitive.get(&top).is_some() {
                continue;
            } else {
                transitive.insert(top.clone());
            }

            // Process every dirty rev dep of the processed node
            let dirty_rev_deps = self.rev_dep_graph
                .get(&top)
                .unwrap()
                .iter()
                .filter(|dep| dirties.contains(dep));
            for rev_dep in dirty_rev_deps {
                to_process.push(rev_dep.clone());
            }
        }
        transitive
    }

    /// Creates a dirty reverse dependency graph using a set of given dirty units.
    fn dirty_rev_dep_graph(
        &self,
        dirties: &HashSet<UnitKey>,
    ) -> HashMap<UnitKey, HashSet<UnitKey>> {
        let dirties = self.transitive_dirty_units(dirties);
        trace!("transitive_dirty_units: {:?}", dirties);

        self.rev_dep_graph.iter()
            // Remove nodes that are not dirty
            .filter(|&(unit, _)| dirties.contains(&unit))
            // Retain only dirty dependencies of the ones that are dirty
            .map(|(k, deps)| (k.clone(), deps.iter().cloned().filter(|d| dirties.contains(&d)).collect()))
            .collect()
    }

    /// Returns a topological ordering of a connected DAG of rev deps. The
    /// output is a stack of units that can be linearly rebuilt, starting from
    /// the last element.
    fn topological_sort(&self, dirties: &HashMap<UnitKey, HashSet<UnitKey>>) -> Vec<UnitKey> {
        let mut visited: HashSet<UnitKey> = HashSet::new();
        let mut output = vec![];

        for (k, _) in dirties {
            if !visited.contains(&k) {
                dfs(k, &self.rev_dep_graph, &mut visited, &mut output);
            }
        }

        return output;

        // Process graph depth-first recursively. A node needs to be pushed
        // after processing every other before to ensure topological ordering.
        fn dfs(
            unit: &UnitKey,
            graph: &HashMap<UnitKey, HashSet<UnitKey>>,
            visited: &mut HashSet<UnitKey>,
            output: &mut Vec<UnitKey>,
        ) {
            if visited.contains(&unit) {
                return;
            } else {
                visited.insert(unit.clone());
                for neighbour in graph.get(&unit).unwrap() {
                    dfs(neighbour, graph, visited, output);
                }
                output.push(unit.clone());
            }
        }
    }

    pub fn prepare_work<T: AsRef<Path> + fmt::Debug>(&self, modified: &[T]) -> WorkStatus {
        if self.is_ready() == false {
            return WorkStatus::NeedsCargo;
        }

        let dirties = self.fetch_dirty_units(modified);
        trace!(
            "fetch_dirty_units: for files {:?}, these units are dirty: {:?}",
            modified,
            dirties
        );

        if dirties
            .iter()
            .any(|&(_, ref kind)| *kind == TargetKind::CustomBuild)
        {
            WorkStatus::NeedsCargo
        } else {
            let graph = self.dirty_rev_dep_graph(&dirties);
            trace!("Constructed dirty rev dep graph: {:?}", graph);

            let queue = self.topological_sort(&graph);
            trace!("Topologically sorted dirty graph: {:?}", queue);
            let jobs: Vec<_> = queue
                .iter()
                .map(|x| self.compiler_jobs.get(x).unwrap().clone())
                .collect();

            if jobs.is_empty() {
                WorkStatus::NeedsCargo
            } else {
                WorkStatus::Execute(JobQueue(jobs))
            }
        }
    }
}

pub enum WorkStatus {
    NeedsCargo,
    Execute(JobQueue),
}

pub struct JobQueue(Vec<ProcessBuilder>);

impl JobQueue {
    pub fn dequeue(&mut self) -> Option<ProcessBuilder> {
        self.0.pop()
    }

    /// Performs a rustc build using cached compiler invocations.
    pub(super) fn execute(mut self, internals: &Internals) -> BuildResult {
        // TODO: In case of an empty job queue we shouldn't be here, since the
        // returned results will replace currently held diagnostics/analyses.
        // Either allow to return a BuildResult::Squashed here or just delegate
        // to Cargo (which we do currently) in `prepare_work`
        assert!(self.0.is_empty() == false);

        let mut compiler_messages = vec![];
        let mut analyses = vec![];
        let (build_dir, mut cwd) = {
            let comp_cx = internals.compilation_cx.lock().unwrap();
            (comp_cx.build_dir.clone().unwrap(), comp_cx.cwd.clone())
        };

        // Go through cached compiler invocations sequentially, collecting each
        // invocation's compiler messages for diagnostics and analysis data
        while let Some(job) = self.dequeue() {
            trace!("Executing: {:?}", job);
            let mut args: Vec<_> = job.get_args()
                .iter()
                .cloned()
                .map(|x| x.into_string().unwrap())
                .collect();

            args.insert(0, job.get_program().clone().into_string().unwrap());

            match super::rustc::rustc(
                &internals.vfs,
                &args,
                job.get_envs(),
                cwd.as_ref().map(|p| &**p),
                &build_dir,
                internals.config.clone(),
                internals.env_lock.as_facade(),
            ) {
                BuildResult::Success(c, mut messages, mut analysis) => {
                    compiler_messages.append(&mut messages);
                    analyses.append(&mut analysis);
                    cwd = Some(c);
                }
                BuildResult::Err => return BuildResult::Err,
                _ => {}
            }
        }

        BuildResult::Success(
            cwd.unwrap_or_else(|| PathBuf::from(".")),
            compiler_messages,
            analyses,
        )
    }
}

fn key_from_unit(unit: &Unit) -> UnitKey {
    (unit.pkg.package_id().clone(), unit.target.kind().clone())
}

macro_rules! print_dep_graph {
    ($name: expr, $graph: expr, $f: expr) => {
        $f.write_str(&format!("{}:\n", $name))?;
        for (key, deps) in &$graph {
            $f.write_str(&format!("{:?}\n", key))?;
            for dep in deps {
                $f.write_str(&format!("- {:?}\n", dep))?;
            }
        }
    }
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&format!("Units: {:?}\n", self.units))?;
        print_dep_graph!("Dependency graph", self.dep_graph, f);
        print_dep_graph!("Reverse dependency graph", self.rev_dep_graph, f);
        f.write_str(&format!("Compiler jobs: {:?}\n", self.compiler_jobs))?;
        Ok(())
    }
}

#[derive(Hash, PartialEq, Eq, Debug)]
/// An owned version of `cargo::core::Unit`.
pub struct OwnedUnit {
    pub id: PackageId,
    pub target: Target,
    pub profile: Profile,
    pub kind: Kind,
}

impl<'a> From<&'a Unit<'a>> for OwnedUnit {
    fn from(unit: &Unit<'a>) -> OwnedUnit {
        OwnedUnit {
            id: unit.pkg.package_id().to_owned(),
            target: unit.target.clone(),
            profile: unit.profile.clone(),
            kind: unit.kind,
        }
    }
}
