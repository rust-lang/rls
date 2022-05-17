extern crate racer_interner;
#[macro_use]
extern crate serde;
extern crate serde_json;

pub mod mapping;
pub mod metadata;

use crate::metadata::Metadata;
use std::env;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::Utf8Error;

#[derive(Debug)]
pub enum ErrorKind {
    Encode(Utf8Error),
    Json(serde_json::Error),
    Io(io::Error),
    Subprocess(String),
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ErrorKind::Encode(e) => fmt::Display::fmt(e, f),
            ErrorKind::Json(e) => fmt::Display::fmt(e, f),
            ErrorKind::Io(e) => fmt::Display::fmt(e, f),
            ErrorKind::Subprocess(s) => write!(f, "stderr: {}", s),
        }
    }
}

impl Error for ErrorKind {}

impl From<Utf8Error> for ErrorKind {
    fn from(e: Utf8Error) -> ErrorKind {
        ErrorKind::Encode(e)
    }
}

impl From<serde_json::Error> for ErrorKind {
    fn from(e: serde_json::Error) -> ErrorKind {
        ErrorKind::Json(e)
    }
}

impl From<io::Error> for ErrorKind {
    fn from(e: io::Error) -> ErrorKind {
        ErrorKind::Io(e)
    }
}

pub fn find_manifest(mut current: &Path) -> Option<PathBuf> {
    let file = "Cargo.toml";
    if current.is_dir() {
        let manifest = current.join(file);
        if manifest.exists() {
            return Some(manifest);
        }
    }
    while let Some(parent) = current.parent() {
        let manifest = parent.join(file);
        if manifest.exists() {
            return Some(manifest);
        }
        current = parent;
    }
    None
}

pub fn run(manifest_path: &Path, frozen: bool) -> Result<Metadata, ErrorKind> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut cmd = Command::new(cargo);
    cmd.arg("metadata");
    cmd.arg("--all-features");
    cmd.args(&["--format-version", "1"]);
    cmd.args(&["--color", "never"]);
    cmd.arg("--manifest-path");
    cmd.arg(manifest_path.as_os_str());
    if frozen {
        cmd.arg("--frozen");
    }
    let op = cmd.output()?;
    if !op.status.success() {
        let stderr = String::from_utf8(op.stderr).map_err(|e| e.utf8_error())?;
        return Err(ErrorKind::Subprocess(stderr));
    }
    serde_json::from_slice(&op.stdout).map_err(From::from)
}
