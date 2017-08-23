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

use cargo::core::{PackageId, Profile, Target, TargetKind};
use cargo::ops::{Kind, Unit, Context};
use cargo::util::{CargoResult, ProcessBuilder};

/// Main key type by which `Unit`s will be distinguished in the build plan.
pub type UnitKey = (PackageId, TargetKind);
/// Holds the information how exactly the build will be performed for a given
/// workspace with given, specified features.
pub struct Plan {
    // Stores a full Cargo `Unit` data for a first processed unit with a given key.
    pub units: HashMap<UnitKey, OwnedUnit>,
    // Main dependency graph between the simplified units.
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
        let null_filter = |_unit: &Unit| { true };
        self.emplace_dep_with_filter(unit, cx, &null_filter)
    }

    /// Emplace a given `Unit`, along with its `Unit` dependencies (recursively)
    /// into the dependency graph as long as the passed `Unit` isn't filtered
    /// out by the `filter` closure.
    pub fn emplace_dep_with_filter<Filter>(&mut self,
                                           unit: &Unit,
                                           cx: &Context,
                                           filter: &Filter) -> CargoResult<()>
                                           where Filter: Fn(&Unit) -> bool {
        if !filter(unit) { return Ok(()); }

        let key = key_from_unit(unit);
        self.units.entry(key.clone()).or_insert(unit.into());
        // Process only those units, which are not yet in the dep graph.
        if self.dep_graph.get(&key).is_some() { return Ok(()); }

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
        self.rev_dep_graph.entry(key.clone()).or_insert(HashSet::new());
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
    pub kind: Kind
}

impl<'a> From<&'a Unit<'a>> for OwnedUnit {
    fn from(unit: &Unit<'a>) -> OwnedUnit {
        OwnedUnit {
            id: unit.pkg.package_id().to_owned(),
            target: unit.target.clone(),
            profile: unit.profile.clone(),
            kind: unit.kind
        }
    }
}
