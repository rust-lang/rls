//! Name resolving
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::{self, vec};

use crate::primitive::PrimKind;
use rustc_ast::ast::BinOpKind;

use crate::ast_types::{ImplHeader, Path as RacerPath, PathPrefix, PathSegment, Ty};
use crate::core::Namespace;
use crate::core::SearchType::{self, ExactMatch, StartsWith};
use crate::core::{
    BytePos, ByteRange, Coordinate, Match, MatchType, Scope, Session, SessionExt, Src,
};
use crate::fileres::{get_crate_file, get_module_file, get_std_file, search_crate_names};
use crate::matchers::{find_doc, ImportInfo, MatchCxt};
use crate::primitive;
use crate::util::{
    self, calculate_str_hash, find_ident_end, get_rust_src_path, strip_words, symbol_matches,
    trim_visibility, txt_matches, txt_matches_with_pos,
};
use crate::{ast, core, matchers, scopes, typeinf};

lazy_static! {
    pub static ref RUST_SRC_PATH: Option<PathBuf> = get_rust_src_path().ok();
}

pub(crate) fn search_struct_fields(
    searchstr: &str,
    structmatch: &Match,
    search_type: SearchType,
    session: &Session<'_>,
) -> Vec<Match> {
    match structmatch.mtype {
        MatchType::Struct(_) | MatchType::EnumVariant(_) | MatchType::Union(_) => {}
        _ => return Vec::new(),
    }
    let src = session.load_source_file(&structmatch.filepath);
    let mut out = Vec::new();

    let struct_start = scopes::expect_stmt_start(src.as_src(), structmatch.point);
    let struct_range = if let Some(end) = scopes::end_of_next_scope(&src[struct_start.0..]) {
        struct_start.0..=(struct_start + end).0
    } else {
        return out;
    };
    let structsrc = match structmatch.mtype {
        MatchType::EnumVariant(_) => "struct ".to_string() + &src[struct_range.clone()],
        _ => src[struct_range.clone()].to_string(),
    };
    let fields = ast::parse_struct_fields(structsrc, core::Scope::from_match(structmatch));
    for (field, field_range, _) in fields {
        if symbol_matches(search_type, searchstr, &field) {
            let raw_src = session.load_raw_file(&structmatch.filepath);
            let contextstr = src[field_range.shift(struct_start).to_range()].to_owned();
            out.push(Match {
                matchstr: field,
                filepath: structmatch.filepath.clone(),
                point: field_range.start + struct_start,
                coords: None,
                local: structmatch.local,
                mtype: MatchType::StructField,
                contextstr,
                docs: find_doc(&raw_src[struct_range.clone()], field_range.start),
            });
        }
    }
    out
}

pub fn search_for_impl_methods(
    match_request: &Match,
    fieldsearchstr: &str,
    point: BytePos,
    fpath: &Path,
    local: bool,
    search_type: SearchType,
    session: &Session<'_>,
) -> Vec<Match> {
    let implsearchstr: &str = &match_request.matchstr;

    debug!(
        "searching for impl methods |{:?}| |{}| {:?}",
        match_request,
        fieldsearchstr,
        fpath.display()
    );

    let mut out = Vec::new();

    for header in search_for_impls(point, implsearchstr, fpath, local, session) {
        debug!("found impl!! |{:?}| looking for methods", header);
        let mut found_methods = HashSet::new();
        let src = session.load_source_file(header.file_path());
        for m in search_scope_for_methods(
            header.scope_start(),
            src.as_src(),
            fieldsearchstr,
            header.file_path(),
            false,
            false,
            search_type,
            session,
        ) {
            found_methods.insert(calculate_str_hash(&m.matchstr));
            out.push(m);
        }
        let trait_path = try_continue!(header.trait_path());
        // search methods coerced by deref
        if trait_path.name() == Some("Deref") {
            let target = search_scope_for_impled_assoc_types(
                &header,
                "Target",
                SearchType::ExactMatch,
                session,
            );
            if let Some((_, target_ty)) = target.into_iter().next() {
                out.extend(search_for_deref_matches(
                    target_ty,
                    match_request,
                    &header,
                    fieldsearchstr,
                    session,
                ));
            }
            continue;
        }
        let trait_match = try_continue!(header.resolve_trait(session, &ImportInfo::default()));
        for m in search_for_trait_methods(trait_match.clone(), fieldsearchstr, search_type, session)
        {
            if !found_methods.contains(&calculate_str_hash(&m.matchstr)) {
                out.push(m);
            }
        }
        for gen_impl_header in search_for_generic_impls(
            trait_match.point,
            &trait_match.matchstr,
            &trait_match.filepath,
            session,
        ) {
            debug!("found generic impl!! {:?}", gen_impl_header);
            let src = session.load_source_file(gen_impl_header.file_path());
            for gen_method in search_generic_impl_scope_for_methods(
                gen_impl_header.scope_start(),
                src.as_src(),
                fieldsearchstr,
                &gen_impl_header,
                search_type,
            ) {
                out.push(gen_method);
            }
        }
    }
    out
}

fn search_scope_for_methods(
    point: BytePos,
    src: Src<'_>,
    searchstr: &str,
    filepath: &Path,
    includes_assoc_fn: bool,
    includes_assoc_ty_and_const: bool,
    search_type: SearchType,
    session: &Session<'_>,
) -> Vec<Match> {
    debug!(
        "searching scope for methods {:?} |{}| {:?}",
        point,
        searchstr,
        filepath.display()
    );
    let scopesrc = src.shift_start(point);
    let mut out = Vec::new();
    macro_rules! ret_or_continue {
        () => {
            if search_type == ExactMatch {
                return out;
            } else {
                continue;
            }
        };
    }
    for blob_range in scopesrc.iter_stmts() {
        let matchcxt = MatchCxt {
            filepath,
            range: blob_range.shift(point),
            search_str: searchstr,
            search_type,
            is_local: true,
        };
        let method = matchers::match_method(src, &matchcxt, includes_assoc_fn, session);
        if let Some(mut m) = method {
            // for backward compatibility
            if m.contextstr.ends_with(";") {
                m.contextstr.pop();
            }
            out.push(m);
            ret_or_continue!();
        }
        if !includes_assoc_ty_and_const {
            continue;
        }
        let type_ = matchers::match_type(src, &matchcxt, session);
        if let Some(mut m) = type_ {
            m.mtype = MatchType::AssocType;
            out.push(m);
            ret_or_continue!();
        }
        let const_ = matchers::match_const(&src, &matchcxt);
        if let Some(m) = const_ {
            out.push(m);
            ret_or_continue!();
        }
    }
    out
}

fn search_generic_impl_scope_for_methods(
    point: BytePos,
    src: Src<'_>,
    searchstr: &str,
    impl_header: &Rc<ImplHeader>,
    search_type: SearchType,
) -> Vec<Match> {
    debug!(
        "searching generic impl scope for methods {:?} |{}|",
        point, searchstr,
    );

    let scopesrc = src.shift_start(point);
    let mut out = Vec::new();
    for blob_range in scopesrc.iter_stmts() {
        let blob = &scopesrc[blob_range.to_range()];
        if let Some(n) = blob.find(|c| c == '{' || c == ';') {
            let signature = blob[..n].trim_end();

            if txt_matches(search_type, &format!("fn {}", searchstr), signature)
                && typeinf::first_param_is_self(blob)
            {
                debug!("found a method starting |{}| |{}|", searchstr, blob);
                // TODO: parse this properly, or, txt_matches should return match pos?
                let start = BytePos::from(blob.find(&format!("fn {}", searchstr)).unwrap() + 3);
                let end = find_ident_end(blob, start);
                let l = &blob[start.0..end.0];
                // TODO: make a better context string for functions
                let m = Match {
                    matchstr: l.to_owned(),
                    filepath: impl_header.file_path().to_owned(),
                    point: point + blob_range.start + start,
                    coords: None,
                    local: true,
                    mtype: MatchType::Method(Some(Box::new(impl_header.generics.clone()))),
                    contextstr: signature.to_owned(),
                    docs: find_doc(&scopesrc, blob_range.start + start),
                };
                out.push(m);
            }
        }
    }
    out
}

fn search_scope_for_impled_assoc_types(
    header: &ImplHeader,
    searchstr: &str,
    search_type: SearchType,
    session: &Session<'_>,
) -> Vec<(String, Ty)> {
    let src = session.load_source_file(header.file_path());
    let scope_src = src.as_src().shift_start(header.scope_start());
    let mut out = vec![];
    let scope = Scope::new(header.file_path().to_owned(), header.scope_start());
    for blob_range in scope_src.iter_stmts() {
        let blob = &scope_src[blob_range.to_range()];
        if blob.starts_with("type") {
            let ast::TypeVisitor { name, type_, .. } = ast::parse_type(blob.to_owned(), &scope);
            let name = try_continue!(name);
            let type_ = try_continue!(type_);
            match search_type {
                SearchType::ExactMatch => {
                    if &name == searchstr {
                        out.push((name, type_));
                        break;
                    }
                }
                SearchType::StartsWith => {
                    if name.starts_with(searchstr) {
                        out.push((name, type_));
                    }
                }
            }
        }
    }
    out
}

// helper function for search_for_impls and etc
fn impl_scope_start(blob: &str) -> Option<usize> {
    if blob.starts_with("impl") {
        if let Some(&b) = blob.as_bytes().get(4) {
            if b == b' ' || b == b'<' {
                return blob.find('{');
            }
        }
    }
    None
}

