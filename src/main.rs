// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(rustc_private)]
#![feature(concat_idents)]
#![feature(type_ascription)]
#![feature(integer_atomics)]
#![feature(fnbox)]

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

#[macro_use]
extern crate serde_json;

extern crate url;
extern crate jsonrpc_core;

use std::env;
use std::sync::Arc;

mod actions;
mod build;
mod cmd;
mod config;
mod lsp_data;
mod server;

#[cfg(test)]
mod test;

// Timeout = 1.5s (totally arbitrary).
const COMPILER_TIMEOUT: u64 = 1500;

const CRATE_BLACKLIST: [&'static str; 10] = [
    "libc", "typenum", "alloc", "idna", "openssl", "libunicode_normalization", "serde",
    "serde_json", "librustc_serialize", "libunicode_segmentation",
];

const RUSTC_SHIM_ENV_VAR_NAME: &'static str = "RLS_RUSTC_SHIM";

type Span = span::Span<span::ZeroIndexed>;

pub fn main() {
    env_logger::init().unwrap();

    if env::var(RUSTC_SHIM_ENV_VAR_NAME).map(|v| v != "0").unwrap_or(false) {
        rustc_shim::run();
        return;
    }

    if let Some(first_arg) = ::std::env::args().skip(1).next() {
        match first_arg.as_str() {
            "--version" | "-V" => println!("rls-preview {}", version()),
            "--help" | "-h" => println!("{}", help()),
            "--cli" => cmd::run(),
            unknown => println!("Unknown argument '{}'. Supported arguments:\n{}", unknown, help()),
        }
        return;
    }

    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());

    server::run_server(analysis, vfs);
}

fn version() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), "-", include_str!(concat!(env!("OUT_DIR"), "/commit-info.txt")))
}

fn help() -> &'static str {
    r#" 
    --version or -V to print the version and commit info
    --help or -h for this message
    --cli starts the RLS in command line mode
    No input starts the RLS as a language server 
    "#
}
