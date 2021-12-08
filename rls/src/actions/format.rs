//! Code formatting using Rustfmt -- by default using statically-linked one or
//! possibly running Rustfmt binary specified by the user.

use std::env::temp_dir;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::string::FromUtf8Error;

use log::debug;
use lsp_types::{Position, Range, TextEdit};
use rand::{distributions, thread_rng, Rng};
use rustfmt_nightly::{Config, Input, ModifiedLines, NewlineStyle, Session};

/// Specifies which `rustfmt` to use.
#[derive(Clone)]
pub enum Rustfmt {
    /// Externally invoked `rustfmt` process.
    External { path: PathBuf, cwd: PathBuf },
    /// Statically linked `rustfmt`.
    Internal,
}

/// Defines a formatting-related error.
#[derive(Debug)]
pub enum Error {
    /// Generic variant of `Error::Rustfmt` error.
    Failed,
    Rustfmt(rustfmt_nightly::ErrorKind),
    Io(std::io::Error),
    ConfigTomlOutput(String),
    OutputNotUtf8(FromUtf8Error),
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Failed => write!(f, "Formatting could not be completed."),
            Error::Rustfmt(err) => write!(f, "Could not format source code: {}", err),
            Error::Io(err) => write!(f, "Encountered I/O error: {}", err),
            Error::ConfigTomlOutput(err) => {
                write!(f, "Config couldn't be converted to TOML for Rustfmt purposes: {}", err)
            }
            Error::OutputNotUtf8(err) => {
                write!(f, "Formatted output is not valid UTF-8 source: {}", err)
            }
        }
    }
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
                let end_line = start_line + u64::from(item.lines_removed);

                let mut new_text = item.lines.join(newline);

                // Rustfmt represents an added line as start_line == end_line, new_text == "",
                // which is a no-op, so we need to add a terminating newline.
                if start_line == end_line && new_text.is_empty() {
                    new_text.push_str(newline);
                }

                // Line deletions are represented as start_line != end_line, new_text == "".
                // If we're not deleting a line, there should always be a terminating newline.
                let delete_only = start_line != end_line && new_text.is_empty();
                if !delete_only && !new_text.ends_with(newline) {
                    new_text.push_str(newline);
                }

                TextEdit {
                    range: Range {
                        start: Position::new(start_line, 0),
                        end: Position::new(end_line, 0),
                    },
                    new_text,
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

    // SAFETY: `Alphanumeric` generates ASCII characters
    let suffix = unsafe {
        String::from_utf8_unchecked(
            thread_rng().sample_iter(&distributions::Alphanumeric).take(SUFFIX_LEN).collect(),
        )
    };
    let path = temp_dir().join(suffix);

    Ok(File::create(&path).map(|file| (file, path))?)
}

fn gen_config_file(config: &Config) -> Result<(File, PathBuf), Error> {
    let (mut file, path) = random_file()?;
    let toml =
        config.all_options().to_toml().map_err(|e| Error::ConfigTomlOutput(e.to_string()))?;
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

    // Otherwise --file-lines [] are treated as no lines rather than FileLines::all()
    if config.file_lines().files().count() > 0 {
        args.push("--file-lines".into());
        let file_lines_json = config.file_lines().to_json_spans();
        let lines = serde_json::to_string(&file_lines_json).unwrap();
        args.push(lines);
    }

    args.push("--config-path".into());
    args.push(config_path.to_str().map(ToOwned::to_owned).unwrap());

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FmtConfig;
    use lsp_types::{Position, Range, TextEdit};
    use rustfmt_nightly::FileLines;
    use std::str::FromStr;

    #[test]
    fn calc_text_edits() {
        fn format(input: &str) -> Vec<TextEdit> {
            let config = || FmtConfig::default().get_rustfmt_config().clone();
            Rustfmt::Internal.calc_text_edits(input.to_string(), config()).unwrap()
        }

        fn test_case(input: &str, output: Vec<(u64, u64, u64, u64, &str)>) {
            assert_eq!(
                format(input),
                output
                    .into_iter()
                    .map(|(start_l, start_c, end_l, end_c, out)| TextEdit {
                        range: Range {
                            start: Position { line: start_l, character: start_c },
                            end: Position { line: end_l, character: end_c },
                        },
                        new_text: out.to_owned(),
                    })
                    .collect::<Vec<_>>()
            )
        }
        // Handle single-line text wrt. added/removed trailing newline
        test_case("fn main() {} ", vec![(0, 0, 1, 0, "fn main() {}\n")]);
        test_case("fn main() {} \n", vec![(0, 0, 1, 0, "fn main() {}\n")]);
        test_case("\nfn main() {} \n", vec![(0, 0, 2, 0, "fn main() {}\n")]);
        // Check that we send two separate edits
        test_case(
            "  struct Upper ;\n\nstruct Lower ;",
            vec![(0, 0, 1, 0, "struct Upper;\n"), (2, 0, 3, 0, "struct Lower;\n")],
        );
    }

    #[test]
    fn no_empty_file_lines() {
        let config_with_lines = {
            let mut config = Config::default();
            config.set().file_lines(
                FileLines::from_str(r#"[{ "file": "stdin", "range": [0, 5] }]"#).unwrap(),
            );
            config
        };
        let args = rustfmt_args(&config_with_lines, Path::new("dummy"));
        assert!(args.join(" ").find("--file-lines").is_some());

        let args = rustfmt_args(&Config::default(), Path::new("dummy"));
        assert_eq!(args.join(" ").find("--file-lines"), None);
    }
}
