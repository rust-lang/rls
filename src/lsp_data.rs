// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::fmt::Debug;

use analysis::Span;
use serde::{Serialize, Serializer};

macro_rules! impl_file_name {
    ($ty_name: ty) => {
        impl $ty_name {
            pub fn file_name(&self) -> &str {
                &self.uri["file://".len()..]
            }
        }
    }
}

/// Position in a text document expressed as zero-based line and character offset.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Position {
    pub line: usize,
    pub character: usize,
}

/// A range in a text document expressed as (zero-based) start and end positions.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Range {
    pub start: Position,
    /// End position is exclusive
    pub end: Position,
}

impl Range {
    pub fn from_span(span: &Span) -> Range {
        Range {
            start: Position {
                line: span.line_start,
                character: span.column_start,
            },
            end: Position {
                line: span.line_end,
                character: span.column_end,
            },
        }
    }

    pub fn to_span(&self, fname: String) -> Span {
        Span {
            file_name: fname,
            line_start: self.start.line,
            column_start: self.start.character,
            line_end: self.end.line,
            column_end: self.end.character,
        }
    }
}

/// Represents a location inside a resource, such as a line inside a text file.
#[derive(Debug, Deserialize, Serialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

impl Location {
    pub fn from_span(span: &Span) -> Location {
        Location {
            uri: format!("file://{}", span.file_name),
            range: Range::from_span(span),
        }
    }

    pub fn from_position(file_name: &str, line: usize, col: usize) -> Location {
        Location {
            uri: format!("file://{}", file_name),
            range: Range {
                start: Position {
                    line: line,
                    character: col,
                },
                end: Position {
                    line: line,
                    character: col,
                },
            },
        }
    }
}

/// The initialize request is sent as the first request from the client to the server.
#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct InitializeParams {
    pub processId: usize,
    pub rootPath: String
}

#[derive(Debug, Deserialize)]
pub struct Document {
    pub uri: String
}

impl_file_name!(Document);

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct TextDocumentIdentifier {
    pub uri: String
}

impl_file_name!(TextDocumentIdentifier);

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct VersionedTextDocumentIdentifier {
    pub version: u64,
    pub uri: String
}

/// An event describing a change to a text document. If range and rangeLength are omitted
/// the new text is considered to be the full content of the document.
#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct TextDocumentContentChangeEvent {
    pub range: Range,
    pub rangeLength: Option<u32>,
    pub text: String
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct ReferenceContext {
    pub includeDeclaration: bool,
}

/// Represents information about programming constructs like variables, classes,
/// interfaces etc.
#[derive(Debug, Serialize)]
pub struct SymbolInformation {
    pub name: String,
    pub kind: u32, // can be made to numeric enum
    pub location: Location,
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

/// Represents a diagnostic, such as a compiler error or warning.
/// Diagnostic objects are only valid in the scope of a resource.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy)]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
//    Information = 3,
//    Hint = 4
}

impl Serialize for DiagnosticSeverity {
    fn serialize<S>(&self, serializer: &mut S) -> Result<(), S::Error>
        where S: Serializer
    {
        serializer.serialize_u8(*self as u8)
    }
}

#[derive(Debug, Serialize)]
pub struct PublishDiagnosticsParams {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
}

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

/// A workspace edit represents changes to many resources managed in the workspace.
#[derive(Debug, Serialize)]
pub struct WorkspaceEdit {
    pub changes: HashMap<String, Vec<TextEdit>>,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct ReferenceParams {
    pub textDocument: Document,
    pub position: Position,
    pub context: ReferenceContext,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct TextDocumentPositionParams {
    pub textDocument: Document,
    pub position: Position,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct ChangeParams {
    pub textDocument: VersionedTextDocumentIdentifier,
    pub contentChanges: Vec<TextDocumentContentChangeEvent>
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct HoverParams {
    pub textDocument: Document,
    pub position: Position
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct RenameParams {
    pub textDocument: Document,
    pub position: Position,
    pub newName: String,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct DocumentSymbolParams {
    pub textDocument: Document,
}

#[derive(Debug, Deserialize)]
pub struct CancelParams {
    pub id: usize
}

/// Defines how the host (editor) should sync document changes to the language server.
#[derive(Debug, Serialize)]
pub enum DocumentSyncKind {
    // None = 0,
    /// Documents are synced by always sending the full content of the document.
    // Full = 1,
    /// Documents are synced by sending the full content on open. After that only incremental
    /// updates to the document are sent.
    Incremental = 2,
}

#[derive(Debug, Serialize)]
pub struct MarkedString {
    pub language: String,
    pub value: String
}

#[derive(Debug, Serialize)]
pub struct HoverSuccessContents {
    pub contents: Vec<MarkedString>
}

#[derive(Debug, Serialize)]
pub struct InitializeCapabilities {
    pub capabilities: ServerCapabilities
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    pub detail: String,
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
pub struct TextEdit {
    pub range: Range,
    pub newText: String,
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
pub struct CompletionOptions {
    /// The server provides support to resolve additional information for a completion item.
    pub resolveProvider: bool,
    pub triggerCharacters: Vec<String>,
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
pub struct SignatureHelpOptions {
    pub triggerCharacters: Vec<String>,
}

// #[allow(non_snake_case)]
// #[derive(Debug, Serialize)]
// pub struct CodeLensOptions {
//     pub resolveProvider: bool,
// }

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct DocumentFormattingParams {
    pub textDocument: TextDocumentIdentifier,
    pub options: FormattingOptions,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct DocumentRangeFormattingParams {
    pub textDocument: TextDocumentIdentifier,
    pub range: Range,
    pub options: FormattingOptions,
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
pub struct FormattingOptions {
    pub tabSize: u32,
    pub insertSpaces: bool,
}

#[allow(non_snake_case)]
#[derive(Debug, Serialize)]
pub struct ServerCapabilities {
    pub textDocumentSync: usize,
    pub hoverProvider: bool,
    pub completionProvider: CompletionOptions,
    pub signatureHelpProvider: SignatureHelpOptions,
    pub definitionProvider: bool,
    pub referencesProvider: bool,
    pub documentHighlightProvider: bool,
    pub documentSymbolProvider: bool,
    pub workshopSymbolProvider: bool,
    pub codeActionProvider: bool,
    pub codeLensProvider: bool,
    pub documentFormattingProvider: bool,
    pub documentRangeFormattingProvider: bool,
    // pub documentOnTypeFormattingProvider
    pub renameProvider: bool,
}
