use std::path::{Path, PathBuf};

use log::*;
use rls_analysis::{Def, DefKind};
use rls_span::{Column, Range, Row, Span, ZeroIndexed};
use rls_vfs::{self as vfs, Vfs};
use rustfmt_nightly::NewlineStyle;
use serde_derive::{Deserialize, Serialize};

use crate::actions::format::Rustfmt;
use crate::actions::requests;
use crate::actions::InitActionContext;
use crate::config::FmtConfig;
use crate::lsp_data::*;
use crate::server::ResponseError;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Tooltip {
    pub contents: Vec<MarkedString>,
    pub range: Range<ZeroIndexed>,
}

/// Cleanup documentation code blocks. The `docs` are expected to have
/// the preceding `///` or `//!` prefixes already trimmed away. Rust code
/// blocks will ignore lines beginning with `#`. Code block annotations
/// that are common to Rust will be converted to `rust` allow for markdown
/// syntax coloring.
pub fn process_docs(docs: &str) -> String {
    trace!("process_docs");
    let mut in_codeblock = false;
    let mut in_rust_codeblock = false;
    let mut processed_docs = Vec::new();
    let mut last_line_ignored = false;
    for line in docs.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_rust_codeblock = trimmed == "```"
                || trimmed.contains("rust")
                || trimmed.contains("no_run")
                || trimmed.contains("ignore")
                || trimmed.contains("should_panic")
                || trimmed.contains("compile_fail");
            in_codeblock = !in_codeblock;
            if !in_codeblock {
                in_rust_codeblock = false;
            }
        }
        let line = if in_rust_codeblock && trimmed.starts_with("```") {
            "```rust".into()
        } else {
            line.to_string()
        };

        // Racer sometimes pulls out comment block headers from the standard library.
        let ignore_slashes = line.starts_with("////");

        let maybe_attribute = trimmed.starts_with("#[") || trimmed.starts_with("#![");
        let is_attribute = maybe_attribute && in_rust_codeblock;
        let is_hidden = trimmed.starts_with('#') && in_rust_codeblock && !is_attribute;

        let ignore_whitespace = last_line_ignored && trimmed.is_empty();
        let ignore_line = ignore_slashes || ignore_whitespace || is_hidden;

        if !ignore_line {
            processed_docs.push(line);
            last_line_ignored = false;
        } else {
            last_line_ignored = true;
        }
    }

    processed_docs.join("\n")
}

/// Extracts documentation from the `file` at the specified `row_start`.
/// If the row is equal to `0`, the scan will include the current row
/// and move _downward_. Otherwise, the scan will ignore the specified
/// row and move _upward_.
pub fn extract_docs(
    vfs: &Vfs,
    file: &Path,
    row_start: Row<ZeroIndexed>,
) -> Result<Vec<String>, vfs::Error> {
    let up = row_start.0 > 0;
    debug!("extract_docs: row_start = {:?}, up = {:?}, file = {:?}", row_start, up, file);

    let mut docs: Vec<String> = Vec::new();
    let mut row = if up {
        Row::new_zero_indexed(row_start.0.saturating_sub(1))
    } else {
        Row::new_zero_indexed(row_start.0)
    };
    let mut in_meta = false;
    let mut hit_top = false;
    loop {
        let line = vfs.load_line(file, row)?;

        let next_row = if up {
            Row::new_zero_indexed(row.0.saturating_sub(1))
        } else {
            Row::new_zero_indexed(row.0.saturating_add(1))
        };

        if row == next_row {
            hit_top = true;
        } else {
            row = next_row;
        }

        let line = line.trim();

        let attr_start = line.starts_with("#[") || line.starts_with("#![");

        if attr_start && line.ends_with(']') && !hit_top {
            // Ignore single-line attributes.
            trace!(
                "extract_docs: ignoring single-line attribute, next_row: {:?}, up: {}",
                next_row,
                up
            );
            continue;
        }

        // Continue with the next line when transitioning out of a
        // multi-line attribute.
        if attr_start || (line.ends_with(']') && !line.starts_with("//")) {
            in_meta = !in_meta;
            if !in_meta && !hit_top {
                trace!(
                    "extract_docs: exiting multi-line attribute, next_row: {:?}, up: {}",
                    next_row,
                    up
                );
                continue;
            };
        }

        if !hit_top && in_meta {
            // Ignore milti-line attributes.
            trace!(
                "extract_docs: ignoring multi-line attribute, next_row: {:?}, up: {}, in_meta: {}",
                next_row,
                up,
                in_meta
            );
            continue;
        } else if line.starts_with("////") {
            trace!(
                "extract_docs: break on comment header block, next_row: {:?}, up: {}",
                next_row,
                up
            );
            break;
        } else if line.starts_with("///") && !up {
            trace!("extract_docs: break on non-module docs, next_row: {:?}, up: {}", next_row, up);
            break;
        } else if line.starts_with("//!") && up {
            trace!("extract_docs: break on module docs, next_row: {:?}, up: {}", next_row, up);
            break;
        } else if line.starts_with("///") || line.starts_with("//!") {
            let pos =
                if line.chars().nth(3).map(char::is_whitespace).unwrap_or(false) { 4 } else { 3 };
            let doc_line = line[pos..].into();
            if up {
                docs.insert(0, doc_line);
            } else {
                docs.push(doc_line);
            }
        }

        if hit_top {
            // The top of the file was reached.
            debug!(
                "extract_docs: bailing out: prev_row == next_row; next_row = {:?}, up = {}",
                next_row, up
            );
            break;
        } else if line.starts_with("//") {
            trace!(
                "extract_docs: continuing after comment line, next_row: {:?}, up: {}",
                next_row,
                up
            );
            continue;
        } else if line.is_empty() {
            trace!(
                "extract_docs: continuing after empty line, next_row: {:?}, up: {}",
                next_row,
                up
            );
            continue;
        } else {
            trace!("extract_docs: end of docs, next_row: {:?}, up: {}", next_row, up);
            break;
        }
    }
    debug!(
        "extract_docs: complete: row_end = {:?} (exclusive), up = {:?}, file = {:?}",
        row, up, file
    );
    Ok(docs)
}

