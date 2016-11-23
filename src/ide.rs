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

use analysis::{raw, Span};

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

#[derive(Debug, Deserialize, Serialize)]
pub struct SaveInput {
    pub project_path: PathBuf,
    pub saved_file: PathBuf,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub enum VscodeKind {
    File,
    Module,
    Namespace,
    Package,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    Interface,
    Function,
    Variable,
    Constant,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Key,
    Null
}

impl From<raw::DefKind> for VscodeKind {
    fn from(k: raw::DefKind) -> VscodeKind {
        match k {
            raw::DefKind::Enum => VscodeKind::Enum,
            raw::DefKind::Tuple => VscodeKind::Array,
            raw::DefKind::Struct => VscodeKind::Class,
            raw::DefKind::Trait => VscodeKind::Interface,
            raw::DefKind::Function => VscodeKind::Function,
            raw::DefKind::Method => VscodeKind::Function,
            raw::DefKind::Macro => VscodeKind::Function,
            raw::DefKind::Mod => VscodeKind::Module,
            raw::DefKind::Type => VscodeKind::Interface,
            raw::DefKind::Local => VscodeKind::Variable,
            raw::DefKind::Static => VscodeKind::Variable,
            raw::DefKind::Const => VscodeKind::Variable,
            raw::DefKind::Field => VscodeKind::Variable,
            raw::DefKind::Import => VscodeKind::Module,
        }
    }
}
