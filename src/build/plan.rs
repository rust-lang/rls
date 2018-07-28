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
use std::sync::{Arc, Mutex};
use std::sync::mpsc::Sender;

use crate::build::PackageArg;
use cargo::core::{PackageId, Target, TargetKind};
use cargo::core::compiler::{Context, Kind, Unit};
use cargo::core::profiles::Profile;
use cargo::util::{CargoResult, ProcessBuilder};
use cargo_metadata;
use crate::lsp_data::parse_file_path;
use url::Url;
use log::{log, trace};

use crate::actions::progress::ProgressUpdate;
use super::{BuildResult, Internals};

/// Main key type by which `Unit`s will be distinguished in the build plan.
crate type UnitKey = (PackageId, TargetKind);

/// Holds the information how exactly the build will be performed for a given
/// workspace with given, specified features.
crate struct Plan {
    /// Stores a full Cargo `Unit` data for a first processed unit with a given key.
    crate units: HashMap<UnitKey, OwnedUnit>,
    /// Main dependency graph between the simplified units.
    crate dep_graph: HashMap<UnitKey, HashSet<UnitKey>>,
    /// Reverse dependency graph that's used to construct a dirty compiler call queue.
    crate rev_dep_graph: HashMap<UnitKey, HashSet<UnitKey>>,
    /// Cached compiler calls used when creating a compiler call queue.
    crate compiler_jobs: HashMap<UnitKey, ProcessBuilder>,
    // An object for finding the package which a file belongs to and this inferring
    // a package argument.
    package_map: Option<PackageMap>,
    /// Packages (names) for which this build plan was prepared.
    /// Used to detect if the plan can reused when building certain packages.
    built_packages: HashSet<String>,
}

impl Plan {
    crate fn new() -> Plan {
        Self::for_packages(HashSet::new())
    }

    crate fn for_packages(pkgs: HashSet<String>) -> Plan {
        Plan {
            units: HashMap::new(),
            dep_graph: HashMap::new(),
            rev_dep_graph: HashMap::new(),
            compiler_jobs: HashMap::new(),
            package_map: None,
            built_packages: pkgs,
        }
    }

    /// Returns whether a build plan has cached compiler invocations and dep
    /// graph so it's at all able to return a job queue via `prepare_work`.
    crate fn is_ready(&self) -> bool {
        !self.compiler_jobs.is_empty()
    }

    /// Cache a given compiler invocation in `ProcessBuilder` for a given
    /// `PackageId` and `TargetKind` in `Target`, to be used when processing
    /// cached build plan.
    crate fn cache_compiler_job(&mut self, id: &PackageId, target: &Target, cmd: &ProcessBuilder) {
        let pkg_key = (id.clone(), target.kind().clone());
        self.compiler_jobs.insert(pkg_key, cmd.clone());
    }

    /// Emplace a given `Unit`, along with its `Unit` dependencies (recursively)
    /// into the dependency graph.
    #[allow(dead_code)]
    crate fn emplace_dep(&mut self, unit: &Unit<'_>, cx: &Context<'_, '_>) -> CargoResult<()> {
        let null_filter = |_unit: &Unit<'_>| true;
        self.emplace_dep_with_filter(unit, cx, &null_filter)
    }