fn extract_and_process_docs(vfs: &Vfs, file: &Path, row_start: Row<ZeroIndexed>) -> Option<String> {
    extract_docs(vfs, &file, row_start)
        .map_err(|e| {
            error!("failed to extract docs: row: {:?}, file: {:?} ({:?})", row_start, file, e);
        })
        .ok()
        .map(|docs| docs.join("\n"))
        .map(|docs| process_docs(&docs))
        .and_then(empty_to_none)
}

/// Extracts a function, method, struct, enum, or trait declaration from source.
pub fn extract_decl(
    vfs: &Vfs,
    file: &Path,
    mut row: Row<ZeroIndexed>,
) -> Result<Vec<String>, vfs::Error> {
    debug!("extract_decl: row_start: {:?}, file: {:?}", row, file);
    let mut lines = Vec::new();
    loop {
        match vfs.load_line(file, row) {
            Ok(line) => {
                row = Row::new_zero_indexed(row.0.saturating_add(1));
                let mut line = line.trim();
                if let Some(pos) = line.rfind('{') {
                    line = &line[0..pos].trim_end();
                    lines.push(line.into());
                    break;
                } else if line.ends_with(';') {
                    let pos = line.len() - 1;
                    line = &line[0..pos].trim_end();
                    lines.push(line.into());
                    break;
                } else {
                    lines.push(line.into());
                }
            }
            Err(e) => {
                error!("extract_decl: error: {:?}", e);
                return Err(e);
            }
        }
    }
    Ok(lines)
}

