#![feature(custom_derive, plugin)]
#![plugin(serde_macros)]

extern crate futures;
#[macro_use]
extern crate hyper;
extern crate rls_analysis as analysis;
extern crate serde;
extern crate serde_json;

use std::sync::Arc;

mod actions;
mod ide;
mod server;
mod vfs;


// TODO overlap with VSCode plugin
fn rustw_span(mut source: analysis::Span) -> analysis::Span {
    source.column_start += 1;
    source.column_end += 1;
    source
}
fn adjust_span_for_vscode(mut source: analysis::Span) -> analysis::Span {
    source.column_start -= 1;
    source.column_end -= 1;
    source
}


pub fn main() {
    let analysis = Arc::new(analysis::AnalysisHost::new(analysis::Target::Debug));
    server::run_server(analysis);
}
