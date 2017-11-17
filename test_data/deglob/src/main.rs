// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

// single wildcard import
// imports two values
use std::io::*;

// multiple wildcard imports
use std::mem::*; use std::cmp::*;

pub fn main() {
    size_of::<i32>();
    size_of::<Stdin>();
    size_of::<Stdout>();
    max(1, 2);
}
