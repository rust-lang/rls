//! Defines an `AnalysisLoader` trait, which allows to specify directories
//! from which save-analysis JSON files can be read. Also supplies a
//! default implementation `CargoAnalysisLoader` for Cargo-emitted save-analysis
//! files.

use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::AnalysisHost;

#[derive(Debug)]
pub struct CargoAnalysisLoader {
    pub path_prefix: Option<PathBuf>,
    pub target: Target,
}

#[derive(Debug, new)]
pub struct SearchDirectory {
    pub path: PathBuf,
    // The directory searched must have spans re-written to be based on a new
    // path prefix. This happens for example when the std lib crates are compiled
    // on the Rust CI, but live in the user's sysroot directory, this adjustment
    // (which happens in `lower_span`) means we have the new source location.
    pub prefix_rewrite: Option<PathBuf>,
}

impl CargoAnalysisLoader {
    pub fn new(target: Target) -> CargoAnalysisLoader {
        CargoAnalysisLoader { path_prefix: None, target }
    }
}

/// Allows to specify from where and which analysis files will be considered
/// when reloading data to lower.
pub trait AnalysisLoader: Sized {
    fn needs_hard_reload(&self, path_prefix: &Path) -> bool;
    fn fresh_host(&self) -> AnalysisHost<Self>;
    fn set_path_prefix(&mut self, path_prefix: &Path);
    fn abs_path_prefix(&self) -> Option<PathBuf>;
    /// Returns every directory in which analysis files are to be considered.
    fn search_directories(&self) -> Vec<SearchDirectory>;
}

impl AnalysisLoader for CargoAnalysisLoader {
    fn needs_hard_reload(&self, path_prefix: &Path) -> bool {
        self.path_prefix.as_ref().map_or(true, |p| p != path_prefix)
    }

    fn fresh_host(&self) -> AnalysisHost<Self> {
        AnalysisHost::new_with_loader(CargoAnalysisLoader {
            path_prefix: self.path_prefix.clone(),
            ..CargoAnalysisLoader::new(self.target)
        })
    }

    fn set_path_prefix(&mut self, path_prefix: &Path) {
        self.path_prefix = Some(path_prefix.to_owned());
    }

    fn abs_path_prefix(&self) -> Option<PathBuf> {
        self.path_prefix.as_ref().map(|s| Path::new(s).canonicalize().unwrap().to_owned())
    }

    fn search_directories(&self) -> Vec<SearchDirectory> {
        let path_prefix = self.path_prefix.as_ref().unwrap();
        let target = self.target.to_string();

        let deps_path =
            path_prefix.join("target").join("rls").join(&target).join("deps").join("save-analysis");
        // FIXME sys_root_path allows to break out of 'sandbox' - is that Ok?
        // FIXME libs_path and src_path both assume the default `libdir = "lib"`.
        let sys_root_path = sys_root_path();
        let target_triple = extract_target_triple(sys_root_path.as_path());
        let libs_path =
            sys_root_path.join("lib").join("rustlib").join(&target_triple).join("analysis");

        let src_path = sys_root_path.join("lib").join("rustlib").join("src").join("rust");

        vec![SearchDirectory::new(libs_path, Some(src_path)), SearchDirectory::new(deps_path, None)]
    }
}

fn extract_target_triple(sys_root_path: &Path) -> String {
    // First try to get the triple from the rustc version output,
    // otherwise fall back on the rustup-style toolchain path.
    // FIXME: Both methods assume that the target is the host triple,
    // which isn't the case for cross-compilation (rust-lang/rls#309).
    extract_rustc_host_triple().unwrap_or_else(|| extract_rustup_target_triple(sys_root_path))
}

fn extract_rustc_host_triple() -> Option<String> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| String::from("rustc"));
    let verbose_version = Command::new(rustc)
        .arg("--verbose")
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())?;

    // Extracts the triple from a line like `host: x86_64-unknown-linux-gnu`
    verbose_version
        .lines()
        .find(|line| line.starts_with("host: "))
        .and_then(|host| host.split_whitespace().nth(1))
        .map(String::from)
}

// FIXME: This can fail when using a custom toolchain in rustup (often linked to
// `/$rust_repo/build/$target/stage2`)
fn extract_rustup_target_triple(sys_root_path: &Path) -> String {
    // Extracts nightly-x86_64-pc-windows-msvc from
    // $HOME/.rustup/toolchains/nightly-x86_64-pc-windows-msvc
    let toolchain =
        sys_root_path.iter().last().and_then(OsStr::to_str).expect("extracting toolchain failed");
    // Extracts x86_64-pc-windows-msvc from nightly-x86_64-pc-windows-pc
    toolchain.splitn(2, '-').last().map(String::from).expect("extracting triple failed")
}

fn sys_root_path() -> PathBuf {
    env::var("SYSROOT")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            Command::new(env::var("RUSTC").unwrap_or_else(|_| String::from("rustc")))
                .arg("--print")
                .arg("sysroot")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| PathBuf::from(s.trim()))
        })
        .expect("need to specify SYSROOT or RUSTC env vars, or rustc must be in PATH")
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Release,
    Debug,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Target::Release => write!(f, "release"),
            Target::Debug => write!(f, "debug"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn windows_path() {
        let path = Path::new(r#"C:\Users\user\.rustup\toolchains\nightly-x86_64-pc-windows-msvc"#);
        assert_eq!(extract_rustup_target_triple(path), String::from("x86_64-pc-windows-msvc"));
    }

    #[test]
    fn unix_path() {
        let path = Path::new("/home/user/.rustup/toolchains/nightly-x86_64-unknown-linux-gnu");
        assert_eq!(extract_rustup_target_triple(path), String::from("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn target_triple() {
        let sys_root_path = sys_root_path();
        let target_triple = extract_target_triple(&sys_root_path);
        let target_path = sys_root_path.join("lib").join("rustlib").join(&target_triple);
        assert!(target_path.is_dir(), "{:?} is not a directory!", target_path);
    }
}
