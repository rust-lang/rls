// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! The Rust Language Server.
//!
//! The RLS provides a server that runs in the background, providing IDEs,
//! editors, and other tools with information about Rust programs. It supports
//! functionality such as 'goto definition', symbol search, reformatting, and
//! code completion, and enables renaming and refactorings.

#![feature(rustc_private)]
#![feature(integer_atomics)]
#![allow(unknown_lints)]
#![warn(clippy)]
#![allow(cyclomatic_complexity)]
#![allow(needless_pass_by_value)]

extern crate cargo;
extern crate cargo_metadata;
extern crate env_logger;
#[macro_use]
extern crate failure;
extern crate languageserver_types as ls_types;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate num_cpus;
extern crate racer;
extern crate rayon;
extern crate rls_analysis as analysis;
extern crate rls_blacklist as blacklist;
extern crate rls_data as data;
extern crate rls_rustc as rustc_shim;
extern crate rls_span as span;
extern crate rls_vfs as vfs;
#[cfg(feature = "rustfmt")]
extern crate rustfmt_nightly as rustfmt;
extern crate serde;
#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate serde_json;

extern crate jsonrpc_core;
extern crate url;

use std::env;
use std::sync::Arc;

pub mod actions;
pub mod build;
pub mod cmd;
pub mod config;
pub mod lsp_data;
pub mod server;

#[cfg(test)]
mod test;

// Timeout = 1.5s (totally arbitrary).
#[cfg(not(test))]
const COMPILER_TIMEOUT: u64 = 1500;

// Timeout for potenially very slow CPU CI boxes
#[cfg(test)]
const COMPILER_TIMEOUT: u64 = 3_600_000;

const RUSTC_SHIM_ENV_VAR_NAME: &str = "RLS_RUSTC_SHIM";

type Span = span::Span<span::ZeroIndexed>;

/// The main entry point to the RLS. Parses CLI arguments and then runs the
/// server.
pub fn main() {
    env_logger::init();

    if env::var(RUSTC_SHIM_ENV_VAR_NAME)
        .map(|v| v != "0")
        .unwrap_or(false)
    {
        rustc_shim::run();
        return;
    }

    if let Some(first_arg) = ::std::env::args().nth(1) {
        match first_arg.as_str() {
            "--version" | "-V" => println!("rls-preview {}", version()),
            "--help" | "-h" => println!("{}", help()),
            "--cli" => cmd::run(),
            unknown => println!(
                "Unknown argument '{}'. Supported arguments:\n{}",
                unknown,
                help()
            ),
        }
        return;
    }

    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());

    server::run_server(analysis, vfs);
}

fn version() -> &'static str {
    concat!(
        env!("CARGO_PKG_VERSION"),
        "-",
        include_str!(concat!(env!("OUT_DIR"), "/commit-info.txt"))
    )
}

fn help() -> &'static str {
    r#"
    --version or -V to print the version and commit info
    --help or -h for this message
    --cli starts the RLS in command line mode
    No input starts the RLS as a language server
    "#
}