// get impl headers from scope
fn search_for_impls(
    pos: BytePos,
    searchstr: &str,
    filepath: &Path,
    local: bool,
    session: &Session<'_>,
) -> Vec<ImplHeader> {
    debug!(
        "search_for_impls {:?}, {}, {:?}",
        pos,
        searchstr,
        filepath.display()
    );
    let s = session.load_source_file(filepath);
    let scope_start = scopes::scope_start(s.as_src(), pos);
    let src = s.get_src_from_start(scope_start);

    let mut out = Vec::new();
    for blob_range in src.iter_stmts() {
        let blob = &src[blob_range.to_range()];
        if let Some(n) = impl_scope_start(blob) {
            if !txt_matches(ExactMatch, searchstr, &blob[..n + 1]) {
                continue;
            }
            let decl = blob[..n + 1].to_owned() + "}";
            let start = blob_range.start + scope_start;
            let impl_header = try_continue!(ast::parse_impl(
                decl,
                filepath,
                blob_range.start + scope_start,
                local,
                start + n.into(),
            ));
            let matched = impl_header
                .self_path()
                .name()
                .map_or(false, |name| symbol_matches(ExactMatch, searchstr, name));
            if matched {
                out.push(impl_header);
            }
        }
    }
    out
}

// trait_only version of search_for_impls
// needs both `Self` type name and trait name
pub(crate) fn search_trait_impls(
    pos: BytePos,
    self_search: &str,
    trait_search: &[&str],
    once: bool,
    filepath: &Path,
    local: bool,
    session: &Session<'_>,
) -> Vec<ImplHeader> {
    debug!(
        "search_trait_impls {:?}, {}, {:?}, {:?}",
        pos,
        self_search,
        trait_search,
        filepath.display()
    );
    let s = session.load_source_file(filepath);
    let scope_start = scopes::scope_start(s.as_src(), pos);
    let src = s.get_src_from_start(scope_start);

    let mut out = Vec::new();
    for blob_range in src.iter_stmts() {
        let blob = &src[blob_range.to_range()];
        if let Some(n) = impl_scope_start(blob) {
            if !txt_matches(ExactMatch, self_search, &blob[..n + 1]) {
                continue;
            }
            let decl = blob[..n + 1].to_owned() + "}";
            let start = blob_range.start + scope_start;
            let impl_header = try_continue!(ast::parse_impl(
                decl,
                filepath,
                blob_range.start + scope_start,
                local,
                start + n.into(),
            ));
            let self_matched = impl_header
                .self_path()
                .name()
                .map_or(false, |name| symbol_matches(ExactMatch, self_search, name));
            if !self_matched {
                continue;
            }
            let trait_matched = {
                let trait_name =
                    try_continue!(impl_header.trait_path().and_then(|tpath| tpath.name()));
                trait_search
                    .into_iter()
                    .any(|ts| symbol_matches(ExactMatch, ts, trait_name))
            };
            if trait_matched {
                out.push(impl_header);
                if once {
                    break;
                }
            }
        }
    }
    out
}

fn cached_generic_impls(
    filepath: &Path,
    session: &Session<'_>,
    scope_start: BytePos,
) -> Vec<Rc<ImplHeader>> {
    // the cache is keyed by path and the scope we search in
    session
        .generic_impls
        .borrow_mut()
        .entry((filepath.into(), scope_start))
        .or_insert_with(|| {
            let s = session.load_source_file(&filepath);
            let src = s.get_src_from_start(scope_start);
            src.iter_stmts()
                .filter_map(|blob_range| {
                    let blob = &src[blob_range.to_range()];
                    let n = impl_scope_start(blob)?;
                    let decl = blob[..n + 1].to_owned() + "}";
                    let start = blob_range.start + scope_start;
                    ast::parse_impl(decl, filepath, start, true, start + n.into()).map(Rc::new)
                })
                .collect()
        })
        .clone()
}

// Find trait impls
fn search_for_generic_impls(
    pos: BytePos,
    searchstr: &str,
    filepath: &Path,
    session: &Session<'_>,
) -> Vec<Rc<ImplHeader>> {
    debug!(
        "search_for_generic_impls {:?}, {}, {:?}",
        pos,
        searchstr,
        filepath.display()
    );
    let s = session.load_source_file(filepath);
    let scope_start = scopes::scope_start(s.as_src(), pos);

    let mut out = Vec::new();

    for header in cached_generic_impls(filepath, session, scope_start).iter() {
        let name_path = header.self_path();
        if !header.is_trait() {
            continue;
        }
        if let Some(name) = name_path.segments.last() {
            for type_param in header.generics().args() {
                if symbol_matches(ExactMatch, type_param.name(), &name.name)
                    && type_param.bounds.find_by_name(searchstr).is_some()
                {
                    out.push(header.to_owned());
                }
            }
        }
    }
    out
}

// scope headers include fn decls, if let, while let etc..
fn search_scope_headers(
    point: BytePos,
    scopestart: BytePos,
    msrc: Src<'_>,
    search_str: &str,
    filepath: &Path,
    search_type: SearchType,
) -> Vec<Match> {
    debug!(
        "search_scope_headers for |{}| pt: {:?}",
        search_str, scopestart
    );

    let get_cxt = |len| MatchCxt {
        filepath,
        search_type,
        search_str,
        range: ByteRange::new(0, len),
        is_local: true,
    };
    let stmtstart = match scopes::find_stmt_start(msrc, scopestart) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let preblock = &msrc[stmtstart.0..scopestart.0];
    debug!("search_scope_headers preblock is |{}|", preblock);
    if preblock_is_fn(preblock) {
        return search_fn_args_and_generics(
            stmtstart,
            scopestart,
            &msrc,
            search_str,
            filepath,
            search_type,
            true,
        );
    // 'if let' can be an expression, so might not be at the start of the stmt
    } else if let Some(n) = preblock.find("if let") {
        let ifletstart = stmtstart + n.into();
        let trimed = msrc[ifletstart.0..scopestart.0].trim();
        if txt_matches(search_type, search_str, trimed) {
            let src = trimed.to_owned() + "{}";
            let match_cxt = get_cxt(src.len());
            let mut out = matchers::match_if_let(&src, ifletstart, &match_cxt);
            for m in &mut out {
                m.point += ifletstart;
            }
            return out;
        }
    } else if preblock.starts_with("while let") {
        let trimed = msrc[stmtstart.0..scopestart.0].trim();
        if txt_matches(search_type, search_str, trimed) {
            let src = trimed.to_owned() + "{}";
            let match_cxt = get_cxt(src.len());
            let mut out = matchers::match_while_let(&src, stmtstart, &match_cxt);
            for m in &mut out {
                m.point += stmtstart;
            }
            return out;
        }
    } else if preblock.starts_with("for ") {
        let trimed = msrc[stmtstart.0..scopestart.0].trim();
        if txt_matches(search_type, search_str, trimed) {
            let src = trimed.to_owned() + "{}";
            let match_cxt = get_cxt(src.len());
            let mut out = matchers::match_for(&src, stmtstart, &match_cxt);
            for m in &mut out {
                m.point += stmtstart;
            }
            return out;
        }
    } else if preblock.starts_with("impl") {
        let trimed = msrc[stmtstart.0..scopestart.0].trim();
        if txt_matches(search_type, search_str, trimed) {
            let src = trimed.to_owned() + "{}";
            let match_cxt = get_cxt(0);
            let mut out = match matchers::match_impl(src, &match_cxt, stmtstart) {
                Some(v) => v,
                None => return Vec::new(),
            };
            for m in &mut out {
                m.local = true;
                m.contextstr = trimed.to_owned();
            }
            return out;
        }
    } else if let Some(n) = preblock.rfind("match ") {
        // TODO: this code is crufty. refactor me!
        let matchstart = stmtstart + n.into();
        let matchstmt = typeinf::get_first_stmt(msrc.shift_start(matchstart));
        if !matchstmt.range.contains(point) {
            return Vec::new();
        }
        // The definition could be in the match LHS arms. Try to find this
        let masked_matchstmt = mask_matchstmt(&matchstmt, scopestart.increment() - matchstart);
        debug!(
            "found match stmt, masked is len {} |{}|",
            masked_matchstmt.len(),
            masked_matchstmt
        );

        // Locate the match arm LHS by finding the => just before point and then backtracking
        // be sure to be on the right side of the ... => ... arm
        let arm = match masked_matchstmt[..(point - matchstart).0].rfind("=>") {
            None =>
            // we are in the first arm enum
            {
                return Vec::new()
            }
            Some(arm) => {
                // be sure not to be in the next arm enum
                if let Some(next_arm) = masked_matchstmt[arm + 2..].find("=>") {
                    let enum_start = scopes::get_start_of_pattern(
                        &masked_matchstmt,
                        BytePos(arm + next_arm + 1),
                    );
                    if point > matchstart + enum_start {
                        return Vec::new();
                    }
                }
                BytePos(arm)
            }
        };

        debug!("PHIL matched arm rhs is |{}|", &masked_matchstmt[arm.0..]);

        let lhs_start = scopes::get_start_of_pattern(&msrc, matchstart + arm);
        let lhs = &msrc[lhs_start.0..(matchstart + arm).0];

        // Now create a pretend match expression with just the one match arm in it
        let faux_prefix_size = scopestart.increment() - matchstart;
        let fauxmatchstmt = format!("{}{{{} => () }};", &msrc[matchstart.0..scopestart.0], lhs);

        debug!("PHIL arm lhs is |{}|", lhs);
        debug!(
            "PHIL arm fauxmatchstmt is |{}|, {:?}",
            fauxmatchstmt, faux_prefix_size
        );
        let mut out = Vec::new();
        for pat_range in ast::parse_pat_idents(fauxmatchstmt) {
            let (start, end) = (
                lhs_start + pat_range.start - faux_prefix_size,
                lhs_start + pat_range.end - faux_prefix_size,
            );
            let s = &msrc[start.0..end.0];

            if symbol_matches(search_type, search_str, s) {
                out.push(Match {
                    matchstr: s.to_owned(),
                    filepath: filepath.to_path_buf(),
                    point: start,
                    coords: None,
                    local: true,
                    mtype: MatchType::MatchArm,
                    contextstr: lhs.trim().to_owned(),
                    docs: String::new(),
                });
                if let SearchType::ExactMatch = search_type {
                    break;
                }
            }
        }
        return out;
    } else if let Some(vec) = search_closure_args(
        search_str,
        preblock,
        stmtstart,
        point - stmtstart,
        filepath,
        search_type,
    ) {
        return vec;
    }
    Vec::new()
}

