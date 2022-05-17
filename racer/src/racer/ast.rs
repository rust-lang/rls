use crate::ast_types::Path as RacerPath;
use crate::ast_types::{
    self, GenericsArgs, ImplHeader, Pat, PathAlias, PathAliasKind, TraitBounds, Ty,
};
use crate::core::{self, BytePos, ByteRange, Match, MatchType, Scope, Session, SessionExt};
use crate::nameres;
use crate::typeinf;

use std::path::Path;

use rustc_ast::ast::{self, ExprKind, FnRetTy, ItemKind, PatKind, UseTree, UseTreeKind};
use rustc_ast::{self, visit};
use rustc_data_structures::sync::Lrc;
use rustc_errors::emitter::Emitter;
use rustc_errors::{Diagnostic, Handler};
use rustc_parse::new_parser_from_source_str;
use rustc_parse::parser::{ForceCollect, Parser};
use rustc_session::parse::ParseSess;
use rustc_span::edition::Edition;
use rustc_span::source_map::{self, FileName, SourceMap};
use rustc_span::Span;

struct DummyEmitter;

impl Emitter for DummyEmitter {
    fn emit_diagnostic(&mut self, _db: &Diagnostic) {}
    fn source_map(&self) -> Option<&Lrc<SourceMap>> {
        None
    }
    fn should_show_explain(&self) -> bool {
        false
    }
    fn fluent_bundle(&self) -> Option<&Lrc<rustc_errors::FluentBundle>> {
        None
    }
    fn fallback_fluent_bundle(&self) -> &rustc_errors::FluentBundle {
        unimplemented!("diagnostic translations are unimplemented in racer");
    }
}

/// construct parser from string
// From syntax/util/parser_testing.rs
pub fn string_to_parser(ps: &ParseSess, source_str: String) -> Parser<'_> {
    new_parser_from_source_str(ps, FileName::Custom("racer-file".to_owned()), source_str)
}

