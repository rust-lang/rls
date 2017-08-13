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

use cargo::core::{Package as CargoPackage, PackageId, Resolve, Target, Workspace};
use cargo::ops::{self, OutputMetadataOptions, Packages};
use cargo::util::CargoResult;

/// Metadata version copied from `cargo_output_metadata.rs`. TODO: Remove
/// when Cargo API will expose more information regarding output metadata.
const VERSION: u32 = 1;

/// Holds the information how exactly the build will be performed for a given
/// workspace with given, specified features.
/// **TODO:** Use it to schedule an analysis build instead of relying on Cargo
/// invocations.
#[derive(Debug)]
pub struct Plan {
    // TODO: Implement/add inter-(package) target dep queue
    // with args/envs per-target/package
    pub metadata: Metadata
}

pub fn create_plan(ws: &Workspace) -> CargoResult<Plan> {
    // TODO: Fill appropriately
    let options = OutputMetadataOptions {
        features: vec![],
        no_default_features: false,
        all_features: false,
        no_deps: false,
        version: VERSION,
    };

    let metadata = metadata_full(ws, &options)?;
    Ok(Plan { metadata: metadata.into() })
}

/// Targets and features for a given package in the dep graph.
#[derive(Debug)]
pub struct Package {
    pub id: PackageId,
    pub targets: Vec<Target>,
    pub features: HashMap<String, Vec<String>>,
}

impl From<CargoPackage> for Package {
    fn from(pkg: CargoPackage) -> Package {
        Package {
            id: pkg.package_id().clone(),
            targets: pkg.targets().iter().map(|x| x.clone()).collect(),
            features: pkg.summary().features().clone()
        }
    }
}

/// Provides inter-package dependency graph and available packages' info in the
/// workspace scope, along with workspace members and root package ids.
#[derive(Debug)]
pub struct Metadata {
    packages: HashMap<PackageId, Package>,
    resolve: Resolve,
    members: Vec<PackageId>,
    root: Option<PackageId>
}

impl From<ExportInfo> for Metadata {
    fn from(info: ExportInfo) -> Metadata {
        // ExportInfo with deps information will always have `Some` resolve
        let MetadataResolve { resolve, root } = info.resolve.unwrap();

        let packages: HashMap<PackageId, Package> = info.packages
            .iter()
            .map(|x| x.to_owned().into())
            .map(|pkg: Package| (pkg.id.clone(), pkg)) // TODO: Can I borrow key from member of value?
            .collect();

        Metadata {
            packages,
            resolve,
            members: info.workspace_members,
            root,
        }
    }
}

// TODO: Copied for now from Cargo, since it's not fully exposed in the API.
// Remove when appropriate members are exposed.
#[derive(Debug)]
pub struct ExportInfo {
    pub packages: Vec<CargoPackage>,
    pub workspace_members: Vec<PackageId>,
    pub resolve: Option<MetadataResolve>,
    pub target_directory: String,
    pub version: u32,
}

#[derive(Debug)]
pub struct MetadataResolve {
    pub resolve: Resolve,
    pub root: Option<PackageId>,
}

fn metadata_full(ws: &Workspace,
                 opt: &OutputMetadataOptions) -> CargoResult<ExportInfo> {
    let specs = Packages::All.into_package_id_specs(ws)?;
    let deps = ops::resolve_ws_precisely(ws,
                                         None,
                                         &opt.features,
                                         opt.all_features,
                                         opt.no_default_features,
                                         &specs)?;
    let (packages, resolve) = deps;

    let packages = packages.package_ids()
                           .map(|i| packages.get(i).map(|p| p.clone()))
                           .collect::<CargoResult<Vec<_>>>()?;

    Ok(ExportInfo {
        packages: packages,
        workspace_members: ws.members().map(|pkg| pkg.package_id().clone()).collect(),
        resolve: Some(MetadataResolve{
            resolve: resolve,
            root: ws.current_opt().map(|pkg| pkg.package_id().clone()),
        }),
        target_directory: ws.target_dir().display().to_string(),
        version: VERSION,
    })
}
