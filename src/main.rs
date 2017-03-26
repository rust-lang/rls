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
#![feature(vec_remove_item)]

extern crate cargo;
#[macro_use]
extern crate derive_new;
extern crate env_logger;
extern crate hyper;
extern crate languageserver_types as ls_types;
#[macro_use]
extern crate log;
extern crate racer;
extern crate rls_analysis as analysis;
extern crate rls_vfs as vfs;
extern crate rls_span as span;
extern crate rls_data as data;
extern crate rustc_serialize;
extern crate rustfmt;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate toml;
extern crate url;
extern crate url_serde;

use std::sync::Arc;

mod build;
mod server;
mod actions;
mod lsp_data;
mod config;

#[cfg(test)]
mod test;

// Timeout = 1.5s (totally arbitrary).
const COMPILER_TIMEOUT: u64 = 1500;

type Span = span::Span<span::ZeroIndexed>;

pub fn main() {
    env_logger::init().unwrap();

    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));

    server::run_server(analysis, vfs, build_queue);
}
