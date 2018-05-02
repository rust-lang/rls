// Copyright 2018 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Conversion of raw rustc-emitted JSON messages into LSP diagnostics.

use std::path::{Path, PathBuf};

use serde_json;
use span::compiler::DiagnosticSpan;
use ls_types::{DiagnosticSeverity, NumberOrString, Range};
use lsp_data::ls_util;

pub use ls_types::Diagnostic;

#[derive(Debug)]
pub struct Suggestion {
    pub range: Range,
    pub new_text: String,
    pub label: String,
}

#[derive(Debug)]
pub struct FileDiagnostic {
    pub file_path: PathBuf,
    pub main: (Diagnostic, Vec<Suggestion>),
    pub secondaries: Vec<(Diagnostic, Vec<Suggestion>)>,
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
    code: String,
}

pub fn parse_diagnostics(message: &str) -> Option<FileDiagnostic> {
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

    let diagnostic_msg = message.message.clone();
    let (first_primary_span_index, first_primary_span) = message
        .spans
        .iter()
        .enumerate()
        .find(|s| s.1.is_primary)
        .unwrap();
    let rls_span = first_primary_span.rls_span().zero_indexed();
    let suggestions = make_suggestions(&message, &rls_span.file);

    let mut source = "rustc";
    let diagnostic = {
        let mut primary_message = diagnostic_msg.clone();
        if let Some(ref primary_label) = first_primary_span.label {
            if primary_label.trim() != primary_message.trim() {
                primary_message.push_str(&format!("\n\n{}", primary_label));
            }
        }

        if let Some(notes) = format_notes(&message.children, first_primary_span) {
            primary_message.push_str(&format!("\n\n{}", notes));
        }

        // A diagnostic source is quite likely to be clippy if it contains
        // the further information link to the rust-clippy project.
        if primary_message.contains("rust-clippy") {
            source = "clippy"
        }

        Diagnostic {
            range: ls_util::rls_to_range(rls_span.range),
            severity: Some(severity(&message.level)),
            code: Some(NumberOrString::String(match message.code {
                Some(ref c) => c.code.clone(),
                None => String::new(),
            })),
            source: Some(source.to_owned()),
            message: primary_message.trim().to_owned(),
            related_information: None,
        }
    };

    // For a compiler error that has secondary spans (e.g. borrow error showing
    // both borrow and error spans) we emit additional diagnostics. These don't
    // include notes and are of an `Information` severity.
    let secondaries = message
        .spans
        .iter()
        .enumerate()
        .filter(|x| x.0 != first_primary_span_index)
        .map(|(_, secondary_span)| {
            let mut secondary_message = if secondary_span.is_within(first_primary_span) {
                String::new()
            } else {
                diagnostic_msg.clone()
            };

            let mut suggestion = secondary_span
                .suggested_replacement
                .as_ref()
                .map(|s| span_suggestion(secondary_span, s));

            if let Some(ref secondary_label) = secondary_span.label {
                let label_suggestion = label_suggestion(secondary_span, secondary_label);
                if suggestion.is_none() && label_suggestion.is_some() {
                    suggestion = label_suggestion;
                } else {
                    secondary_message.push_str(&format!("\n\n{}", secondary_label));
                }
            }
            let severity = Some(if secondary_span.is_primary {
                severity(&message.level)
            } else {
                DiagnosticSeverity::Information
            });
            let rls_span = secondary_span.rls_span().zero_indexed();

            let diag = Diagnostic {
                range: ls_util::rls_to_range(rls_span.range),
                severity,
                code: Some(NumberOrString::String(match message.code {
                    Some(ref c) => c.code.clone(),
                    None => String::new(),
                })),
                source: Some(source.to_owned()),
                message: secondary_message.trim().to_owned(),
                related_information: None,
            };
            (diag, suggestion.map(|s| vec![s]).unwrap_or_default())
        })
        .collect();

    Some(FileDiagnostic {
        file_path: rls_span.file,
        main: (diagnostic, suggestions),
        secondaries,
    })
}

