// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::fmt;

use cargo::core::{PackageId, Profile, Target, TargetKind};
use cargo::ops::{Kind, Unit, Context};
use cargo::util::{CargoResult, ProcessBuilder};

pub type DependencyGraph = HashMap<OwnedUnit, Vec<OwnedUnit>>;
/// Holds the information how exactly the build will be performed for a given
/// workspace with given, specified features.
/// **TODO:** Use it to schedule an analysis build instead of relying on Cargo
/// invocations.
pub struct Plan {
    pub dep_graph: DependencyGraph,
    /// We don't make a distinction between Units with different Profiles,
    /// as we're practically interested in bin, lib and (built, not run)
    /// build scripts for each package, because for these we can run `rustc` job
    pub compiler_jobs: HashMap<(PackageId, TargetKind), ProcessBuilder>,
}

impl Plan {
    pub fn new() -> Plan {
        Plan {
            dep_graph: HashMap::new(),
            compiler_jobs: HashMap::new(),
        }
    }


    /// Cache a given compiler invocation in `ProcessBuilder` for a given `PackageId`
    /// and `TargetKind` in `Target`, to be used when processing cached  build plan
    pub fn cache_compiler_job(&mut self, id: &PackageId, target: &Target, cmd: &ProcessBuilder) {
        let pkg_key = (id.clone(), target.kind().clone());
        self.compiler_jobs.insert(pkg_key, cmd.clone());
    }

    /// Emplace a given `Unit`, along with its `Unit` dependencies (recursively)
    /// into dependency graph
    #[allow(dead_code)]
    pub fn emplace_dep(&mut self, unit: &Unit, cx: &Context) -> CargoResult<()> {
        let null_filter = |_unit: &Unit| { true };
        self.emplace_dep_with_filter(unit, cx, &null_filter)
    }

    /// Emplace a given `Unit`, along with its `Unit` dependencies (recursively)
    /// into dependency graph as long as the passed `Unit` isn't filtered out by
    /// the `filter` closure.
    pub fn emplace_dep_with_filter<Filter>(&mut self,
                                           unit: &Unit,
                                           cx: &Context,
                                           filter: &Filter) -> CargoResult<()>
                                           where Filter: Fn(&Unit) -> bool {
        // We might not want certain deps to be added transitively (e.g. when
        // creating only a sub-dep-graph, limiting the scope to the workspace)
        if filter(unit) == false { return Ok(()); }

        let key: OwnedUnit = unit.into();
        // Process only those units, which are not yet in the dep graph
        if let None = self.dep_graph.get(&key) {
            let units = cx.dep_targets(unit)?;
            let dep_keys: Vec<OwnedUnit> = units
                                          .iter()
                                          .map(|x| x.into())
                                          .collect();
            self.dep_graph.insert(key, dep_keys);
            // Recursively process other remaining dependencies.
            // TODO: Should we be careful about blowing the stack and do it
            // iteratively instead?
            for unit in units {
                self.emplace_dep_with_filter(&unit, cx, filter)?;
            }
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.dep_graph.clear();
        self.compiler_jobs.clear();
    }
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("Dep graph:\n")?;
        for (key, deps) in &self.dep_graph {
            f.write_str(&format!("{:?}\n", key))?;
            for dep in deps {
                f.write_str(&format!("- {:?}\n", dep))?;
            }
        }
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