/// Get parser from string s and then apply closure f to it
// TODO: use Result insated of Option
pub fn with_error_checking_parse<F, T>(s: String, f: F) -> Option<T>
where
    F: FnOnce(&mut Parser<'_>) -> Option<T>,
{
    // FIXME: Set correct edition based on the edition of the target crate.
    rustc_span::create_session_if_not_set_then(Edition::Edition2018, |_| {
        let codemap = Lrc::new(SourceMap::new(source_map::FilePathMapping::empty()));
        // We use DummyEmitter here not to print error messages to stderr
        let handler = Handler::with_emitter(false, None, Box::new(DummyEmitter {}));
        let parse_sess = ParseSess::with_span_handler(handler, codemap);

        let mut p = string_to_parser(&parse_sess, s);
        f(&mut p)
    })
}

/// parse string source_str as statement and then apply f to it
/// return false if we can't parse s as statement
// TODO: make F FnOnce(&ast::Stmt) -> Result<Something, Error>
pub fn with_stmt<F>(source_str: String, f: F) -> bool
where
    F: FnOnce(&ast::Stmt),
{
    with_error_checking_parse(source_str, |p| {
        let stmt = match p.parse_stmt(ForceCollect::No) {
            Ok(Some(stmt)) => stmt,
            _ => return None,
        };
        f(&stmt);
        Some(())
    })
    .is_some()
}

pub(crate) fn destruct_span(span: Span) -> (u32, u32) {
    let source_map::BytePos(lo) = span.lo();
    let source_map::BytePos(hi) = span.hi();
    (lo, hi)
}

pub(crate) fn get_span_start(span: Span) -> u32 {
    let source_map::BytePos(lo) = span.lo();
    lo
}

/// collect paths from syntax::ast::UseTree
#[derive(Debug)]
pub struct UseVisitor {
    pub path_list: Vec<PathAlias>,
    pub contains_glob: bool,
}

impl<'ast> visit::Visitor<'ast> for UseVisitor {
    fn visit_item(&mut self, i: &ast::Item) {
        // collect items from use tree recursively
        // returns (Paths, contains_glab)
        fn collect_nested_items(
            use_tree: &UseTree,
            parent_path: Option<&ast_types::Path>,
        ) -> (Vec<PathAlias>, bool) {
            let mut res = vec![];
            let mut path = if let Some(parent) = parent_path {
                let relative_path = RacerPath::from_ast_nogen(&use_tree.prefix);
                let mut path = parent.clone();
                path.extend(relative_path);
                path
            } else {
                RacerPath::from_ast_nogen(&use_tree.prefix)
            };
            let mut contains_glob = false;
            match use_tree.kind {
                UseTreeKind::Simple(rename, _, _) => {
                    let ident = use_tree.ident().name.to_string();
                    let rename_pos: Option<BytePos> =
                        rename.map(|id| destruct_span(id.span).0.into());
                    let kind = if let Some(last_seg) = path.segments.last() {
                        //` self` is treated normaly in libsyntax,
                        //  but we distinguish it here to make completion easy
                        if last_seg.name == "self" {
                            PathAliasKind::Self_(ident, rename_pos)
                        } else {
                            PathAliasKind::Ident(ident, rename_pos)
                        }
                    } else {
                        PathAliasKind::Ident(ident, rename_pos)
                    };
                    if let PathAliasKind::Self_(..) = kind {
                        path.segments.pop();
                    }
                    res.push(PathAlias {
                        kind,
                        path,
                        range: ByteRange::from(use_tree.span),
                    });
                }
                UseTreeKind::Nested(ref nested) => {
                    nested.iter().for_each(|(ref tree, _)| {
                        let (items, has_glob) = collect_nested_items(tree, Some(&path));
                        res.extend(items);
                        contains_glob |= has_glob;
                    });
                }
                UseTreeKind::Glob => {
                    res.push(PathAlias {
                        kind: PathAliasKind::Glob,
                        path,
                        range: ByteRange::from(use_tree.span),
                    });
                    contains_glob = true;
                }
            }
            (res, contains_glob)
        }
        if let ItemKind::Use(ref use_tree) = i.kind {
            let (path_list, contains_glob) = collect_nested_items(use_tree, None);
            self.path_list = path_list;
            self.contains_glob = contains_glob;
        }
    }
}

pub struct PatBindVisitor {
    ident_points: Vec<ByteRange>,
}

impl<'ast> visit::Visitor<'ast> for PatBindVisitor {
    fn visit_local(&mut self, local: &ast::Local) {
        // don't visit the RHS (init) side of the let stmt
        self.visit_pat(&local.pat);
    }

    fn visit_expr(&mut self, ex: &ast::Expr) {
        // don't visit the RHS or block of an 'if let' or 'for' stmt
        match &ex.kind {
            ExprKind::If(let_stmt, ..) | ExprKind::While(let_stmt, ..) => {
                if let ExprKind::Let(pat, ..) = &let_stmt.kind {
                    self.visit_pat(pat);
                }
            }
            ExprKind::ForLoop(pat, ..) => self.visit_pat(pat),
            _ => visit::walk_expr(self, ex),
        }
    }

    fn visit_pat(&mut self, p: &ast::Pat) {
        match p.kind {
            PatKind::Ident(_, ref spannedident, _) => {
                self.ident_points.push(spannedident.span.into());
            }
            _ => {
                visit::walk_pat(self, p);
            }
        }
    }
}

pub struct PatVisitor {
    ident_points: Vec<ByteRange>,
}

impl<'ast> visit::Visitor<'ast> for PatVisitor {
    fn visit_pat(&mut self, p: &ast::Pat) {
        match p.kind {
            PatKind::Ident(_, ref spannedident, _) => {
                self.ident_points.push(spannedident.span.into());
            }
            _ => {
                visit::walk_pat(self, p);
            }
        }
    }
}

pub struct FnArgVisitor {
    idents: Vec<(Pat, Option<Ty>, ByteRange)>,
    generics: GenericsArgs,
    scope: Scope,
    offset: i32,
}

impl<'ast> visit::Visitor<'ast> for FnArgVisitor {
    fn visit_fn(&mut self, fk: visit::FnKind<'_>, _: source_map::Span, _: ast::NodeId) {
        let fd = match fk {
            visit::FnKind::Fn(_, _, ref fn_sig, _, _, _) => &*fn_sig.decl,
            visit::FnKind::Closure(ref fn_decl, _) => fn_decl,
        };
        debug!("[FnArgVisitor::visit_fn] inputs: {:?}", fd.inputs);
        self.idents = fd
            .inputs
            .iter()
            .map(|arg| {
                debug!("[FnArgTypeVisitor::visit_fn] type {:?} was found", arg.ty);
                let pat = Pat::from_ast(&arg.pat.kind, &self.scope);
                let ty = Ty::from_ast(&arg.ty, &self.scope);
                let source_map::BytePos(lo) = arg.pat.span.lo();
                let source_map::BytePos(hi) = arg.ty.span.hi();
                (pat, ty, ByteRange::new(lo, hi))
            })
            .collect();
    }
    fn visit_generics(&mut self, g: &'ast ast::Generics) {
        let generics = GenericsArgs::from_generics(g, &self.scope.filepath, self.offset);
        self.generics.extend(generics);
    }
}

fn point_is_in_span(point: BytePos, span: &Span) -> bool {
    let point: u32 = point.0 as u32;
    let (lo, hi) = destruct_span(*span);
    point >= lo && point < hi
}

// The point must point to an ident within the pattern.
fn destructure_pattern_to_ty(
    pat: &ast::Pat,
    point: BytePos,
    ty: &Ty,
    scope: &Scope,
    session: &Session<'_>,
) -> Option<Ty> {
    debug!(
        "destructure_pattern_to_ty point {:?} ty {:?} pat: {:?}",
        point, ty, pat.kind
    );
    match pat.kind {
        PatKind::Ident(_, ref spannedident, _) => {
            if point_is_in_span(point, &spannedident.span) {
                debug!("destructure_pattern_to_ty matched an ident!");
                Some(ty.clone())
            } else {
                panic!(
                    "Expecting the point to be in the patident span. pt: {:?}",
                    point
                );
            }
        }
        PatKind::Tuple(ref tuple_elements) => match *ty {
            Ty::Tuple(ref typeelems) => {
                for (i, p) in tuple_elements.iter().enumerate() {
                    if !point_is_in_span(point, &p.span) {
                        continue;
                    }
                    if let Some(ref ty) = typeelems[i] {
                        return destructure_pattern_to_ty(p, point, ty, scope, session);
                    }
                }
                None
            }
            _ => panic!("Expecting TyTuple"),
        },
        PatKind::TupleStruct(_, ref path, ref children) => {
            let m = resolve_ast_path(path, &scope.filepath, scope.point, session)?;
            let contextty = path_to_match(ty.clone(), session);
            for (i, p) in children.iter().enumerate() {
                if point_is_in_span(point, &p.span) {
                    return typeinf::get_tuplestruct_field_type(i, &m, session)
                        .and_then(|ty| {
                            // if context ty is a match, use its generics
                            if let Some(Ty::Match(ref contextm)) = contextty {
                                path_to_match_including_generics(
                                    ty,
                                    contextm.to_generics(),
                                    session,
                                )
                            } else {
                                path_to_match(ty, session)
                            }
                        })
                        .and_then(|ty| destructure_pattern_to_ty(p, point, &ty, scope, session));
                }
            }
            None
        }
        PatKind::Struct(_, ref path, ref children, _) => {
            let m = resolve_ast_path(path, &scope.filepath, scope.point, session)?;
            let contextty = path_to_match(ty.clone(), session);
            for child in children {
                if point_is_in_span(point, &child.span) {
                    return typeinf::get_struct_field_type(&child.ident.name.as_str(), &m, session)
                        .and_then(|ty| {
                            if let Some(Ty::Match(ref contextm)) = contextty {
                                path_to_match_including_generics(
                                    ty,
                                    contextm.to_generics(),
                                    session,
                                )
                            } else {
                                path_to_match(ty, session)
                            }
                        })
                        .and_then(|ty| {
                            destructure_pattern_to_ty(&child.pat, point, &ty, scope, session)
                        });
                }
            }
            None
        }
        _ => {
            debug!("Could not destructure pattern {:?}", pat);
            None
        }
    }
}

struct LetTypeVisitor<'c, 's> {
    scope: Scope,
    session: &'s Session<'c>,
    pos: BytePos, // pos is relative to the srctxt, scope is global
    result: Option<Ty>,
}

impl<'c, 's, 'ast> visit::Visitor<'ast> for LetTypeVisitor<'c, 's> {
    fn visit_local(&mut self, local: &ast::Local) {
        let ty = match &local.ty {
            Some(annon) => Ty::from_ast(&*annon, &self.scope),
            None => local.kind.init().as_ref().and_then(|initexpr| {
                debug!("[LetTypeVisitor] initexpr is {:?}", initexpr.kind);
                let mut v = ExprTypeVisitor::new(self.scope.clone(), self.session);
                v.visit_expr(initexpr);
                v.result
            }),
        };
        debug!("[LetTypeVisitor] ty is {:?}. pos is {:?}", ty, self.pos);
        self.result = ty
            .and_then(|ty| {
                destructure_pattern_to_ty(&local.pat, self.pos, &ty, &self.scope, self.session)
            })
            .and_then(|ty| path_to_match(ty, self.session));
    }
}

struct MatchTypeVisitor<'c, 's> {
    scope: Scope,
    session: &'s Session<'c>,
    pos: BytePos, // pos is relative to the srctxt, scope is global
    result: Option<Ty>,
}

impl<'c, 's, 'ast> visit::Visitor<'ast> for MatchTypeVisitor<'c, 's> {
    fn visit_expr(&mut self, ex: &ast::Expr) {
        if let ExprKind::Match(ref subexpression, ref arms) = ex.kind {
            debug!("PHIL sub expr is {:?}", subexpression);

            let mut v = ExprTypeVisitor::new(self.scope.clone(), self.session);
            v.visit_expr(subexpression);

            debug!("PHIL sub type is {:?}", v.result);

            for arm in arms {
                if !point_is_in_span(self.pos, &arm.pat.span) {
                    continue;
                }
                debug!("PHIL point is in pattern |{:?}|", arm.pat);
                self.result = v
                    .result
                    .as_ref()
                    .and_then(|ty| {
                        destructure_pattern_to_ty(&arm.pat, self.pos, ty, &self.scope, self.session)
                    })
                    .and_then(|ty| path_to_match(ty, self.session));
            }
        }
    }
}

fn resolve_ast_path(
    path: &ast::Path,
    filepath: &Path,
    pos: BytePos,
    session: &Session<'_>,
) -> Option<Match> {
    let scope = Scope::new(filepath.to_owned(), pos);
    let path = RacerPath::from_ast(path, &scope);
    nameres::resolve_path_with_primitive(
        &path,
        filepath,
        pos,
        core::SearchType::ExactMatch,
        core::Namespace::Path,
        session,
    )
    .into_iter()
    .nth(0)
}

fn path_to_match(ty: Ty, session: &Session<'_>) -> Option<Ty> {
    match ty {
        Ty::PathSearch(paths) => {
            find_type_match(&paths.path, &paths.filepath, paths.point, session).map(Ty::Match)
        }
        Ty::RefPtr(ty, _) => path_to_match(*ty, session),
        _ => Some(ty),
    }
}

pub(crate) fn find_type_match(
    path: &RacerPath,
    fpath: &Path,
    pos: BytePos,
    session: &Session<'_>,
) -> Option<Match> {
    debug!("find_type_match {:?}, {:?}", path, fpath);
    let mut res = nameres::resolve_path_with_primitive(
        path,
        fpath,
        pos,
        core::SearchType::ExactMatch,
        core::Namespace::Type,
        session,
    )
    .into_iter()
    .nth(0)
    .and_then(|m| match m.mtype {
        MatchType::Type => typeinf::get_type_of_typedef(&m, session),
        _ => Some(m),
    })?;
    // TODO: 'Type' support
    // if res is Enum/Struct and has a generic type paramter, let's resolve it.
    for (param, typ) in res.generics_mut().zip(path.generic_types()) {
        param.resolve(typ.to_owned());
    }
    Some(res)
}

struct ExprTypeVisitor<'c, 's> {
    scope: Scope,
    session: &'s Session<'c>,
    // what we have before calling typeinf::get_type_of_match
    path_match: Option<Match>,
    result: Option<Ty>,
}

impl<'c: 's, 's> ExprTypeVisitor<'c, 's> {
    fn new(scope: Scope, session: &'s Session<'c>) -> Self {
        ExprTypeVisitor {
            scope,
            session,
            path_match: None,
            result: None,
        }
    }
    fn same_scope(&self) -> Self {
        Self {
            scope: self.scope.clone(),
            session: self.session,
            path_match: None,
            result: None,
        }
    }
}

