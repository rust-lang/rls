//! This module represents the RLS view of the Cargo project model:
//! a graph of interdependent packages.
use cargo::{
    core::{
        registry::PackageRegistry,
        resolver::{CliFeatures, EncodableResolve, HasDevUnits, Resolve},
        PackageId, Workspace,
    },
    ops,
    util::{errors::CargoResult, important_paths::find_root_manifest_for_wd, toml},
    Config,
};
use log::warn;
use rls_vfs::{FileContents, Vfs};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub struct ProjectModel {
    manifest_to_id: HashMap<PathBuf, Package>,
    packages: Vec<PackageData>,
}

#[derive(Debug, Clone, Copy)]
pub struct Package(usize);

#[derive(Debug)]
struct PackageData {
    lib: Option<(PathBuf, String)>,
    deps: Vec<Dep>,
    edition: racer::Edition,
}

#[derive(Debug)]
pub struct Dep {
    pub crate_name: String,
    pub pkg: Package,
}

impl ProjectModel {
    pub fn load(ws_manifest: &Path, vfs: &Vfs) -> Result<ProjectModel, anyhow::Error> {
        assert!(ws_manifest.ends_with("Cargo.toml"));
        let mut config = Config::default()?;
        // Enable nightly flag for cargo(see #1043)
        config.nightly_features_allowed = true;
        // frozen = false, locked = false, offline = false
        config.configure(0, true, None, false, false, false, &None, &[], &[])?;
        let ws = Workspace::new(&ws_manifest, &config)?;
        // get resolve from lock file
        let prev = {
            let lock_path = ws.root().to_owned().join("Cargo.lock");
            match vfs.load_file(&lock_path) {
                Ok(FileContents::Text(lockfile)) => {
                    let resolve = toml::parse(&lockfile, &lock_path, ws.config())?;
                    let v: EncodableResolve = resolve.try_into()?;
                    Some(v.into_resolve(&lockfile, &ws)?)
                }
                _ => None,
            }
        };
        let mut registry = PackageRegistry::new(ws.config())?;
        let resolve = resolve_with_prev(&mut registry, &ws, prev.as_ref())?;
        let cargo_packages = {
            let ids: Vec<PackageId> = resolve.iter().collect();
            registry.get(&ids)?
        };
        let mut pkg_id_to_pkg = HashMap::new();
        let mut manifest_to_id = HashMap::new();
        let mut packages = Vec::new();
        for (idx, pkg_id) in resolve.iter().enumerate() {
            let pkg = Package(idx);
            pkg_id_to_pkg.insert(pkg_id, pkg);
            let cargo_pkg = cargo_packages.get_one(pkg_id)?;
            let manifest = cargo_pkg.manifest_path().to_owned();
            packages.push(PackageData {
                lib: cargo_pkg
                    .targets()
                    .iter()
                    .find(|t| t.is_lib())
                    // racer expect name 'underscored'(crate) name
                    .map(|t| {
                        (
                            t.src_path().path().expect("lib must have a path").to_owned(),
                            t.name().replace('-', "_"),
                        )
                    }),
                deps: Vec::new(),
                edition: match cargo_pkg.manifest().edition() {
                    cargo::core::Edition::Edition2015 => racer::Edition::Ed2015,
                    cargo::core::Edition::Edition2018 => racer::Edition::Ed2018,
                    // FIXME: Use Racer's Ed2021 once
                    // https://github.com/racer-rust/racer/pull/1152 is published.
                    cargo::core::Edition::Edition2021 => racer::Edition::Ed2018,
                },
            });
            manifest_to_id.insert(manifest, pkg);
        }
        for pkg_id in resolve.iter() {
            for (dep_id, _) in resolve.deps(pkg_id) {
                let pkg = cargo_packages.get_one(dep_id)?;
                let lib = pkg.targets().iter().find(|t| t.is_lib());
                if let Some(lib) = lib {
                    let crate_name = resolve.extern_crate_name(pkg_id, dep_id, &lib)?;
                    packages[pkg_id_to_pkg[&pkg_id].0]
                        .deps
                        .push(Dep { crate_name, pkg: pkg_id_to_pkg[&dep_id] })
                }
            }
        }
        Ok(ProjectModel { manifest_to_id, packages })
    }

    pub fn package_for_manifest(&self, manifest_path: &Path) -> Option<Package> {
        self.manifest_to_id.get(manifest_path).cloned()
    }

    fn get(&self, pkg: Package) -> &PackageData {
        &self.packages[pkg.0]
    }

    fn get_lib(&self, pkg: Package) -> Option<&(PathBuf, String)> {
        self.packages[pkg.0].lib.as_ref()
    }
}

impl Package {
    pub fn deps(self, project: &ProjectModel) -> &[Dep] {
        &project.get(self).deps
    }
    pub fn lib_root(self, project: &ProjectModel) -> Option<&Path> {
        project.get(self).lib.as_ref().map(|p| p.0.as_path())
    }
}

// We use the following wrappers to teach Racer about the structure
// of the project.

pub struct RacerProjectModel(pub Arc<ProjectModel>);

impl racer::ProjectModelProvider for RacerProjectModel {
    fn edition(&self, manifest: &Path) -> Option<racer::Edition> {
        self.0.package_for_manifest(manifest).map(|pkg| self.0.get(pkg).edition)
    }

    fn search_dependencies(
        &self,
        manifest: &Path,
        search_fn: Box<dyn Fn(&str) -> bool>,
    ) -> Vec<(String, PathBuf)> {
        let pkg = match self.0.package_for_manifest(manifest) {
            Some(pkg) => pkg,
            None => return vec![],
        };

        pkg.deps(&self.0)
            .iter()
            .filter(|d| search_fn(&d.crate_name))
            .filter_map(|d| self.0.get(d.pkg).lib.as_ref())
            .cloned()
            .map(|(src_path, crate_name)| (crate_name, src_path))
            .collect()
    }

    fn discover_project_manifest(&self, path: &Path) -> Option<PathBuf> {
        match find_root_manifest_for_wd(path) {
            Ok(val) => Some(val),
            Err(err) => {
                warn!("Error in cargo: {}", err);
                None
            }
        }
    }
    fn resolve_dependency(&self, manifest: &Path, libname: &str) -> Option<PathBuf> {
        let pkg = self.0.package_for_manifest(manifest)?;
        // if current package has a library target, we have to provide its own name
        // in examples/tests/benches directory
        if let Some(lib) = self.0.get_lib(pkg) {
            if lib.1 == libname {
                return Some(lib.0.clone());
            }
        }
        let dep = pkg.deps(&self.0).iter().find(|dep| dep.crate_name == libname)?.pkg;

        dep.lib_root(&self.0).map(ToOwned::to_owned)
    }
}

pub struct RacerFallbackModel;

impl racer::ProjectModelProvider for RacerFallbackModel {
    fn edition(&self, _manifest: &Path) -> Option<racer::Edition> {
        None
    }

    fn search_dependencies(
        &self,
        _manifest: &Path,
        _search_fn: Box<dyn Fn(&str) -> bool>,
    ) -> Vec<(String, PathBuf)> {
        Vec::new()
    }

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
        &CliFeatures::new_all(true),
        HasDevUnits::Yes,
        prev,
        None,
        &[],
        true,
    )
}
