use lazycell;
extern crate racer_cargo_metadata as metadata;
use self::lazycell::LazyCell;
use self::metadata::mapping::{Edition as Ed, PackageIdx, PackageMap};
use crate::project_model::{Edition, ProjectModelProvider};
use std::cell::Cell;
use std::path::{Path, PathBuf};

struct MetadataCache {
    pkg_map: LazyCell<PackageMap>,
    manifest_path: Option<PathBuf>,
    failed_to_fill: Cell<bool>,
}

impl MetadataCache {
    fn new(manifest_path: Option<PathBuf>) -> Self {
        MetadataCache {
            pkg_map: LazyCell::new(),
            manifest_path,
            failed_to_fill: Cell::new(false),
        }
    }
    fn fill_impl(&self, manifest: &Path) -> Result<(), ()> {
        let meta = metadata::run(manifest, true)
            .or_else(|e| {
                if let metadata::ErrorKind::Subprocess(ref s) = e {
                    // HACK: if --frozen failed, try again without --frozen
                    // see https://github.com/rust-lang/cargo/blob/master/src/cargo/ops/registry.rs#L344
                    if s.contains("--frozen") {
                        info!("MetadataCache: try again without --frozen");
                        return metadata::run(manifest, false);
                    }
                }
                Err(e)
            })
            .map_err(|e| {
                warn!("Error in cargo metadata: {}", e);
            })?;
        let pkg_map = PackageMap::from_metadata(meta);
        self.pkg_map.fill(pkg_map).map_err(|_| {
            warn!("Error in initialize lazy cell");
        })
    }
    fn fill(&self, manifest: &Path) {
        if !self.pkg_map.filled() && !self.failed_to_fill.get() {
            self.failed_to_fill.set(self.fill_impl(manifest).is_err());
        }
    }
    fn setup(&self, manifest: &Path) -> Option<(&PackageMap, PackageIdx)> {
        self.fill(manifest);
        let pkg_map: &PackageMap = self.pkg_map.borrow().unwrap();
        let idx = if manifest.is_relative() {
            let path = manifest.canonicalize().ok()?;
            pkg_map.get_idx(&path)?
        } else {
            pkg_map.get_idx(manifest)?
        };
        Some((pkg_map, idx))
    }
}

impl ProjectModelProvider for MetadataCache {
    fn edition(&self, manifest: &Path) -> Option<Edition> {
        let (pkg_map, idx) = self.setup(manifest)?;
        let edition = pkg_map.get_edition(idx);
        Some(match edition {
            Ed::Ed2015 => Edition::Ed2015,
            Ed::Ed2018 => Edition::Ed2018,
            Ed::Ed2021 => Edition::Ed2021,
        })
    }
    fn discover_project_manifest(&self, path: &Path) -> Option<PathBuf> {
        let cur_manifest = metadata::find_manifest(path)?;
        let manifest = self.manifest_path.as_ref()?;
        self.fill(manifest);
        Some(cur_manifest)
    }
    fn search_dependencies(
        &self,
        manifest: &Path,
        search_fn: Box<dyn Fn(&str) -> bool>,
    ) -> Vec<(String, PathBuf)> {
        let (pkg_map, idx) = match self.setup(manifest) {
            Some(x) => x,
            None => return vec![],
        };
        let deps = pkg_map
            .get_dependencies(idx)
            .iter()
            .filter(|(s, _)| search_fn(s))
            .map(|(s, p)| (s.to_string(), p.to_path_buf()));
        let lib = pkg_map
            .get_lib(idx)
            .filter(|t| search_fn(&t.name))
            .map(|t| (t.name.to_string(), t.src_path.to_path_buf()));
        deps.chain(lib).collect()
    }
    fn resolve_dependency(&self, manifest: &Path, libname: &str) -> Option<PathBuf> {
        debug!(
            "MetadataCache::resolve_dependency manifest: {:?} libname: {}",
            manifest, libname
        );
        let (pkg_map, idx) = self.setup(manifest)?;
        pkg_map
            .get_src_path_from_libname(idx, libname)
            .or_else(|| {
                let hyphnated = libname.replace('_', "-");
                pkg_map.get_src_path_from_libname(idx, &hyphnated)
            })
            .or_else(|| {
                let target = pkg_map.get_lib(idx)?;
                if target.name.replace('-', "_") == libname {
                    Some(&target.src_path)
                } else {
                    None
                }
            })
            .map(|p| p.to_owned())
    }
}

pub fn project_model(project_path: Option<&Path>) -> Box<dyn ProjectModelProvider> {
    let manifest = project_path.and_then(|p| metadata::find_manifest(p));
    Box::new(MetadataCache::new(manifest))
}
