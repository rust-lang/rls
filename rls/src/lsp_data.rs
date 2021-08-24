//! Types, helpers, and conversions to and from LSP and `racer` types.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

pub use lsp_types::notification::Notification as LSPNotification;
pub use lsp_types::request::Request as LSPRequest;
pub use lsp_types::*;
use rls_analysis::DefKind;
use rls_span as span;
use serde_derive::{Deserialize, Serialize};
use url::Url;

use crate::actions::hover;
use crate::config;

/// An error that can occur when parsing a file URI.
#[derive(Debug)]
pub enum UrlFileParseError {
    /// The URI scheme is not `file`.
    InvalidScheme,
    /// Invalid file path in the URI.
    InvalidFilePath,
}

impl Error for UrlFileParseError {}

impl fmt::Display for UrlFileParseError
where
    UrlFileParseError: Error,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = match self {
            UrlFileParseError::InvalidScheme => "URI scheme is not `file`",
            UrlFileParseError::InvalidFilePath => "Invalid file path in URI",
        };
        write!(f, "{}", description)
    }
}

/// Parses the given URI into a `PathBuf`.
pub fn parse_file_path(uri: &Url) -> Result<PathBuf, UrlFileParseError> {
    if uri.scheme() == "file" {
        uri.to_file_path().map_err(|_err| UrlFileParseError::InvalidFilePath)
    } else {
        Err(UrlFileParseError::InvalidScheme)
    }
}

/// Creates an edit for the given location and text.
pub fn make_workspace_edit(location: Location, new_text: String) -> WorkspaceEdit {
    let changes = vec![(location.uri, vec![TextEdit { range: location.range, new_text }])]
        .into_iter()
        .collect();

    WorkspaceEdit { changes: Some(changes), document_changes: None }
}

/// Utilities for working with the language server protocol.
pub mod ls_util {
    use super::*;
    use crate::Span;

    /// Converts a language server protocol range into an RLS range.
    /// NOTE: this does not translate LSP UTF-16 code units offsets into Unicode
    /// Scalar Value offsets as expected by RLS/Rust.
    pub fn range_to_rls(r: Range) -> span::Range<span::ZeroIndexed> {
        span::Range::from_positions(position_to_rls(r.start), position_to_rls(r.end))
    }

    /// Converts a language server protocol position into an RLS position.
    pub fn position_to_rls(p: Position) -> span::Position<span::ZeroIndexed> {
        span::Position::new(
            span::Row::new_zero_indexed(p.line as u32),
            span::Column::new_zero_indexed(p.character as u32),
        )
    }

    /// Converts a language server protocol location into an RLS span.
    pub fn location_to_rls(
        l: &Location,
    ) -> Result<span::Span<span::ZeroIndexed>, UrlFileParseError> {
        parse_file_path(&l.uri).map(|path| Span::from_range(range_to_rls(l.range), path))
    }

    /// Converts an RLS span into a language server protocol location.
    pub fn rls_to_location(span: &Span) -> Location {
        // An RLS span has the same info as an LSP `Location`.
        Location { uri: Url::from_file_path(&span.file).unwrap(), range: rls_to_range(span.range) }
    }

    /// Converts an RLS location into a language server protocol location.
    pub fn rls_location_to_location(l: &span::Location<span::ZeroIndexed>) -> Location {
        Location {
            uri: Url::from_file_path(&l.file).unwrap(),
            range: rls_to_range(span::Range::from_positions(l.position, l.position)),
        }
    }

    /// Converts an RLS range into a language server protocol range.
    pub fn rls_to_range(r: span::Range<span::ZeroIndexed>) -> Range {
        Range { start: rls_to_position(r.start()), end: rls_to_position(r.end()) }
    }

    /// Converts an RLS position into a language server protocol range.
    pub fn rls_to_position(p: span::Position<span::ZeroIndexed>) -> Position {
        Position { line: p.row.0.into(), character: p.col.0.into() }
    }