    /// Emplace a given `Unit`, along with its `Unit` dependencies (recursively)
    /// into the dependency graph as long as the passed `Unit` isn't filtered
    /// out by the `filter` closure.
    crate fn emplace_dep_with_filter<Filter>(
        &mut self,
        unit: &Unit<'_>,
        cx: &Context<'_, '_>,
        filter: &Filter,
    ) -> CargoResult<()>
    where
        Filter: Fn(&Unit<'_>) -> bool,
    {
        if !filter(unit) {
            return Ok(());
        }

        let key = key_from_unit(unit);
        self.units.entry(key.clone()).or_insert_with(|| unit.into());
        // Process only those units, which are not yet in the dep graph.
        if self.dep_graph.get(&key).is_some() {
            return Ok(());
        }

        // Keep all the additional Unit information for a given unit (It's
        // worth remembering, that the units are only discriminated by a
        // pair of (PackageId, TargetKind), so only first occurrence will be saved.
        self.units.insert(key.clone(), unit.into());

        // Fetch and insert relevant unit dependencies to the forward dep graph.
        let units = cx.dep_targets(unit);
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
            .or_insert_with(HashSet::new);
        for unit in dep_keys {
            let revs = self.rev_dep_graph.entry(unit).or_insert_with(HashSet::new);
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
    fn fetch_dirty_units<T: AsRef<Path>>(&self, files: &[T]) -> HashSet<UnitKey> {
        let mut result = HashSet::new();

        let build_scripts: HashMap<&Path, UnitKey> = self.units
            .iter()
            .filter(|&(&(_, ref kind), _)| *kind == TargetKind::CustomBuild)
            .map(|(key, unit)| (unit.target.src_path(), key.clone()))
            .collect();
        let other_targets: HashMap<UnitKey, &Path> = self.units
            .iter()
            .filter(|&(&(_, ref kind), _)| *kind != TargetKind::CustomBuild)
            .map(|(key, unit)| {
                (
                    key.clone(),
                    unit.target
                        .src_path()
                        .parent()
                        .expect("no parent for src_path"),
                )
            })
            .collect();

        for modified in files.iter().map(|x| x.as_ref()) {
            if let Some(unit) = build_scripts.get(modified) {
                result.insert(unit.clone());
            } else {
                // Not a build script, so we associate a dirty package with a
                // dirty file by finding longest (most specified) path prefix
                let unit = other_targets.iter().max_by_key(|&(_, src_dir)| {
                    if !modified.starts_with(src_dir) {
                        return 0;
                    }
                    modified
                        .components()
                        .zip(src_dir.components())
                        .take_while(|&(a, b)| a == b)
                        .count()
                });
                match unit {
                    None => trace!(
                        "Modified file {:?} doesn't correspond to any package!",
                        modified.display()
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
            }
            transitive.insert(top.clone());

            // Process every dirty rev dep of the processed node
            let dirty_rev_deps = self.rev_dep_graph
                .get(&top)
                .expect("missing key in rev_dep_graph")
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
            .filter(|&(unit, _)| dirties.contains(unit))
            // Retain only dirty dependencies of the ones that are dirty
            .map(|(k, deps)| (k.clone(), deps.iter().cloned().filter(|d| dirties.contains(d)).collect()))
            .collect()
    }

    /// Returns a topological ordering of a connected DAG of rev deps. The
    /// output is a stack of units that can be linearly rebuilt, starting from
    /// the last element.
    fn topological_sort(&self, dirties: &HashMap<UnitKey, HashSet<UnitKey>>) -> Vec<UnitKey> {
        let mut visited: HashSet<UnitKey> = HashSet::new();
        let mut output = vec![];

        for k in dirties.keys() {
            if !visited.contains(k) {
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
            if visited.contains(unit) {
                return;
            } else {
                visited.insert(unit.clone());
                for neighbour in &graph[unit] {
                    dfs(neighbour, graph, visited, output);
                }
                output.push(unit.clone());
            }
        }
    }

    crate fn prepare_work<T: AsRef<Path> + fmt::Debug>(
        &mut self,
        manifest_path: &Path,
        modified: &[T],
        requested_cargo: bool,
    ) -> WorkStatus {
        if self.package_map.is_none() || requested_cargo {
            self.package_map = Some(PackageMap::new(manifest_path));
        }

        if !self.is_ready() || requested_cargo {
            return WorkStatus::NeedsCargo(PackageArg::Default);
        }

        let dirty_packages = self.package_map
            .as_ref()
            .unwrap()
            .compute_dirty_packages(modified);

        let needs_more_packages = dirty_packages
            .difference(&self.built_packages)
            .next()
            .is_some();

        let needed_packages = self.built_packages
            .union(&dirty_packages)
            .cloned()
            .collect();

        // We modified a file from a packages, that are not included in the
        // cached build plan - run Cargo to recreate the build plan including them
        if needs_more_packages {
            return WorkStatus::NeedsCargo(PackageArg::Packages(needed_packages));
        }

        let dirties = self.fetch_dirty_units(modified);
        trace!(
            "fetch_dirty_units: for files {:?}, these units are dirty: {:?}",
            modified,
            dirties,
        );

        if dirties
            .iter()
            .any(|&(_, ref kind)| *kind == TargetKind::CustomBuild)
        {
            WorkStatus::NeedsCargo(PackageArg::Packages(needed_packages))
        } else {
            let graph = self.dirty_rev_dep_graph(&dirties);
            trace!("Constructed dirty rev dep graph: {:?}", graph);

            if graph.is_empty() {
                return WorkStatus::NeedsCargo(PackageArg::Default);
            }

            let queue = self.topological_sort(&graph);
            trace!("Topologically sorted dirty graph: {:?} {}", queue, self.is_ready());
            let jobs: Option<Vec<_>> = queue
                .iter()
                .map(|x| {
                    self.compiler_jobs
                        .get(x)
                        .cloned()
                })
                .collect();

            // It is possible that we want a job which is not in our cache (compiler_jobs),
            // for example we might be building a workspace with an error in a crate and later
            // crates within the crate that depend on the error-ing one have never been built.
            // In that case we need to build from scratch so that everything is in our cache, or
            // we cope with the error. In the error case, jobs will be None.
            match jobs {
                None => WorkStatus::NeedsCargo(PackageArg::Default),
                Some(jobs) => {
                    assert!(!jobs.is_empty());
                    WorkStatus::Execute(JobQueue(jobs))
                }
            }
        }
    }
}

#[derive(Debug)]
crate enum WorkStatus {
    NeedsCargo(PackageArg),
    Execute(JobQueue),
}

/// Maps paths to packages.
///
/// The point of the PackageMap is detect if additional packages need to be
/// included in the cached build plan. The cache can represent only a subset of
/// the entire workspace, hence why we need to detect if a package was modified
/// that's outside the cached build plan - if so, we need to recreate it,
/// including the new package.
#[derive(Debug)]
struct PackageMap {
    // A map from a manifest directory to the package name.
    package_paths: HashMap<PathBuf, String>,
    // A map from a file's path, to the package it belongs to.
    map_cache: Mutex<HashMap<PathBuf, String>>,
}

impl PackageMap {
    fn new(manifest_path: &Path) -> PackageMap {
        PackageMap {
            package_paths: Self::discover_package_paths(manifest_path),
            map_cache: Mutex::new(HashMap::new()),
        }
    }

    // Find each package in the workspace and record the root directory and package name.
    fn discover_package_paths(manifest_path: &Path) -> HashMap<PathBuf, String> {
        trace!("read metadata {:?}", manifest_path);
        let metadata = match cargo_metadata::metadata(Some(manifest_path)) {
            Ok(metadata) => metadata,
            Err(_) => return HashMap::new(),
        };
        metadata
            .workspace_members
            .into_iter()
            .map(|wm| {
                assert!(wm.url.starts_with("path+"));
                let url = Url::parse(&wm.url[5..]).expect("Bad URL");
                let path = parse_file_path(&url).expect("URL not a path");
                (path, wm.name)
            })
            .collect()
    }

    /// Given modified set of files, returns a set of corresponding dirty packages.
    fn compute_dirty_packages<T: AsRef<Path> + fmt::Debug>(&self, modified_files: &[T]) -> HashSet<String> {
        modified_files
            .iter()
            .filter_map(|p| self.map(p.as_ref()))
            .collect()
    }

    // Map a file to the package which it belongs to.
    // We do this by walking up the directory tree from `path` until we get to
    // one of the recorded package root directories.
    fn map(&self, path: &Path) -> Option<String> {
        if self.package_paths.is_empty() {
            return None;
        }

        let mut map_cache = self.map_cache.lock().unwrap();
        if map_cache.contains_key(path) {
            return Some(map_cache[path].clone());
        }

        let result = Self::map_uncached(path, &self.package_paths)?;

        map_cache.insert(path.to_owned(), result.clone());
        Some(result)
    }

    fn map_uncached(path: &Path, package_paths: &HashMap<PathBuf, String>) -> Option<String> {
        if package_paths.is_empty() {
            return None;
        }

        match package_paths.get(path) {
            Some(package) => Some(package.clone()),
            None => Self::map_uncached(path.parent()?, package_paths),
        }
    }
}

#[derive(Debug)]
crate struct JobQueue(Vec<ProcessBuilder>);

impl JobQueue {
    crate fn dequeue(&mut self) -> Option<ProcessBuilder> {
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
        let (build_dir, mut cwd) = {
            let comp_cx = internals.compilation_cx.lock().unwrap();
            (
                comp_cx.build_dir.clone().expect("no build directory"),
                comp_cx.cwd.clone(),
            )
        };

        // Go through cached compiler invocations sequentially, collecting each
        // invocation's compiler messages for diagnostics and analysis data
        while let Some(job) = self.dequeue() {
            trace!("Executing: {:?}", job);
            let mut args: Vec<_> = job.get_args()
                .iter()
                .cloned()
                .map(|x| x.into_string().expect("cannot stringify job args"))
                .collect();

            let program = job.get_program()
                .clone()
                .into_string()
                .expect("cannot stringify job program");
            args.insert(0, program.clone());

            // Send a window/progress notification. At this point we know the percentage
            // started out of the entire cached build.
            // FIXME. We could communicate the "program" being built here, but
            // it seems window/progress notification should have message OR percentage.
            {
                // divide by zero is avoided by earlier assert!
                let percentage = compiler_messages.len() as f64 / self.0.len() as f64;
                progress_sender
                    .send(ProgressUpdate::Percentage(percentage))
                    .expect("Failed to send progress update");
            }

            match super::rustc::rustc(
                &internals.vfs,
                &args,
                job.get_envs(),
                cwd.as_ref().map(|p| &**p),
                &build_dir,
                Arc::clone(&internals.config),
                &internals.env_lock.as_facade(),
            ) {
                BuildResult::Success(c, mut messages, mut analysis, success) => {
                    compiler_messages.append(&mut messages);
                    analyses.append(&mut analysis);
                    cwd = Some(c);

                    // This compilation failed, but the build as a whole does not
                    // need to error out.
                    if !success {
                        return BuildResult::Success(
                            cwd.unwrap(),
                            compiler_messages,
                            analyses,
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
            true,
        )
    }
}

fn key_from_unit(unit: &Unit<'_>) -> UnitKey {
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
    };
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format!("Units: {:?}\n", self.units))?;
        print_dep_graph!("Dependency graph", self.dep_graph, f);
        print_dep_graph!("Reverse dependency graph", self.rev_dep_graph, f);
        f.write_str(&format!("Compiler jobs: {:?}\n", self.compiler_jobs))?;
        Ok(())
    }
}

#[derive(Hash, PartialEq, Eq, Debug)]
/// An owned version of `cargo::core::Unit`.
crate struct OwnedUnit {
    crate id: PackageId,
    crate target: Target,
    crate profile: Profile,
    crate kind: Kind,
}

impl<'a> From<&'a Unit<'a>> for OwnedUnit {
    fn from(unit: &Unit<'a>) -> OwnedUnit {
        OwnedUnit {
            id: unit.pkg.package_id().to_owned(),
            target: unit.target.clone(),
            profile: unit.profile,
            kind: unit.kind,
        }
    }
}
