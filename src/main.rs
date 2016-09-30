#![feature(custom_derive, plugin)]
#![feature(rustc_private)]
#![plugin(serde_macros)]

#[macro_use]
extern crate hyper;
extern crate rls_analysis as analysis;
extern crate rls_vfs as vfs;
extern crate serde;
extern crate serde_json;

use std::sync::Arc;

mod actions;
mod build;
mod ide;
mod server;

pub fn main() {
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    server::run_server(analysis, vfs, build_queue);
}
