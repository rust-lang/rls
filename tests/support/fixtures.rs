use std::path::{Path, PathBuf};

use lazy_static::lazy_static;

lazy_static! {
    static ref MANIFEST_DIR: &'static Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    pub static ref FIXTURES_DIR: PathBuf = MANIFEST_DIR.join("tests/fixtures");
}