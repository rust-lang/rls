use crate::metadata::{Metadata, Package, PackageId, Resolve, ResolveNode, Target};
use racer_interner::InternedString;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Cached dependencies for racer
#[derive(Clone, Debug)]
pub struct PackageMap {
    manifest_to_idx: HashMap<PathBuf, PackageIdx>,
    id_to_idx: HashMap<PackageId, PackageIdx>,
    packages: Vec<PackageInner>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Edition {
    Ed2015,
    Ed2018,
    Ed2021,
}

impl Edition {
    pub fn from_str(s: &str) -> Self {
        match s {
            "2015" => Edition::Ed2015,
            "2018" => Edition::Ed2018,
            "2021" => Edition::Ed2021,
            _ => unreachable!("got unexpected edition {}", s),
        }
    }
}

#[derive(Clone, Debug)]
struct PackageInner {
    edition: Edition,
    deps: Vec<(InternedString, PathBuf)>,
    lib: Option<Target>,
    id: PackageId,
}

impl PackageInner {
    fn new(ed: InternedString, id: PackageId, lib: Option<Target>) -> Self {
        PackageInner {
            edition: Edition::from_str(ed.as_str()),
            deps: Vec::new(),
            id,
            lib,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageIdx(usize);

impl PackageMap {
    pub fn from_metadata(meta: Metadata) -> Self {
        let Metadata {
            packages, resolve, ..
        } = meta;
        PackageMap::new(packages, resolve)
    }
    pub fn new(packages: Vec<Package>, resolve: Option<Resolve>) -> Self {
        let mut manifest_to_idx = HashMap::new();
        let mut id_to_idx = HashMap::new();
        let mut inner = Vec::new();
        for (i, package) in packages.into_iter().enumerate() {
            let Package {
                id,
                targets,
                manifest_path,
                edition,
                ..
            } = package;
            id_to_idx.insert(id, PackageIdx(i));
            manifest_to_idx.insert(manifest_path, PackageIdx(i));
            let lib = targets.into_iter().find(|t| t.is_lib()).to_owned();
            inner.push(PackageInner::new(edition, id, lib));
        }
        if let Some(res) = resolve {
            construct_deps(res.nodes, &id_to_idx, &mut inner);
        }
        PackageMap {
            manifest_to_idx,
            id_to_idx,
            packages: inner,
        }
    }
    pub fn ids<'a>(&'a self) -> impl 'a + Iterator<Item = PackageId> {
        self.packages.iter().map(|p| p.id)
    }
    pub fn id_to_idx(&self, id: PackageId) -> Option<PackageIdx> {
        self.id_to_idx.get(&id).map(|&x| x)
    }
    pub fn get_idx(&self, path: &Path) -> Option<PackageIdx> {
        self.manifest_to_idx.get(path).map(|&id| id)
    }
    pub fn get_id(&self, idx: PackageIdx) -> PackageId {
        self.packages[idx.0].id
    }
    pub fn get_edition(&self, idx: PackageIdx) -> Edition {
        self.packages[idx.0].edition
    }
    pub fn get_lib(&self, idx: PackageIdx) -> Option<&Target> {
        self.packages[idx.0].lib.as_ref()
    }
    pub fn get_lib_src_path(&self, idx: PackageIdx) -> Option<&Path> {
        self.get_lib(idx).map(|t| t.src_path.as_ref())
    }
    pub fn get_dependencies(&self, idx: PackageIdx) -> &[(InternedString, PathBuf)] {
        self.packages[idx.0].deps.as_ref()
    }
    pub fn get_src_path_from_libname(&self, id: PackageIdx, s: &str) -> Option<&Path> {
        let deps = self.get_dependencies(id);
        let query_str = InternedString::new_if_exists(s)?;
        deps.iter().find(|t| t.0 == query_str).map(|t| t.1.as_ref())
    }
}

fn construct_deps(
    nodes: Vec<ResolveNode>,
    id_to_idx: &HashMap<PackageId, PackageIdx>,
    res: &mut [PackageInner],
) -> Option<()> {
    for node in nodes {
        let idx = id_to_idx.get(&node.id)?;
        let deps: Vec<_> = node
            .dependencies
            .into_iter()
            .filter_map(|id| {
                let idx = id_to_idx.get(&id)?;
                res[idx.0]
                    .lib
                    .as_ref()
                    .map(|l| (l.name, l.src_path.clone()))
            })
            .collect();
        res[idx.0].deps.extend(deps);
    }
    Some(())
}