/// Checks if a scope preblock is a function declaration.
// TODO: handle extern ".." fn
fn preblock_is_fn(preblock: &str) -> bool {
    let s = trim_visibility(preblock);
    let p = strip_words(s, &["const", "unsafe", "async"]);
    if p.0 < s.len() {
        s[p.0..].starts_with("fn")
    } else {
        false
    }
}

#[test]
fn is_fn() {
    assert!(preblock_is_fn("pub fn bar()"));
    assert!(preblock_is_fn("fn foo()"));
    assert!(preblock_is_fn("async fn foo()"));
    assert!(preblock_is_fn("const fn baz()"));
    assert!(preblock_is_fn("pub(crate) fn bar()"));
    assert!(preblock_is_fn("pub(in foo::bar) fn bar()"));
    assert!(preblock_is_fn("crate fn bar()"));
    assert!(preblock_is_fn("crate const unsafe fn bar()"));
}

fn mask_matchstmt(matchstmt_src: &str, innerscope_start: BytePos) -> String {
    let s = scopes::mask_sub_scopes(&matchstmt_src[innerscope_start.0..]);
    matchstmt_src[..innerscope_start.0].to_owned() + &s
}

#[test]
fn test_mask_match_stmt() {
    let src = "
    match foo {
        Some(a) => { something }
    }";
    let res = mask_matchstmt(src, BytePos(src.find('{').unwrap() + 1));
    debug!("PHIL res is |{}|", res);
}

fn search_fn_args_and_generics(
    fnstart: BytePos,
    open_brace_pos: BytePos,
    msrc: &str,
    searchstr: &str,
    filepath: &Path,
    search_type: SearchType,
    local: bool,
) -> Vec<Match> {
    let mut out = Vec::new();
    // wrap in 'impl blah {}' so that methods get parsed correctly too
    let mut fndecl = "impl blah {".to_owned();
    let offset = fnstart.0 as i32 - fndecl.len() as i32;
    let impl_header_len = fndecl.len();
    fndecl += &msrc[fnstart.0..open_brace_pos.increment().0];
    fndecl += "}}";
    debug!(
        "search_fn_args: found start of fn!! {:?} |{}| {}",
        fnstart, fndecl, searchstr
    );
    if !txt_matches(search_type, searchstr, &fndecl) {
        return Vec::new();
    }
    let (args, generics) = ast::parse_fn_args_and_generics(
        fndecl.clone(),
        Scope::new(filepath.to_owned(), fnstart),
        offset,
    );
    for (pat, ty, range) in args {
        debug!("search_fn_args: arg pat is {:?}", pat);
        if let Some(matchstr) = pat.search_by_name(searchstr, search_type) {
            let context_str = &fndecl[range.to_range()];
            if let Some(p) = context_str.find(searchstr) {
                let ty = ty.map(|t| t.replace_by_generics(&generics));
                let m = Match {
                    matchstr: matchstr,
                    filepath: filepath.to_path_buf(),
                    point: fnstart + range.start + p.into() - impl_header_len.into(),
                    coords: None,
                    local: local,
                    mtype: MatchType::FnArg(Box::new((pat, ty))),
                    contextstr: context_str.to_owned(),
                    docs: String::new(),
                };
                out.push(m);
                if search_type == SearchType::ExactMatch {
                    break;
                }
            }
        }
    }
    for type_param in generics.0 {
        if symbol_matches(search_type, searchstr, type_param.name()) {
            out.push(type_param.into_match());
            if search_type == SearchType::ExactMatch {
                break;
            }
        }
    }
    out
}

#[test]
fn test_do_file_search_std() {
    let cache = core::FileCache::default();
    let path = Path::new(".");
    let session = Session::new(&cache, Some(path));
    let matches = do_file_search("std", path, &session);
    assert!(matches
        .into_iter()
        .any(|m| m.filepath.ends_with("std/src/lib.rs")));
}

#[test]
fn test_do_file_search_local() {
    let cache = core::FileCache::default();
    let path = Path::new("fixtures/arst/src");
    let session = Session::new(&cache, Some(path));
    let matches = do_file_search("submodule", path, &session);
    assert!(matches
        .into_iter()
        .any(|m| m.filepath.ends_with("fixtures/arst/src/submodule/mod.rs")));
}

pub fn do_file_search(searchstr: &str, currentdir: &Path, session: &Session<'_>) -> Vec<Match> {
    debug!("do_file_search with search string \"{}\"", searchstr);
    let mut out = Vec::new();

    let std_path = RUST_SRC_PATH.as_ref();
    debug!("do_file_search std_path: {:?}", std_path);

    let (v_1, v_2);
    let v = if let Some(std_path) = std_path {
        v_2 = [std_path, currentdir];
        &v_2[..]
    } else {
        v_1 = [currentdir];
        &v_1[..]
    };

    debug!("do_file_search v: {:?}", v);
    for srcpath in v {
        if let Ok(iter) = std::fs::read_dir(srcpath) {
            for fpath_buf in iter.filter_map(|res| res.ok().map(|entry| entry.path())) {
                // skip filenames that can't be decoded
                let fname = match fpath_buf.file_name().and_then(|n| n.to_str()) {
                    Some(fname) => fname,
                    None => continue,
                };
                // Firstly, try the original layout, e.g. libstd/lib.rs
                if fname.starts_with(&format!("lib{}", searchstr)) {
                    let filepath = fpath_buf.join("lib.rs");
                    if filepath.exists() || session.contains_file(&filepath) {
                        let m = Match {
                            matchstr: fname[3..].to_owned(),
                            filepath: filepath.to_path_buf(),
                            point: BytePos::ZERO,
                            coords: Some(Coordinate::start()),
                            local: false,
                            mtype: MatchType::Module,
                            contextstr: fname[3..].to_owned(),
                            docs: String::new(),
                        };
                        out.push(m);
                    }
                }
                // Secondly, try the new standard library layout, e.g. std/src/lib.rs
                if fname.starts_with(searchstr) {
                    let filepath = fpath_buf.join("src").join("lib.rs");
                    if filepath.exists() || session.contains_file(&filepath) {
                        let m = Match {
                            matchstr: fname.to_owned(),
                            filepath: filepath.to_path_buf(),
                            point: BytePos::ZERO,
                            coords: Some(Coordinate::start()),
                            local: false,
                            mtype: MatchType::Module,
                            contextstr: fname.to_owned(),
                            docs: String::new(),
                        };
                        out.push(m);
                    }
                }

                if fname.starts_with(searchstr) {
                    for name in &[&format!("{}.rs", fname)[..], "mod.rs", "lib.rs"] {
                        let filepath = fpath_buf.join(name);
                        if filepath.exists() || session.contains_file(&filepath) {
                            let m = Match {
                                matchstr: fname.to_owned(),
                                filepath: filepath.to_path_buf(),
                                point: BytePos::ZERO,
                                coords: Some(Coordinate::start()),
                                local: false,
                                mtype: MatchType::Module,
                                contextstr: filepath.to_str().unwrap().to_owned(),
                                docs: String::new(),
                            };
                            out.push(m);
                        }
                    }
                    // try just <name>.rs
                    if fname.ends_with(".rs")
                        && (fpath_buf.exists() || session.contains_file(&fpath_buf))
                    {
                        let m = Match {
                            matchstr: fname[..(fname.len() - 3)].to_owned(),
                            filepath: fpath_buf.clone(),
                            point: BytePos::ZERO,
                            coords: Some(Coordinate::start()),
                            local: false,
                            mtype: MatchType::Module,
                            contextstr: fpath_buf.to_str().unwrap().to_owned(),
                            docs: String::new(),
                        };
                        out.push(m);
                    }
                }
            }
        }
    }
    out
}

pub fn search_crate_root(
    pathseg: &PathSegment,
    modfpath: &Path,
    searchtype: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
    // Skip current file or not
    // If we aren't searching paths with global prefix, should do so
    skip_modfpath: bool,
) -> Vec<Match> {
    debug!("search_crate_root |{:?}| {:?}", pathseg, modfpath.display());

    let mut crateroots = find_possible_crate_root_modules(modfpath.parent().unwrap(), session);
    // for cases when file is not part of a project
    if crateroots.is_empty() {
        crateroots.push(modfpath.to_path_buf());
    }

    let mut out = Vec::new();
    for crateroot in crateroots
        .into_iter()
        .filter(|c| !skip_modfpath || modfpath != c)
    {
        debug!(
            "going to search for {:?} in crateroot {:?}",
            pathseg,
            crateroot.display()
        );
        for m in resolve_name(
            pathseg,
            &crateroot,
            BytePos::ZERO,
            searchtype,
            namespace,
            session,
            import_info,
        ) {
            out.push(m);
            if let ExactMatch = searchtype {
                break;
            }
        }
    }
    out
}