impl<'c, 's, 'ast> visit::Visitor<'ast> for ExprTypeVisitor<'c, 's> {
    fn visit_expr(&mut self, expr: &ast::Expr) {
        debug!(
            "ExprTypeVisitor::visit_expr {:?}(kind: {:?})",
            expr, expr.kind
        );
        //walk_expr(self, ex, e)
        match expr.kind {
            ExprKind::Unary(_, ref expr) | ExprKind::AddrOf(_, _, ref expr) => {
                self.visit_expr(expr);
            }
            ExprKind::Path(_, ref path) => {
                let source_map::BytePos(lo) = path.span.lo();
                self.result = resolve_ast_path(
                    path,
                    &self.scope.filepath,
                    self.scope.point + lo.into(),
                    self.session,
                )
                .and_then(|m| {
                    let msrc = self.session.load_source_file(&m.filepath);
                    self.path_match = Some(m.clone());
                    typeinf::get_type_of_match(m, msrc.as_src(), self.session)
                });
            }
            ExprKind::Call(ref callee_expression, ref caller_expr) => {
                self.visit_expr(callee_expression);
                self.result = self.result.take().and_then(|m| {
                    if let Ty::Match(mut m) = m {
                        match m.mtype {
                            MatchType::Function => {
                                typeinf::get_return_type_of_function(&m, &m, self.session)
                                    .and_then(|ty| path_to_match(ty, self.session))
                            }
                            MatchType::Method(ref gen) => {
                                let mut return_ty =
                                    typeinf::get_return_type_of_function(&m, &m, self.session);
                                // Account for already resolved generics if the return type is Self
                                // (in which case we return bare type as found in the `impl` header)
                                if let (Some(Ty::Match(ref mut m)), Some(gen)) =
                                    (&mut return_ty, gen)
                                {
                                    for (type_param, arg) in m.generics_mut().zip(gen.args()) {
                                        if let Some(resolved) = arg.resolved() {
                                            type_param.resolve(resolved.clone());
                                        }
                                    }
                                }
                                return_ty.and_then(|ty| {
                                    path_to_match_including_generics(
                                        ty,
                                        gen.as_ref().map(AsRef::as_ref),
                                        self.session,
                                    )
                                })
                            }
                            // if we find tuple struct / enum variant, try to resolve its generics name
                            MatchType::Struct(ref mut gen)
                            | MatchType::Enum(ref mut gen)
                            | MatchType::Union(ref mut gen) => {
                                if gen.is_empty() {
                                    return Some(Ty::Match(m));
                                }
                                let tuple_fields = match self.path_match {
                                    Some(ref m) => typeinf::get_tuplestruct_fields(m, self.session),
                                    None => return Some(Ty::Match(m)),
                                };
                                // search what is in callee e.g. Some(String::new()<-) for generics
                                for ((_, _, ty), expr) in tuple_fields.into_iter().zip(caller_expr)
                                {
                                    let ty = try_continue!(ty).dereference();
                                    if let Ty::PathSearch(paths) = ty {
                                        let (id, _) =
                                            try_continue!(gen.search_param_by_path(&paths.path));
                                        let mut visitor = self.same_scope();
                                        visitor.visit_expr(expr);
                                        if let Some(ty) = visitor.result {
                                            gen.0[id].resolve(ty.dereference());
                                        }
                                    }
                                }
                                Some(Ty::Match(m))
                            }
                            MatchType::TypeParameter(ref traitbounds)
                                if traitbounds.has_closure() =>
                            {
                                let mut output = None;
                                if let Some(path_search) = traitbounds.get_closure() {
                                    for seg in path_search.path.segments.iter() {
                                        if seg.output.is_some() {
                                            output = seg.output.clone();
                                            break;
                                        }
                                    }
                                }
                                output
                            }
                            _ => {
                                debug!(
                                    "ExprTypeVisitor: Cannot handle ExprCall of {:?} type",
                                    m.mtype
                                );
                                None
                            }
                        }
                    } else {
                        None
                    }
                });
            }
            ExprKind::Struct(ref struct_expr) => {
                let ast::StructExpr { ref path, .. } = **struct_expr;
                let pathvec = RacerPath::from_ast(path, &self.scope);
                self.result = find_type_match(
                    &pathvec,
                    &self.scope.filepath,
                    self.scope.point,
                    self.session,
                )
                .map(Ty::Match);
            }
            ExprKind::MethodCall(ref method_def, ref arguments, _) => {
                let methodname = method_def.ident.name.as_str();
                debug!("method call ast name {}", methodname);

                // arguments[0] is receiver(e.g. self)
                let objexpr = &arguments[0];
                self.visit_expr(objexpr);
                let result = self.result.take();
                let get_method_output_ty = |contextm: Match| {
                    let matching_methods = nameres::search_for_fields_and_methods(
                        contextm.clone(),
                        &methodname,
                        core::SearchType::ExactMatch,
                        true,
                        self.session,
                    );
                    matching_methods
                        .into_iter()
                        .filter_map(|method| {
                            let ty = typeinf::get_return_type_of_function(
                                &method,
                                &contextm,
                                self.session,
                            )?;
                            path_to_match_including_generics(
                                ty,
                                contextm.to_generics(),
                                self.session,
                            )
                        })
                        .nth(0)
                };
                self.result = result.and_then(|ty| {
                    ty.resolve_as_field_match(self.session)
                        .and_then(get_method_output_ty)
                });
            }
            ExprKind::Field(ref subexpression, spannedident) => {
                let fieldname = spannedident.name.to_string();
                debug!("exprfield {}", fieldname);
                self.visit_expr(subexpression);
                let result = self.result.take();
                let match_to_field_ty = |structm: Match| {
                    typeinf::get_struct_field_type(&fieldname, &structm, self.session).and_then(
                        |fieldtypepath| {
                            find_type_match_including_generics(
                                fieldtypepath,
                                &structm.filepath,
                                structm.point,
                                &structm,
                                self.session,
                            )
                        },
                    )
                };
                self.result = result.and_then(|ty| {
                    ty.resolve_as_field_match(self.session)
                        .and_then(match_to_field_ty)
                });
            }
            ExprKind::Tup(ref exprs) => {
                let mut v = Vec::new();
                for expr in exprs {
                    self.visit_expr(expr);
                    v.push(self.result.take());
                }
                self.result = Some(Ty::Tuple(v));
            }
            ExprKind::Lit(ref lit) => self.result = Ty::from_lit(lit),
            ExprKind::Try(ref expr) => {
                self.visit_expr(&expr);
                debug!("ExprKind::Try result: {:?} expr: {:?}", self.result, expr);
                self.result = if let Some(&Ty::Match(ref m)) = self.result.as_ref() {
                    // HACK for speed up (kngwyu)
                    // Yeah there're many corner cases but it'll work well in most cases
                    if m.matchstr == "Result" || m.matchstr == "Option" {
                        debug!("Option or Result: {:?}", m);
                        m.resolved_generics().next().map(|x| x.to_owned())
                    } else {
                        debug!("Unable to desugar Try expression; type was {:?}", m);
                        None
                    }
                } else {
                    None
                };
            }
            ExprKind::Match(_, ref arms) => {
                debug!("match expr");

                for arm in arms {
                    self.visit_expr(&arm.body);

                    // All match arms need to return the same result, so if we found a result
                    // we can end the search.
                    if self.result.is_some() {
                        break;
                    }
                }
            }
            ExprKind::If(_, ref block, ref else_block) => {
                debug!("if/iflet expr");
                if let Some(stmt) = block.stmts.last() {
                    visit::walk_stmt(self, stmt);
                }
                if self.result.is_some() {
                    return;
                }
                // if the block does not resolve to a type, try the else block
                if let Some(expr) = else_block {
                    self.visit_expr(expr);
                }
            }
            ExprKind::Block(ref block, ref _label) => {
                debug!("block expr");
                if let Some(stmt) = block.stmts.last() {
                    visit::walk_stmt(self, stmt);
                }
            }
            ExprKind::Index(ref body, ref _index) => {
                self.visit_expr(body);
                // TODO(kngwyu) now we don't have support for literal so don't parse index
                // but in the future, we should handle index's type
                self.result = self
                    .result
                    .take()
                    .and_then(|ty| typeinf::get_type_of_indexed_value(ty, self.session));
            }
            ExprKind::Array(ref exprs) => {
                for expr in exprs {
                    self.visit_expr(expr);
                    if self.result.is_some() {
                        self.result = self
                            .result
                            .take()
                            .map(|ty| Ty::Array(Box::new(ty), format!("{}", exprs.len())));
                        break;
                    }
                }
                if self.result.is_none() {
                    self.result = Some(Ty::Array(Box::new(Ty::Unsupported), String::new()));
                }
            }
            ExprKind::MacCall(ref m) => {
                if let Some(name) = m.path.segments.last().map(|seg| seg.ident) {
                    // use some ad-hoc rules
                    if name.as_str() == "vec" {
                        let path = RacerPath::from_iter(
                            true,
                            ["std", "vec", "Vec"].iter().map(|s| s.to_string()),
                        );
                        self.result = find_type_match(
                            &path,
                            &self.scope.filepath,
                            self.scope.point,
                            self.session,
                        )
                        .map(Ty::Match);
                    }
                }
            }
            ExprKind::Binary(bin, ref left, ref right) => {
                self.visit_expr(left);
                let type_match = match self.result.take() {
                    Some(Ty::Match(m)) => m,
                    Some(Ty::PathSearch(ps)) => match ps.resolve_as_match(self.session) {
                        Some(m) => m,
                        _ => {
                            return;
                        }
                    },
                    _ => {
                        return;
                    }
                };

                self.visit_expr(right);
                let right_expr_type = match self.result.take() {
                    Some(Ty::Match(m)) => Some(m.matchstr),
                    Some(Ty::PathSearch(ps)) => {
                        ps.resolve_as_match(self.session).map(|m| m.matchstr)
                    }
                    _ => None,
                };
                self.result = nameres::resolve_binary_expr_type(
                    &type_match,
                    bin.node,
                    right_expr_type.as_ref().map(|s| s.as_str()),
                    self.session,
                );
            }
            _ => {
                debug!("- Could not match expr node type: {:?}", expr.kind);
            }
        };
    }
    /// Just do nothing if we see a macro, but also prevent the panic! in the default impl.
    fn visit_mac_call(&mut self, _mac: &ast::MacCall) {}
}

