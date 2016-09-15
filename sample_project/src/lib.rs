// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

pub struct Request {
    filepath: String,
    line: usize,
    col: usize,
}

impl Request {
    pub fn new() -> Request {
        Request {
            filepath: "Hello".to_owned(),
            line: 42,
            col: 0,
        }
    }
}

pub fn foo() {
    let r = Request { filepath: "foo".to_string(), line: 3, col: 4 };

    let s = String::new();

    r;
}