pub fn find_possible_crate_root_modules(currentdir: &Path, session: &Session<'_>) -> Vec<PathBuf> {
    let mut res = Vec::new();

    for root in &["lib.rs", "main.rs"] {
        let filepath = currentdir.join(root);
        if filepath.exists() || session.contains_file(&filepath) {
            res.push(filepath);
            return res; // for now stop at the first match
        }
    }
    // recurse up the directory structure
    if let Some(parentdir) = currentdir.parent() {
        if parentdir != currentdir {
            res.append(&mut find_possible_crate_root_modules(parentdir, session));
            return res; // for now stop at the first match
        }
    }
    res
}

pub fn search_next_scope(
    mut startpoint: BytePos,
    pathseg: &PathSegment,
    filepath: &Path,
    search_type: SearchType,
    local: bool,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    let filesrc = session.load_source_file(filepath);
    if startpoint != BytePos::ZERO {
        // is a scope inside the file. Point should point to the definition
        // (e.g. mod blah {...}), so the actual scope is past the first open brace.
        let src = &filesrc[startpoint.0..];
        // find the opening brace and skip to it.
        if let Some(n) = src.find('{') {
            startpoint += BytePos(n + 1);
        }
    }
    search_scope(
        startpoint,
        None,
        filesrc.as_src(),
        pathseg,
        filepath,
        search_type,
        local,
        namespace,
        session,
        import_info,
    )
}

pub fn search_scope(
    start: BytePos,
    complete_point: Option<BytePos>,
    src: Src<'_>,
    pathseg: &PathSegment,
    filepath: &Path,
    search_type: SearchType,
    is_local: bool,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    let search_str = &pathseg.name;
    let mut out = Vec::new();

    debug!(
        "searching scope {:?} start: {:?} point: {:?} '{}' {:?} {:?} local: {}",
        namespace,
        start,
        complete_point,
        search_str,
        filepath.display(),
        search_type,
        is_local,
    );

    let scopesrc = src.shift_start(start);
    let mut delayed_single_imports = Vec::new();
    let mut delayed_glob_imports = Vec::new();
    let mut codeit = scopesrc.iter_stmts();
    let mut v = Vec::new();

    let get_match_cxt = |range| MatchCxt {
        filepath,
        search_str,
        search_type,
        is_local,
        range,
    };
    if let Some(point) = complete_point {
        // collect up to point so we can search backwards for let bindings
        //  (these take precidence over local fn declarations etc..
        for blob_range in &mut codeit {
            v.push(blob_range);
            if blob_range.start > point {
                break;
            }
        }
        // search backwards from point for let bindings
        for &blob_range in v.iter().rev() {
            if (start + blob_range.end) >= point {
                continue;
            }
            let range = blob_range.shift(start);
            let match_cxt = get_match_cxt(range);
            for m in matchers::match_let(&src, range.start, &match_cxt) {
                out.push(m);
                if let ExactMatch = search_type {
                    return out;
                }
            }
        }
    }
    // since we didn't find a `let` binding, now search from top of scope for items etc..
    let mut codeit = v.into_iter().chain(codeit);
    for blob_range in &mut codeit {
        let blob = &scopesrc[blob_range.to_range()];
        if util::trim_visibility(blob).starts_with("use") {
            // A `use` item can import a value
            // with the same name as a "type" (type/module/etc.) in the same scope.
            // However, that type might appear after the `use`,
            // so we need to process the type first and the `use` later (if necessary).
            // If we didn't delay imports,
            // we'd try to resolve such a `use` item by recursing onto itself.

            // Optimisation: if the search string is not in the blob and it is not
            // a glob import, this cannot match so fail fast!
            let is_glob_import = blob.contains("::*");
            if !is_glob_import && !blob.contains(search_str.trim_end_matches('!')) {
                continue;
            }

            if is_glob_import {
                delayed_glob_imports.push(blob_range);
            } else {
                delayed_single_imports.push(blob_range);
            }
            continue;
        }

        if search_str == "core" && blob.starts_with("#![no_std]") {
            debug!("Looking for core and found #![no_std], which implicitly imports it");
            if let Some(cratepath) = get_crate_file("core", filepath, session) {
                let context = cratepath.to_str().unwrap().to_owned();
                out.push(Match {
                    matchstr: "core".into(),
                    filepath: cratepath,
                    point: BytePos::ZERO,
                    coords: Some(Coordinate::start()),
                    local: false,
                    mtype: MatchType::Module,
                    contextstr: context,
                    docs: String::new(),
                });
            }
        }

        // Optimisation: if the search string is not in the blob,
        // this cannot match so fail fast!
        if !blob.contains(search_str.trim_end_matches('!')) {
            continue;
        }

        // if we find extern block, let's look up inner scope
        if blob.starts_with("extern") {
            if let Some(block_start) = blob[7..].find('{') {
                debug!("[search_scope] found extern block!");
                // move to the point next to {
                let start = blob_range.start + BytePos(block_start + 8);
                out.extend(search_scope(
                    start,
                    None,
                    src,
                    pathseg,
                    filepath,
                    search_type,
                    is_local,
                    namespace,
                    session,
                    import_info,
                ));
                continue;
            }
        }
        // There's a good chance of a match. Run the matchers
        let match_cxt = get_match_cxt(blob_range.shift(start));
        out.extend(run_matchers_on_blob(
            src,
            &match_cxt,
            namespace,
            session,
            import_info,
        ));
        if let ExactMatch = search_type {
            if !out.is_empty() {
                return out;
            }
        }
    }

    let delayed_import_len = delayed_single_imports.len() + delayed_glob_imports.len();

    if delayed_import_len > 0 {
        trace!(
            "Searching {} delayed imports for `{}`",
            delayed_import_len,
            search_str
        );
    }

    // Finally, process the imports that we skipped before.
    // Process single imports first, because they shadow glob imports.
    for blob_range in delayed_single_imports
        .into_iter()
        .chain(delayed_glob_imports)
    {
        // There's a good chance of a match. Run the matchers
        let match_cxt = get_match_cxt(blob_range.shift(start));
        for m in run_matchers_on_blob(src, &match_cxt, namespace, session, import_info) {
            out.push(m);
            if let ExactMatch = search_type {
                return out;
            }
        }
    }

    if let Some(point) = complete_point {
        if let Some(vec) = search_closure_args(
            search_str,
            &scopesrc[0..],
            start,
            point - start,
            filepath,
            search_type,
        ) {
            for mat in vec {
                out.push(mat);
                if let ExactMatch = search_type {
                    return out;
                }
            }
        }
    }
    debug!("search_scope found matches {:?} {:?}", search_type, out);
    out
}

fn search_closure_args(
    search_str: &str,
    scope_src: &str,
    scope_src_pos: BytePos,
    point: BytePos,
    filepath: &Path,
    search_type: SearchType,
) -> Option<Vec<Match>> {
    if search_str.is_empty() {
        return None;
    }

    trace!(
        "Closure definition match is looking for `{}` in {} characters",
        search_str,
        scope_src.len()
    );

    if let Some((pipe_range, body_range)) = util::find_closure(scope_src) {
        let pipe_str = &scope_src[pipe_range.to_range()];
        if point < pipe_range.start || point > body_range.end {
            return None;
        }

        debug!(
            "search_closure_args found valid closure arg scope: {}",
            pipe_str
        );
        if !txt_matches(search_type, search_str, pipe_str) {
            return None;
        }
        // Add a fake body for parsing
        let closure_def = String::from(pipe_str) + "{}";
        let scope = Scope::new(filepath.to_owned(), scope_src_pos);
        let args = ast::parse_closure_args(closure_def.clone(), scope);
        let mut out: Vec<Match> = Vec::new();
        for (pat, ty, arg_range) in args {
            if let Some(matchstr) = pat.search_by_name(search_str, search_type) {
                let context_str = &closure_def[arg_range.to_range()];
                if let Some(p) = context_str.find(search_str) {
                    let m = Match {
                        matchstr: matchstr,
                        filepath: filepath.to_path_buf(),
                        point: scope_src_pos + pipe_range.start + arg_range.start + p.into(),
                        coords: None,
                        local: true,
                        mtype: MatchType::FnArg(Box::new((pat, ty))),
                        // TODO: context_str(without pipe) is better?
                        contextstr: pipe_str.to_owned(),
                        docs: String::new(),
                    };
                    debug!("search_closure_args matched: {:?}", m);
                    out.push(m);
                }
            }
        }
        return Some(out);
    }
    None
}

fn run_matchers_on_blob(
    src: Src<'_>,
    context: &MatchCxt<'_, '_>,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    debug!(
        "[run_matchers_on_blob] cxt: {:?}, namespace: {:?}",
        context, namespace
    );
    macro_rules! run_matcher_common {
        ($ns: expr, $matcher: expr) => {
            if namespace.contains($ns) {
                if let Some(m) = $matcher {
                    return vec![m];
                }
            }
        };
    }
    macro_rules! run_matcher {
        ($ns: expr, $matcher: path) => {
            run_matcher_common!($ns, $matcher(src, context, session))
        };
    }
    macro_rules! run_const_matcher {
        ($ns: expr, $matcher: path) => {
            run_matcher_common!($ns, $matcher(&src, context))
        };
    }
    run_matcher!(Namespace::Crate, matchers::match_extern_crate);
    run_matcher!(Namespace::Mod, matchers::match_mod);
    run_matcher!(Namespace::Enum, matchers::match_enum);
    run_matcher!(Namespace::Struct, matchers::match_struct);
    run_matcher!(Namespace::Union, matchers::match_union);
    run_matcher!(Namespace::Trait, matchers::match_trait);
    run_matcher!(Namespace::TypeDef, matchers::match_type);
    run_matcher!(Namespace::Func, matchers::match_fn);
    run_const_matcher!(Namespace::Const, matchers::match_const);
    run_const_matcher!(Namespace::Static, matchers::match_static);
    // TODO(kngwyu): support use_extern_macros
    run_matcher!(Namespace::Global, matchers::match_macro);
    let mut out = Vec::new();
    if namespace.intersects(Namespace::PathParen) {
        for m in matchers::match_use(src, context, session, import_info) {
            out.push(m);
            if context.search_type == ExactMatch {
                return out;
            }
        }
    }
    out
}

