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
#![feature(concat_idents)]
#![feature(type_ascription)]
#![feature(integer_atomics)]
#![feature(fnbox)]
#![deny(missing_docs)]

extern crate cargo;
extern crate env_logger;
extern crate languageserver_types as ls_types;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate racer;
extern crate rls_analysis as analysis;
extern crate rls_data as data;
extern crate rls_rustc as rustc_shim;
extern crate rls_span as span;
extern crate rls_vfs as vfs;
extern crate rustfmt_nightly as rustfmt;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate rayon;

#[macro_use]
extern crate serde_json;

extern crate url;
extern crate jsonrpc_core;
extern crate getopts;

use std::env;
use getopts::Options;

mod actions;
mod build;
mod config;
mod lsp_data;
mod server;

#[cfg(test)]
mod test;

type Span = span::Span<span::ZeroIndexed>;

// Timeout = 1.5s (totally arbitrary).
#[cfg(not(test))]
const COMPILER_TIMEOUT: u64 = 1500;

// Timeout for potenially very slow CPU CI boxes
#[cfg(test)]
const COMPILER_TIMEOUT: u64 = 3_600_000;

const CRATE_BLACKLIST: [&'static str; 10] = [
    "libc", "typenum", "alloc", "idna", "openssl", "libunicode_normalization", "serde",
    "serde_json", "librustc_serialize", "libunicode_segmentation",
];

const RUSTC_SHIM_ENV_VAR_NAME: &'static str = "RLS_RUSTC_SHIM";
const VERSION: &'static str = concat!(env!("CARGO_PKG_VERSION"), "-", include_str!(concat!(env!("OUT_DIR"), "/commit-info.txt")));
const BRIEF_DESCRIPTION: &'static str =
    r#"
Usage: rls [options]

    The Rust Language Server is a server that runs in the background, providing
    IDEs, editors, and other tools with information about Rust programs. It
    supports functionality such as 'goto definition', symbol search,
    reformatting, and code completion, and enables renaming and refactorings.

    For more info, please visit https://github.com/rust-lang-nursery/rls.
    "#
;

/// The main entry point to the RLS. Parses CLI arguments and then runs the
/// server.
pub fn main() {
    env_logger::init().unwrap();

    if env::var(RUSTC_SHIM_ENV_VAR_NAME).map(|v| v != "0").unwrap_or(false) {
        rustc_shim::run();
        return;
    }

    // apply parameters to the program
    let mut opts = Options::new();
    configure_options(&mut opts);

    let args: Vec<String> = env::args().collect();
    // verify options, error out if unknown
    let matches = match opts.parse(&args[1..]) {
        Ok(matches) => { matches },
        Err(f) => {
            reason_with_help(format!("{}", f.to_string()), &opts);
            return;
    };

    if matches.opt_present("h") {
        help(&opts);
        return;
    }

    if matches.opt_present("V") {
        version();
        return;
    }

    // startup the server in the correct mode
    if matches.opt_present("cli") {
        server::run_server(server::ServerMode::Cli);
    } else {
        server::run_server(server::ServerMode::Stdio);
    }
}

fn version() {
    println!("{}", String::from(VERSION));
}

fn configure_options(opts: &mut Options) {
    opts.optflag("h", "help", "print this help menu");
    opts.optflag("V", "version", "print the version and commit info");
    opts.optflag("", "cli", "initialize in CLI mode");
}

fn help(opts: &Options) {
    println!("{}", opts.usage(BRIEF_DESCRIPTION));
}

fn reason_with_help(reason: String, opts: &Options) {
    println!("{}\n", reason);
    help(&opts);
}