// gets generics info from the context match
fn path_to_match_including_generics(
    mut ty: Ty,
    generics: Option<&GenericsArgs>,
    session: &Session<'_>,
) -> Option<Ty> {
    if let Some(gen) = generics {
        ty = ty.replace_by_generics(gen);
    }
    match ty {
        Ty::PathSearch(paths) => {
            let fieldtypepath = &paths.path;
            find_type_match(&fieldtypepath, &paths.filepath, paths.point, session).map(Ty::Match)
        }
        _ => Some(ty),
    }
}

fn find_type_match_including_generics(
    fieldtype: Ty,
    filepath: &Path,
    pos: BytePos,
    structm: &Match,
    session: &Session<'_>,
) -> Option<Ty> {
    assert_eq!(&structm.filepath, filepath);
    let fieldtypepath = match fieldtype {
        Ty::PathSearch(paths) => paths.path,
        Ty::RefPtr(ty, _) => match ty.dereference() {
            Ty::PathSearch(paths) => paths.path,
            Ty::Match(m) => return Some(Ty::Match(m)),
            _ => return None,
        },
        // already resolved
        Ty::Match(m) => return Some(Ty::Match(m)),
        _ => {
            return None;
        }
    };
    let generics = match &structm.mtype {
        MatchType::Struct(gen) => gen,
        _ => return None,
    };
    if fieldtypepath.segments.len() == 1 {
        // could be a generic arg! - try and resolve it
        if let Some((_, param)) = generics.search_param_by_path(&fieldtypepath) {
            if let Some(res) = param.resolved() {
                return Some(res.to_owned());
            }
            let mut m = param.to_owned().into_match();
            m.local = structm.local;
            return Some(Ty::Match(m));
        }
    }

    find_type_match(&fieldtypepath, filepath, pos, session).map(Ty::Match)
}