fn search_local_scopes(
    pathseg: &PathSegment,
    filepath: &Path,
    msrc: Src<'_>,
    point: BytePos,
    search_type: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    debug!(
        "search_local_scopes {:?} {:?} {:?} {:?} {:?}",
        pathseg,
        filepath.display(),
        point,
        search_type,
        namespace
    );

    if point == BytePos::ZERO {
        // search the whole file
        search_scope(
            BytePos::ZERO,
            None,
            msrc,
            pathseg,
            filepath,
            search_type,
            true,
            namespace,
            session,
            import_info,
        )
    } else {
        let mut out = Vec::new();
        let mut start = point;
        // search each parent scope in turn
        while start > BytePos::ZERO {
            start = scopes::scope_start(msrc, start);
            for m in search_scope(
                start,
                Some(point),
                msrc,
                pathseg,
                filepath,
                search_type,
                true,
                namespace,
                session,
                import_info,
            ) {
                out.push(m);
                if search_type == ExactMatch {
                    return out;
                }
            }
            if start == BytePos::ZERO {
                break;
            }
            start = start.decrement();
            let searchstr = &pathseg.name;

            // scope headers = fn decls, if let, match, etc..
            for m in search_scope_headers(point, start, msrc, searchstr, filepath, search_type) {
                out.push(m);
                if let ExactMatch = search_type {
                    return out;
                }
            }
        }
        out
    }
}

pub fn search_prelude_file(
    pathseg: &PathSegment,
    search_type: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    debug!(
        "search_prelude file {:?} {:?} {:?}",
        pathseg, search_type, namespace
    );
    let mut out: Vec<Match> = Vec::new();

    // find the prelude file from the search path and scan it
    if let Some(ref std_path) = *RUST_SRC_PATH {
        let filepath = std_path.join("std").join("src").join("prelude").join("v1.rs");
        if filepath.exists() || session.contains_file(&filepath) {
            let msrc = session.load_source_file(&filepath);
            let is_local = true;
            for m in search_scope(
                BytePos::ZERO,
                None,
                msrc.as_src(),
                pathseg,
                &filepath,
                search_type,
                is_local,
                namespace,
                session,
                import_info,
            ) {
                out.push(m);
            }
        }
    }
    out
}

pub fn resolve_path_with_primitive(
    path: &RacerPath,
    filepath: &Path,
    pos: BytePos,
    search_type: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
) -> Vec<Match> {
    debug!("resolve_path_with_primitive {:?}", path);

    let mut out = Vec::new();
    if path.segments.len() == 1 {
        primitive::get_primitive_mods(&path.segments[0].name, search_type, &mut out);
        if search_type == ExactMatch && !out.is_empty() {
            return out;
        }
    }
    let generics = match path.segments.last() {
        Some(seg) => &seg.generics,
        None => return out,
    };
    for mut m in resolve_path(
        path,
        filepath,
        pos,
        search_type,
        namespace,
        session,
        &ImportInfo::default(),
    ) {
        m.resolve_generics(generics);
        out.push(m);
        if search_type == ExactMatch {
            break;
        }
    }
    out
}

#[derive(PartialEq, Debug)]
pub struct Search {
    path: Vec<String>,
    filepath: String,
    pos: BytePos,
}

/// Attempt to resolve a name which occurs in a given file.
pub fn resolve_name(
    pathseg: &PathSegment,
    filepath: &Path,
    pos: BytePos,
    search_type: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    let mut out = Vec::new();
    let searchstr = &pathseg.name;

    let msrc = session.load_source_file(filepath);
    let is_exact_match = search_type == ExactMatch;

    if is_exact_match && &searchstr[..] == "Self" {
        if let Some(Ty::Match(m)) =
            typeinf::get_type_of_self(pos, filepath, true, msrc.as_src(), session)
        {
            out.push(m.clone());
        }
    }

    if (is_exact_match && &searchstr[..] == "std")
        || (!is_exact_match && "std".starts_with(searchstr))
    {
        if let Some(cratepath) = get_std_file("std", session) {
            let context = cratepath.to_str().unwrap().to_owned();
            out.push(Match {
                matchstr: "std".into(),
                filepath: cratepath,
                point: BytePos::ZERO,
                coords: Some(Coordinate::start()),
                local: false,
                mtype: MatchType::Module,
                contextstr: context,
                docs: String::new(),
            });
        }

        if is_exact_match && !out.is_empty() {
            return out;
        }
    }

    for m in search_local_scopes(
        pathseg,
        filepath,
        msrc.as_src(),
        pos,
        search_type,
        namespace,
        session,
        import_info,
    ) {
        out.push(m);
        if is_exact_match {
            return out;
        }
    }

    for m in search_crate_root(
        pathseg,
        filepath,
        search_type,
        namespace,
        session,
        import_info,
        true,
    ) {
        out.push(m);
        if is_exact_match {
            return out;
        }
    }

    if namespace.contains(Namespace::Crate) {
        out.extend(search_crate_names(
            searchstr,
            search_type,
            filepath,
            true,
            session,
        ));
        if is_exact_match && !out.is_empty() {
            return out;
        }
    }

    if namespace.contains(Namespace::Primitive) {
        primitive::get_primitive_docs(searchstr, search_type, session, &mut out);
        if is_exact_match && !out.is_empty() {
            return out;
        }
    }
    if namespace.contains(Namespace::StdMacro) {
        get_std_macros(searchstr, search_type, session, &mut out);
        if is_exact_match && !out.is_empty() {
            return out;
        }
    }

    for m in search_prelude_file(pathseg, search_type, namespace, session, import_info) {
        out.push(m);
        if is_exact_match {
            return out;
        }
    }
    // filesearch. Used to complete e.g. mod foo
    if let StartsWith = search_type {
        for m in do_file_search(searchstr, filepath.parent().unwrap(), session) {
            out.push(m);
        }
    }
    out
}

