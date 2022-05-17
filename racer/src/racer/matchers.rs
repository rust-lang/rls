use crate::ast_types::{ImplHeader, PathAlias, PathAliasKind, PathSegment};
use crate::core::MatchType::{
    self, Const, Enum, EnumVariant, For, Function, IfLet, Let, Macro, Module, Static, Struct,
    Trait, Type, WhileLet,
};
use crate::core::Namespace;
use crate::core::SearchType::{self, ExactMatch, StartsWith};
use crate::core::{BytePos, ByteRange, Coordinate, Match, Session, SessionExt, Src};
use crate::fileres::{get_crate_file, get_module_file};
use crate::nameres::resolve_path;
use crate::util::*;
use crate::{ast, scopes, typeinf};
use std::path::Path;
use std::{str, vec};

/// The location of an import (`use` item) currently being resolved.
#[derive(PartialEq, Eq)]
struct PendingImport<'fp> {
    filepath: &'fp Path,
    range: ByteRange,
}

/// A stack of imports (`use` items) currently being resolved.
type PendingImports<'stack, 'fp> = StackLinkedListNode<'stack, PendingImport<'fp>>;

const GLOB_LIMIT: usize = 2;
/// Import information(pending imports, glob, and etc.)
pub struct ImportInfo<'stack, 'fp> {
    /// A stack of imports currently being resolved
    imports: PendingImports<'stack, 'fp>,
    /// the max number of times where we can go through glob continuously
    /// if current search path isn't constructed via glob, it's none
    glob_limit: Option<usize>,
}

