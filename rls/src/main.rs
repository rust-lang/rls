//! The Rust Language Server.
//!
//! The RLS provides a server that runs in the background, providing IDEs,
//! editors, and other tools with information about Rust programs. It supports
//! functionality such as 'goto definition', symbol search, reformatting, and
//! code completion, and enables renaming and refactorings.

use log::warn;
use rls_rustc as rustc_shim;

use std::env;
use std::sync::Arc;

const RUSTC_WRAPPER_ENV_VAR: &str = "RUSTC_WRAPPER";

/// The main entry point to the RLS. Parses CLI arguments and then runs the server.
pub fn main() {
    let exit_code = main_inner();
    std::process::exit(exit_code);
}

fn main_inner() -> i32 {
    env_logger::init();

    // [workaround]
    // Currently sccache breaks RLS with obscure error messages.
    // Until it's actually fixed disable the wrapper completely
    // in the current process tree.
    //
    // See https://github.com/rust-lang/rls/issues/703
    // and https://github.com/mozilla/sccache/issues/303
    if env::var_os(RUSTC_WRAPPER_ENV_VAR).is_some() {
        warn!(
            "The {} environment variable is incompatible with RLS, \
             removing it from the process environment",
            RUSTC_WRAPPER_ENV_VAR
        );
        env::remove_var(RUSTC_WRAPPER_ENV_VAR);
    }

    if env::var(rls::RUSTC_SHIM_ENV_VAR_NAME).ok().map_or(false, |v| v != "0") {
        match rustc_shim::run() {
            Ok(..) => return 0,
            Err(..) => return 101,
        }
    }

    if let Some(first_arg) = env::args().nth(1) {
        return match first_arg.as_str() {
            "--version" | "-V" => {
                println!("{}", rls::version());
                0
            }
            "--help" | "-h" => {
                println!("{}", help());
                0
            }
            "--cli" => {
                rls::cmd::run();
                0
            }
            unknown => {
                println!("Unknown argument '{}'. Supported arguments:\n{}", unknown, help());
                101
            }
        };
    }

    let analysis = Arc::new(rls::AnalysisHost::new(rls::Target::Debug));
    let vfs = Arc::new(rls::Vfs::new());

    rls::server::run_server(analysis, vfs)
}

fn help() -> &'static str {
    r#"
    --version or -V to print the version and commit info
    --help or -h for this message
    --cli starts the RLS in command line mode
    No input starts the RLS as a language server
    "#
}