    /// Creates a `Range` spanning the whole file as currently known by `Vfs`
    ///
    /// Panics if `Vfs` cannot load the file.
    pub fn range_from_file_string(content: impl AsRef<str>) -> Range {
        let content = content.as_ref();

        if content.is_empty() {
            Range { start: Position::new(0, 0), end: Position::new(0, 0) }
        } else {
            let mut line_count = content.lines().count() as u64 - 1;
            let col = if content.ends_with('\n') {
                line_count += 1;
                0
            } else {
                content
                    .lines()
                    .last()
                    .expect("String is not empty.")
                    .chars()
                    // LSP uses UTF-16 code units offset.
                    .map(|chr| chr.len_utf16() as u64)
                    .sum()
            };
            // Range is zero-based and end position is exclusive.
            Range { start: Position::new(0, 0), end: Position::new(line_count, col) }
        }
    }
}

/// Converts an RLS def-kind to a language server protocol symbol-kind.
pub fn source_kind_from_def_kind(k: DefKind) -> SymbolKind {
    match k {
        DefKind::Enum | DefKind::Union => SymbolKind::Enum,
        DefKind::Static | DefKind::Const | DefKind::ForeignStatic => SymbolKind::Constant,
        DefKind::Tuple => SymbolKind::Array,
        DefKind::Struct => SymbolKind::Struct,
        DefKind::Function | DefKind::Macro | DefKind::ForeignFunction => SymbolKind::Function,
        DefKind::Method => SymbolKind::Method,
        DefKind::Mod => SymbolKind::Module,
        DefKind::Trait => SymbolKind::Interface,
        DefKind::Type | DefKind::ExternType => SymbolKind::TypeParameter,
        DefKind::Local => SymbolKind::Variable,
        DefKind::Field => SymbolKind::Field,
        DefKind::TupleVariant | DefKind::StructVariant => SymbolKind::EnumMember,
    }
}

/// Indicates the kind of completion for this racer match type.
pub fn completion_kind_from_match_type(m: racer::MatchType) -> CompletionItemKind {
    match m {
        racer::MatchType::Crate | racer::MatchType::Module => CompletionItemKind::Module,
        racer::MatchType::Struct(_) => CompletionItemKind::Struct,
        racer::MatchType::Union(_) => CompletionItemKind::Struct,
        racer::MatchType::Enum(_) => CompletionItemKind::Enum,
        racer::MatchType::StructField | racer::MatchType::EnumVariant(_) => {
            CompletionItemKind::Field
        }
        racer::MatchType::Macro
        | racer::MatchType::Function
        | racer::MatchType::Method(_)
        | racer::MatchType::FnArg(_) => CompletionItemKind::Function,
        racer::MatchType::Type | racer::MatchType::Trait => CompletionItemKind::Interface,
        racer::MatchType::Let(_)
        | racer::MatchType::IfLet(_)
        | racer::MatchType::WhileLet(_)
        | racer::MatchType::For(_)
        | racer::MatchType::MatchArm
        | racer::MatchType::Const
        | racer::MatchType::Static => CompletionItemKind::Variable,
        racer::MatchType::TypeParameter(_) => CompletionItemKind::TypeParameter,
        racer::MatchType::Builtin(_) => CompletionItemKind::Keyword,
        racer::MatchType::UseAlias(m) => match m.mtype {
            racer::MatchType::UseAlias(_) => unreachable!("Nested use aliases"),
            typ => completion_kind_from_match_type(typ),
        },
        racer::MatchType::AssocType => CompletionItemKind::TypeParameter,
    }
}

/// Converts a racer match into an RLS completion.
pub fn completion_item_from_racer_match(m: &racer::Match) -> CompletionItem {
    let mut item = CompletionItem::new_simple(m.matchstr.clone(), m.contextstr.clone());
    item.kind = Some(completion_kind_from_match_type(m.mtype.clone()));

    if !m.docs.is_empty() {
        item.documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: hover::process_docs(&m.docs),
        }));
    }

    item
}

/* ------  Extension methods for JSON-RPC protocol types ------ */

/// Provides additional methods for the remote `Range` type.
pub trait RangeExt {
    /// `true` if both `Range`s overlap.
    fn overlaps(&self, other: &Self) -> bool;
}