// Get the scope corresponding to super::
pub fn get_super_scope(
    filepath: &Path,
    pos: BytePos,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Option<core::Scope> {
    let msrc = session.load_source_file(filepath);
    let mut path = scopes::get_local_module_path(msrc.as_src(), pos);
    debug!(
        "get_super_scope: path: {:?} filepath: {:?} {:?} {:?}",
        path, filepath, pos, session
    );
    if path.is_empty() {
        let moduledir = if filepath.ends_with("mod.rs") || filepath.ends_with("lib.rs") {
            // Need to go up to directory above
            filepath.parent()?.parent()?
        } else {
            // module is in current directory
            filepath.parent()?
        };

        for filename in &["mod.rs", "lib.rs"] {
            let f_path = moduledir.join(&filename);
            if f_path.exists() || session.contains_file(&f_path) {
                return Some(core::Scope {
                    filepath: f_path,
                    point: BytePos::ZERO,
                });
            }
        }
        None
    } else if path.len() == 1 {
        Some(core::Scope {
            filepath: filepath.to_path_buf(),
            point: BytePos::ZERO,
        })
    } else {
        path.pop();
        let path = RacerPath::from_svec(false, path);
        debug!("get_super_scope looking for local scope {:?}", path);
        resolve_path(
            &path,
            filepath,
            BytePos::ZERO,
            SearchType::ExactMatch,
            Namespace::PathParen,
            session,
            import_info,
        )
        .into_iter()
        .nth(0)
        .and_then(|m| {
            msrc[m.point.0..].find('{').map(|p| core::Scope {
                filepath: filepath.to_path_buf(),
                point: m.point + BytePos(p + 1),
            })
        })
    }
}

fn get_enum_variants(
    search_path: &PathSegment,
    search_type: SearchType,
    context: &Match,
    session: &Session<'_>,
) -> Vec<Match> {
    let mut out = Vec::new();
    debug!("context: {:?}", context);
    match context.mtype {
        // TODO(kngwyu): use generics
        MatchType::Enum(ref _generics) => {
            let filesrc = session.load_source_file(&context.filepath);
            let scopestart = scopes::find_stmt_start(filesrc.as_src(), context.point)
                .expect("[resolve_path] statement start was not found");
            let scopesrc = filesrc.get_src_from_start(scopestart);
            if let Some(blob_range) = scopesrc.iter_stmts().nth(0) {
                let match_cxt = MatchCxt {
                    filepath: &context.filepath,
                    search_str: &search_path.name,
                    search_type,
                    range: blob_range.shift(scopestart),
                    is_local: true,
                };
                for mut enum_var in matchers::match_enum_variants(&filesrc, &match_cxt) {
                    debug!(
                        "Found enum variant {} with enum type {}",
                        enum_var.matchstr, context.matchstr
                    );
                    // return Match which has enum simultaneously, for method completion
                    enum_var.mtype = MatchType::EnumVariant(Some(Box::new(context.clone())));
                    out.push(enum_var);
                }
            }
        }
        _ => {}
    }
    out
}

fn search_impl_scope(
    path: &PathSegment,
    search_type: SearchType,
    header: &ImplHeader,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    let src = session.load_source_file(header.file_path());
    let search_str = &path.name;
    let scope_src = src.as_src().shift_start(header.scope_start());
    let mut out = Vec::new();
    for blob_range in scope_src.iter_stmts() {
        let match_cxt = MatchCxt {
            filepath: header.file_path(),
            search_str,
            search_type,
            is_local: header.is_local(),
            range: blob_range.shift(header.scope_start()),
        };
        out.extend(run_matchers_on_blob(
            src.as_src(),
            &match_cxt,
            Namespace::Impl,
            session,
            import_info,
        ));
    }
    out
}

fn get_impled_items(
    search_path: &PathSegment,
    search_type: SearchType,
    context: &Match,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    let mut out = get_enum_variants(search_path, search_type, context, session);
    for header in search_for_impls(
        context.point,
        &context.matchstr,
        &context.filepath,
        context.local,
        session,
    ) {
        out.extend(search_impl_scope(
            &search_path,
            search_type,
            &header,
            session,
            import_info,
        ));
        let trait_match = try_continue!(header.resolve_trait(session, import_info));
        for timpl_header in search_for_generic_impls(
            trait_match.point,
            &trait_match.matchstr,
            &trait_match.filepath,
            session,
        ) {
            debug!("found generic impl!! {:?}", timpl_header);
            out.extend(search_impl_scope(
                &search_path,
                search_type,
                &timpl_header,
                session,
                import_info,
            ));
        }
    }
    if search_type != ExactMatch {
        return out;
    }
    // for return type inference
    if let Some(gen) = context.to_generics() {
        for m in &mut out {
            if m.mtype == MatchType::Function {
                m.mtype = MatchType::Method(Some(Box::new(gen.to_owned())));
            }
        }
    }
    out
}

pub fn resolve_path(
    path: &RacerPath,
    filepath: &Path,
    pos: BytePos,
    search_type: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    debug!(
        "resolve_path {:?} {:?} {:?} {:?}",
        path,
        filepath.display(),
        pos,
        search_type
    );
    let len = path.len();
    if let Some(ref prefix) = path.prefix {
        match prefix {
            // TODO: Crate, Self,..
            PathPrefix::Super => {
                if let Some(scope) = get_super_scope(filepath, pos, session, import_info) {
                    debug!("PHIL super scope is {:?}", scope);
                    let mut newpath = path.clone();
                    newpath.prefix = None;
                    newpath.set_prefix();
                    return resolve_path(
                        &newpath,
                        &scope.filepath,
                        scope.point,
                        search_type,
                        namespace,
                        session,
                        import_info,
                    );
                } else {
                    // can't find super scope. Return no matches
                    debug!("can't resolve path {:?}, returning no matches", path);
                    return Vec::new();
                }
            }
            PathPrefix::Global => {
                return resolve_global_path(
                    path,
                    filepath,
                    search_type,
                    namespace,
                    session,
                    import_info,
                )
                .unwrap_or_else(Vec::new);
            }
            _ => {}
        }
    }
    if len == 1 {
        let pathseg = &path.segments[0];
        resolve_name(
            pathseg,
            filepath,
            pos,
            search_type,
            namespace,
            session,
            import_info,
        )
    } else if len != 0 {
        let mut parent_path = path.clone();
        let last_seg = parent_path.segments.pop().unwrap();
        let context = resolve_path(
            &parent_path,
            filepath,
            pos,
            ExactMatch,
            Namespace::PathParen,
            session,
            import_info,
        )
        .into_iter()
        .nth(0);
        debug!(
            "[resolve_path] context: {:?}, last_seg: {:?}",
            context, last_seg
        );
        if let Some(followed_match) = context {
            resolve_following_path(
                followed_match,
                &last_seg,
                namespace,
                search_type,
                import_info,
                session,
            )
        } else {
            Vec::new()
        }
    } else {
        // TODO: Should this better be an assertion ? Why do we have a core::Path
        // with empty segments in the first place ?
        Vec::new()
    }
}

/// resolve paths like ::path::to::file
fn resolve_global_path(
    path: &RacerPath,
    filepath: &Path,
    search_type: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Option<Vec<Match>> {
    let mut segs = path.segments.iter().enumerate();
    let first_stype = if path.segments.len() == 1 {
        search_type
    } else {
        SearchType::ExactMatch
    };
    let mut context = search_crate_root(
        segs.next()?.1,
        filepath,
        first_stype,
        namespace,
        session,
        import_info,
        false,
    );
    for (i, seg) in segs {
        let cxt = context.into_iter().next()?;
        let is_last = i + 1 == path.segments.len();
        let stype = if is_last {
            search_type
        } else {
            SearchType::ExactMatch
        };
        context = resolve_following_path(cxt, seg, namespace, stype, import_info, session);
    }
    Some(context)
}

fn resolve_following_path(
    followed_match: Match,
    following_seg: &PathSegment,
    namespace: Namespace,
    search_type: SearchType,
    import_info: &ImportInfo<'_, '_>,
    session: &Session<'_>,
) -> Vec<Match> {
    match followed_match.mtype {
        MatchType::Module | MatchType::Crate => {
            let mut searchstr: &str = &following_seg.name;
            if let Some(i) = searchstr.rfind(',') {
                searchstr = searchstr[i + 1..].trim();
            }
            if searchstr.starts_with('{') {
                searchstr = &searchstr[1..];
            }
            let pathseg = PathSegment::new(searchstr.to_owned(), vec![], None);
            debug!(
                "searching a module '{}' for {}",
                followed_match.matchstr, pathseg.name,
            );
            search_next_scope(
                followed_match.point,
                &pathseg,
                &followed_match.filepath,
                search_type,
                false,
                namespace,
                session,
                import_info,
            )
        }
        MatchType::Enum(_) | MatchType::Struct(_) | MatchType::Union(_) => get_impled_items(
            following_seg,
            search_type,
            &followed_match,
            session,
            import_info,
        ),
        MatchType::Trait => search_for_trait_items(
            followed_match,
            &following_seg.name,
            search_type,
            true,
            true,
            session,
        )
        .collect(),
        MatchType::TypeParameter(bounds) => bounds
            .get_traits(session)
            .into_iter()
            .map(|m| {
                search_for_trait_items(m, &following_seg.name, search_type, true, true, session)
            })
            .flatten()
            .collect(),
        MatchType::Type => {
            if let Some(match_) = typeinf::get_type_of_typedef(&followed_match, session) {
                get_impled_items(following_seg, search_type, &match_, session, import_info)
            } else {
                // TODO: Should use STUB here
                Vec::new()
            }
        }
        MatchType::UseAlias(inner) => resolve_following_path(
            *inner,
            following_seg,
            namespace,
            search_type,
            import_info,
            session,
        ),
        _ => Vec::new(),
    }
}

pub fn resolve_method(
    point: BytePos,
    msrc: Src<'_>,
    searchstr: &str,
    filepath: &Path,
    search_type: SearchType,
    session: &Session<'_>,
    import_info: &ImportInfo<'_, '_>,
) -> Vec<Match> {
    let scopestart = scopes::scope_start(msrc, point);
    debug!(
        "resolve_method for |{}| pt: {:?}; scopestart: {:?}",
        searchstr, point, scopestart,
    );

    let parent_scope = match scopestart.try_decrement() {
        Some(x) => x,
        None => return vec![],
    };

    if let Some(stmtstart) = scopes::find_stmt_start(msrc, parent_scope) {
        let preblock = &msrc[stmtstart.0..scopestart.0];
        debug!("search_scope_headers preblock is |{}|", preblock);

        if preblock.starts_with("impl") {
            if let Some(n) = preblock.find(" for ") {
                let start = scopes::get_start_of_search_expr(preblock, n.into());
                let expr = &preblock[start.0..n];

                debug!("found impl of trait : expr is |{}|", expr);
                let path = RacerPath::from_vec(false, expr.split("::").collect::<Vec<_>>());
                let m = resolve_path(
                    &path,
                    filepath,
                    stmtstart + BytePos(n - 1),
                    SearchType::ExactMatch,
                    Namespace::Trait,
                    session,
                    import_info,
                )
                .into_iter()
                .filter(|m| m.mtype == MatchType::Trait)
                .nth(0);
                if let Some(m) = m {
                    debug!("found trait : match is |{:?}|", m);
                    let mut out = Vec::new();
                    let src = session.load_source_file(&m.filepath);
                    if let Some(n) = src[m.point.0..].find('{') {
                        let point = m.point + BytePos(n + 1);
                        for m in search_scope_for_methods(
                            point,
                            src.as_src(),
                            searchstr,
                            &m.filepath,
                            true,
                            false,
                            search_type,
                            session,
                        ) {
                            out.push(m);
                        }
                    }

                    trace!(
                        "Found {} methods matching `{}` for trait `{}`",
                        out.len(),
                        searchstr,
                        m.matchstr
                    );

                    return out;
                }
            }
        }
    }

    Vec::new()
}

pub fn do_external_search(
    path: &[&str],
    filepath: &Path,
    pos: BytePos,
    search_type: SearchType,
    namespace: Namespace,
    session: &Session<'_>,
) -> Vec<Match> {
    debug!(
        "do_external_search path {:?} {:?}",
        path,
        filepath.display()
    );
    let mut out = Vec::new();
    if path.len() == 1 {
        let searchstr = path[0];
        // hack for now
        let pathseg = PathSegment::new(path[0].to_owned(), vec![], None);
        out.extend(search_next_scope(
            pos,
            &pathseg,
            filepath,
            search_type,
            false,
            namespace,
            session,
            &ImportInfo::default(),
        ));

        if let Some(path) = get_module_file(searchstr, filepath.parent().unwrap(), session) {
            let context = path.to_str().unwrap().to_owned();
            out.push(Match {
                matchstr: searchstr.to_owned(),
                filepath: path,
                point: BytePos::ZERO,
                coords: Some(Coordinate::start()),
                local: false,
                mtype: MatchType::Module,
                contextstr: context,
                docs: String::new(),
            });
        }
    } else {
        let parent_path = &path[..(path.len() - 1)];
        let context = do_external_search(
            parent_path,
            filepath,
            pos,
            ExactMatch,
            Namespace::PathParen,
            session,
        )
        .into_iter()
        .nth(0);
        context.map(|m| {
            let import_info = &ImportInfo::default();
            match m.mtype {
                MatchType::Module => {
                    debug!("found an external module {}", m.matchstr);
                    // deal with started with "{", so that "foo::{bar" will be same as "foo::bar"
                    let searchstr = match path[path.len() - 1].chars().next() {
                        Some('{') => &path[path.len() - 1][1..],
                        _ => path[path.len() - 1],
                    };
                    let pathseg = PathSegment::new(searchstr.to_owned(), vec![], None);
                    for m in search_next_scope(
                        m.point,
                        &pathseg,
                        &m.filepath,
                        search_type,
                        false,
                        namespace,
                        session,
                        import_info,
                    ) {
                        out.push(m);
                    }
                }

                MatchType::Struct(_) => {
                    debug!("found a pub struct. Now need to look for impl");
                    for impl_header in
                        search_for_impls(m.point, &m.matchstr, &m.filepath, m.local, session)
                    {
                        // deal with started with "{", so that "foo::{bar" will be same as "foo::bar"
                        let searchstr = match path[path.len() - 1].chars().next() {
                            Some('{') => &path[path.len() - 1][1..],
                            _ => path[path.len() - 1],
                        };
                        let pathseg = PathSegment::new(searchstr.to_owned(), vec![], None);
                        debug!("about to search impl scope...");
                        for m in search_next_scope(
                            impl_header.impl_start(),
                            &pathseg,
                            impl_header.file_path(),
                            search_type,
                            impl_header.is_local(),
                            namespace,
                            session,
                            import_info,
                        ) {
                            out.push(m);
                        }
                    }
                }
                _ => (),
            }
        });
    }
    out
}

/// collect inherited traits by Depth First Search
fn collect_inherited_traits(trait_match: Match, s: &Session<'_>) -> Vec<Match> {
    // search node
    struct Node {
        target_str: String,
        offset: i32,
        filepath: PathBuf,
    }
    impl Node {
        fn from_match(m: &Match) -> Self {
            let target_str = m.contextstr.to_owned() + "{}";
            let offset = m.point.0 as i32 - "trait ".len() as i32;
            Node {
                target_str: target_str,
                offset: offset,
                filepath: m.filepath.clone(),
            }
        }
    }
    // DFS stack
    let mut stack = vec![Node::from_match(&trait_match)];
    // we have to store hashes of trait names to prevent infinite loop!
    let mut trait_names = HashSet::new();
    trait_names.insert(calculate_str_hash(&trait_match.matchstr));
    let mut res = vec![trait_match];
    // DFS
    while let Some(t) = stack.pop() {
        if let Some(bounds) = ast::parse_inherited_traits(t.target_str, t.filepath, t.offset) {
            let traits = bounds.get_traits(s);
            let filtered = traits.into_iter().filter(|tr| {
                let hash = calculate_str_hash(&tr.matchstr);
                if trait_names.contains(&hash) {
                    return false;
                }
                trait_names.insert(hash);
                let tr_info = Node::from_match(&tr);
                stack.push(tr_info);
                true
            });
            res.extend(filtered);
        }
    }
    res
}

pub fn search_for_fields_and_methods(
    context: Match,
    searchstr: &str,
    search_type: SearchType,
    only_methods: bool,
    session: &Session<'_>,
) -> Vec<Match> {
    let m = context;
    let mut out = Vec::new();
    match m.mtype {
        MatchType::Struct(_) | MatchType::Union(_) => {
            debug!(
                "got a struct or union, looking for fields and impl methods!! {}",
                m.matchstr,
            );
            if !only_methods {
                for m in search_struct_fields(searchstr, &m, search_type, session) {
                    out.push(m);
                }
            }
            for m in search_for_impl_methods(
                &m,
                searchstr,
                m.point,
                &m.filepath,
                m.local,
                search_type,
                session,
            ) {
                out.push(m);
            }
        }
        MatchType::Builtin(kind) => {
            if let Some(files) = kind.get_impl_files() {
                for file in files {
                    for m in search_for_impl_methods(
                        &m,
                        searchstr,
                        BytePos::ZERO,
                        &file,
                        false,
                        search_type,
                        session,
                    ) {
                        out.push(m);
                    }
                }
            }
        }
        MatchType::Enum(_) => {
            debug!("got an enum, looking for impl methods {}", m.matchstr);
            for m in search_for_impl_methods(
                &m,
                searchstr,
                m.point,
                &m.filepath,
                m.local,
                search_type,
                session,
            ) {
                out.push(m);
            }
        }
        MatchType::Trait => {
            debug!("got a trait, looking for methods {}", m.matchstr);
            out.extend(search_for_trait_methods(m, searchstr, search_type, session))
        }
        MatchType::TypeParameter(bounds) => {
            debug!("got a trait bound, looking for methods {}", m.matchstr);
            let traits = bounds.get_traits(session);
            traits.into_iter().for_each(|m| {
                out.extend(search_for_trait_methods(m, searchstr, search_type, session))
            });
        }
        _ => {
            debug!(
                "WARN!! context wasn't a Struct, Enum, Builtin or Trait {:?}",
                m
            );
        }
    };
    out
}

#[inline(always)]
fn search_for_trait_methods<'s, 'sess: 's>(
    traitm: Match,
    search_str: &'s str,
    search_type: SearchType,
    session: &'sess Session<'sess>,
) -> impl 's + Iterator<Item = Match> {
    search_for_trait_items(traitm, search_str, search_type, false, false, session)
}

