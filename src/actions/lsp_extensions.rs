// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use url_serde;
use lsp_data::*;
use url::Url;

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct PublishRustDiagnosticsParams {
    /// The URI for which diagnostic information is reported.
    #[serde(deserialize_with = "url_serde::deserialize", serialize_with = "url_serde::serialize")]
    pub uri: Url,

    /// An array of diagnostic information items.
    pub diagnostics: Vec<RustDiagnostic>,
}

/// A range in a text document expressed as (zero-based) start and end positions.
/// A range is comparable to a selection in an editor. Therefore the end position is exclusive.
#[derive(Debug, PartialEq, Clone, Default, Deserialize, Serialize)]
pub struct LabelledRange {
    /// The range's start position.
    pub start: Position,
    /// The range's end position.
    pub end: Position,
    /// The optional label.
    pub label: Option<String>,
}

/// Represents a diagnostic, such as a compiler error or warning.
/// Diagnostic objects are only valid in the scope of a resource.
#[allow(non_snake_case)]
#[derive(Debug, PartialEq, Clone, Default, Deserialize, Serialize)]
pub struct RustDiagnostic {
    /// The primary range at which the message applies.
    pub range: LabelledRange,

    /// The secondary ranges that apply to the message
    pub secondaryRanges: Vec<LabelledRange>,

    /// The diagnostic's severity. Can be omitted. If omitted it is up to the
    /// client to interpret diagnostics as error, warning, info or hint.
    pub severity: Option<DiagnosticSeverity>,

    /// The diagnostic's code. Can be omitted.
    pub code: Option<NumberOrString>,

    /// A human-readable string describing the source of this
    /// diagnostic, e.g. 'typescript' or 'super lint'.
    pub source: Option<String>,

    /// The diagnostic's message.
    pub message: String,
}

