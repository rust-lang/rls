// Copyright 2018 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Code formatting using Rustfmt - by default using statically-linked one or
//! possibly running Rustfmt binary specified by the user.

use std::env::temp_dir;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rand::{Rng, thread_rng};
use log::{log, debug};
use rustfmt_nightly::{Config, Session, Input};
use serde_json;

struct External<'a>(&'a Path, &'a Path);
struct Internal;

/// Specified which `rustfmt` to use.
pub enum Rustfmt {
    /// (Path to external `rustfmt`, cwd where it should be spawned at)
    External(PathBuf, PathBuf),
    /// Statically linked `rustfmt`
    Internal
}

impl From<Option<(String, PathBuf)>> for Rustfmt {
    fn from(value: Option<(String, PathBuf)>) -> Rustfmt {
        match value {
            Some((path, cwd)) => Rustfmt::External(PathBuf::from(path), cwd),
            None => Rustfmt::Internal
        }
    }
}


pub trait Formatter {
    fn format(&self, input: String, cfg: Config) -> Result<String, String>;
}

impl Formatter for External<'_> {
    fn format(&self, input: String, cfg: Config) -> Result<String, String> {
        let External(path, cwd) = self;

        let (_file_handle, config_path) = gen_config_file(&cfg)?;
        let args = rustfmt_args(&cfg, &config_path);

        let mut rustfmt = Command::new(path)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| format!("Couldn't spawn `{}`", path.display()))?;

        {
            let stdin = rustfmt.stdin.as_mut()
                .ok_or_else(|| "Failed to open rustfmt stdin".to_string())?;
            stdin.write_all(input.as_bytes())
                .map_err(|_| "Failed to pass input to rustfmt".to_string())?;
        }

        rustfmt.wait_with_output()
            .map_err(|err| format!("Error running rustfmt: {}", err))
            .and_then(|out| String::from_utf8(out.stdout)
                .map_err(|_| "Formatted code is not valid UTF-8".to_string()))
    }
}

impl Formatter for Internal {
    fn format(&self, input: String, config: Config) -> Result<String, String> {
        let mut buf = Vec::<u8>::new();

        {
            let mut session = Session::new(config, Some(&mut buf));

            match session.format(Input::Text(input)) {
                Ok(report) => {
                    // Session::format returns Ok even if there are any errors, i.e., parsing errors.
                    if session.has_operational_errors() || session.has_parsing_errors() {
                        debug!(
                            "reformat: format_input failed: has errors, report = {}",
                            report
                        );

                        return Err("Reformat failed to complete successfully".into());
                    }
                }
                Err(e) => {
                    debug!("Reformat failed: {:?}", e);

                    return Err("Reformat failed to complete successfully".into());
                }
            }
        }

        String::from_utf8(buf)
            .map_err(|_| "Reformat output is not a valid UTF-8".into())
    }
}

impl Formatter for Rustfmt {
    fn format(&self, input: String, cfg: Config) -> Result<String, String> {
        match self {
            Rustfmt::Internal => Internal.format(input, cfg),
            Rustfmt::External(path, cwd) => {
                External(path.as_path(), cwd.as_path()).format(input, cfg)
            }
        }
    }
}

#[allow(dead_code)]
fn random_file() -> Result<(File, PathBuf), String> {
    const SUFFIX_LEN: usize = 10;

    let suffix: String = thread_rng().gen_ascii_chars().take(SUFFIX_LEN).collect();
    let path = temp_dir().join(suffix);

    Ok(File::create(&path)
        .map(|file| (file, path))
        .map_err(|_| "Config file could not be created".to_string())?)
}

#[allow(dead_code)]
fn gen_config_file(config: &Config) -> Result<(File, PathBuf), String> {
    let (mut file, path) = random_file()?;
    let toml = config.used_options().to_toml()?;
    file.write(toml.as_bytes())
        .map_err(|_| "Could not write config TOML file contents".to_string())?;

    Ok((file, path))
}

#[allow(unused_variables)]
fn rustfmt_args(config: &Config, config_path: &Path) -> Vec<String> {
    let mut args = vec![
        "--unstable-features".into(),
        "--skip-children".into(),
        "--emit".into(),
        "stdout".into(),
        "--quiet".into(),
    ];

    args.push("--file-lines".into());
    let file_lines_json = config.file_lines().to_json_spans();
    let lines: String = serde_json::to_string(&file_lines_json).unwrap();
    args.push(lines);

    // We will spawn Rustfmt at the project directory and so it should pick up
    // appropriate config file on its own.
    // FIXME: Since in format request handling we modify some of the config and
    // pass it via `Formatter::format()`, we should ideally fix `gen_config_file`
    // and make it so that `used_options()` will give accurate TOML
    // representation for the current format request.

    // args.push("--config-path".into());
    // args.push(config_path.to_str().map(|x| x.to_string()).unwrap());

    args
}