fn format_notes(children: &[CompilerMessage], primary: &DiagnosticSpan) -> Option<String> {
    if !children.is_empty() {
        let mut notes = String::new();
        for &CompilerMessage {
            ref message,
            ref level,
            ref spans,
            ..
        } in children
        {
            macro_rules! add_message_to_notes {
                ($msg: expr) => {{
                    let mut lines = message.lines();
                    notes.push_str(&format!("\n{}: {}", level, lines.next().unwrap()));
                    for line in lines {
                        notes.push_str(&format!(
                            "\n{:indent$}{line}",
                            "",
                            indent = level.len() + 2,
                            line = line,
                        ));
                    }
                }};
            }

            if spans.is_empty() {
                add_message_to_notes!(message);
            } else if spans.len() == 1 && spans[0].is_within(primary) {
                add_message_to_notes!(message);
                if let Some(ref suggested) = spans[0].suggested_replacement {
                    notes.push_str(&format!(": `{}`", suggested));
                }
            }
        }

        if notes.is_empty() {
            None
        } else {
            Some(notes.trim().to_string())
        }
    } else {
        None
    }
}

fn severity(level: &str) -> DiagnosticSeverity {
    if level == "error" {
        DiagnosticSeverity::Error
    } else {
        DiagnosticSeverity::Warning
    }
}

fn make_suggestions(message: &CompilerMessage, file: &Path) -> Vec<Suggestion> {
    let mut suggestions = vec![];
    for sp in message.children.iter().flat_map(|msg| &msg.spans) {
        let span = sp.rls_span().zero_indexed();
        if span.file == file {
            if let Some(ref s) = sp.suggested_replacement {
                let range = ls_util::rls_to_range(span.range);
                let action = if range.start == range.end {
                    "Add"
                } else {
                    "Change to"
                };
                let label = if message
                    .spans
                    .iter()
                    .filter(|s| s.is_primary)
                    .map(|s| s.rls_span().zero_indexed())
                    .any(|s| s.range.row_start == span.range.row_start)
                {
                    // on the same line as diagnostic
                    format!("{} `{}`", action, s)
                } else {
                    format!("Line {}: {} `{}`", range.start.line + 1, action, s)
                };
                let suggestion = Suggestion {
                    new_text: s.clone(),
                    range,
                    label,
                };
                suggestions.push(suggestion);
            }
        }
    }
    suggestions
}

fn span_suggestion(span: &DiagnosticSpan, suggested: &str) -> Suggestion {
    let zspan = span.rls_span().zero_indexed();
    let range = ls_util::rls_to_range(zspan.range);
    let action = if range.start == range.end {
        "Add"
    } else {
        "Change to"
    };
    let label = format!("{} `{}`", action, suggested);
    Suggestion {
        new_text: suggested.to_string(),
        range,
        label,
    }
}

fn label_suggestion(span: &DiagnosticSpan, label: &str) -> Option<Suggestion> {
    let suggest_label = "consider changing this to `";
    if label.starts_with(suggest_label) && label.ends_with('`') {
        let suggested_replacement = &label[suggest_label.len()..label.len() - 1];
        return Some(span_suggestion(span, suggested_replacement));
    }
    None
}

trait IsWithin {
    /// Returns whether `other` is considered within `self`
    /// note: a thing should be 'within' itself
    fn is_within(&self, other: &Self) -> bool;
}
impl<T: PartialOrd<T>> IsWithin for ::std::ops::Range<T> {
    fn is_within(&self, other: &Self) -> bool {
        self.start >= other.start && self.start <= other.end && self.end <= other.end
            && self.end >= other.start
    }
}
impl IsWithin for DiagnosticSpan {
    fn is_within(&self, other: &Self) -> bool {
        let DiagnosticSpan {
            line_start,
            line_end,
            column_start,
            column_end,
            ..
        } = *self;
        (line_start..line_end + 1).is_within(&(other.line_start..other.line_end + 1))
            && (column_start..column_end + 1).is_within(&(other.column_start..other.column_end + 1))
    }
}

/// Tests for formatted messages from the compilers json output
/// run cargo with `--message-format=json` to generate the json for new tests and add .json
/// message files to '../../test_data/compiler_message/'
#[cfg(test)]
mod diagnostic_message_test {
    use super::*;

    pub(super) fn parse_compiler_message(compiler_message: &str) -> FileDiagnostic {
        let _ = ::env_logger::try_init();
        parse_diagnostics(compiler_message).expect("failed to parse compiler message")
    }

