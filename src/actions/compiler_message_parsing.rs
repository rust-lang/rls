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
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug)]
pub struct Suggestion {
    pub range: Range,
    pub new_text: String,
    pub label: String,
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
    let file_path = primary_span.file.clone();

    let mut suggestions = vec![];
    for c in message.children {
        for sp in c.spans {
            let span = sp.rls_span().zero_indexed();
            if span.file == file_path {
                if let Some(s) = sp.suggested_replacement {
                    let suggestion = Suggestion {
                        new_text: s.clone(),
                        range: ls_util::rls_to_range(span.range),
                        label: format!("{}: `{}`", c.message, s),
                    };
                    suggestions.push(suggestion);
                }
            }
        }
    }

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
        message: {
            if let Some(label) = primary.label {
                format!("{}\n{}", message.message, label)
            } else {
                message.message
            }
        },
    };

    Ok(FileDiagnostic {
        file_path: file_path,
        diagnostic: diagnostic,
        suggestions: suggestions,
    })
}
