use crate::ast::with_error_checking_parse;
use crate::core::{Match, Session};
use crate::typeinf::get_function_declaration;

use rustc_ast::ast::AssocItemKind;
use rustc_parse::parser::ForceCollect;

/// Returns completion snippets usable by some editors
///
/// Generates a snippet string given a `Match`. The provided snippet contains
/// substrings like "${1:name}" which some editors can use to quickly fill in
/// arguments.
///
/// # Examples
///
/// ```no_run
/// extern crate racer;
///
/// use std::path::Path;
///
/// let path = Path::new(".");
/// let cache = racer::FileCache::default();
/// let session = racer::Session::new(&cache, Some(path));
///
/// let m = racer::complete_fully_qualified_name(
///     "std::fs::canonicalize",
///     &path,
///     &session
/// ).next().unwrap();
///
/// let snip = racer::snippet_for_match(&m, &session);
/// assert_eq!(snip, "canonicalize(${1:path})");
/// ```
pub fn snippet_for_match(m: &Match, session: &Session<'_>) -> String {
    if m.mtype.is_function() {
        let method = get_function_declaration(m, session);
        if let Some(m) = MethodInfo::from_source_str(&method) {
            m.snippet()
        } else {
            "".into()
        }
    } else {
        m.matchstr.clone()
    }
}

struct MethodInfo {
    name: String,
    args: Vec<String>,
}

impl MethodInfo {
    ///Parses method declaration as string and returns relevant data
    fn from_source_str(source: &str) -> Option<MethodInfo> {
        let trim: &[_] = &['\n', '\r', '{', ' '];
        let decorated = format!("{} {{}}()", source.trim_end_matches(trim));

        trace!("MethodInfo::from_source_str: {:?}", decorated);
        with_error_checking_parse(decorated, |p| {
            if let Ok(Some(Some(method))) = p.parse_impl_item(ForceCollect::No) {
                if let AssocItemKind::Fn(ref fn_kind) = method.kind {
                    let decl = &fn_kind.sig.decl;
                    return Some(MethodInfo {
                        // ident.as_str calls Ident.name.as_str
                        name: method.ident.name.to_string(),
                        args: decl
                            .inputs
                            .iter()
                            .map(|arg| {
                                let source_map = &p.sess.source_map();
                                let var_name = match source_map.span_to_snippet(arg.pat.span) {
                                    Ok(name) => name,
                                    _ => "".into(),
                                };
                                match source_map.span_to_snippet(arg.ty.span) {
                                    Ok(ref type_name) if !type_name.is_empty() => {
                                        format!("{}: {}", var_name, type_name)
                                    }
                                    _ => var_name,
                                }
                            })
                            .collect(),
                    });
                }
            }
            debug!("Unable to parse method declaration. |{}|", source);
            None
        })
    }

    ///Returns completion snippets usable by some editors
    fn snippet(&self) -> String {
        format!(
            "{}({})",
            self.name,
            &self
                .args
                .iter()
                .filter(|&s| !s.ends_with("self"))
                .enumerate()
                .fold(String::new(), |cur, (i, ref s)| {
                    let arg = format!("${{{}:{}}}", i + 1, s);
                    let delim = if i > 0 { ", " } else { "" };
                    cur + delim + &arg
                })
        )
    }
}

#[test]
fn method_info_test() {
    let info = MethodInfo::from_source_str("pub fn new() -> Vec<T>").unwrap();
    assert_eq!(info.name, "new");
    assert_eq!(info.args.len(), 0);
    assert_eq!(info.snippet(), "new()");

    let info = MethodInfo::from_source_str("pub fn reserve(&mut self, additional: usize)").unwrap();
    assert_eq!(info.name, "reserve");
    assert_eq!(info.args.len(), 2);
    // it looks odd, but no problme because what our clients see is only snippet
    assert_eq!(info.args[0], "&mut self: &mut self");
    assert_eq!(info.snippet(), "reserve(${1:additional: usize})");
}
