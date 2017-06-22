// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::path::PathBuf;

use ls_types::{Diagnostic, Range, DiagnosticSeverity, NumberOrString};
use serde_json;
use span::compiler::DiagnosticSpan;

use lsp_data::ls_util;

#[derive(Debug, Deserialize)]
struct CompilerMessageCode {
    code: String
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    message: String,
    code: Option<CompilerMessageCode>,
    level: String,
    spans: Vec<DiagnosticSpan>,
    children: Vec<CompilerMessage>,
}

#[derive(Debug)]
pub struct FileDiagnostic {
    pub file_path: PathBuf,
    pub diagnostic: Diagnostic,
}

#[derive(Debug)]
pub enum ParseError {
    JsonError(serde_json::Error),
    NoSpans,
}

impl From<serde_json::Error> for ParseError {
    fn from(error: serde_json::Error) -> Self {
        ParseError::JsonError(error)
    }
}

pub fn parse(message: &str) -> Result<FileDiagnostic, ParseError> {
    let message = serde_json::from_str::<CompilerMessage>(message)?;

    if message.spans.is_empty() {
        return Err(ParseError::NoSpans);
    }

    let primary = message.spans
        .iter()
        .filter(|x| x.is_primary)
        .next()
        .unwrap()
        .clone();
    let primary_span = primary.rls_span().zero_indexed();
    let primary_range = ls_util::rls_to_range(primary_span.range);

    let diagnostic = Diagnostic {
        range: Range {
            start: primary_range.start,
            end: primary_range.end,
        },
        severity: Some(if message.level == "error" {
            DiagnosticSeverity::Error
        } else {
            DiagnosticSeverity::Warning
        }),
        code: Some(NumberOrString::String(match message.code {
            Some(c) => c.code.clone(),
            None => String::new(),
        })),
        source: Some("rustc".into()),
        message: message.message,
    };


// c in error.children
//     s in c.spans
//         if s.suggested_replacement
//             suggestions.push({
//                 suggested_replacement: s.suggested_replacement,
//                 span: s.[..],
//                 label: c.message
//             })

    Ok(FileDiagnostic {
        file_path: primary_span.file.clone(),
        diagnostic: diagnostic
    })
}
