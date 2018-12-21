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

#![feature(rustc_private, integer_atomics, drain_filter)]
#![feature(crate_visibility_modifier)] // needed for edition 2018
#![allow(unknown_lints)]
#![warn(clippy::all, rust_2018_idioms)]
#![allow(
    clippy::cyclomatic_complexity,
    clippy::too_many_arguments
)]

pub use rls_analysis::{AnalysisHost, Target};
pub use rls_vfs::Vfs;

pub mod actions;
pub mod build;
pub mod cmd;
pub mod concurrency;
pub mod config;
pub mod lsp_data;
pub mod project_model;
pub mod server;

#[cfg(test)]
mod test;

type Span = rls_span::Span<rls_span::ZeroIndexed>;

pub const RUSTC_SHIM_ENV_VAR_NAME: &str = "RLS_RUSTC_SHIM";

pub fn version() -> String {
    use rustc_tools_util::VersionInfo;

    rustc_tools_util::get_version_info!().to_string()
}
