// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::env;
use std::fs;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str;

use support::paths::TestPathExt;

pub mod harness;
pub mod paths;

#[derive(PartialEq,Clone)]
struct FileBuilder {
    path: PathBuf,
    body: String
}

impl FileBuilder {
    pub fn new(path: PathBuf, body: &str) -> FileBuilder {
        FileBuilder { path: path, body: body.to_string() }
    }

    fn mk(&self) {
        self.dirname().mkdir_p();

        let mut file = fs::File::create(&self.path).unwrap_or_else(|e| {
            panic!("could not create file {}: {}", self.path.display(), e)
        });

        file.write_all(self.body.as_bytes()).unwrap();
    }

    fn dirname(&self) -> &Path {
        self.path.parent().unwrap()
    }
}

#[derive(PartialEq,Clone)]
pub struct Project{
    root: PathBuf,
}

#[must_use]
#[derive(PartialEq,Clone)]
pub struct ProjectBuilder {
    name: String,
    root: Project,
    files: Vec<FileBuilder>,
}

impl ProjectBuilder {
    pub fn new(name: &str, root: PathBuf) -> ProjectBuilder {
        ProjectBuilder {
            name: name.to_string(),
            root: Project{ root },
            files: vec![],
        }
    }

    pub fn file<B: AsRef<Path>>(mut self, path: B,
                                body: &str) -> Self {
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

        for file in self.files.iter() {
            file.mk();
        }

        self.root
    }

    fn rm_root(&self) {
        self.root.root.rm_rf()
    }
}

impl Project {
    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn rls(&self) -> Command {
        let mut cmd = Command::new(rls_exe());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .current_dir(self.root());
        cmd
    }
}

// Generates a project layout
pub fn project(name: &str) -> ProjectBuilder {
    ProjectBuilder::new(name, paths::root().join(name))
}

// Path to cargo executables
pub fn target_conf_dir() -> PathBuf {
    let mut path = env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path
}

pub fn rls_exe() -> PathBuf {
    target_conf_dir().join(format!("rls{}", env::consts::EXE_SUFFIX))
}

pub fn main_file(println: &str, deps: &[&str]) -> String {
    let mut buf = String::new();

    for dep in deps.iter() {
        buf.push_str(&format!("extern crate {};\n", dep));
    }

    buf.push_str("fn main() { println!(");
    buf.push_str(&println);
    buf.push_str("); }\n");

    buf.to_string()
}

pub fn basic_bin_manifest(name: &str) -> String {
    format!(r#"
        [package]
        name = "{}"
        version = "0.5.0"
        authors = ["wycats@example.com"]
        [[bin]]
        name = "{}"
    "#, name, name)
}

pub fn basic_lib_manifest(name: &str) -> String {
    format!(r#"
        [package]
        name = "{}"
        version = "0.5.0"
        authors = ["wycats@example.com"]
        [lib]
        name = "{}"
    "#, name, name)
}