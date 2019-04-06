//! Code formatting using Rustfmt -- by default using statically-linked one or
//! possibly running Rustfmt binary specified by the user.

use std::env::temp_dir;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::string::FromUtf8Error;

use log::debug;
use lsp_types::{Position, Range, TextEdit};
use rand::{distributions, thread_rng, Rng};
use rustfmt_nightly::{Config, Input, ModifiedLines, NewlineStyle, Session};
use serde_json;

/// Specifies which `rustfmt` to use.
#[derive(Clone)]
pub enum Rustfmt {
    /// Externally invoked `rustfmt` process.
    External { path: PathBuf, cwd: PathBuf },
    /// Statically linked `rustfmt`.
    Internal,
}

/// Defines a formatting-related error.
#[derive(Fail, Debug)]
pub enum Error {
    /// Generic variant of `Error::Rustfmt` error.
    #[fail(display = "Formatting could not be completed.")]
    Failed,
    #[fail(display = "Could not format source code: {}", _0)]
    Rustfmt(rustfmt_nightly::ErrorKind),
    #[fail(display = "Encountered I/O error: {}", _0)]
    Io(std::io::Error),
    #[fail(display = "Config couldn't be converted to TOML for Rustfmt purposes: {}", _0)]
    ConfigTomlOutput(String),
    #[fail(display = "Formatted output is not valid UTF-8 source: {}", _0)]
    OutputNotUtf8(FromUtf8Error),
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Error {
        Error::Io(err)
    }
}

impl From<FromUtf8Error> for Error {
    fn from(err: FromUtf8Error) -> Error {
        Error::OutputNotUtf8(err)
    }
}

impl From<Option<(String, PathBuf)>> for Rustfmt {
    fn from(value: Option<(String, PathBuf)>) -> Rustfmt {
        match value {
            Some((path, cwd)) => Rustfmt::External { path: PathBuf::from(path), cwd },
            None => Rustfmt::Internal,
        }
    }
}

impl Rustfmt {
    pub fn format(&self, input: String, cfg: Config) -> Result<String, Error> {
        match self {
            Rustfmt::Internal => format_internal(input, cfg),
            Rustfmt::External { path, cwd } => format_external(path, cwd, input, cfg),
        }
    }

    pub fn calc_text_edits(&self, input: String, mut cfg: Config) -> Result<Vec<TextEdit>, Error> {
        cfg.set().emit_mode(rustfmt_nightly::EmitMode::ModifiedLines);

        let native = if cfg!(windows) { "\r\n" } else { "\n" };
        let newline = match cfg.newline_style() {
            NewlineStyle::Windows => "\r\n",
            NewlineStyle::Unix | NewlineStyle::Auto => "\n",
            NewlineStyle::Native => native,
        };

        let output = self.format(input, cfg)?;
        let ModifiedLines { chunks } = output.parse().map_err(|_| Error::Failed)?;

        Ok(chunks
            .into_iter()
            .map(|item| {
                // Rustfmt's line indices are 1-based
                let start_line = u64::from(item.line_number_orig) - 1;
                // Could underflow if we don't remove lines and there's only one
                let removed = u64::from(item.lines_removed).saturating_sub(1);
                TextEdit {
                    range: Range {
                        start: Position::new(start_line, 0),
                        // We don't extend the range past the last line because
                        // sometimes it may not exist, skewing the diff and
                        // making us add an invalid additional trailing newline.
                        end: Position::new(start_line + removed, u64::max_value()),
                    },
                    new_text: item.lines.join(newline),
                }
            })
            .collect())
    }
}

fn format_external(
    path: &PathBuf,
    cwd: &PathBuf,
    input: String,
    cfg: Config,
) -> Result<String, Error> {
    let (_file_handle, config_path) = gen_config_file(&cfg)?;
    let args = rustfmt_args(&cfg, &config_path);

    let mut rustfmt = Command::new(path)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(Error::Io)?;

    {
        let stdin = rustfmt.stdin.as_mut().unwrap(); // Safe because stdin is piped
        stdin.write_all(input.as_bytes())?;
    }

    let output = rustfmt.wait_with_output()?;
    Ok(String::from_utf8(output.stdout)?)
}

fn format_internal(input: String, config: Config) -> Result<String, Error> {
    let mut buf = Vec::<u8>::new();

    {
        let mut session = Session::new(config, Some(&mut buf));

        match session.format(Input::Text(input)) {
            Ok(report) => {
                // `Session::format` returns `Ok` even if there are any errors, i.e., parsing
                // errors.
                if session.has_operational_errors() || session.has_parsing_errors() {
                    debug!("reformat: format_input failed: has errors, report = {}", report);

                    return Err(Error::Failed);
                }
            }
            Err(e) => {
                debug!("Reformat failed: {:?}", e);

                return Err(Error::Rustfmt(e));
            }
        }
    }

    Ok(String::from_utf8(buf)?)
}

fn random_file() -> Result<(File, PathBuf), Error> {
    const SUFFIX_LEN: usize = 10;

    let suffix: String =
        thread_rng().sample_iter(&distributions::Alphanumeric).take(SUFFIX_LEN).collect();
    let path = temp_dir().join(suffix);

    Ok(File::create(&path).map(|file| (file, path))?)
}

fn gen_config_file(config: &Config) -> Result<(File, PathBuf), Error> {
    let (mut file, path) = random_file()?;
    let toml = config.all_options().to_toml().map_err(Error::ConfigTomlOutput)?;
    file.write_all(toml.as_bytes())?;

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
    args.push(config_path.to_str().map(ToOwned::to_owned).unwrap());

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FmtConfig;
    use lsp_types::{Position, Range, TextEdit};

    #[test]
    fn calc_text_edits() {
        let config = || FmtConfig::default().get_rustfmt_config().clone();
        let format = |x: &str| Rustfmt::Internal.calc_text_edits(x.to_string(), config()).unwrap();
        let line_range = |start, end| Range {
            start: Position { line: start, character: 0 },
            end: Position { line: end, character: u64::max_value() },
        };
        // Handle single-line text wrt. added/removed trailing newline
        assert_eq!(
            format("fn main() {} "),
            vec![TextEdit { range: line_range(0, 0), new_text: "fn main() {}\n".to_owned() }]
        );

        assert_eq!(
            format("fn main() {} \n"),
            vec![TextEdit { range: line_range(0, 0), new_text: "fn main() {}".to_owned() }]
        );

        assert_eq!(
            format("\nfn main() {} \n"),
            vec![TextEdit { range: line_range(0, 1), new_text: "fn main() {}".to_owned() }]
        );
        // Check that we send two separate edits
        assert_eq!(
            format("  struct Upper ;\n\nstruct Lower ;"),
            vec![
                TextEdit { range: line_range(0, 0), new_text: "struct Upper;".to_owned() },
                TextEdit { range: line_range(2, 2), new_text: "struct Lower;\n".to_owned() }
            ]
        );
    }
}
