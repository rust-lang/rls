/// This module represents the RLS view of the Cargo project model:
/// a graph of interdependent packages.
use std::{
    collections::HashMap,
    sync::Arc,
    path::{Path, PathBuf},
};
use log::{log, warn};
use rls_vfs::{Vfs, FileContents};
use cargo::{
    Config,
    ops,
    util::{errors::CargoResult, important_paths::find_root_manifest_for_wd, toml},
    core::{
        Workspace, PackageId, registry::PackageRegistry,
        resolver::{EncodableResolve, Method, Resolve},
    }
};
use racer;

#[derive(Debug)]
pub struct ProjectModel {
    packages: Vec<PackageData>,
}

#[derive(Debug, Clone, Copy)]
pub struct Package(usize);

#[derive(Debug)]
struct PackageData {
    manifest: PathBuf,
    lib: Option<PathBuf>,
    deps: Vec<Dep>,
}

#[derive(Debug)]
pub struct Dep {
    pub crate_name: String,
    pub pkg: Package,
}

impl ProjectModel {
    pub fn load(manifest: &Path, vfs: &Vfs) -> Result<ProjectModel, failure::Error> {
        assert!(manifest.ends_with("Cargo.toml"));
        let mut config = Config::default()?;
        // frozen=true, locked=true
        config.configure(0, Some(true), &None, true, true, &None, &[])?;
        let ws = Workspace::new(&manifest, &config)?;
        // get resolve from lock file
        let prev = {
            let lock_path = ws.root().to_owned().join("Cargo.lock");
            match vfs.load_file(&lock_path) {
                Ok(FileContents::Text(lockfile)) => {
                    let resolve = toml::parse(&lockfile, &lock_path, ws.config())?;
                    let v: EncodableResolve = resolve.try_into()?;
                    Some(v.into_resolve(&ws)?)
                }
                _ => None
            }
        };
        // then resolve precisely and add overrides
        let mut registry = PackageRegistry::new(ws.config())?;
        let resolve = resolve_with_prev(&mut registry, &ws, prev.as_ref())?;
        let cargo_packages = {
            let ids: Vec<PackageId> = resolve.iter().cloned().collect();
            registry.get(&ids)
        };

        let mut pkg_id_to_pkg = HashMap::new();
        let mut packages = Vec::new();
        for (idx, pkg_id) in resolve.iter().enumerate() {
            let pkg = Package(idx);
            pkg_id_to_pkg.insert(pkg_id.clone(), pkg);
            let cargo_pkg = cargo_packages.get(pkg_id)?;
            packages.push(PackageData {
                manifest: cargo_pkg.manifest_path().to_owned(),
                lib: cargo_pkg.targets().iter()
                    .find(|t| t.is_lib())
                    .map(|t| t.src_path().to_owned()),
                deps: Vec::new(),
            })
        }
        for pkg_id in resolve.iter() {
            let deps = resolve.deps(&pkg_id)
                .filter_map(|(dep_id, dep_specs)| {
                    let crate_name = dep_specs.iter()
                        .map(|d| d.name_in_toml().to_string())
                        .next()?;
                    Some(Dep {
                        crate_name,
                        pkg: pkg_id_to_pkg[dep_id],
                    })
                }).collect::<Vec<_>>();
            packages[pkg_id_to_pkg[pkg_id].0].deps = deps;
        }
        Ok(ProjectModel { packages })
    }

    pub fn package_for_manifest(&self, manifest_path: &Path) -> Option<Package> {
        self.packages.iter()
            .enumerate()
            .find(|(_idx, p)| p.manifest == manifest_path)
            .map(|(idx, _p)| Package(idx))
    }

    fn get(&self, pkg: Package) -> &PackageData {
        &self.packages[pkg.0]
    }

}

impl Package {
    pub fn deps(self, project: &ProjectModel) -> &[Dep] {
        &project.get(self).deps
    }
    pub fn lib_root(self, project: &ProjectModel) -> Option<&Path> {
        project.get(self).lib.as_ref().map(|p| p.as_path())
    }
}

// We use the following wrappers to teach Racer about the structure
// of the project.

pub struct RacerProjectModel(pub Arc<ProjectModel>);

impl racer::ProjectModelProvider for RacerProjectModel {
    fn discover_project_manifest(&self, path: &Path) -> Option<PathBuf> {
        match find_root_manifest_for_wd(path) {
            Ok(val) => Some(val),
            Err(err) => {
                warn!("Error in cargo: {}", err);
                return None;
            }
        }
    }
    fn resolve_dependency(&self, manifest: &Path, libname: &str) -> Option<PathBuf> {
        let pkg = self.0.package_for_manifest(manifest)?;
        let dep = pkg.deps(&self.0)
            .iter()
            .find(|dep| dep.crate_name == libname)?
            .pkg;

        dep.lib_root(&self.0).map(|p| p.to_owned())
    }
}

pub struct RacerFallbackModel;

impl racer::ProjectModelProvider for RacerFallbackModel {
    fn discover_project_manifest(&self, _path: &Path) -> Option<PathBuf> {
        None
    }
    fn resolve_dependency(&self, _manifest: &Path, _libname: &str) -> Option<PathBuf> {
        None
    }
}

// wrapper of resolve_with_previous
fn resolve_with_prev<'cfg>(
    registry: &mut PackageRegistry<'cfg>,
    ws: &Workspace<'cfg>,
    prev: Option<&Resolve>,
) -> CargoResult<Resolve> {
    ops::resolve_with_previous(
        registry,
        ws,
        Method::Everything,
        prev,
        None,
        &[],
        true,
        false,
    )
}