fn tooltip_local_variable_usage(
    ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_local_variable_usage: {}", def.name);

    let the_type = def.value.trim().into();
    let mut context = String::new();
    if ctx.config.lock().unwrap().show_hover_context {
        match ctx.vfs.load_line(&def.span.file, def.span.range.row_start) {
            Ok(line) => {
                context.push_str(line.trim());
            }
            Err(e) => {
                error!("tooltip_local_variable_usage: error = {:?}", e);
            }
        }
        if context.ends_with('{') {
            context.push_str(" ... }");
        }
    }

    let context = empty_to_none(context);
    let docs = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_type(ctx: &InitActionContext, def: &Def, doc_url: Option<String>) -> Vec<MarkedString> {
    debug!("tooltip_type: {}", def.name);

    let vfs = &ctx.vfs;

    let the_type = || def.value.trim().into();
    let the_type = def_decl(def, &vfs, the_type);
    let docs = def_docs(def, &vfs);
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_field_or_variant(
    ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_field_or_variant: {}", def.name);

    let vfs = &ctx.vfs;

    let the_type = def.value.trim().into();
    let docs = def_docs(def, &vfs);
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_struct_enum_union_trait(
    ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_struct_enum_union_trait: {}", def.name);

    let vfs = &ctx.vfs;
    let fmt_config = ctx.fmt_config();
    // We hover often, so use the in-process one to speed things up.
    let fmt = Rustfmt::Internal;

    // Fallback in case source extration fails.
    let the_type = || match def.kind {
        DefKind::Struct => format!("struct {}", def.name),
        DefKind::Enum => format!("enum {}", def.name),
        DefKind::Union => format!("union {}", def.name),
        DefKind::Trait => format!("trait {}", def.value),
        _ => def.value.trim().to_string(),
    };

    let decl = def_decl(def, &vfs, the_type);

    let the_type = format_object(fmt, &fmt_config, decl);
    let docs = def_docs(def, &vfs);
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_mod(ctx: &InitActionContext, def: &Def, doc_url: Option<String>) -> Vec<MarkedString> {
    debug!("tooltip_mod: name: {}", def.name);

    let vfs = &ctx.vfs;

    let the_type = def.value.trim();
    let the_type = the_type.replace("\\\\", "/");
    let the_type = the_type.replace("\\", "/");

    let mod_path = if let Some(dir) = ctx.current_project.file_name() {
        if Path::new(&the_type).starts_with(dir) {
            the_type.chars().skip(dir.len() + 1).collect()
        } else {
            the_type
        }
    } else {
        the_type
    };

    let docs = def_docs(def, &vfs);
    let context = None;

    create_tooltip(mod_path, doc_url, context, docs)
}

fn tooltip_function_method(
    ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_function_method: {}", def.name);

    let vfs = &ctx.vfs;
    let fmt_config = ctx.fmt_config();
    // We hover often, so use the in-process one to speed things up.
    let fmt = Rustfmt::Internal;

    let the_type = || {
        def.value
            .trim()
            .replacen("fn ", &format!("fn {}", def.name), 1)
            .replace("> (", ">(")
            .replace("->(", "-> (")
    };

    let decl = def_decl(def, &vfs, the_type);

    let the_type = format_method(fmt, &fmt_config, decl);
    let docs = def_docs(def, &vfs);
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_local_variable_decl(
    _ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_local_variable_decl: {}", def.name);

    let the_type = def.value.trim().into();
    let docs = None;
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_function_arg_usage(
    _ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_function_arg_usage: {}", def.name);

    let the_type = def.value.trim().into();
    let docs = None;
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_function_signature_arg(
    _ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_function_signature_arg: {}", def.name);

    let the_type = def.value.trim().into();
    let docs = None;
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn tooltip_static_const_decl(
    ctx: &InitActionContext,
    def: &Def,
    doc_url: Option<String>,
) -> Vec<MarkedString> {
    debug!("tooltip_static_const_decl: {}", def.name);

    let vfs = &ctx.vfs;

    let the_type = def_decl(def, &vfs, || def.value.trim().into());
    let docs = def_docs(def, &vfs);
    let context = None;

    create_tooltip(the_type, doc_url, context, docs)
}

fn empty_to_none(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Extracts and processes source documentation for the give `def`.
fn def_docs(def: &Def, vfs: &Vfs) -> Option<String> {
    let save_analysis_docs = || empty_to_none(def.docs.trim().into());
    extract_and_process_docs(&vfs, def.span.file.as_ref(), def.span.range.row_start)
        .or_else(save_analysis_docs)
        .filter(|docs| !docs.trim().is_empty())
}

/// Returns the type or function declaration from source. If source
/// extraction fails, the result of `the_type` is used as a fallback.
fn def_decl<F>(def: &Def, vfs: &Vfs, the_type: F) -> String
where
    F: FnOnce() -> String,
{
    extract_decl(vfs, &def.span.file, def.span.range.row_start)
        .map(|lines| lines.join("\n"))
        .ok()
        .or_else(|| Some(the_type()))
        .unwrap()
}

/// Creates a tooltip using the function, type or other declaration and
/// optional doc URL, context, or markdown documentation. No additional
/// processing or formatting is performed.
fn create_tooltip(
    the_type: String,
    doc_url: Option<String>,
    context: Option<String>,
    docs: Option<String>,
) -> Vec<MarkedString> {
    let mut tooltip = vec![];
    let rust = "rust".to_string();
    if !the_type.trim().is_empty() {
        tooltip.push(MarkedString::from_language_code(rust.clone(), the_type));
    }
    if let Some(doc_url) = doc_url {
        tooltip.push(MarkedString::from_markdown(doc_url));
    }
    if let Some(context) = context {
        tooltip.push(MarkedString::from_language_code(rust, context));
    }
    if let Some(docs) = docs {
        tooltip.push(MarkedString::from_markdown(docs));
    }
    tooltip
}

/// Collapses parent directory references inside of paths.
///
/// # Example
///
/// ```ignore
/// # use std::path::PathBuf;
///
/// let path = PathBuf::from("libstd/../liballoc/string.rs");
/// let collapsed = collapse_parents(path);
/// let expected = PathBuf::from("liballoc/string.rs");
/// assert_eq!(expected, collapsed);
/// ```
fn collapse_parents(path: PathBuf) -> PathBuf {
    use std::path::Component;
    let mut components = Vec::new();
    let mut skip;
    let mut skip_prev = false;
    for comp in path.components().rev() {
        if comp == Component::ParentDir {
            skip = true;
        } else {
            skip = false;
        }
        if !skip && !skip_prev {
            components.insert(0, comp);
        }
        skip_prev = skip;
    }

    components.iter().fold(PathBuf::new(), |mut path, comp| {
        path.push(comp);
        path
    })
}

/// Converts a racer `Match` to a save-analysis `Def`. Returns
/// `None` if the coordinates are not available on the match.
fn racer_match_to_def(ctx: &InitActionContext, m: &racer::Match) -> Option<Def> {
    use racer::MatchType;
    fn to_def_kind(kind: &MatchType) -> DefKind {
        match kind {
            MatchType::Struct(_) => DefKind::Struct,
            MatchType::Union(_) => DefKind::Union,
            MatchType::Module => DefKind::Mod,
            MatchType::MatchArm => DefKind::Local,
            MatchType::Function | MatchType::Method(_) => DefKind::Function,
            MatchType::Crate => DefKind::Mod,
            MatchType::Let(_)
            | MatchType::IfLet(_)
            | MatchType::WhileLet(_)
            | MatchType::For(_) => DefKind::Local,
            MatchType::StructField => DefKind::Field,
            MatchType::Enum(_) => DefKind::Enum,
            MatchType::EnumVariant(_) => DefKind::StructVariant,
            MatchType::Type | MatchType::TypeParameter(_) | MatchType::AssocType => DefKind::Type,
            MatchType::FnArg(_) => DefKind::Local,
            MatchType::Trait => DefKind::Trait,
            MatchType::Const => DefKind::Const,
            MatchType::Static => DefKind::Static,
            MatchType::Macro => DefKind::Macro,
            MatchType::Builtin(_) => DefKind::Macro,
            MatchType::UseAlias(m) => match m.mtype {
                MatchType::UseAlias(_) => unreachable!("Nested use aliases"),
                _ => to_def_kind(&m.mtype),
            },
        }
    }
    let kind = to_def_kind(&m.mtype);

    let contextstr = if kind == DefKind::Mod {
        use std::env;

        let home = home::home_dir().unwrap_or_default();
        let cargo_home =
            env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| home.join(".cargo"));
        let cargo_registry_src =
            cargo_home.join("registry").join("src").join("github.com-1ecc6299db9ec823");
        let rust_src_path = racer::get_rust_src_path().ok();

        let contextstr = m.contextstr.replacen("\\\\?\\", "", 1);
        let contextstr_path = PathBuf::from(&contextstr);
        let contextstr_path = collapse_parents(contextstr_path);

        // Attempt to tidy up the module path
        rust_src_path
            .and_then(|rust_src_path| {
                // Make the path relative to Rust src root
                contextstr_path.strip_prefix(rust_src_path).ok().map(ToOwned::to_owned)
            })
            .or_else(|| {
                // Make the path relative to the package root cached in Cargo registry
                contextstr_path.strip_prefix(cargo_registry_src).ok().map(ToOwned::to_owned)
            })
            .or_else(|| {
                // Make the path relative to the root of the project
                contextstr_path.strip_prefix(&ctx.current_project).ok().map(ToOwned::to_owned)
            })
            .and_then(|path| path.to_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| contextstr.to_string())
    } else {
        m.contextstr.trim_end_matches('{').trim().to_string()
    };

    let filepath = m.filepath.clone();
    let matchstr = m.matchstr.clone();
    let matchstr_len = matchstr.len() as u32;
    let docs = m.docs.trim().to_string();
    m.coords.map(|coords| {
        assert!(
            coords.row.0 > 0,
            "racer_match_to_def: racer returned `0` for a 1-based row: {:?}",
            m
        );
        let (row, col1) = requests::from_racer_coord(coords);
        let col2 = Column::new_zero_indexed(col1.0 + matchstr_len);
        let row = Row::new_zero_indexed(row.0 - 1);
        let span = Span::new(row, row, col1, col2, filepath);
        let def = Def {
            kind,
            span,
            name: matchstr,
            value: contextstr,
            qualname: "".to_string(),
            distro_crate: false,
            parent: None,
            docs,
        };
        trace!(
            "racer_match_to_def: Def {{ kind: {:?}, span: {:?}, name: {:?}, \
             value: {:?}, qualname: {:?}, distro_crate: {:?}, \
             parent: {:?}, docs.is_empty: {:?} }}",
            def.kind,
            def.span,
            def.name,
            def.value,
            def.qualname,
            def.distro_crate,
            def.parent,
            def.docs.is_empty()
        );
        def
    })
}

/// Uses racer to synthesize a `Def` for the given `span`. If no appropriate
/// match is found with coordinates, `None` is returned.
fn racer_def(ctx: &InitActionContext, span: &Span<ZeroIndexed>) -> Option<Def> {
    let file_path = &span.file;

    if !file_path.as_path().exists() {
        error!("racer_def: skipping non-existant file: {:?}", file_path);
        return None;
    }

    let name = ctx.vfs.load_line(file_path.as_path(), span.range.row_start).ok().and_then(|line| {
        let col_start = span.range.col_start.0 as usize;
        let col_end = span.range.col_end.0 as usize;
        line.get(col_start..col_end).map(ToOwned::to_owned)
    });

    debug!("racer_def: name: {:?}", name);

    let results = ::std::panic::catch_unwind(move || {
        let cache = ctx.racer_cache();
        let session = ctx.racer_session(&cache);
        let row = span.range.row_end.one_indexed();
        let coord = requests::racer_coord(row, span.range.col_end);
        let location = racer::Location::Coords(coord);
        trace!("racer_def: file_path: {:?}, location: {:?}", file_path, location);
        let racer_match = racer::find_definition(file_path, location, &session);
        trace!("racer_def: match: {:?}", racer_match);
        racer_match
            // Avoid creating tooltip text that is exactly the item being hovered over.
            .filter(|m| name.as_ref().map(|name| name != &m.contextstr).unwrap_or(true))
            .and_then(|m| racer_match_to_def(ctx, &m))
    });

    let results = results.map_err(|_| {
        error!("racer_def: racer panicked");
    });

    results.unwrap_or(None)
}

/// Formats a struct, enum, union, or trait. The original type is returned
/// in the event of an error.
fn format_object(rustfmt: Rustfmt, fmt_config: &FmtConfig, the_type: String) -> String {
    debug!("format_object: {}", the_type);
    let mut config = fmt_config.get_rustfmt_config().clone();
    config.set().newline_style(NewlineStyle::Unix);
    let trimmed = the_type.trim();

    // Normalize the ending for rustfmt.
    let object = if trimmed.ends_with(')') {
        format!("{};", trimmed)
    } else if trimmed.ends_with('}') || trimmed.ends_with(';') {
        trimmed.to_string()
    } else if trimmed.ends_with('{') {
        let trimmed = trimmed.trim_end_matches('{').to_string();
        format!("{}{{}}", trimmed)
    } else {
        format!("{}{{}}", trimmed)
    };

    let formatted = match rustfmt.format(object.clone(), config) {
        Ok(lines) => match lines.rfind('{') {
            Some(pos) => lines[0..pos].into(),
            None => lines,
        },
        Err(e) => {
            error!("format_object: error: {:?}, input: {:?}", e, object);
            trimmed.to_string()
        }
    };

    // If it's a tuple, remove the trailing ';' and hide non-pub components
    // for pub types.
    let result = if formatted.trim().ends_with(';') {
        let mut decl = formatted.trim().trim_end_matches(';');
        if let (Some(pos), true) = (decl.rfind('('), decl.ends_with(')')) {
            let mut hidden_count = 0;
            let tuple_parts = decl[pos + 1..decl.len() - 1]
                .split(',')
                .map(|part| {
                    let part = part.trim();
                    if decl.starts_with("pub") && !part.starts_with("pub") {
                        hidden_count += 1;
                        "_".to_string()
                    } else {
                        part.to_string()
                    }
                })
                .collect::<Vec<String>>();
            decl = &decl[0..pos];
            if hidden_count != tuple_parts.len() {
                format!("{}({})", decl, tuple_parts.join(", "))
            } else {
                decl.to_string()
            }
        } else {
            // Not a tuple.
            decl.into()
        }
    } else {
        // Not a tuple or unit struct.
        formatted
    };

    result.trim().into()
}

/// Formats a method or function. The original type is returned
/// in the event of an error.
fn format_method(rustfmt: Rustfmt, fmt_config: &FmtConfig, the_type: String) -> String {
    trace!("format_method: {}", the_type);
    let the_type = the_type.trim().trim_end_matches(';').to_string();

    let mut config = fmt_config.get_rustfmt_config().clone();
    config.set().newline_style(NewlineStyle::Unix);
    let tab_spaces = config.tab_spaces();

    let method = format!("impl Dummy {{ {} {{ unimplemented!() }} }}", the_type);

    let result = match rustfmt.format(method.clone(), config) {
        Ok(mut lines) => {
            if let Some(front_pos) = lines.find('{') {
                lines = lines[front_pos..].chars().skip(1).collect();
            }
            if let Some(back_pos) = lines.rfind('{') {
                lines = lines[0..back_pos].into();
            }
            lines
                .lines()
                .filter(|line| line.trim() != "")
                .map(|line| {
                    let mut spaces = tab_spaces + 1;
                    let should_trim = |c: char| {
                        spaces = spaces.saturating_sub(1);
                        spaces > 0 && c.is_whitespace()
                    };
                    let line = line.trim_start_matches(should_trim);
                    format!("{}\n", line)
                })
                .collect()
        }
        Err(e) => {
            error!("format_method: error: {:?}, input: {:?}", e, method);
            the_type
        }
    };

    result.trim().into()
}

/// Builds a hover tooltip composed of the function signature or type declaration, doc URL
/// (if available in the save-analysis), source extracted documentation, and code context
/// for local variables.
pub fn tooltip(
    ctx: &InitActionContext,
    params: &TextDocumentPositionParams,
) -> Result<Tooltip, ResponseError> {
    let analysis = &ctx.analysis;

    let hover_file_path = parse_file_path!(&params.text_document.uri, "hover")?;
    let hover_span = ctx.convert_pos_to_span(hover_file_path, params.position);
    let hover_span_doc = analysis.docs(&hover_span).unwrap_or_else(|_| String::new());
    let hover_span_typ = analysis.show_type(&hover_span).unwrap_or_else(|_| String::new());
    let hover_span_def = analysis.id(&hover_span).and_then(|id| analysis.get_def(id));

    trace!("tooltip: span: {:?}", hover_span);
    trace!("tooltip: span_doc: {:?}", hover_span_doc);
    trace!("tooltip: span_typ: {:?}", hover_span_typ);
    trace!("tooltip: span_def: {:?}", hover_span_def);

    let racer_fallback_enabled = ctx.config.lock().unwrap().racer_completion;

    // Fallback to racer if the def was not available and racer is enabled.
    let hover_span_def = hover_span_def.or_else(|e| {
        debug!("tooltip: racer_fallback_enabled: {}", racer_fallback_enabled);
        if racer_fallback_enabled {
            debug!("tooltip: span_def is empty, attempting with racer");
            racer_def(&ctx, &hover_span).ok_or_else(|| {
                debug!("tooltip: racer returned an empty result");
                e
            })
        } else {
            Err(e)
        }
    });

    let doc_url = analysis.doc_url(&hover_span).ok();

    let contents = if let Ok(def) = hover_span_def {
        if def.kind == DefKind::Local && def.span == hover_span && def.qualname.contains('$') {
            tooltip_local_variable_decl(&ctx, &def, doc_url)
        } else if def.kind == DefKind::Local
            && def.span != hover_span
            && !def.qualname.contains('$')
        {
            tooltip_function_arg_usage(&ctx, &def, doc_url)
        } else if def.kind == DefKind::Local && def.span != hover_span && def.qualname.contains('$')
        {
            tooltip_local_variable_usage(&ctx, &def, doc_url)
        } else if def.kind == DefKind::Local && def.span == hover_span {
            tooltip_function_signature_arg(&ctx, &def, doc_url)
        } else {
            match def.kind {
                DefKind::TupleVariant | DefKind::StructVariant | DefKind::Field => {
                    tooltip_field_or_variant(&ctx, &def, doc_url)
                }
                DefKind::Enum | DefKind::Union | DefKind::Struct | DefKind::Trait => {
                    tooltip_struct_enum_union_trait(&ctx, &def, doc_url)
                }
                DefKind::Function | DefKind::Method | DefKind::ForeignFunction => {
                    tooltip_function_method(&ctx, &def, doc_url)
                }
                DefKind::Mod => tooltip_mod(&ctx, &def, doc_url),
                DefKind::Static | DefKind::ForeignStatic | DefKind::Const => {
                    tooltip_static_const_decl(&ctx, &def, doc_url)
                }
                DefKind::Type => tooltip_type(&ctx, &def, doc_url),
                _ => {
                    debug!(
                        "tooltip: ignoring def: \
                         name: {:?}, \
                         kind: {:?}, \
                         value: {:?}, \
                         qualname: {:?}, \
                         parent: {:?}",
                        def.name, def.kind, def.value, def.qualname, def.parent
                    );

                    Vec::default()
                }
            }
        }
    } else {
        debug!("tooltip: def is empty");
        Vec::default()
    };
    debug!("tooltip: contents.len: {}", contents.len());
    Ok(Tooltip { contents, range: hover_span.range })
}

#[cfg(test)]
#[allow(clippy::expect_fun_call)]
pub mod test {
    use super::*;
    use crate::actions::format::Rustfmt;

    pub fn fixtures_dir() -> &'static Path {
        Path::new(env!("FIXTURES_DIR"))
    }

    /// Strips indentation from string literals by examining
    /// the indent of the first non-empty line. Preceding
    /// and trailing whitespace is also removed.
    fn noindent(text: &str) -> String {
        let indent = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .peekable()
            .peek()
            .map(|first_non_empty_line| {
                first_non_empty_line
                    .chars()
                    .scan(0, |_, ch| if ch.is_whitespace() { Some(1) } else { None })
                    .fuse()
                    .sum()
            })
            .unwrap_or(0);

        text.lines()
            .map(|line| line.chars().skip(indent).collect::<String>())
            .collect::<Vec<String>>()
            .join("\n")
            .trim()
            .to_string()
    }

    #[test]
    fn test_noindent() {
        let lines = noindent(
            "

            Hello, world ! ! !
            The next line
                Indented line
            Last line

        ",
        );
        assert_eq!("Hello, world ! ! !\nThe next line\n    Indented line\nLast line", &lines);

        let lines = noindent(
            "

                Hello, world ! ! !
                The next line
                    Indented line
                Last line

        ",
        );
        assert_eq!("Hello, world ! ! !\nThe next line\n    Indented line\nLast line", &lines);
    }

    #[test]
    fn test_process_docs_rust_blocks() {
        let docs = &noindent("
            Brief one liner.

            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Phasellus vitae ex
            vel mi egestas semper in non dolor. Proin ut arcu at odio hendrerit consequat.

            # Examples

            Donec ullamcorper risus quis massa sollicitudin, id faucibus nibh bibendum.

            ## Hidden code lines and proceeding whitespace is removed and meta attributes are preserved

            ```
            # extern crate foo;

            use foo::bar;

            #[derive(Debug)]
            struct Baz(u32);

            let baz = Baz(1);
            ```

            ## Rust code block attributes are converted to 'rust'

            ```compile_fail,E0123
            let foo = \"compile_fail\"
            ```

            ```no_run
            let foo = \"no_run\";
            ```

            ```ignore
            let foo = \"ignore\";
            ```

            ```should_panic
            let foo = \"should_panic\";
            ```

            ```should_panic,ignore,no_run,compile_fail
            let foo = \"should_panic,ignore,no_run,compile_fail\";
            ```

            ## Inner comments and indentation is preserved

            ```
            /// inner doc comment
            fn foobar() {
                // inner comment
                let indent = 1;
            }
            ```

            ## Module attributes are preserved

            ```
            #![allow(dead_code, unused_imports)]
            ```
        ");

        let expected = noindent("
            Brief one liner.

            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Phasellus vitae ex
            vel mi egestas semper in non dolor. Proin ut arcu at odio hendrerit consequat.

            # Examples

            Donec ullamcorper risus quis massa sollicitudin, id faucibus nibh bibendum.

            ## Hidden code lines and proceeding whitespace is removed and meta attributes are preserved

            ```rust
            use foo::bar;

            #[derive(Debug)]
            struct Baz(u32);

            let baz = Baz(1);
            ```

            ## Rust code block attributes are converted to 'rust'

            ```rust
            let foo = \"compile_fail\"
            ```

            ```rust
            let foo = \"no_run\";
            ```

            ```rust
            let foo = \"ignore\";
            ```

            ```rust
            let foo = \"should_panic\";
            ```

            ```rust
            let foo = \"should_panic,ignore,no_run,compile_fail\";
            ```

            ## Inner comments and indentation is preserved

            ```rust
            /// inner doc comment
            fn foobar() {
                // inner comment
                let indent = 1;
            }
            ```

            ## Module attributes are preserved

            ```rust
            #![allow(dead_code, unused_imports)]
            ```
        ");

        let actual = process_docs(docs);
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_process_docs_bash_block() {
        let expected = noindent(
            "
            Brief one liner.

            ```bash
            # non rust-block comment lines are preserved
            ls -la
            ```
        ",
        );

        let actual = process_docs(&expected);
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_process_docs_racer_returns_extra_slashes() {
        let docs = noindent(
            "
            ////////////////////////////////////////////////////////////////////////////////

            Spawns a new thread, returning a [`JoinHandle`] for it.

            The join handle will implicitly *detach* the child thread upon being
            dropped. In this case, the child thread may outlive the parent (unless
        ",
        );

        let expected = noindent(
            "
            Spawns a new thread, returning a [`JoinHandle`] for it.

            The join handle will implicitly *detach* the child thread upon being
            dropped. In this case, the child thread may outlive the parent (unless
        ",
        );

        let actual = process_docs(&docs);
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_format_method() {
        let fmt = Rustfmt::Internal;
        let config = &FmtConfig::default();

        let input = "fn foo() -> ()";
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(input, &result, "function explicit void return");

        let input = "fn foo()";
        let expected = "fn foo()";
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(expected, &result, "function");

        let input = "fn foo() -> Thing";
        let expected = "fn foo() -> Thing";
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(expected, &result, "function with return");

        let input = "fn foo(&self);";
        let expected = "fn foo(&self)";
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(expected, &result, "method");

        let input = "fn foo<T>(t: T) where T: Copy";
        let expected = noindent(
            "
            fn foo<T>(t: T)
            where
                T: Copy,
        ",
        );
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(expected, result, "function with generic parameters");

        let input = "fn foo<T>(&self, t: T) where T: Copy";
        let expected = noindent(
            "
            fn foo<T>(&self, t: T)
            where
                T: Copy,
        ",
        );
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(expected, result, "method with type parameters");

        let input = noindent(
            "   fn foo<T>(
                    &self,
            t: T)
                where
            T: Copy

        ",
        );
        let expected = noindent(
            "
            fn foo<T>(&self, t: T)
            where
                T: Copy,
        ",
        );
        let result = format_method(fmt.clone(), config, input);
        assert_eq!(expected, result, "method with type parameters; corrected spacing");

        let input = "fn really_really_really_really_long_name<T>(foo_thing: String, bar_thing: Thing, baz_thing: Vec<T>, foo_other: u32, bar_other: i32) -> Thing";
        let expected = noindent(
            "
            fn really_really_really_really_long_name<T>(
                foo_thing: String,
                bar_thing: Thing,
                baz_thing: Vec<T>,
                foo_other: u32,
                bar_other: i32,
            ) -> Thing
        ",
        );
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(expected, result, "long function signature");

        let input = "fn really_really_really_really_long_name(&self, foo_thing: String, bar_thing: Thing, baz_thing: Vec<T>, foo_other: u32, bar_other: i32) -> Thing";
        let expected = noindent(
            "
            fn really_really_really_really_long_name(
                &self,
                foo_thing: String,
                bar_thing: Thing,
                baz_thing: Vec<T>,
                foo_other: u32,
                bar_other: i32,
            ) -> Thing
        ",
        );
        let result = format_method(fmt.clone(), config, input.into());
        assert_eq!(expected, result, "long method signature with generic");

        let input = noindent(
            "
            fn matrix_something(
            _a_matrix: [[f32; 4]; 4],
            _b_matrix: [[f32; 4]; 4],
            _c_matrix: [[f32; 4]; 4],
            _d_matrix: [[f32; 4]; 4],
            )
        ",
        );
        let expected = noindent(
            "
            fn matrix_something(
                _a_matrix: [[f32; 4]; 4],
                _b_matrix: [[f32; 4]; 4],
                _c_matrix: [[f32; 4]; 4],
                _d_matrix: [[f32; 4]; 4],
            )
        ",
        );
        let result = format_method(fmt, config, input);
        assert_eq!(expected, result, "function with multiline args");
    }

    #[test]
    fn test_extract_decl() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_decl.rs");

        let expected = "pub fn foo() -> Foo<u32>";
        let row_start = Row::new_zero_indexed(0);
        let actual = extract_decl(&vfs, &file, row_start).expect("function declaration").join("\n");
        assert_eq!(expected, actual);

        let expected = "pub struct Foo<T>";
        let row_start = Row::new_zero_indexed(5);
        let actual = extract_decl(&vfs, &file, row_start).expect("struct declaration").join("\n");
        assert_eq!(expected, actual);

        let expected = "pub enum Bar";
        let row_start = Row::new_zero_indexed(10);
        let actual = extract_decl(&vfs, &file, row_start).expect("enum declaration").join("\n");
        assert_eq!(expected, actual);

        let expected = "pub struct NewType(pub u32, f32)";
        let row_start = Row::new_zero_indexed(15);
        let actual = extract_decl(&vfs, &file, row_start).expect("tuple declaration").join("\n");
        assert_eq!(expected, actual);

        let expected = "pub fn new() -> NewType";
        let row_start = Row::new_zero_indexed(18);
        let actual =
            extract_decl(&vfs, &file, row_start).expect("struct function declaration").join("\n");
        assert_eq!(expected, actual);

        let expected = "pub fn bar<T: Copy + Add>(&self, the_really_long_name_string: String, the_really_long_name_foo: Foo<T>) -> Vec<(String, Foo<T>)>";
        let row_start = Row::new_zero_indexed(22);
        let actual = extract_decl(&vfs, &file, row_start)
            .expect("long struct method declaration with generics")
            .join("\n");
        assert_eq!(expected, actual);

        let expected = "pub trait Baz<T> where T: Copy";
        let row_start = Row::new_zero_indexed(27);
        let actual = extract_decl(&vfs, &file, row_start).expect("enum declaration").join("\n");
        assert_eq!(expected, actual);

        let expected = "fn make_copy(&self) -> Self";
        let row_start = Row::new_zero_indexed(28);
        let actual =
            extract_decl(&vfs, &file, row_start).expect("trait method declaration").join("\n");
        assert_eq!(expected, actual);

        let expected = "fn make_copy(&self) -> Self";
        let row_start = Row::new_zero_indexed(32);
        let actual =
            extract_decl(&vfs, &file, row_start).expect("trait method implementation").join("\n");
        assert_eq!(expected, actual);

        let expected = noindent(
            "
            pub trait Qeh<T, U>
            where T: Copy,
            U: Clone
        ",
        );
        let row_start = Row::new_zero_indexed(37);
        let actual =
            extract_decl(&vfs, &file, row_start).expect("trait declaration multiline").join("\n");
        assert_eq!(expected, actual);

        let expected = noindent(
            "
            pub fn multiple_lines(
            s: String,
            i: i32
            )
        ",
        );
        let row_start = Row::new_zero_indexed(43);
        let actual = extract_decl(&vfs, &file, row_start)
            .expect("function declaration multiline")
            .join("\n");
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_format_object() {
        let fmt = Rustfmt::Internal;
        let config = &FmtConfig::default();

        let input = "pub struct Box<T: ?Sized>(Unique<T>);";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!(
            "pub struct Box<T: ?Sized>", &result,
            "tuple struct with all private fields has hidden components"
        );

        let input = "pub struct Thing(pub u32);";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!(
            "pub struct Thing(pub u32)", &result,
            "tuple struct with trailing ';' from racer"
        );

        let input = "pub struct Thing(pub u32)";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!("pub struct Thing(pub u32)", &result, "pub tuple struct");

        let input = "pub struct Thing(pub u32, i32)";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!(
            "pub struct Thing(pub u32, _)", &result,
            "non-pub components of pub tuples should be hidden"
        );

        let input = "struct Thing(u32, i32)";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!(
            "struct Thing(u32, i32)", &result,
            "private tuple struct may show private components"
        );

        let input = "pub struct Thing<T: Copy>";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!("pub struct Thing<T: Copy>", &result, "pub struct");

        let input = "pub struct Thing<T: Copy> {";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!(
            "pub struct Thing<T: Copy>", &result,
            "pub struct with trailing '{{' from racer"
        );

        let input = "pub struct Thing { x: i32 }";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!("pub struct Thing", &result, "pub struct with body");

        let input = "pub enum Foobar { Foo, Bar }";
        let result = format_object(fmt.clone(), config, input.into());
        assert_eq!("pub enum Foobar", &result, "pub enum with body");

        let input = "pub trait Thing<T, U> where T: Copy + Sized, U: Clone";
        let expected = noindent(
            "
            pub trait Thing<T, U>
            where
                T: Copy + Sized,
                U: Clone,
        ",
        );
        let result = format_object(fmt, config, input.into());
        assert_eq!(expected, result, "trait with where clause");
    }

    #[test]
    fn test_extract_decl_multiline_empty_function() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_decl_multiline_empty_function.rs");

        let expected = noindent(
            "
            fn matrix_something(
            _a_matrix: [[f32; 4]; 4],
            _b_matrix: [[f32; 4]; 4],
            _c_matrix: [[f32; 4]; 4],
            _d_matrix: [[f32; 4]; 4],
            )
        ",
        );
        let row_start = Row::new_zero_indexed(1);
        let actual = extract_decl(&vfs, &file, row_start)
            .expect("the empty body should not be extracted")
            .join("\n");
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_extract_docs_module_docs_with_attribute() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_docs_module_docs_with_attribute.rs");
        let row_start = Row::new_zero_indexed(0);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            Begin module docs

            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Maecenas
            tincidunt tristique maximus. Sed venenatis urna vel sagittis tempus.
            In hac habitasse platea dictumst.

            End module docs.
        ",
        );

        assert_eq!(expected, actual, "module docs without a copyright header");
    }

    #[test]
    fn test_extract_docs_module_docs_no_copyright() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_docs_module_docs_no_copyright.rs");
        let row_start = Row::new_zero_indexed(0);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            Begin module docs

            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Maecenas
            tincidunt tristique maximus. Sed venenatis urna vel sagittis tempus.
            In hac habitasse platea dictumst.

            End module docs.
        ",
        );

        assert_eq!(expected, actual, "module docs without a copyright header");
    }

    #[test]
    fn test_extract_docs_comment_block() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_docs_comment_block.rs");
        let row_start = Row::new_zero_indexed(11);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            The standard library often has comment header blocks that should not be
            included.

            Nam efficitur dapibus lectus consequat porta. Pellentesque augue metus,
            vestibulum nec massa at, aliquet consequat ex.

            End of spawn docs
        ",
        );

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_extract_docs_empty_line_before_decl() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_docs_empty_line_before_decl.rs");
        let row_start = Row::new_zero_indexed(8);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            Begin empty before decl

            Cras malesuada mattis massa quis ornare. Suspendisse in ex maximus,
            iaculis ante non, ultricies nulla. Nam ultrices convallis ex, vel
            lacinia est rhoncus sed. Nullam sollicitudin finibus ex at placerat.

            End empty line before decl.
        ",
        );

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_extract_docs_module_docs() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_docs_module_docs.rs");

        let row_start = Row::new_zero_indexed(0);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            Begin module docs

            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Maecenas
            tincidunt tristique maximus. Sed venenatis urna vel sagittis tempus.
            In hac habitasse platea dictumst.

            End module docs.
        ",
        );

        assert_eq!(expected, actual);

        let row_start = Row::new_zero_indexed(11);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            Begin first item docs

            The first item docs should not pick up the module docs.
        ",
        );

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_extract_docs_attributes() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_docs_attributes.rs");

        let row_start = Row::new_zero_indexed(11);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            Begin multiline attribute

            Cras malesuada mattis massa quis ornare. Suspendisse in ex maximus,
            iaculis ante non, ultricies nulla. Nam ultrices convallis ex, vel
            lacinia est rhoncus sed. Nullam sollicitudin finibus ex at placerat.

            End multiline attribute
        ",
        );

        assert_eq!(expected, actual);

        let row_start = Row::new_zero_indexed(22);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = noindent(
            "
            Begin single line attribute

            Cras malesuada mattis massa quis ornare. Suspendisse in ex maximus,
            iaculis ante non, ultricies nulla. Nam ultrices convallis ex, vel
            lacinia est rhoncus sed. Nullam sollicitudin finibus ex at placerat.

            End single line attribute.
        ",
        );

        assert_eq!(expected, actual);
    }

    #[test]
    fn test_extract_docs_comment_first_line() {
        let vfs = Vfs::new();
        let file = fixtures_dir().join("hover/src/test_extract_docs_comment_first_line.rs");

        let row_start = Row::new_zero_indexed(1);
        let actual = extract_docs(&vfs, &file, row_start)
            .expect(&format!("failed to extract docs: {:?}", file))
            .join("\n");

        let expected = "First line comment";

        assert_eq!(expected, actual);
    }
}
