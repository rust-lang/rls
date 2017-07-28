// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate rls;
extern crate env_logger;

use std::sync::Arc;

use rls::analysis;
use rls::server;
use rls::vfs;
use rls::cmd;

pub fn main() {
    env_logger::init().unwrap();

    if let Some(first_arg) = ::std::env::args().skip(1).next() {
        match first_arg.as_str() {
            "--version" | "-V" => println!("rls {}", version()),
            "--help" | "-h" => println!("{}", help()),
            _ => cmd::run(),
        }
        return;
    }

    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());

    server::run_server(analysis, vfs);
}

fn version() -> &'static str {
    // FIXME when we have non-nightly channels, we shouldn't hardwire the "nightly" string here.
    concat!(env!("CARGO_PKG_VERSION"), "-nightly", include_str!(concat!(env!("OUT_DIR"), "/commit-info.txt")))
}
fn help() -> &'static str {
    r#"
    --version or -V to print the version and commit info
    --help or -h for this message
    Other input starts the RLS in command line mode
    No input starts the RLS as a language server
    "#
}
