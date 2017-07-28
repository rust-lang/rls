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
pub extern crate rls_analysis as analysis;
pub extern crate rls_vfs as vfs;
extern crate rls_span as span;
extern crate rls_data as data;
extern crate rustfmt_nightly as rustfmt;
extern crate serde;
#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate serde_json;

extern crate toml;
extern crate url;
extern crate url_serde;
extern crate jsonrpc_core;

pub mod actions;
pub mod build;
pub mod cmd;
pub mod config;
pub mod lsp_data;
pub mod server;

#[cfg(test)]
mod test;

// Timeout = 1.5s (totally arbitrary).
const COMPILER_TIMEOUT: u64 = 1500;

type Span = span::Span<span::ZeroIndexed>;


