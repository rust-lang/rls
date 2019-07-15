use std::io;
use std::path::{Path, PathBuf};

use crate::syntax::source_map::FileLoader;

pub struct IpcFileLoader;

impl FileLoader for IpcFileLoader {
    fn file_exists(&self, _path: &Path) -> bool {
        unimplemented!()
    }

    fn abs_path(&self, _path: &Path) -> Option<PathBuf> {
        unimplemented!()
    }

    fn read_file(&self, _path: &Path) -> io::Result<String> {
        unimplemented!()
    }
}
