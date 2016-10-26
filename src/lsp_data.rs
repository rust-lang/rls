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
use std::path::{Path, PathBuf};

use std::convert::TryFrom;

use analysis::Span;
use analysis::raw;
use hyper::Url;
use serde::{Serialize};

macro_rules! impl_file_name {
    ($ty_name: ty) => {
        impl $ty_name {
            pub fn file_name(&self) -> PathBuf {
                uri_string_to_file_name(&self.uri)
            }
        }
    }
}

pub fn uri_string_to_file_name(uri: &str) -> PathBuf {
    let uri = Url::parse(&uri).unwrap();
    uri.to_file_path().unwrap()
}

pub fn from_usize(pos: usize) -> u64 {
	TryFrom::try_from(pos).unwrap() // XXX: Should we do error handling or assume it's ok?
}

pub fn to_usize(pos: u64) -> usize {
	TryFrom::try_from(pos).unwrap() // FIXME: for this one we definitely need to add error checking
}

pub use ls_types::Position;

pub use ls_types::Range;

pub struct RangeUtil;

impl RangeUtil {
    pub fn from_span(span: &Span) -> Range {
        Range {
            start: Position {
                line: from_usize(span.line_start),
                character: from_usize(span.column_start),
            },
            end: Position {
                line: from_usize(span.line_end),
                character: from_usize(span.column_end),
            },
        }
    }

    pub fn to_span(this: Range, fname: PathBuf) -> Span {
        Span {
            file_name: fname,
            line_start: to_usize(this.start.line),
            column_start: to_usize(this.start.character),
            line_end: to_usize(this.end.line),
            column_end: to_usize(this.end.character),
        }
    }
}

pub use ls_types::Location;

pub struct LocationUtil;

impl LocationUtil {
    pub fn from_span(span: &Span) -> Location {
        Location {
            uri: Url::from_file_path(&span.file_name).unwrap().into_string(),
            range: RangeUtil::from_span(span),
        }
    }

    pub fn from_position(file_name: &Path, line: usize, col: usize) -> Location {
        Location {
            uri: Url::from_file_path(&file_name).unwrap().into_string(),
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

pub use ls_types::InitializeParams;
pub use ls_types::NumberOrString;

pub use ls_types::TextDocumentIdentifier;
pub use ls_types::VersionedTextDocumentIdentifier;

/// An event describing a change to a text document. If range and rangeLength are omitted
/// the new text is considered to be the full content of the document.
#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct TextDocumentContentChangeEvent {
    pub range: Range,
    pub rangeLength: Option<u32>,
    pub text: String
}
//pub use ls_types::TextDocumentContentChangeEvent;


pub use ls_types::ReferenceContext;

pub use ls_types::SymbolInformation;
pub use ls_types::SymbolKind;

pub fn sk_from_def_kind(k: raw::DefKind) -> SymbolKind {
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

pub use ls_types::Diagnostic;
pub use ls_types::DiagnosticSeverity;
pub use ls_types::PublishDiagnosticsParams;

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

pub use ls_types::WorkspaceEdit;

pub use ls_types::ReferenceParams;
pub use ls_types::TextDocumentPositionParams;

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct ChangeParams {
    pub textDocument: VersionedTextDocumentIdentifier,
    pub contentChanges: Vec<TextDocumentContentChangeEvent>
}

pub type HoverParams = TextDocumentPositionParams;
pub use ls_types::RenameParams;
pub use ls_types::DocumentSymbolParams;


pub use ls_types::CancelParams;
pub use ls_types::TextDocumentSyncKind;

pub use ls_types::MarkedString;

pub use ls_types::Hover;
pub use ls_types::InitializeResult;

pub use ls_types::CompletionItem;

pub fn new_completion_item(label: String, detail: String) -> CompletionItem {
	CompletionItem {
        label : label,
        kind: None,
        detail: Some(detail),
        documentation: None,
        sort_text: None,
        filter_text: None,
        insert_text: None,
        text_edit: None,
        additional_text_edits: None,
        command: None,
        data: None,
	}
}

pub use ls_types::TextEdit;
pub use ls_types::CompletionOptions;

pub use ls_types::SignatureHelpOptions;

pub use ls_types::DocumentFormattingParams;
pub use ls_types::DocumentRangeFormattingParams;
pub use ls_types::FormattingOptions;

pub use ls_types::ServerCapabilities;
