use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Edition {
    Ed2015,
    Ed2018,
    Ed2021,
}

pub trait ProjectModelProvider {
    fn edition(&self, manifest: &Path) -> Option<Edition>;
    fn discover_project_manifest(&self, path: &Path) -> Option<PathBuf>;
    fn search_dependencies(
        &self,
        manifest: &Path,
        search_fn: Box<dyn Fn(&str) -> bool>,
    ) -> Vec<(String, PathBuf)>;
    fn resolve_dependency(&self, manifest: &Path, dep_name: &str) -> Option<PathBuf>;
}
