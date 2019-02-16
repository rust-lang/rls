//! Code formatting using Rustfmt -- by default using statically-linked one or
//! possibly running Rustfmt binary specified by the user.

use std::env::temp_dir;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use log::debug;
use rand::{distributions, thread_rng, Rng};
use rustfmt_nightly::{Config, Input, Session};
use serde_json;

/// Specifies which `rustfmt` to use.
#[derive(Clone)]
pub enum Rustfmt {
    /// `(path to external `rustfmt`, current working directory to spawn at)`
    External(PathBuf, PathBuf),
    /// Statically linked `rustfmt`.
    Internal,
}

impl From<Option<(String, PathBuf)>> for Rustfmt {
    fn from(value: Option<(String, PathBuf)>) -> Rustfmt {
        match value {
            Some((path, cwd)) => Rustfmt::External(PathBuf::from(path), cwd),
            None => Rustfmt::Internal,
        }
    }
}

impl Rustfmt {
    pub fn format(&self, input: String, cfg: Config) -> Result<String, String> {
        match self {
            Rustfmt::Internal => format_internal(input, cfg),
            Rustfmt::External(path, cwd) => format_external(path, cwd, input, cfg),
        }
    }
}

fn format_external(
    path: &PathBuf,
    cwd: &PathBuf,
    input: String,
    cfg: Config,
) -> Result<String, String> {
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
        let stdin =
            rustfmt.stdin.as_mut().ok_or_else(|| "Failed to open rustfmt stdin".to_string())?;
        stdin
            .write_all(input.as_bytes())
            .map_err(|_| "Failed to pass input to rustfmt".to_string())?;
    }

    rustfmt.wait_with_output().map_err(|err| format!("Error running rustfmt: {}", err)).and_then(
        |out| {
            String::from_utf8(out.stdout)
                .map_err(|_| "Formatted code is not valid UTF-8".to_string())
        },
    )
}

fn format_internal(input: String, config: Config) -> Result<String, String> {
    let mut buf = Vec::<u8>::new();

    {
        let mut session = Session::new(config, Some(&mut buf));

        match session.format(Input::Text(input)) {
            Ok(report) => {
                // `Session::format` returns `Ok` even if there are any errors, i.e., parsing
                // errors.
                if session.has_operational_errors() || session.has_parsing_errors() {
                    debug!("reformat: format_input failed: has errors, report = {}", report);

                    return Err("Reformat failed to complete successfully".into());
                }
            }
            Err(e) => {
                debug!("Reformat failed: {:?}", e);

                return Err("Reformat failed to complete successfully".into());
            }
        }
    }

    String::from_utf8(buf).map_err(|_| "Reformat output is not a valid UTF-8".into())
}

fn random_file() -> Result<(File, PathBuf), String> {
    const SUFFIX_LEN: usize = 10;

    let suffix: String =
        thread_rng().sample_iter(&distributions::Alphanumeric).take(SUFFIX_LEN).collect();
    let path = temp_dir().join(suffix);

    Ok(File::create(&path)
        .map(|file| (file, path))
        .map_err(|_| "Config file could not be created".to_string())?)
}

fn gen_config_file(config: &Config) -> Result<(File, PathBuf), String> {
    let (mut file, path) = random_file()?;
    let toml = config.all_options().to_toml()?;
    file.write(toml.as_bytes())
        .map_err(|_| "Could not write config TOML file contents".to_string())?;

    Ok((file, path))
}

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

    args.push("--config-path".into());
    args.push(config_path.to_str().map(|x| x.to_string()).unwrap());

    args
}