    pub(super) trait FileDiagnosticTestExt {
        /// Returns (primary message, secondary messages)
        fn to_messages(&self) -> (String, Vec<String>);

        /// Returns all primary & secondary suggestions
        fn all_suggestions(&self) -> Vec<&Suggestion>;
    }

    impl FileDiagnosticTestExt for FileDiagnostic {
        fn to_messages(&self) -> (String, Vec<String>) {
            (
                self.main.0.message.clone(),
                self.secondaries
                    .iter()
                    .map(|d| d.0.message.clone())
                    .collect(),
            )
        }

        fn all_suggestions(&self) -> Vec<&Suggestion> {
            self.main
                .1
                .iter()
                .chain(self.secondaries.iter().flat_map(|s| &s.1))
                .collect()
        }
    }

    /// ```
    /// fn use_after_move() {
    ///     let s = String::new();
    ///     ::std::mem::drop(s);
    ///     ::std::mem::drop(s);
    /// }
    /// ```
    #[test]
    fn message_use_after_move() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/use-after-move.json"
        ));

        assert_eq!(diag.main.0.source, Some("rustc".into()));
        for source in diag.secondaries.iter().map(|d| d.0.source.as_ref()) {
            assert_eq!(source, Some(&"rustc".into()));
        }

        let (msg, others) = diag.to_messages();
        assert_eq!(
            msg,
            "use of moved value: `s`\n\n\
            value used here after move\n\n\
            note: move occurs because `s` has type `std::string::String`, which does not implement the `Copy` trait"
        );

        assert_eq!(
            others,
            vec![
                "use of moved value: `s`\n\n\
                 value moved here",
            ]
        );
    }

    /// ```
    /// fn type_annotations_needed() {
    ///     let v = Vec::new();
    /// }
    /// ```
    #[test]
    fn message_type_annotations_needed() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/type-annotations-needed.json"
        )).to_messages();
        assert_eq!(
            msg,
            "type annotations needed\n\n\
             cannot infer type for `T`",
        );

        assert_eq!(
            others,
            vec![
                "type annotations needed\n\n\
                 consider giving `v` a type",
            ]
        );
    }

    /// ```
    /// fn mismatched_types() -> usize {
    ///     123_i32
    /// }
    /// ```
    #[test]
    fn message_mismatched_types() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/mismatched-types.json"
        )).to_messages();
        assert_eq!(
            msg,
            "mismatched types\n\n\
             expected usize, found i32",
        );

        assert_eq!(
            others,
            vec![
                "mismatched types\n\n\
                 expected `usize` because of return type",
            ]
        );
    }

    /// ```
    /// fn not_mut() {
    ///     let string = String::new();
    ///     let _s1 = &mut string;
    /// }
    /// ```
    #[test]
    fn message_not_mutable() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/not-mut.json"
        )).to_messages();
        assert_eq!(
            msg,
            "cannot borrow immutable local variable `string` as mutable\n\n\
             cannot borrow mutably",
        );

        // note: consider message becomes a suggetion
        assert_eq!(
            others,
            vec!["cannot borrow immutable local variable `string` as mutable"]
        );
    }

    /// ```
    /// fn consider_borrow() {
    ///     fn takes_ref(s: &str) {}
    ///     let string = String::new();
    ///     takes_ref(string);
    /// }
    /// ```
    #[test]
    fn message_consider_borrowing() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/consider-borrowing.json"
        )).to_messages();
        assert_eq!(
            msg,
            r#"mismatched types

expected &str, found struct `std::string::String`

note: expected type `&str`
         found type `std::string::String`
help: consider borrowing here: `&string`"#,
        );

        assert!(others.is_empty(), "{:?}", others);
    }

    /// ```
    /// fn move_out_of_borrow() {
    ///     match &Some(String::new()) {
    ///         &Some(string) => takes_borrow(&string),
    ///         &None => {},
    ///     }
    /// }
    /// ```
    #[test]
    fn message_move_out_of_borrow() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/move-out-of-borrow.json"
        )).to_messages();
        assert_eq!(msg, "cannot move out of borrowed content");

        assert_eq!(
            others,
            vec![
                "hint: to prevent move, use `ref string` or `ref mut string`",
            ]
        );
    }

    /// ```
    /// use std::{f64, u64, u8 as Foo};
    /// ```
    #[test]
    fn message_unused_use() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/unused-use.json"
        )).to_messages();
        assert_eq!(
            msg,
            "unused imports: `f64`, `u64`, `u8 as Foo`\n\n\
             note: #[warn(unused_imports)] on by default"
        );

        // 2 more warnings for the other two imports
        assert_eq!(
            others,
            vec![
                "unused imports: `f64`, `u64`, `u8 as Foo`",
                "unused imports: `f64`, `u64`, `u8 as Foo`",
            ]
        );
    }

    #[test]
    fn message_cannot_find_type() {
        let (msg, others) = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/cannot-find-type.json"
        )).to_messages();
        assert_eq!(
            msg,
            "cannot find type `HashSet` in this scope\n\n\
             not found in this scope"
        );

        assert!(others.is_empty(), "{:?}", others);
    }

    /// ```
    /// let _s = 1 / 1;
    /// ```
    #[test]
    fn message_clippy_identity_op() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/clippy-identity-op.json"
        ));

        assert_eq!(diag.main.0.source, Some("clippy".into()));
        for source in diag.secondaries.iter().map(|d| d.0.source.as_ref()) {
            assert_eq!(source, Some(&"clippy".into()));
        }

        let (msg, others) = diag.to_messages();
        println!("\n---message---\n{}\n---", msg);

        let link = {
            let link_index = msg.find("https://rust-lang-nursery.github.io/rust-clippy/")
                .expect("no clippy link found in message");
            &msg[link_index..]
        };

        assert_eq!(
            msg,
            "the operation is ineffective. Consider reducing it to `1`\n\n\
             note: #[warn(identity_op)] implied by #[warn(clippy)]\n\
             help: for further information visit "
                .to_owned() + link
        );

        assert!(others.is_empty(), "{:?}", others);
    }
}

