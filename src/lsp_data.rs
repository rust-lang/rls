// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fmt::Debug;
use std::path::PathBuf;

use std::error::Error;

use analysis::Span;
use analysis::raw;
use hyper::Url;
use serde::{Serialize};


pub use ls_types::*;

macro_rules! impl_file_name {
    ($ty_name: ty) => {
        impl $ty_name {
            pub fn file_name(&self) -> PathBuf {
                uri_string_to_file_name(&self.uri)
            }
        }
    }
}

pub fn parse_file_path(uri: &Url) -> Result<PathBuf, Box<Error>> {
    if uri.scheme() != "file" {
        Err("URI scheme is not `file`".into())
    } else {
        uri.to_file_path().map_err(|_err| "Invalid file path in URI".into())
    }
}

pub fn from_usize(pos: usize) -> u64 {
    pos as u64
}

pub fn to_usize(pos: u64) -> usize {
    pos as usize // Truncation might happen if usize is 32 bits.
}


pub mod ls_util {
    use vfs::Vfs;

    use super::*;
    use std::path::{Path, PathBuf};

    use analysis::Span;
    use hyper::Url;
    
    pub fn range_from_span(span: &Span) -> Range {
        Range {
            start: Position::new(
                from_usize(span.line_start),
                from_usize(span.column_start),
            ),
            end: Position::new(
                from_usize(span.line_end),
                from_usize(span.column_end),
            ),
        }
    }

    pub fn range_to_span(this: Range, fname: PathBuf) -> Span {
        Span {
            file_name: fname,
            line_start: to_usize(this.start.line),
            column_start: to_usize(this.start.character),
            line_end: to_usize(this.end.line),
            column_end: to_usize(this.end.character),
        }
    }
    
    pub fn range_from_vfs_file(_vfs: &Vfs, _fname: &Path) -> Range {
        // FIXME: todo, endpos must be the end of the document, this is not correct
        
        let end_pos = Position::new(0, 0);
        Range{ start : Position::new(0, 0), end : end_pos }
    }
    
    pub fn location_from_span(span: &Span) -> Location {
        Location {
            uri: Url::from_file_path(&span.file_name).unwrap(),
            range: range_from_span(span),
        }
    }

    pub fn location_from_position(file_name: &Path, line: usize, col: usize) -> Location {
        Location {
            uri: Url::from_file_path(&file_name).unwrap(),
            range: Range {
                start: Position {
                    line: from_usize(line),
                    character: from_usize(col),
                },
                end: Position {
                    line: from_usize(line),
                    character: from_usize(col),
                },
            },
        }
    }
}

pub fn source_kind_from_def_kind(k: raw::DefKind) -> SymbolKind {
    match k {
        raw::DefKind::Enum => SymbolKind::Enum,
        raw::DefKind::Tuple => SymbolKind::Array,
        raw::DefKind::Struct => SymbolKind::Class,
        raw::DefKind::Trait => SymbolKind::Interface,
        raw::DefKind::Function => SymbolKind::Function,
        raw::DefKind::Method => SymbolKind::Function,
        raw::DefKind::Macro => SymbolKind::Function,
        raw::DefKind::Mod => SymbolKind::Module,
        raw::DefKind::Type => SymbolKind::Interface,
        raw::DefKind::Local => SymbolKind::Variable,
        raw::DefKind::Static => SymbolKind::Variable,
        raw::DefKind::Const => SymbolKind::Variable,
        raw::DefKind::Field => SymbolKind::Variable,
        raw::DefKind::Import => SymbolKind::Module,
    }
}

/* -----------------  Compiler message  ----------------- */
// FIXME: These types are not LSP related, should be moved to a different module.

#[derive(Debug, Deserialize)]
pub struct CompilerMessageCode {
    pub code: String
}

#[derive(Debug, Deserialize)]
pub struct CompilerMessage {
    pub message: String,
    pub code: Option<CompilerMessageCode>,
    pub level: String,
    pub spans: Vec<Span>,
}

/* -----------------  JSON-RPC protocol types ----------------- */
// FIXME: These types are not directly LSP related, should be moved to a JSON-RPC module.

/// An event-like (no response needed) notification message.
#[derive(Debug, Serialize)]
pub struct NotificationMessage<T>
    where T: Debug + Serialize
{
    jsonrpc: &'static str,
    pub method: String,
    pub params: T,
}

impl <T> NotificationMessage<T> where T: Debug + Serialize {
    pub fn new(method: String, params: T) -> Self {
        NotificationMessage {
            jsonrpc: "2.0",
            method: method,
            params: params
        }
    }
}
