#![feature(test)]

extern crate rls_analysis;
#[macro_use]
extern crate derive_new;
#[macro_use]
extern crate lazy_static;
extern crate test;
use test::Bencher;

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use rls_analysis::{AnalysisHost, AnalysisLoader, SearchDirectory};

#[derive(Clone, new)]
struct TestAnalysisLoader {
    path: PathBuf,
}

impl AnalysisLoader for TestAnalysisLoader {
    fn needs_hard_reload(&self, _path_prefix: &Path) -> bool {
        true
    }

    fn fresh_host(&self) -> AnalysisHost<Self> {
        AnalysisHost::new_with_loader(self.clone())
    }

    fn set_path_prefix(&mut self, _path_prefix: &Path) {}

    fn abs_path_prefix(&self) -> Option<PathBuf> {
        panic!();
    }

    fn search_directories(&self) -> Vec<SearchDirectory> {
        vec![SearchDirectory::new(self.path.clone(), None)]
    }
}

lazy_static! {
    static ref STDLIB_FILE_PATH: PathBuf = PathBuf::from("/checkout/src/libstd/lib.rs");
    static ref STDLIB_DATA_PATH: PathBuf = PathBuf::from("test_data/rust-analysis");
    static ref HOST: RwLock<AnalysisHost<TestAnalysisLoader>> = {
        let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(STDLIB_DATA_PATH.clone()));
        host.reload(&STDLIB_DATA_PATH, &STDLIB_DATA_PATH).unwrap();
        RwLock::new(host)
    };
}

#[bench]
fn search_for_id(b: &mut Bencher) {
    let host = HOST.read().unwrap();

    b.iter(|| {
        let _ = host.search_for_id("no_std");
    });
}

#[bench]
fn search(b: &mut Bencher) {
    let host = HOST.read().unwrap();
    b.iter(|| {
        let _ = host.search("some_inexistent_symbol");
    })
}

#[bench]
fn symbols(b: &mut Bencher) {
    let host = HOST.read().unwrap();
    b.iter(|| {
        let _ = host.symbols(&STDLIB_FILE_PATH);
    })
}

#[bench]
fn reload(b: &mut Bencher) {
    let host = HOST.write().unwrap();
    b.iter(|| {
        host.reload(&STDLIB_DATA_PATH, &STDLIB_DATA_PATH).unwrap();
    })
}