// search trait items by search_str
fn search_for_trait_items<'s, 'sess: 's>(
    traitm: Match,
    search_str: &'s str,
    search_type: SearchType,
    includes_assoc_fn: bool,
    includes_assoc_ty_and_const: bool,
    session: &'sess Session<'sess>,
) -> impl 's + Iterator<Item = Match> {
    let traits = collect_inherited_traits(traitm, session);
    traits
        .into_iter()
        .filter_map(move |tr| {
            let src = session.load_source_file(&tr.filepath);
            src[tr.point.0..].find('{').map(|start| {
                search_scope_for_methods(
                    tr.point + BytePos(start + 1),
                    src.as_src(),
                    search_str,
                    &tr.filepath,
                    includes_assoc_fn,
                    includes_assoc_ty_and_const,
                    search_type,
                    session,
                )
            })
        })
        .flatten()
}

fn search_for_deref_matches(
    target_ty: Ty,      // target = ~
    type_match: &Match, // the type which implements Deref
    impl_header: &ImplHeader,
    fieldsearchstr: &str,
    session: &Session<'_>,
) -> Vec<Match> {
    match target_ty {
        Ty::PathSearch(ref paths) => {
            let ty = match get_assoc_type_from_header(&paths.path, type_match, impl_header, session)
            {
                Some(t) => t,
                None => return vec![],
            };
            get_field_matches_from_ty(ty, fieldsearchstr, SearchType::StartsWith, session)
        }
        _ => get_field_matches_from_ty(target_ty, fieldsearchstr, SearchType::StartsWith, session),
    }
}

pub(crate) fn get_field_matches_from_ty(
    ty: Ty,
    searchstr: &str,
    stype: SearchType,
    session: &Session<'_>,
) -> Vec<Match> {
    match ty {
        Ty::Match(m) => search_for_fields_and_methods(m, searchstr, stype, false, session),
        Ty::PathSearch(paths) => paths.resolve_as_match(session).map_or_else(Vec::new, |m| {
            search_for_fields_and_methods(m, searchstr, stype, false, session)
        }),
        Ty::Self_(scope) => {
            let msrc = session.load_source_file(&scope.filepath);
            let ty = typeinf::get_type_of_self(
                scope.point,
                &scope.filepath,
                true,
                msrc.as_src(),
                session,
            );
            match ty {
                Some(Ty::Match(m)) => {
                    search_for_fields_and_methods(m, searchstr, stype, false, session)
                }
                _ => Vec::new(),
            }
        }
        Ty::Tuple(v) => get_tuple_field_matches(v.len(), searchstr, stype, session).collect(),
        Ty::RefPtr(ty, _) => {
            // TODO(kngwyu): support impl &Type {..}
            get_field_matches_from_ty(*ty, searchstr, stype, session)
        }
        Ty::Array(_, _) | Ty::Slice(_) => {
            let mut m = primitive::PrimKind::Slice.to_module_match().unwrap();
            m.matchstr = "[T]".to_owned();
            search_for_fields_and_methods(m, searchstr, stype, false, session)
        }
        Ty::TraitObject(traitbounds) => traitbounds
            .into_iter()
            .flat_map(|ps| get_field_matches_from_ty(Ty::PathSearch(ps), searchstr, stype, session))
            .collect(),
        Ty::Future(_, scope) => get_future(scope, session)
            .into_iter()
            .flat_map(|f| search_for_trait_methods(f, searchstr, stype, session))
            .chain(
                txt_matches_with_pos(stype, searchstr, "await")
                    .and_then(|_| PrimKind::Await.to_doc_match(session))
                    .into_iter(),
            )
            .collect(),
        _ => vec![],
    }
}

fn get_future(scope: Scope, session: &Session<'_>) -> Option<Match> {
    let path = RacerPath::from_iter(
        false,
        ["std", "future", "Future"].iter().map(|s| s.to_string()),
    );

    ast::find_type_match(&path, &scope.filepath, scope.point, session)
}

