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
