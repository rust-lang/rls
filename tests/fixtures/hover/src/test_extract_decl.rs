// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

pub fn foo() -> Foo<u32> {
    Foo { t: 1 }
}

#[derive(Debug)]
pub struct Foo<T> {
    pub t: T
}

#[derive(Debug)]
pub enum Bar {
    Baz
}

#[derive(Debug)]
pub struct NewType(pub u32, f32);

impl NewType {
    pub fn new() -> NewType {
        NewType(1, 2.0)
    }

    pub fn bar<T: Copy + Add>(&self, the_really_long_name_string: String, the_really_long_name_foo: Foo<T>) -> Vec<(String, Foo<T>)> {
        Vec::default()
    }
}

pub trait Baz<T> where T: Copy { 
    fn make_copy(&self) -> Self;
}

impl<T> Baz<T> for Foo<T> where T: Copy {
    fn make_copy(&self) -> Self {
        Foo { t: self.t }
    }
}

pub trait Qeh<T, U>
where T: Copy, 
U: Clone {
    
}

pub fn multiple_lines(
    s: String,
    i: i32
) {
    drop(s);
    drop(i);
}

pub fn bar() -> Bar {
    Bar::Baz
}