/// Tests for creating suggestions from the compilers json output
#[cfg(test)]
mod diagnostic_suggestion_test {
    use super::*;
    use self::diagnostic_message_test::*;
    use ls_types;

    #[test]
    fn suggest_use_when_cannot_find_type() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/cannot-find-type.json"
        ));
        let suggestions = diag.all_suggestions();

        eprintln!("{:#?}", suggestions);

        let use_hash_set = suggestions
            .iter()
            .find(|s| s.new_text == "use std::collections::HashSet;\n")
            .expect("`use std::collections::HashSet` not found");

        assert_eq!(
            use_hash_set.label,
            "Line 15: Add `use std::collections::HashSet;\n`"
        );

        let expected_position = ls_types::Position {
            line: 14,
            character: 0,
        };
        assert_eq!(
            use_hash_set.range,
            Range {
                start: expected_position,
                end: expected_position,
            }
        );
    }

    #[test]
    fn suggest_mut_when_not_mut() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/not-mut.json"
        ));
        let suggestions = diag.all_suggestions();

        eprintln!("{:#?}", suggestions);

        let change_to_mut = suggestions
            .iter()
            .find(|s| s.new_text == "mut string")
            .expect("`mut string` not found");

        assert_eq!(change_to_mut.label, "Change to `mut string`");

        assert_eq!(
            change_to_mut.range,
            Range {
                start: ls_types::Position {
                    line: 132,
                    character: 12,
                },
                end: ls_types::Position {
                    line: 132,
                    character: 18,
                },
            }
        );
    }

    /// ```
    /// pub const WINDOW_PROGRESS: &'static str = "window/progress";
    /// ```
    #[test]
    fn suggest_clippy_const_static() {
        let diag = parse_compiler_message(include_str!(
            "../../test_data/compiler_message/clippy-const-static-lifetime.json"
        ));
        let suggestions = diag.all_suggestions();

        eprintln!("{:#?}", suggestions);

        let change_to_mut = suggestions
            .iter()
            .find(|s| s.new_text == "&str")
            .expect("`&str` not found");

        assert_eq!(change_to_mut.label, "Change to `&str`");

        assert_eq!(
            change_to_mut.range,
            Range {
                start: ls_types::Position {
                    line: 354,
                    character: 34,
                },
                end: ls_types::Position {
                    line: 354,
                    character: 46,
                },
            }
        );
    }
}
