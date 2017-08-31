// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::path::{Path, PathBuf};

use ls_types::{Diagnostic, Range, DiagnosticSeverity, NumberOrString};
use serde_json;
use span::compiler::DiagnosticSpan;
use Span;

use lsp_data::ls_util;

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

pub fn parse(message: &str) -> Option<FileDiagnostic> {
    let message = match serde_json::from_str::<CompilerMessage>(message) {
        Ok(m) => m,
        Err(e) => {
            debug!("build error {:?}", e);
            debug!("from {}", message);
            return None;
        }
    };

    if message.spans.is_empty() {
        return None;
    }

    let primary_span = primary_span(&message);
    let suggestions = make_suggestions(message.children, &primary_span.file);

    let diagnostic = Diagnostic {
        range: ls_util::rls_to_range(primary_span.range),
        severity: Some(severity(&message.level)),
        code: Some(NumberOrString::String(match message.code {
            Some(c) => c.code.clone(),
            None => String::new(),
        })),
        source: Some("rustc".into()),
        message: message.message,
    };

    Some(FileDiagnostic {
        file_path: primary_span.file,
        diagnostic: diagnostic,
        suggestions: suggestions,
    })
}

fn severity(level: &str) -> DiagnosticSeverity {
    if level == "error" {
        DiagnosticSeverity::Error
    } else {
        DiagnosticSeverity::Warning
    }
}

fn make_suggestions(children: Vec<CompilerMessage>, file: &Path) -> Vec<Suggestion> {
    let mut suggestions = vec![];
    for c in children {
        for sp in c.spans {
            let span = sp.rls_span().zero_indexed();
            if span.file == file {
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
    suggestions
}

fn primary_span(message: &CompilerMessage) -> Span {
    let primary = message.spans
        .iter()
        .filter(|x| x.is_primary)
        .next()
        .unwrap()
        .clone();
    primary.rls_span().zero_indexed()
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    message: String,
    code: Option<CompilerMessageCode>,
    level: String,
    spans: Vec<DiagnosticSpan>,
    children: Vec<CompilerMessage>,
}

#[derive(Debug, Deserialize)]
struct CompilerMessageCode {
    code: String
}
