///! These are the structures emitted by the compiler as part of JSON errors.
///! The original source can be found at
///! https://github.com/rust-lang/rust/blob/master/src/librustc_errors/json.rs
use std::path::PathBuf;

#[cfg(feature = "derive")]
use serde::Deserialize;

use crate::{Column, OneIndexed, Row, Span};

#[cfg_attr(feature = "derive", derive(Deserialize))]
#[cfg_attr(feature = "serialize-rustc", derive(RustcDecodable))]
#[derive(Debug, Clone)]
pub struct DiagnosticSpan {
    pub file_name: String,
    pub byte_start: u32,
    pub byte_end: u32,
    /// 1-based.
    pub line_start: usize,
    pub line_end: usize,
    /// 1-based, character offset.
    pub column_start: usize,
    pub column_end: usize,
    /// Is this a "primary" span -- meaning the point, or one of the points,
    /// where the error occurred?
    pub is_primary: bool,
    /// Source text from the start of line_start to the end of line_end.
    pub text: Vec<DiagnosticSpanLine>,
    /// Label that should be placed at this location (if any)
    pub label: Option<String>,
    /// If we are suggesting a replacement, this will contain text
    /// that should be sliced in atop this span. You may prefer to
    /// load the fully rendered version from the parent `Diagnostic`,
    /// however.
    pub suggested_replacement: Option<String>,
    /// Macro invocations that created the code at this span, if any.
    pub expansion: Option<Box<DiagnosticSpanMacroExpansion>>,
}

impl DiagnosticSpan {
    pub fn rls_span(&self) -> Span<OneIndexed> {
        Span::new(
            Row::new(self.line_start as u32),
            Row::new(self.line_end as u32),
            Column::new(self.column_start as u32),
            Column::new(self.column_end as u32),
            PathBuf::from(&self.file_name),
        )
    }
}

#[cfg_attr(feature = "derive", derive(Deserialize))]
#[cfg_attr(feature = "serialize-rustc", derive(RustcDecodable))]
#[derive(Debug, Clone)]
pub struct DiagnosticSpanLine {
    pub text: String,

    /// 1-based, character offset in self.text.
    pub highlight_start: usize,

    pub highlight_end: usize,
}

#[cfg_attr(feature = "derive", derive(Deserialize))]
#[cfg_attr(feature = "serialize-rustc", derive(RustcDecodable))]
#[derive(Debug, Clone)]
pub struct DiagnosticSpanMacroExpansion {
    /// span where macro was applied to generate this code; note that
    /// this may itself derive from a macro (if
    /// `span.expansion.is_some()`)
    pub span: DiagnosticSpan,

    /// name of macro that was applied (e.g., "foo!" or "#[derive(Eq)]")
    pub macro_decl_name: String,

    /// span where macro was defined (if known)
    pub def_site_span: Option<DiagnosticSpan>,
}