struct StructVisitor {
    pub scope: Scope,
    pub fields: Vec<(String, ByteRange, Option<Ty>)>,
}

impl<'ast> visit::Visitor<'ast> for StructVisitor {
    fn visit_variant_data(&mut self, struct_definition: &ast::VariantData) {
        for field in struct_definition.fields() {
            let ty = Ty::from_ast(&field.ty, &self.scope);
            let name = match field.ident {
                Some(ref ident) => ident.to_string(),
                // name unnamed field by its ordinal, since self.0 works
                None => format!("{}", self.fields.len()),
            };
            self.fields.push((name, field.span.into(), ty));
        }
    }
}

#[derive(Debug)]
pub struct TypeVisitor<'s> {
    pub name: Option<String>,
    pub type_: Option<Ty>,
    scope: &'s Scope,
}

impl<'ast, 's> visit::Visitor<'ast> for TypeVisitor<'s> {
    fn visit_item(&mut self, item: &ast::Item) {
        if let ItemKind::TyAlias(ref ty_kind) = item.kind {
            if let Some(ref ty) = ty_kind.ty {
                self.name = Some(item.ident.name.to_string());
                self.type_ = Ty::from_ast(&ty, self.scope);
                debug!("typevisitor type is {:?}", self.type_);
            }
        }
    }
}