fn get_assoc_type_from_header(
    target_path: &RacerPath, // type target = ~
    type_match: &Match,      // the type which implements trait
    impl_header: &ImplHeader,
    session: &Session<'_>,
) -> Option<Ty> {
    debug!(
        "[search_for_deref_matches] target: {:?} impl: {:?}",
        target_path, impl_header
    );
    if let Some((pos, _)) = impl_header.generics().search_param_by_path(target_path) {
        type_match
            .resolved_generics()
            .nth(pos)
            .map(|x| x.to_owned())
    } else {
        resolve_path_with_primitive(
            &target_path,
            impl_header.file_path(),
            BytePos::ZERO,
            SearchType::ExactMatch,
            Namespace::Type,
            session,
        )
        .into_iter()
        .next()
        .map(Ty::Match)
    }
}

fn get_std_macros(
    searchstr: &str,
    search_type: SearchType,
    session: &Session<'_>,
    out: &mut Vec<Match>,
) {
    let std_path = if let Some(ref p) = *RUST_SRC_PATH {
        p
    } else {
        return;
    };
    let searchstr = if searchstr.ends_with("!") {
        let len = searchstr.len();
        &searchstr[..len - 1]
    } else {
        searchstr
    };
    for macro_file in &[
        "std/src/macros.rs",
        "core/src/macros.rs",
        "core/src/macros/mod.rs",
        "alloc/src/macros.rs",
    ] {
        let macro_path = std_path.join(macro_file);
        if !macro_path.exists() {
            continue;
        }
        get_std_macros_(
            &macro_path,
            searchstr,
            macro_file == &"core/src/macros.rs",
            search_type,
            session,
            out,
        );
    }
}

fn get_std_macros_(
    macro_path: &Path,
    searchstr: &str,
    is_core: bool,
    search_type: SearchType,
    session: &Session<'_>,
    out: &mut Vec<Match>,
) {
    let raw_src = session.load_raw_file(&macro_path);
    let src = session.load_source_file(&macro_path);
    let mut export = false;
    let mut get_macro_def = |blob: &str| -> Option<(BytePos, String)> {
        if blob.starts_with("#[macro_export]") | blob.starts_with("#[rustc_doc_only_macro]") {
            export = true;
            return None;
        }
        if !export {
            return None;
        }
        if !blob.starts_with("macro_rules!") {
            return None;
        }
        export = false;
        let mut start = BytePos(12);
        for &b in blob[start.0..].as_bytes() {
            match b {
                b if util::is_whitespace_byte(b) => start = start.increment(),
                _ => break,
            }
        }
        if !blob[start.0..].starts_with(searchstr) {
            return None;
        }
        let end = find_ident_end(blob, start + BytePos(searchstr.len()));
        let mut matchstr = blob[start.0..end.0].to_owned();
        if search_type == SearchType::ExactMatch && searchstr != matchstr {
            return None;
        }
        matchstr.push_str("!");
        Some((start, matchstr))
    };
    let mut builtin_start = None;
    out.extend(src.as_src().iter_stmts().filter_map(|range| {
        let blob = &src[range.to_range()];
        // for builtin macros in libcore/macros.rs
        if is_core && blob.starts_with("mod builtin") {
            builtin_start = blob.find("#").map(|u| range.start + u.into());
        }
        let (offset, matchstr) = get_macro_def(blob)?;
        let start = range.start + offset;
        Some(Match {
            matchstr,
            filepath: macro_path.to_owned(),
            point: start,
            coords: raw_src.point_to_coords(start),
            local: false,
            mtype: MatchType::Macro,
            contextstr: matchers::first_line(blob),
            docs: matchers::find_doc(&raw_src, range.start),
        })
    }));
    if let Some(builtin_start) = builtin_start {
        let mod_src = src.get_src_from_start(builtin_start);
        out.extend(mod_src.iter_stmts().filter_map(|range| {
            let blob = &mod_src[range.to_range()];
            let (offset, matchstr) = get_macro_def(blob)?;
            let start = builtin_start + range.start + offset;
            Some(Match {
                matchstr,
                filepath: macro_path.to_owned(),
                point: start,
                coords: raw_src.point_to_coords(start),
                local: false,
                mtype: MatchType::Macro,
                contextstr: matchers::first_line(blob),
                docs: matchers::find_doc(&raw_src, range.start),
            })
        }));
    }
}

pub(crate) fn get_iter_item(selfm: &Match, session: &Session<'_>) -> Option<Ty> {
    let iter_header = search_trait_impls(
        selfm.point,
        &selfm.matchstr,
        &["IntoIterator", "Iterator"],
        true,
        &selfm.filepath,
        selfm.local,
        session,
    )
    .into_iter()
    .next()?;
    let item = search_scope_for_impled_assoc_types(
        &iter_header,
        "Item",
        core::SearchType::ExactMatch,
        session,
    );
    item.into_iter()
        .next()
        .and_then(|(_, item_ty)| match item_ty {
            Ty::PathSearch(paths) => {
                get_assoc_type_from_header(&paths.path, selfm, &iter_header, session)
            }
            _ => Some(item_ty),
        })
}

pub(crate) fn get_tuple_field_matches<'a, 'b: 'a>(
    fields: usize,
    search_str: &'a str,
    search_type: SearchType,
    session: &'b Session<'_>,
) -> impl 'a + Iterator<Item = Match> {
    util::gen_tuple_fields(fields).filter_map(move |field| {
        if txt_matches(search_type, search_str, field) {
            primitive::PrimKind::Tuple
                .to_doc_match(session)
                .map(|mut m| {
                    m.matchstr = field.to_owned();
                    m.mtype = MatchType::StructField;
                    m
                })
        } else {
            None
        }
    })
}

pub(crate) fn get_index_output(selfm: &Match, session: &Session<'_>) -> Option<Ty> {
    // short cut
    if selfm.matchstr == "Vec" {
        return selfm.resolved_generics().next().map(|ty| ty.to_owned());
    }
    let index_header = search_trait_impls(
        selfm.point,
        &selfm.matchstr,
        &["Index"],
        true,
        &selfm.filepath,
        selfm.local,
        session,
    )
    .into_iter()
    .next()?;
    get_associated_type_match(&index_header, "Output", selfm, session)
}

pub(crate) fn get_associated_type_match(
    impl_header: &ImplHeader,
    type_name: &str,
    context: &Match,
    session: &Session<'_>,
) -> Option<Ty> {
    let output = search_scope_for_impled_assoc_types(
        impl_header,
        type_name,
        core::SearchType::ExactMatch,
        session,
    );
    output
        .into_iter()
        .next()
        .and_then(|(_, item_ty)| match item_ty {
            Ty::PathSearch(paths) => {
                get_assoc_type_from_header(&paths.path, context, impl_header, session)
            }
            _ => Some(item_ty),
        })
}

pub(crate) fn get_struct_fields(
    path: &RacerPath,
    search_str: &str,
    filepath: &Path,
    complete_pos: BytePos,
    stype: SearchType,
    session: &Session<'_>,
) -> Vec<Match> {
    resolve_path(
        &path,
        filepath,
        complete_pos,
        SearchType::ExactMatch,
        Namespace::HasField,
        session,
        &ImportInfo::default(),
    )
    .into_iter()
    .next()
    .map_or_else(
        || Vec::new(),
        |m| match m.mtype {
            MatchType::Struct(_) | MatchType::EnumVariant(_) => {
                search_struct_fields(search_str, &m, stype, session)
            }
            MatchType::Type => {
                let m = try_vec!(typeinf::get_type_of_typedef(&m, session));
                search_struct_fields(search_str, &m, stype, session)
            }
            MatchType::UseAlias(m) => search_struct_fields(search_str, &*m, stype, session),
            _ => Vec::new(),
        },
    )
}

/// Checks if trait_impl is the impl TraitName<OtherType>
fn has_impl_for_other_type(
    trait_impl: &ImplHeader,
    trait_name: &str,
    other_type: Option<&str>,
) -> bool {
    if let Some(ref path) = trait_impl.trait_path() {
        if path.name() == Some(trait_name) {
            if other_type.is_none() && path.segments[0].generics.len() == 0 {
                return true;
            }
            if let Some(ty) = path.segments[0].generics.get(0) {
                return match ty.to_owned().dereference() {
                    // TODO: Handle generics arguments
                    Ty::PathSearch(ref g) => other_type == g.path.name(),
                    _ => false,
                };
            }
            // default is self
            return trait_impl.self_path().name() == other_type;
        }
    }
    false
}

/// Resolves the type of a binary expression
/// # Arguments
/// * base_type: the type on the left hand side
/// * node: the operator
/// * other_type: the type on the right hand side
pub(crate) fn resolve_binary_expr_type(
    base_type: &Match,
    node: BinOpKind,
    other_type: Option<&str>,
    session: &Session<'_>,
) -> Option<Ty> {
    let trait_name = typeinf::get_operator_trait(node);
    if trait_name == "bool" {
        return PrimKind::Bool.to_module_match().map(Ty::Match);
    }

    let matching_impl = search_trait_impls(
        base_type.point,
        &base_type.matchstr,
        &[trait_name],
        false,
        &base_type.filepath,
        base_type.local,
        session,
    )
    .into_iter()
    .filter(|trait_impl| has_impl_for_other_type(trait_impl, trait_name, other_type))
    .next();
    if let Some(matching_impl) = matching_impl {
        get_associated_type_match(&matching_impl, "Output", &base_type, session)
            .or_else(|| Some(Ty::Match(base_type.clone())))
    } else {
        // default to base type if an impl can't be found
        Some(Ty::Match(base_type.clone()))
    }
}