impl<'stack, 'fp: 'stack> Default for ImportInfo<'stack, 'fp> {
    fn default() -> Self {
        ImportInfo {
            imports: PendingImports::empty(),
            glob_limit: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchCxt<'s, 'p> {
    pub filepath: &'p Path,
    pub search_str: &'s str,
    pub range: ByteRange,
    pub search_type: SearchType,
    pub is_local: bool,
}

impl<'s, 'p> MatchCxt<'s, 'p> {
    fn get_key_ident(
        &self,
        blob: &str,
        keyword: &str,
        ignore: &[&str],
    ) -> Option<(BytePos, String)> {
        find_keyword(blob, keyword, ignore, self).map(|start| {
            let s = match self.search_type {
                ExactMatch => self.search_str.to_owned(),
                StartsWith => {
                    let end = find_ident_end(blob, start + BytePos(self.search_str.len()));
                    blob[start.0..end.0].to_owned()
                }
            };
            (start, s)
        })
    }
}

pub(crate) fn find_keyword(
    src: &str,
    pattern: &str,
    ignore: &[&str],
    context: &MatchCxt<'_, '_>,
) -> Option<BytePos> {
    find_keyword_impl(
        src,
        pattern,
        context.search_str,
        ignore,
        context.search_type,
        context.is_local,
    )
}

fn find_keyword_impl(
    src: &str,
    pattern: &str,
    search_str: &str,
    ignore: &[&str],
    search_type: SearchType,
    is_local: bool,
) -> Option<BytePos> {
    let mut start = BytePos::ZERO;

    if let Some(offset) = strip_visibility(&src[..]) {
        start += offset;
    } else if !is_local {
        // TODO: too about
        return None;
    }

    if ignore.len() > 0 {
        start += strip_words(&src[start.0..], ignore);
    }
    // mandatory pattern\s+
    if !src[start.0..].starts_with(pattern) {
        return None;
    }
    // remove whitespaces ... must have one at least
    start += pattern.len().into();
    let oldstart = start;
    for &b in src[start.0..].as_bytes() {
        match b {
            b if is_whitespace_byte(b) => start = start.increment(),
            _ => break,
        }
    }
    if start == oldstart {
        return None;
    }

    let search_str_len = search_str.len();
    if src[start.0..].starts_with(search_str) {
        match search_type {
            StartsWith => Some(start),
            ExactMatch => {
                if src.len() > start.0 + search_str_len
                    && !is_ident_char(char_at(src, start.0 + search_str_len))
                {
                    Some(start)
                } else {
                    None
                }
            }
        }
    } else {
        None
    }
}

fn is_const_fn(src: &str, blob_range: ByteRange) -> bool {
    if let Some(b) = strip_word(&src[blob_range.to_range()], "const") {
        let s = src[(blob_range.start + b).0..].trim_start();
        s.starts_with("fn") || s.starts_with("unsafe")
    } else {
        false
    }
}

fn match_pattern_start(
    src: &str,
    context: &MatchCxt<'_, '_>,
    pattern: &str,
    ignore: &[&str],
    mtype: MatchType,
) -> Option<Match> {
    // ast currently doesn't contain the ident coords, so match them with a hacky
    // string search

    let blob = &src[context.range.to_range()];
    if let Some(start) = find_keyword(blob, pattern, ignore, context) {
        if let Some(end) = blob[start.0..].find(|c: char| c == ':' || c.is_whitespace()) {
            if blob[start.0 + end..].trim_start().chars().next() == Some(':') {
                let s = &blob[start.0..start.0 + end];
                return Some(Match {
                    matchstr: s.to_owned(),
                    filepath: context.filepath.to_path_buf(),
                    point: context.range.start + start,
                    coords: None,
                    local: context.is_local,
                    mtype: mtype,
                    contextstr: first_line(blob),
                    docs: String::new(),
                });
            }
        }
    }
    None
}

pub fn match_const(msrc: &str, context: &MatchCxt<'_, '_>) -> Option<Match> {
    if is_const_fn(msrc, context.range) {
        return None;
    }
    // Here we don't have to ignore "unsafe"
    match_pattern_start(msrc, context, "const", &[], Const)
}

pub fn match_static(msrc: &str, context: &MatchCxt<'_, '_>) -> Option<Match> {
    // Here we don't have to ignore "unsafe"
    match_pattern_start(msrc, context, "static", &[], Static)
}

fn match_let_impl(msrc: &str, context: &MatchCxt<'_, '_>, mtype: MatchType) -> Vec<Match> {
    let mut out = Vec::new();
    let coords = ast::parse_pat_bind_stmt(msrc.to_owned());
    for pat_range in coords {
        let s = &msrc[pat_range.to_range()];
        if symbol_matches(context.search_type, context.search_str, s) {
            let start = context.range.start + pat_range.start;
            debug!("match_pattern_let point is {:?}", start);
            out.push(Match {
                matchstr: s.to_owned(),
                filepath: context.filepath.to_path_buf(),
                point: start,
                coords: None,
                local: context.is_local,
                mtype: mtype.clone(),
                contextstr: msrc.to_owned(),
                docs: String::new(),
            });
            if context.search_type == ExactMatch {
                break;
            }
        }
    }
    out
}

pub fn match_if_let(msrc: &str, start: BytePos, context: &MatchCxt<'_, '_>) -> Vec<Match> {
    match_let_impl(msrc, context, IfLet(start))
}

pub fn match_while_let(msrc: &str, start: BytePos, context: &MatchCxt<'_, '_>) -> Vec<Match> {
    match_let_impl(msrc, context, WhileLet(start))
}

pub fn match_let(msrc: &str, start: BytePos, context: &MatchCxt<'_, '_>) -> Vec<Match> {
    let blob = &msrc[context.range.to_range()];
    if blob.starts_with("let ") && txt_matches(context.search_type, context.search_str, blob) {
        match_let_impl(blob, context, Let(start))
    } else {
        Vec::new()
    }
}

pub fn match_for(msrc: &str, for_start: BytePos, context: &MatchCxt<'_, '_>) -> Vec<Match> {
    let mut out = Vec::new();
    let blob = &msrc[context.range.to_range()];
    let coords = ast::parse_pat_bind_stmt(blob.to_owned());
    for pat_range in coords {
        let s = &blob[pat_range.to_range()];
        if symbol_matches(context.search_type, context.search_str, s) {
            let start = pat_range.start + context.range.start;
            debug!("match_for point is {:?}, found ident {}", start, s);
            out.push(Match {
                matchstr: s.to_owned(),
                filepath: context.filepath.to_path_buf(),
                point: start, // it's 'for ~' start
                coords: None,
                local: context.is_local,
                mtype: For(for_start),
                contextstr: blob.to_owned(),
                docs: String::new(),
            });
        }
    }
    out
}

pub fn first_line(blob: &str) -> String {
    blob[..blob.find('\n').unwrap_or(blob.len())].to_owned()
}

/// Get the match's cleaned up context string
///
/// Strip all whitespace, including newlines in order to have a single line
/// context string.
pub fn get_context(blob: &str, context_end: &str) -> String {
    blob[..blob.find(context_end).unwrap_or(blob.len())]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn match_extern_crate(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let mut res = None;
    let mut blob = &msrc[context.range.to_range()];

    // Temporary fix to parse reexported crates by skipping pub
    // keyword until racer understands crate visibility.
    if let Some(offset) = strip_visibility(blob) {
        blob = &blob[offset.0..];
    }

    if txt_matches(
        context.search_type,
        &format!("extern crate {}", context.search_str),
        blob,
    ) && !(txt_matches(
        context.search_type,
        &format!("extern crate {} as", context.search_str),
        blob,
    )) || (blob.starts_with("extern crate")
        && txt_matches(
            context.search_type,
            &format!("as {}", context.search_str),
            blob,
        ))
    {
        debug!("found an extern crate: |{}|", blob);

        let extern_crate = ast::parse_extern_crate(blob.to_owned());

        if let Some(ref name) = extern_crate.name {
            let realname = extern_crate.realname.as_ref().unwrap_or(name);
            if let Some(cratepath) = get_crate_file(realname, context.filepath, session) {
                let raw_src = session.load_raw_file(&cratepath);
                res = Some(Match {
                    matchstr: name.clone(),
                    filepath: cratepath.to_path_buf(),
                    point: BytePos::ZERO,
                    coords: Some(Coordinate::start()),
                    local: false,
                    mtype: Module,
                    contextstr: cratepath.to_str().unwrap().to_owned(),
                    docs: find_mod_doc(&raw_src, BytePos::ZERO),
                });
            }
        }
    }
    res
}

pub fn match_mod(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    let (start, s) = context.get_key_ident(blob, "mod", &[])?;
    if blob.find('{').is_some() {
        debug!("found a module inline: |{}|", blob);
        return Some(Match {
            matchstr: s,
            filepath: context.filepath.to_path_buf(),
            point: context.range.start + start,
            coords: None,
            local: false,
            mtype: Module,
            contextstr: context.filepath.to_str().unwrap().to_owned(),
            docs: String::new(),
        });
    } else {
        debug!("found a module declaration: |{}|", blob);
        // the name of the file where we found the module declaration (foo.rs)
        // without its extension!
        let filename = context.filepath.file_stem()?;
        let parent_path = context.filepath.parent()?;
        // if we found the declaration in `src/foo.rs`, then let's look for the
        // submodule in `src/foo/` as well!
        let filename_subdir = parent_path.join(filename);
        // if we are looking for "foo::bar", we have two cases:
        //   1. we found `pub mod bar;` in either `src/foo/mod.rs`
        // (or `src/lib.rs`). As such we are going to search for `bar.rs` in
        // the same directory (`src/foo/`, or `src/` respectively).
        //   2. we found `pub mod bar;` in `src/foo.rs`. This means that we also
        // need to seach in `src/foo/` if it exists!
        let search_path = if filename_subdir.exists() {
            filename_subdir.as_path()
        } else {
            parent_path
        };
        match_mod_inner(msrc, context, session, search_path, s)
    }
}

fn match_mod_inner(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
    search_path: &Path,
    s: String,
) -> Option<Match> {
    let ranged_raw = session.load_raw_src_ranged(&msrc, context.filepath);
    // get module from path attribute
    if let Some(modpath) =
        scopes::get_module_file_from_path(msrc, context.range.start, search_path, ranged_raw)
    {
        let doc_src = session.load_raw_file(&modpath);
        return Some(Match {
            matchstr: s,
            filepath: modpath.to_path_buf(),
            point: BytePos::ZERO,
            coords: Some(Coordinate::start()),
            local: false,
            mtype: Module,
            contextstr: modpath.to_str().unwrap().to_owned(),
            docs: find_mod_doc(&doc_src, BytePos::ZERO),
        });
    }
    // get internal module nesting
    // e.g. is this in an inline submodule?  mod foo{ mod bar; }
    // because if it is then we need to search further down the
    // directory hierarchy - e.g. <cwd>/foo/bar.rs
    let internalpath = scopes::get_local_module_path(msrc, context.range.start);
    let mut searchdir = (*search_path).to_owned();
    for s in internalpath {
        searchdir.push(&s);
    }
    if let Some(modpath) = get_module_file(&s, &searchdir, session) {
        let doc_src = session.load_raw_file(&modpath);
        let context = modpath.to_str().unwrap().to_owned();
        return Some(Match {
            matchstr: s,
            filepath: modpath,
            point: BytePos::ZERO,
            coords: Some(Coordinate::start()),
            local: false,
            mtype: Module,
            contextstr: context,
            docs: find_mod_doc(&doc_src, BytePos::ZERO),
        });
    }
    None
}

fn find_generics_end(blob: &str) -> Option<BytePos> {
    // Naive version that attempts to skip over attributes
    let mut in_attr = false;
    let mut attr_level = 0;

    let mut level = 0;
    for (i, b) in blob.as_bytes().into_iter().enumerate() {
        // Naively skip attributes `#[...]`
        if in_attr {
            match b {
                b'[' => attr_level += 1,
                b']' => {
                    attr_level -=1;
                    if attr_level == 0 {
                        in_attr = false;
                        continue;
                    }
                },
                _ => continue,
            }
        }
        // ...otherwise just try to find the last `>`
        match b {
            b'{' | b'(' | b';' => return None,
            b'<' => level += 1,
            b'>' => {
                level -= 1;
                if level == 0 {
                    return Some(i.into());
                }
            }
            b'#' if blob.bytes().nth(i + 1) == Some(b'[') => in_attr = true,
            _ => {}
        }
    }
    None
}

pub fn match_struct(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    let (start, s) = context.get_key_ident(blob, "struct", &[])?;

    debug!("found a struct |{}|", s);
    let generics =
        find_generics_end(&blob[start.0..]).map_or_else(Default::default, |generics_end| {
            let header = format!("struct {}();", &blob[start.0..=(start + generics_end).0]);
            ast::parse_generics(header, context.filepath)
        });
    let start = context.range.start + start;
    let doc_src = session.load_raw_src_ranged(&msrc, context.filepath);
    Some(Match {
        matchstr: s,
        filepath: context.filepath.to_path_buf(),
        point: start,
        coords: None,
        local: context.is_local,
        mtype: Struct(Box::new(generics)),
        contextstr: get_context(blob, "{"),
        docs: find_doc(&doc_src, start),
    })
}

pub fn match_union(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    let (start, s) = context.get_key_ident(blob, "union", &[])?;

    debug!("found a union |{}|", s);
    let generics =
        find_generics_end(&blob[start.0..]).map_or_else(Default::default, |generics_end| {
            let header = format!("union {}();", &blob[start.0..=(start + generics_end).0]);
            ast::parse_generics(header, context.filepath)
        });
    let start = context.range.start + start;
    let doc_src = session.load_raw_src_ranged(&msrc, context.filepath);
    Some(Match {
        matchstr: s,
        filepath: context.filepath.to_path_buf(),
        point: start,
        coords: None,
        local: context.is_local,
        mtype: MatchType::Union(Box::new(generics)),
        contextstr: get_context(blob, "{"),
        docs: find_doc(&doc_src, start),
    })
}

pub fn match_type(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    let (start, s) = context.get_key_ident(blob, "type", &[])?;
    debug!("found!! a type {}", s);
    // parse type here
    let start = context.range.start + start;
    let doc_src = session.load_raw_src_ranged(&msrc, context.filepath);
    Some(Match {
        matchstr: s,
        filepath: context.filepath.to_path_buf(),
        point: start,
        coords: None,
        local: context.is_local,
        mtype: Type,
        contextstr: first_line(blob),
        docs: find_doc(&doc_src, start),
    })
}

pub fn match_trait(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    let (start, s) = context.get_key_ident(blob, "trait", &["unsafe"])?;
    debug!("found!! a trait {}", s);
    let start = context.range.start + start;
    let doc_src = session.load_raw_src_ranged(&msrc, context.filepath);
    Some(Match {
        matchstr: s,
        filepath: context.filepath.to_path_buf(),
        point: start,
        coords: None,
        local: context.is_local,
        mtype: Trait,
        contextstr: get_context(blob, "{"),
        docs: find_doc(&doc_src, start),
    })
}

pub fn match_enum_variants(msrc: &str, context: &MatchCxt<'_, '_>) -> Vec<Match> {
    let blob = &msrc[context.range.to_range()];
    let mut out = Vec::new();
    let parsed_enum = ast::parse_enum(blob.to_owned());
    for (name, offset) in parsed_enum.values {
        if name.starts_with(context.search_str) {
            let start = context.range.start + offset;
            let m = Match {
                matchstr: name,
                filepath: context.filepath.to_path_buf(),
                point: start,
                coords: None,
                local: context.is_local,
                mtype: EnumVariant(None),
                contextstr: first_line(&blob[offset.0..]),
                docs: find_doc(msrc, start),
            };
            out.push(m);
        }
    }
    out
}

pub fn match_enum(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    let (start, s) = context.get_key_ident(blob, "enum", &[])?;

    debug!("found!! an enum |{}|", s);

    let generics =
        find_generics_end(&blob[start.0..]).map_or_else(Default::default, |generics_end| {
            let header = format!("enum {}{{}}", &blob[start.0..=(start + generics_end).0]);
            ast::parse_generics(header, context.filepath)
        });
    let start = context.range.start + start;
    let doc_src = session.load_raw_src_ranged(&msrc, context.filepath);
    Some(Match {
        matchstr: s,
        filepath: context.filepath.to_path_buf(),
        point: start,
        coords: None,
        local: context.is_local,
        mtype: Enum(Box::new(generics)),
        contextstr: first_line(blob),
        docs: find_doc(&doc_src, start),
    })
}

pub fn match_use(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    let import = PendingImport {
        filepath: context.filepath,
        range: context.range,
    };

    let blob = &msrc[context.range.to_range()];

    // If we're trying to resolve the same import recursively,
    // do not return any matches this time.
    if import_info.imports.contains(&import) {
        debug!("import {} involved in a cycle; ignoring", blob);
        return Vec::new();
    }

    // Push this import on the stack of pending imports.
    let pending_imports = import_info.imports.push(import);

    let mut out = Vec::new();

    if find_keyword_impl(blob, "use", "", &[], StartsWith, context.is_local).is_none() {
        return out;
    }

    let use_item = ast::parse_use(blob.to_owned());
    debug!(
        "[match_use] found item: {:?}, searchstr: {}",
        use_item, context.search_str
    );
    // for speed up!
    if !use_item.contains_glob && !txt_matches(context.search_type, context.search_str, blob) {
        return out;
    }
    let mut import_info = ImportInfo {
        imports: pending_imports,
        glob_limit: import_info.glob_limit,
    };
    let alias_match = |ident, start, inner, cstr| Match {
        matchstr: ident,
        filepath: context.filepath.to_owned(),
        point: context.range.start + start,
        coords: None,
        local: context.is_local,
        mtype: MatchType::UseAlias(Box::new(inner)),
        contextstr: cstr,
        docs: String::new(),
    };
    // common utilities
    macro_rules! with_match {
        ($path:expr, $ns: expr, $f:expr) => {
            let path_iter = resolve_path(
                $path,
                context.filepath,
                context.range.start,
                ExactMatch,
                $ns,
                session,
                &import_info,
            );
            for m in path_iter {
                out.push($f(m));
                if context.search_type == ExactMatch {
                    return out;
                }
            }
        };
    }
    // let's find searchstr using path_aliases
    for path_alias in use_item.path_list {
        let PathAlias {
            path: mut alias_path,
            kind: alias_kind,
            range: alias_range,
        } = path_alias;
        alias_path.set_prefix();
        match alias_kind {
            PathAliasKind::Ident(ref ident, rename_start) => {
                if !symbol_matches(context.search_type, context.search_str, &ident) {
                    continue;
                }
                with_match!(&alias_path, Namespace::Path, |m: Match| {
                    debug!("[match_use] PathAliasKind::Ident {:?} was found", ident);
                    let rename_start = match rename_start {
                        Some(r) => r,
                        None => return m,
                    };
                    // if use A as B found, we treat this type as type alias
                    let context_str = &msrc[alias_range.shift(context.range.start).to_range()];
                    alias_match(ident.clone(), rename_start, m, context_str.to_owned())
                });
            }
            PathAliasKind::Self_(ref ident, rename_start) => {
                if let Some(last_seg) = alias_path.segments.last() {
                    let search_name = if rename_start.is_some() {
                        ident
                    } else {
                        &last_seg.name
                    };
                    if !symbol_matches(context.search_type, context.search_str, search_name) {
                        continue;
                    }
                    with_match!(&alias_path, Namespace::PathParen, |m: Match| {
                        debug!("[match_use] PathAliasKind::Self_ {:?} was found", ident);
                        let rename_start = match rename_start {
                            Some(r) => r,
                            None => return m,
                        };
                        // if use A as B found, we treat this type as type alias
                        let context_str = &msrc[alias_range.shift(context.range.start).to_range()];
                        alias_match(ident.clone(), rename_start, m, context_str.to_owned())
                    });
                }
            }
            PathAliasKind::Glob => {
                let glob_depth_reserved = if let Some(ref mut d) = import_info.glob_limit {
                    if *d == 0 {
                        continue;
                    }
                    *d -= 1;
                    Some(*d + 1)
                } else {
                    // heuristics for issue #844
                    import_info.glob_limit = Some(GLOB_LIMIT - 1);
                    None
                };
                let mut search_path = alias_path;
                search_path.segments.push(PathSegment::new(
                    context.search_str.to_owned(),
                    vec![],
                    None,
                ));
                let path_iter = resolve_path(
                    &search_path,
                    context.filepath,
                    context.range.start,
                    context.search_type,
                    Namespace::Path,
                    session,
                    &import_info,
                );
                import_info.glob_limit = glob_depth_reserved;
                debug!("[match_use] resolve_path returned {:?} for Glob", path_iter,);
                out.extend(path_iter);
            }
        }
    }
    out
}

/// TODO: Handle `extern` functions
pub fn match_fn(msrc: Src<'_>, context: &MatchCxt<'_, '_>, session: &Session<'_>) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    if typeinf::first_param_is_self(blob) {
        return None;
    }
    match_fn_common(blob, msrc, context, session)
}

pub fn match_method(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    include_assoc_fn: bool,
    session: &Session<'_>,
) -> Option<Match> {
    let blob = &msrc[context.range.to_range()];
    if !include_assoc_fn && !typeinf::first_param_is_self(blob) {
        return None;
    }
    match_fn_common(blob, msrc, context, session)
}

fn match_fn_common(
    blob: &str,
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let (start, s) = context.get_key_ident(blob, "fn", &["const", "unsafe", "async"])?;
    let start = context.range.start + start;
    let doc_src = session.load_raw_src_ranged(&msrc, context.filepath);
    Some(Match {
        matchstr: s,
        filepath: context.filepath.to_path_buf(),
        point: start,
        coords: None,
        local: context.is_local,
        mtype: Function,
        contextstr: get_context(blob, "{"),
        docs: find_doc(&doc_src, start),
    })
}

pub fn match_macro(
    msrc: Src<'_>,
    context: &MatchCxt<'_, '_>,
    session: &Session<'_>,
) -> Option<Match> {
    let trimed = context.search_str.trim_end_matches('!');
    let mut context = context.clone();
    context.search_str = trimed;
    let blob = &msrc[context.range.to_range()];
    let (start, mut s) = context.get_key_ident(blob, "macro_rules!", &[])?;
    s.push('!');
    debug!("found a macro {}", s);
    let doc_src = session.load_raw_src_ranged(&msrc, context.filepath);
    Some(Match {
        matchstr: s,
        filepath: context.filepath.to_owned(),
        point: context.range.start + start,
        coords: None,
        local: context.is_local,
        mtype: Macro,
        contextstr: first_line(blob),
        docs: find_doc(&doc_src, context.range.start),
    })
}

pub fn find_doc(msrc: &str, match_point: BytePos) -> String {
    let blob = &msrc[0..match_point.0];
    blob.lines()
        .rev()
        .skip(1) // skip the line that the match is on
        .map(|line| line.trim())
        .take_while(|line| line.starts_with("///") || line.starts_with("#[") || line.is_empty())
        .filter(|line| !(line.trim().starts_with("#[") || line.is_empty())) // remove the #[flags]
        .collect::<Vec<_>>() // These are needed because
        .iter() // you cannot `rev`an `iter` that
        .rev() // has already been `rev`ed.
        .map(|line| if line.len() >= 4 { &line[4..] } else { "" }) // Remove "/// "
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn find_mod_doc(msrc: &str, blobstart: BytePos) -> String {
    let blob = &msrc[blobstart.0..];
    let mut doc = String::new();

    let mut iter = blob
        .lines()
        .map(|line| line.trim())
        .take_while(|line| line.starts_with("//") || line.is_empty())
        // Skip over the copyright notice and empty lines until you find
        // the module's documentation (it will go until the end of the
        // file if the module doesn't have any docs).
        .filter(|line| line.starts_with("//!"))
        .peekable();

    // Use a loop to avoid unnecessary collect and String allocation
    while let Some(line) = iter.next() {
        // Remove "//! " and push to doc string to be returned
        doc.push_str(if line.len() >= 4 { &line[4..] } else { "" });
        if iter.peek() != None {
            doc.push_str("\n");
        }
    }
    doc
}

// DON'T USE MatchCxt's range
pub fn match_impl(decl: String, context: &MatchCxt<'_, '_>, offset: BytePos) -> Option<Vec<Match>> {
    let ImplHeader { generics, .. } =
        ast::parse_impl(decl, context.filepath, offset, true, offset)?;
    let mut out = Vec::new();
    for type_param in generics.0 {
        if !symbol_matches(context.search_type, context.search_str, &type_param.name) {
            continue;
        }
        out.push(type_param.into_match());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn find_generics_end() {
        use super::find_generics_end;
        assert_eq!(
            find_generics_end("Vec<T, #[unstable(feature = \"\", issue = \"\"] A: AllocRef = Global>"),
            Some(BytePos(64))
        );
        assert_eq!(
            find_generics_end("Vec<T, A: AllocRef = Global>"),
            Some(BytePos(27))
        );
        assert_eq!(
            find_generics_end("Result<Vec<String>, Option<&str>>"),
            Some(BytePos(32))
        );
    }
}