pub struct TraitVisitor {
    pub name: Option<String>,
}

impl<'ast> visit::Visitor<'ast> for TraitVisitor {
    fn visit_item(&mut self, item: &ast::Item) {
        if let ItemKind::Trait(..) = item.kind {
            self.name = Some(item.ident.name.to_string());
        }
    }
}

#[derive(Debug)]
pub struct ImplVisitor<'p> {
    pub result: Option<ImplHeader>,
    filepath: &'p Path,
    offset: BytePos,
    block_start: BytePos, // the point { appears
    local: bool,
}

impl<'p> ImplVisitor<'p> {
    fn new(filepath: &'p Path, offset: BytePos, local: bool, block_start: BytePos) -> Self {
        ImplVisitor {
            result: None,
            filepath,
            offset,
            block_start,
            local,
        }
    }
}

impl<'ast, 'p> visit::Visitor<'ast> for ImplVisitor<'p> {
    fn visit_item(&mut self, item: &ast::Item) {
        if let ItemKind::Impl(ref impl_kind) = item.kind {
            let ast::Impl {
                ref generics,
                ref of_trait,
                ref self_ty,
                ..
            } = **impl_kind;
            let impl_start = self.offset + get_span_start(item.span).into();
            self.result = ImplHeader::new(
                generics,
                self.filepath,
                of_trait,
                self_ty,
                self.offset,
                self.local,
                impl_start,
                self.block_start,
            );
        }
    }
}

pub struct ExternCrateVisitor {
    pub name: Option<String>,
    pub realname: Option<String>,
}

