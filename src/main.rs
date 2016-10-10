#![feature(rustc_private)]
#![feature(proc_macro)]

#[macro_use]
extern crate hyper;
#[macro_use]
extern crate log;
extern crate rls_analysis as analysis;
extern crate rls_vfs as vfs;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

use std::sync::Arc;

mod actions;
mod build;
mod ide;
mod server;
#[cfg(test)]
mod test;

pub fn main() {
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    let vfs = Arc::new(vfs::Vfs::new());
    let build_queue = Arc::new(build::BuildQueue::new(vfs.clone()));
    server::run_server(analysis, vfs, build_queue);
}
