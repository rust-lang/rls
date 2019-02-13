// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// at http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// Used to configure save-analysis.
#[cfg_attr(feature = "serialize-serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serialize-rustc", derive(RustcDecodable, RustcEncodable))]
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// File to output save-analysis data to.
    pub output_file: Option<String>,
    /// Include all documentation for items. (If `false`, only includes the
    /// summary (first paragraph) for each item).
    pub full_docs: bool,
    /// If true only includes data for public items in a crate (useful for
    /// library crates).
    pub pub_only: bool,
    /// If true only includes data for items reachable from the crate root.
    pub reachable_only: bool,
    /// True if and only if the analysed crate is part of the standard Rust distro.
    pub distro_crate: bool,
    /// Include signature information.
    pub signatures: bool,
    /// Include experimental borrow data.
    pub borrow_data: bool,
}