impl RangeExt for Range {
    fn overlaps(&self, other: &Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// `DidChangeConfigurationParams.settings` payload reading the `{ rust: {...} }` bit.
#[derive(Debug, Deserialize)]
pub struct ChangeConfigSettings {
    pub rust: config::Config,
}

impl ChangeConfigSettings {
    /// try to deserialize a ChangeConfigSettings from a json value, val is
    /// expected to be a Value::Object containing only one key "rust", all first
    /// level keys of rust's value are converted to snake_case, duplicated and
    /// unknown keys are reported
    pub fn try_deserialize(
        val: &serde_json::value::Value,
        dups: &mut std::collections::HashMap<String, Vec<String>>,
        unknowns: &mut Vec<String>,
        deprecated: &mut Vec<String>,
    ) -> Result<ChangeConfigSettings, ()> {
        let mut ret = Err(());
        if let serde_json::Value::Object(map) = val {
            for (k, v) in map.iter() {
                if k != "rust" {
                    unknowns.push(k.to_string());
                    continue;
                }
                if let serde_json::Value::Object(_) = v {
                    if let Ok(rust) = config::Config::try_deserialize(v, dups, unknowns, deprecated)
                    {
                        ret = Ok(ChangeConfigSettings { rust });
                    }
                } else {
                    return Err(());
                }
            }
        }
        ret
    }
}

/* -----------------  JSON-RPC protocol types ----------------- */

/// Supported initialization options that can be passed in the `initialize`
/// request, under `initialization_options` key. These are specific to the RLS.
#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InitializationOptions {
    /// `true` if build should not be triggered immediately after receiving `initialize`.
    pub omit_init_build: bool,
    pub cmd_run: bool,
    /// `DidChangeConfigurationParams.settings` payload for upfront configuration.
    pub settings: Option<ChangeConfigSettings>,
}

impl InitializationOptions {
    /// try to deserialize an Initialization from a json value. If exists,
    /// val.settings is expected to be a Value::Object containing only one key,
    /// "rust", all first level keys of rust's value are converted to
    /// snake_case, duplicated and unknown keys are reported
    pub fn try_deserialize(
        mut val: serde_json::value::Value,
        dups: &mut std::collections::HashMap<String, Vec<String>>,
        unknowns: &mut Vec<String>,
        deprecated: &mut Vec<String>,
    ) -> Result<InitializationOptions, ()> {
        let settings = val.get_mut("settings").map(|x| x.take()).and_then(|set| {
            ChangeConfigSettings::try_deserialize(&set, dups, unknowns, deprecated).ok()
        });

        Ok(InitializationOptions { settings, ..serde_json::from_value(val).map_err(|_| ())? })
    }
}

impl Default for InitializationOptions {
    fn default() -> Self {
        InitializationOptions { omit_init_build: false, cmd_run: false, settings: None }
    }
}

// Subset of flags from lsp_types::ClientCapabilities that affects this RLS.
// Passed in the `initialize` request under `capabilities`.
#[derive(Debug, PartialEq, Deserialize, Serialize, Clone, Copy, Default)]
#[serde(default)]
pub struct ClientCapabilities {
    pub code_completion_has_snippet_support: bool,
    pub related_information_support: bool,
}

impl ClientCapabilities {
    pub fn new(params: &lsp_types::InitializeParams) -> ClientCapabilities {
        // `lsp_types::ClientCapabilities` is a rather awkward object to use internally
        // (for instance, it doesn't `Clone`). Instead we pick out the bits of it that we
        // are going to handle into `ClientCapabilities`. The upside of
        // using this very simple struct is that it can be kept thread safe
        // without mutex locking it on every request.
        let code_completion_has_snippet_support = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|doc| doc.completion.as_ref())
            .and_then(|comp| comp.completion_item.as_ref())
            .and_then(|item| item.snippet_support.as_ref())
            .copied()
            .unwrap_or(false);

        let related_information_support = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|doc| doc.publish_diagnostics.as_ref())
            .and_then(|diag| diag.related_information.as_ref())
            .copied()
            .unwrap_or(false);

        ClientCapabilities { code_completion_has_snippet_support, related_information_support }
    }
}
