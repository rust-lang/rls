use std::path::PathBuf;

use ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString};
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
    let message = serde_json::from_str::<CompilerMessage>(&message)?;

    if message.spans.is_empty() {
        return Err(ParseError::NoSpans);
    }

    let span = message.spans[0].rls_span().zero_indexed();

    let message_text = compose_message(&message);

    let diagnostic = Diagnostic {
        range: ls_util::rls_to_range(span.range),
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
        file_path: span.file.clone(),
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
