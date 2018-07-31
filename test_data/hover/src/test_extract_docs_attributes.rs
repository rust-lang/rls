// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// Begin multiline attribute
/// 
/// Cras malesuada mattis massa quis ornare. Suspendisse in ex maximus,
/// iaculis ante non, ultricies nulla. Nam ultrices convallis ex, vel
/// lacinia est rhoncus sed. Nullam sollicitudin finibus ex at placerat.
/// 
/// End multiline attribute
#[derive(
    Copy,
    Clone
)]
struct MultilineAttribute;


/// Begin single line attribute
/// 
/// Cras malesuada mattis massa quis ornare. Suspendisse in ex maximus,
/// iaculis ante non, ultricies nulla. Nam ultrices convallis ex, vel
/// lacinia est rhoncus sed. Nullam sollicitudin finibus ex at placerat.
/// 
/// End single line attribute.
#[derive(Debug)]
struct SingleLineAttribute;