impl<'ast> visit::Visitor<'ast> for ExternCrateVisitor {
    fn visit_item(&mut self, item: &ast::Item) {
        if let ItemKind::ExternCrate(ref optional_s) = item.kind {
            self.name = Some(item.ident.name.to_string());
            if let Some(ref istr) = *optional_s {
                self.realname = Some(istr.to_string());
            }
        }
    }
    fn visit_mac_call(&mut self, _mac: &ast::MacCall) {}
}

#[derive(Debug)]
struct GenericsVisitor<P> {
    result: GenericsArgs,
    filepath: P,
}

impl<'ast, P: AsRef<Path>> visit::Visitor<'ast> for GenericsVisitor<P> {
    fn visit_generics(&mut self, g: &ast::Generics) {
        let path = &self.filepath;
        if !self.result.0.is_empty() {
            warn!("[visit_generics] called for multiple generics!");
        }
        self.result.extend(GenericsArgs::from_generics(g, path, 0));
    }
}

pub struct EnumVisitor {
    pub name: String,
    pub values: Vec<(String, BytePos)>,
}

impl<'ast> visit::Visitor<'ast> for EnumVisitor {
    fn visit_item(&mut self, i: &ast::Item) {
        if let ItemKind::Enum(ref enum_definition, _) = i.kind {
            self.name = i.ident.name.to_string();
            let (point1, point2) = destruct_span(i.span);
            debug!("name point is {} {}", point1, point2);

            for variant in &enum_definition.variants {
                let source_map::BytePos(point) = variant.span.lo();
                self.values.push((variant.ident.to_string(), point.into()));
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct StaticVisitor {
    pub ty: Option<Ty>,
    pub is_mutable: bool,
    scope: Scope,
}

impl StaticVisitor {
    fn new(scope: Scope) -> Self {
        StaticVisitor {
            ty: None,
            is_mutable: false,
            scope,
        }
    }
}

impl<'ast> visit::Visitor<'ast> for StaticVisitor {
    fn visit_item(&mut self, i: &ast::Item) {
        match i.kind {
            ItemKind::Const(_, ref ty, ref _expr) => self.ty = Ty::from_ast(ty, &self.scope),
            ItemKind::Static(ref ty, m, ref _expr) => {
                self.is_mutable = m == ast::Mutability::Mut;
                self.ty = Ty::from_ast(ty, &self.scope);
            }
            _ => {}
        }
    }
}

pub fn parse_use(s: String) -> UseVisitor {
    let mut v = UseVisitor {
        path_list: Vec::new(),
        contains_glob: false,
    };

    // visit::walk_crate can be panic so we don't use it here
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}

pub fn parse_pat_bind_stmt(s: String) -> Vec<ByteRange> {
    let mut v = PatBindVisitor {
        ident_points: Vec::new(),
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.ident_points
}

pub fn parse_struct_fields(s: String, scope: Scope) -> Vec<(String, ByteRange, Option<Ty>)> {
    let mut v = StructVisitor {
        scope,
        fields: Vec::new(),
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.fields
}

pub fn parse_impl(
    s: String,
    path: &Path,
    offset: BytePos,
    local: bool,
    scope_start: BytePos,
) -> Option<ImplHeader> {
    let mut v = ImplVisitor::new(path, offset, local, scope_start);
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.result
}

pub fn parse_trait(s: String) -> TraitVisitor {
    let mut v = TraitVisitor { name: None };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}

/// parse traits and collect inherited traits as TraitBounds
pub fn parse_inherited_traits<P: AsRef<Path>>(
    s: String,
    filepath: P,
    offset: i32,
) -> Option<TraitBounds> {
    let mut v = InheritedTraitsVisitor {
        result: None,
        file_path: filepath,
        offset: offset,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.result
}

pub fn parse_generics(s: String, filepath: &Path) -> GenericsArgs {
    let mut v = GenericsVisitor {
        result: GenericsArgs::default(),
        filepath: filepath,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.result
}

pub fn parse_type<'s>(s: String, scope: &'s Scope) -> TypeVisitor<'s> {
    let mut v = TypeVisitor {
        name: None,
        type_: None,
        scope,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}

pub fn parse_fn_args_and_generics(
    s: String,
    scope: Scope,
    offset: i32,
) -> (Vec<(Pat, Option<Ty>, ByteRange)>, GenericsArgs) {
    let mut v = FnArgVisitor {
        idents: Vec::new(),
        generics: GenericsArgs::default(),
        scope,
        offset,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    (v.idents, v.generics)
}

pub fn parse_closure_args(s: String, scope: Scope) -> Vec<(Pat, Option<Ty>, ByteRange)> {
    let mut v = FnArgVisitor {
        idents: Vec::new(),
        generics: GenericsArgs::default(),
        scope,
        offset: 0,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.idents
}

pub fn parse_pat_idents(s: String) -> Vec<ByteRange> {
    let mut v = PatVisitor {
        ident_points: Vec::new(),
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    debug!("ident points are {:?}", v.ident_points);
    v.ident_points
}

pub fn parse_fn_output(s: String, scope: Scope) -> (Option<Ty>, bool) {
    let mut v = FnOutputVisitor::new(scope);
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    let FnOutputVisitor { ty, is_async, .. } = v;
    (ty, is_async)
}

pub fn parse_extern_crate(s: String) -> ExternCrateVisitor {
    let mut v = ExternCrateVisitor {
        name: None,
        realname: None,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}

pub fn parse_enum(s: String) -> EnumVisitor {
    let mut v = EnumVisitor {
        name: String::new(),
        values: Vec::new(),
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}

pub fn parse_static(s: String, scope: Scope) -> StaticVisitor {
    let mut v = StaticVisitor::new(scope);
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}

pub fn get_type_of(s: String, fpath: &Path, pos: BytePos, session: &Session<'_>) -> Option<Ty> {
    let startscope = Scope {
        filepath: fpath.to_path_buf(),
        point: pos,
    };

    let mut v = ExprTypeVisitor::new(startscope, session);

    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.result
}

// pos points to an ident in the lhs of the stmtstr
pub fn get_let_type(s: String, pos: BytePos, scope: Scope, session: &Session<'_>) -> Option<Ty> {
    let mut v = LetTypeVisitor {
        scope,
        session,
        pos,
        result: None,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.result
}

pub fn get_match_arm_type(
    s: String,
    pos: BytePos,
    scope: Scope,
    session: &Session<'_>,
) -> Option<Ty> {
    let mut v = MatchTypeVisitor {
        scope,
        session,
        pos,
        result: None,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v.result
}

pub struct FnOutputVisitor {
    scope: Scope,
    pub ty: Option<Ty>,
    pub is_async: bool,
}

impl FnOutputVisitor {
    pub(crate) fn new(scope: Scope) -> Self {
        FnOutputVisitor {
            scope,
            ty: None,
            is_async: false,
        }
    }
}

impl<'ast> visit::Visitor<'ast> for FnOutputVisitor {
    fn visit_fn(&mut self, kind: visit::FnKind<'_>, _: source_map::Span, _: ast::NodeId) {
        let fd = match kind {
            visit::FnKind::Fn(_, _, ref fn_sig, _, _, _) => &*fn_sig.decl,
            visit::FnKind::Closure(ref fn_decl, _) => fn_decl,
        };
        self.is_async = kind
            .header()
            .map(|header| header.asyncness.is_async())
            .unwrap_or(false);
        self.ty = match fd.output {
            FnRetTy::Ty(ref ty) => Ty::from_ast(ty, &self.scope),
            FnRetTy::Default(_) => Some(Ty::Default),
        };
    }
}

/// Visitor to collect Inherited Traits
pub struct InheritedTraitsVisitor<P> {
    /// search result(list of Inherited Traits)
    result: Option<TraitBounds>,
    /// the file trait appears
    file_path: P,
    /// thecode point 'trait' statement starts
    offset: i32,
}

impl<'ast, P> visit::Visitor<'ast> for InheritedTraitsVisitor<P>
where
    P: AsRef<Path>,
{
    fn visit_item(&mut self, item: &ast::Item) {
        if let ItemKind::Trait(ref trait_kind) = item.kind {
            self.result = Some(TraitBounds::from_generic_bounds(
                &trait_kind.bounds,
                &self.file_path,
                self.offset,
            ));
        }
    }
}

/// Visitor for for ~ in .. statement
pub(crate) struct ForStmtVisitor<'r, 's> {
    pub(crate) for_pat: Option<Pat>,
    pub(crate) in_expr: Option<Ty>,
    scope: Scope,
    session: &'r Session<'s>,
}

impl<'ast, 'r, 's> visit::Visitor<'ast> for ForStmtVisitor<'r, 's> {
    fn visit_expr(&mut self, ex: &'ast ast::Expr) {
        if let ExprKind::ForLoop(ref pat, ref expr, _, _) = ex.kind {
            let for_pat = Pat::from_ast(&pat.kind, &self.scope);
            let mut expr_visitor = ExprTypeVisitor::new(self.scope.clone(), self.session);
            expr_visitor.visit_expr(expr);
            self.in_expr = expr_visitor.result;
            self.for_pat = Some(for_pat);
        }
    }
}

pub(crate) fn parse_for_stmt<'r, 's: 'r>(
    s: String,
    scope: Scope,
    session: &'r Session<'s>,
) -> ForStmtVisitor<'r, 's> {
    let mut v = ForStmtVisitor {
        for_pat: None,
        in_expr: None,
        scope,
        session,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}

/// Visitor for if let / while let statement
pub(crate) struct IfLetVisitor<'r, 's> {
    pub(crate) let_pat: Option<Pat>,
    pub(crate) rh_expr: Option<Ty>,
    scope: Scope,
    session: &'r Session<'s>,
}

impl<'ast, 'r, 's> visit::Visitor<'ast> for IfLetVisitor<'r, 's> {
    fn visit_expr(&mut self, ex: &'ast ast::Expr) {
        match &ex.kind {
            ExprKind::If(let_stmt, ..) | ExprKind::While(let_stmt, ..) => {
                if let ExprKind::Let(pat, expr, _span) = &let_stmt.kind {
                    self.let_pat = Some(Pat::from_ast(&pat.kind, &self.scope));
                    let mut expr_visitor = ExprTypeVisitor::new(self.scope.clone(), self.session);
                    expr_visitor.visit_expr(expr);
                    self.rh_expr = expr_visitor.result;
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn parse_if_let<'r, 's: 'r>(
    s: String,
    scope: Scope,
    session: &'r Session<'s>,
) -> IfLetVisitor<'r, 's> {
    let mut v = IfLetVisitor {
        let_pat: None,
        rh_expr: None,
        scope,
        session,
    };
    with_stmt(s, |stmt| visit::walk_stmt(&mut v, stmt));
    v
}
