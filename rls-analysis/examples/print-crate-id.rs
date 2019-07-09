extern crate env_logger;
extern crate rls_analysis;

use rls_analysis::{AnalysisHost, AnalysisLoader, SearchDirectory};
use std::env;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Loader {
    deps_dir: PathBuf,
}

impl Loader {
    pub fn new(deps_dir: PathBuf) -> Self {
        Self { deps_dir }
    }
}

impl AnalysisLoader for Loader {
    fn needs_hard_reload(&self, _: &Path) -> bool {
        true
    }

    fn fresh_host(&self) -> AnalysisHost<Self> {
        AnalysisHost::new_with_loader(self.clone())
    }

    fn set_path_prefix(&mut self, _: &Path) {}

    fn abs_path_prefix(&self) -> Option<PathBuf> {
        None
    }
    fn search_directories(&self) -> Vec<SearchDirectory> {
        vec![SearchDirectory { path: self.deps_dir.clone(), prefix_rewrite: None }]
    }
}

fn main() {
    env_logger::init();
    if env::args().len() < 2 {
        println!("Usage: print-crate-id <save-analysis-dir>");
        std::process::exit(1);
    }
    let loader = Loader::new(PathBuf::from(env::args().nth(1).unwrap()));
    let crates =
        rls_analysis::read_analysis_from_files(&loader, Default::default(), &[] as &[&str]);

    for krate in &crates {
        println!("Crate {:?} data version {:?}", krate.id, krate.analysis.version);
    }
}
