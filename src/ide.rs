// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::path::PathBuf;

use analysis::{Span};

use vfs::Change;

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct Position {
    pub filepath: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Serialize, Eq, PartialEq, Deserialize)]
pub enum Provider {
    Compiler,
    Racer,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Input {
    pub pos: Position,
    pub span: Span,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Output {
    Ok(Position, Provider),
    Err,
}

#[derive(Debug, Deserialize)]
pub struct ChangeInput {
    pub project_path: PathBuf,
    pub changes: Vec<Change>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SaveInput {
    pub project_path: PathBuf,
    pub saved_file: PathBuf,
}
