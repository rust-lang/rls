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

use cargo::core::{PackageId, Profile, Target};
use cargo::ops::{Kind, Unit, Context};
use cargo::util::{CargoResult};

/// Holds the information how exactly the build will be performed for a given
/// workspace with given, specified features.
/// **TODO:** Use it to schedule an analysis build instead of relying on Cargo
/// invocations.
pub type DependencyGraph = HashMap<OwnedUnit, Vec<OwnedUnit>>;
pub struct Plan {
    pub dep_graph: DependencyGraph
}

impl Plan {
    pub fn new() -> Plan {
        Plan {
            dep_graph: HashMap::new()
        }
    }
    /// Emplace a given `Unit`, along with its dependencies (recursively) into
    /// dependency graph.
    pub fn emplace_dep(&mut self, unit: &Unit, cx: &Context) -> CargoResult<()> {
        let key: OwnedUnit = unit.into();
        // Only process units that are not yet in the dep graph
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
                self.emplace_dep(&unit, cx)?;
            }

        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.dep_graph.clear()
    }
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for (key, deps) in &self.dep_graph {
            f.write_str(&format!("{:?}\n", key))?;
            for dep in deps {
                f.write_str(&format!("- {:?}\n", dep))?;
            }
        }
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
