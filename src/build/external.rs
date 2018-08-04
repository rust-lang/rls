// Copyright 2018 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Performs a build using a provided black-box build command, which ought to
//! return a list of save-analysis JSON files to be reloaded by the RLS.
//! Please note that since the command is ran externally (at a file/OS level)
//! this doesn't work with files that are not saved.

use std::io::Read;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::BuildResult;

use rls_data::Analysis;
use log::{log, trace};

/// Performs a build using an external command and interprets the results.
/// The command should output on stdout a list of save-analysis .json files
/// to be reloaded by the RLS.
/// Note: This is *very* experimental and preliminary - this can viewed as
/// an experimentation until a more complete solution emerges.
pub(super) fn build_with_external_cmd<S: AsRef<str>>(path: S, build_dir: PathBuf) -> BuildResult {
    let path = path.as_ref();
    let (cmd, args) = {
        let mut words = path.split_whitespace();
        let cmd = match words.next() {
            Some(cmd) => cmd,
            None => {
                return BuildResult::Err("Specified build_command is empty".into(), None);
            }
        };
        (cmd, words)
    };
    let spawned = Command::new(&cmd)
        .args(args)
        .current_dir(&build_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let child = match spawned {
        Ok(child) => child,
        Err(io) => {
            let err_msg = format!("Couldn't execute: {} ({:?})", path, io.kind());
            trace!("{}", err_msg);
            return BuildResult::Err(err_msg, Some(path.to_owned()));
        },
    };

    // TODO: Timeout?
    let reader = std::io::BufReader::new(child.stdout.unwrap());
    use std::io::BufRead;

    let files = reader.lines().filter_map(|res| res.ok())
        .map(PathBuf::from)
        // Relative paths are relative to build command, not RLS itself (cwd may be different)
        .map(|path| if !path.is_absolute() { build_dir.join(path) } else { path });

    let analyses = match read_analysis_files(files) {
        Ok(analyses) => analyses,
        Err(cause) => {
            let err_msg = format!("Couldn't read analysis data: {}", cause);
            return BuildResult::Err(err_msg, Some(path.to_owned()));
        }
    };

    BuildResult::Success(build_dir.clone(), vec![], analyses, false)
}

/// Reads and deserializes given save-analysis JSON files into corresponding
/// `rls_data::Analysis` for each file. If an error is encountered, a `String`
/// with the error message is returned.
fn read_analysis_files<I>(files: I) -> Result<Vec<Analysis>, String>
where
    I: Iterator,
    I::Item: AsRef<Path>
{
    let mut analyses = Vec::new();

    for path in files {
        trace!("external::read_analysis_files: Attempt to read `{}`", path.as_ref().display());

        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|e| e.to_string())?;

        let data = rustc_serialize::json::decode(&contents).map_err(|e| e.to_string())?;
        analyses.push(data);
    }

    Ok(analyses)
}
