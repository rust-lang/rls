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

use ls_types::{DiagnosticSeverity, NumberOrString};
use serde_json;
use span::compiler::DiagnosticSpan;
use span;
use actions::lsp_extensions::{RustDiagnostic, LabelledRange};

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
    pub diagnostic: RustDiagnostic,
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

    let message_text = compose_message(&message);
    let primary = message.spans.iter()
                                    .filter(|x| x.is_primary)
                                    .collect::<Vec<&span::compiler::DiagnosticSpan>>()[0].clone();
    let primary_span = primary.rls_span().zero_indexed();
    let primary_range = ls_util::rls_to_range(primary_span.range);

    // build up the secondary spans
    let secondary_labels: Vec<LabelledRange> = message.spans.iter()
                                                            .filter(|x| !x.is_primary)
                                                            .map(|x| {
            let secondary_range = ls_util::rls_to_range(x.rls_span().zero_indexed().range);

            LabelledRange {
                start: secondary_range.start,
                end: secondary_range.end,
                label: x.label.clone(),
            }
        }).collect();


    let diagnostic = RustDiagnostic {
        range: LabelledRange {
                  start: primary_range.start,
                  end: primary_range.end,
                  label: primary.label.clone(),
               },
        secondaryRanges: secondary_labels,
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
        message: message_text,
    };

    Ok(FileDiagnostic {
        file_path: primary_span.file.clone(),
        diagnostic: diagnostic
    })
}

/// Builds a more sophisticated error message
fn compose_message(compiler_message: &CompilerMessage) -> String {
    let mut message = compiler_message.message.clone();
    for sp in &compiler_message.spans {
        if !sp.is_primary {
            continue;
        }
        if let Some(ref label) = sp.label {
            message.push_str("\n");
            message.push_str(label);
        }
    }
    if !compiler_message.children.is_empty() {
        message.push_str("\n");
        for child in &compiler_message.children {
            message.push_str(&format!("\n{}: {}", child.level, child.message));
        }
    }
    message
}
