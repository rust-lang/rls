//! Contains helper types that are capable of dynamically creating project
//! layouts under target/ for testing purposes.
//! This module is currently pulled by main binary and Cargo integration tests.
#![allow(dead_code)]

use walkdir::WalkDir;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::paths::{self, TestPathExt};

#[derive(PartialEq, Clone)]
struct FileBuilder {
    path: PathBuf,
    body: String,
}

impl FileBuilder {
    pub fn new(path: PathBuf, body: &str) -> FileBuilder {
        FileBuilder { path, body: body.to_string() }
    }

    fn mk(&self) {
        self.dirname().mkdir_p();

        let mut file = fs::File::create(&self.path)
            .unwrap_or_else(|e| panic!("could not create file {}: {}", self.path.display(), e));

        file.write_all(self.body.as_bytes()).unwrap();
    }

    fn dirname(&self) -> &Path {
        self.path.parent().unwrap()
    }
}

#[derive(PartialEq, Clone)]
pub struct Project {
    root: PathBuf,
}

#[must_use]
#[derive(PartialEq, Clone)]
pub struct ProjectBuilder {
    root: Project,
    files: Vec<FileBuilder>,
}

impl ProjectBuilder {
    pub fn new(root: PathBuf) -> ProjectBuilder {
        ProjectBuilder { root: Project { root }, files: vec![] }
    }

    pub fn try_from_fixture(fixture_dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let fixture_dir = fixture_dir.as_ref();

        let dirname = fixture_dir
            .file_name()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "No filename"))?;

        // Generate a new, unique directory for working dir under target/
        let genroot = paths::root();
        let mut builder = ProjectBuilder::new(genroot.join(dirname));

        // Read existing fixture data to be later copied into scratch genroot
        for entry in WalkDir::new(fixture_dir).into_iter() {
            let entry = entry?;
            let path = entry.path();
            let body = if !std::fs::metadata(path)?.is_dir() {
                std::fs::read_to_string(path)?
            } else {
                continue;
            };

            let relative = entry.path().strip_prefix(fixture_dir).unwrap();
            builder._file(relative, &body);
        }

        Ok(builder)
    }

    pub fn file<B: AsRef<Path>>(mut self, path: B, body: &str) -> Self {
        self._file(path.as_ref(), body);
        self
    }

    fn _file(&mut self, path: &Path, body: &str) {
        self.files.push(FileBuilder::new(self.root.root.join(path), body));
    }

    pub fn build(self) -> Project {
        // First, clean the directory if it already exists
        self.rm_root();

        // Create the empty directory
        self.root.root.mkdir_p();

        for file in &self.files {
            file.mk();
        }

        self.root
    }

    fn rm_root(&self) {
        self.root.root.rm_rf()
    }
}

impl Project {
    pub fn root(&self) -> &Path {
        &self.root
    }
}

// Generates a project layout
pub fn project(name: &str) -> ProjectBuilder {
    ProjectBuilder::new(paths::root().join(name))
}
