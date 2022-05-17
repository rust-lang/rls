//! type conversion between racer types and libsyntax types
use super::ast::find_type_match;
use crate::core::{
    self, BytePos, ByteRange, Match, MatchType, Scope, SearchType, Session, SessionExt,
};
use crate::matchers::ImportInfo;
use crate::nameres;
use crate::primitive;
use crate::primitive::PrimKind;
use crate::typeinf;
use crate::util;
use rustc_ast::ast::{
    self, GenericBound, GenericBounds, GenericParamKind, LitKind, PatKind, TraitRef, TyKind,
    WherePredicate,
};
use rustc_ast_pretty::pprust;
use rustc_span::source_map;
use std::fmt;
use std::path::{Path as FilePath, PathBuf};
// we can only re-export types without thread-local interned string
pub use rustc_ast::ast::{BindingMode, Mutability};

/// The leaf of a `use` statement.
#[derive(Clone, Debug)]
pub struct PathAlias {
    /// the leaf of Use Tree
    /// it can be one of one of 3 types, e.g.
    /// use std::collections::{self, hashmap::*, HashMap};
    pub kind: PathAliasKind,
    /// The path.
    pub path: Path,
    /// range of item
    pub range: ByteRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathAliasKind {
    Ident(String, Option<BytePos>),
    Self_(String, Option<BytePos>),
    Glob,
}

impl AsRef<Path> for PathAlias {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

// Represents a type. Equivilent to rustc's ast::Ty but can be passed across threads
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Match(Match),
    PathSearch(PathSearch), // A path + the scope to be able to resolve it
    Tuple(Vec<Option<Ty>>),
    Array(Box<Ty>, String), // ty, length expr as string
    RefPtr(Box<Ty>, Mutability),
    Slice(Box<Ty>),
    Ptr(Box<Ty>, Mutability),
    TraitObject(TraitBounds),
    Self_(Scope),
    Future(Box<Ty>, Scope),
    Never,
    Default,
    Unsupported,
}

impl Ty {
    pub(crate) fn replace_by_resolved_generics(self, gen: &GenericsArgs) -> Self {
        let (ty, deref_cnt) = self.deref_with_count(0);
        if let Ty::PathSearch(ref paths) = ty {
            if let Some((_, param)) = gen.search_param_by_path(&paths.path) {
                if let Some(resolved) = param.resolved() {
                    return resolved.to_owned().wrap_by_ref(deref_cnt);
                }
            }
        }
        ty.wrap_by_ref(deref_cnt)
    }
    pub(crate) fn replace_by_generics(self, gen: &GenericsArgs) -> Self {
        let (mut ty, deref_cnt) = self.deref_with_count(0);
        if let Ty::PathSearch(ref mut paths) = ty {
            if let Some((_, param)) = gen.search_param_by_path(&paths.path) {
                if let Some(resolved) = param.resolved() {
                    return resolved.to_owned().wrap_by_ref(deref_cnt);
                } else {
                    return Ty::Match(param.clone().into_match());
                }
            } else {
                paths.path.replace_by_bounds(gen);
            }
        }
        ty.wrap_by_ref(deref_cnt)
    }
    pub(crate) fn dereference(self) -> Self {
        if let Ty::RefPtr(ty, _) = self {
            ty.dereference()
        } else {
            self
        }
    }
    fn wrap_by_ref(self, count: usize) -> Self {
        let mut ty = self;
        // TODO: it's incorrect
        for _ in 0..count {
            ty = Ty::RefPtr(Box::new(ty), Mutability::Not);
        }
        ty
    }
    fn deref_with_count(self, count: usize) -> (Self, usize) {
        if let Ty::RefPtr(ty, _) = self {
            ty.deref_with_count(count + 1)
        } else {
            (self, count)
        }
    }
    pub(crate) fn from_ast(ty: &ast::Ty, scope: &Scope) -> Option<Ty> {
        match ty.kind {
            TyKind::Tup(ref items) => Some(Ty::Tuple(
                items.into_iter().map(|t| Ty::from_ast(t, scope)).collect(),
            )),
            TyKind::Rptr(ref _lifetime, ref ty) => {
                Ty::from_ast(&ty.ty, scope).map(|ref_ty| Ty::RefPtr(Box::new(ref_ty), ty.mutbl))
            }
            TyKind::Path(_, ref path) => Some(Ty::PathSearch(PathSearch {
                path: Path::from_ast(path, scope),
                filepath: scope.filepath.clone(),
                point: scope.point,
            })),
            TyKind::Array(ref ty, ref expr) => Ty::from_ast(ty, scope)
                .map(|racer_ty| Ty::Array(Box::new(racer_ty), pprust::expr_to_string(&expr.value))),
            TyKind::Slice(ref ty) => {
                Ty::from_ast(ty, scope).map(|ref_ty| Ty::Slice(Box::new(ref_ty)))
            }
            TyKind::Ptr(ref ty) => {
                Ty::from_ast(&*ty.ty, scope).map(|rty| Ty::Ptr(Box::new(rty), ty.mutbl))
            }
            TyKind::Never => None,
            TyKind::TraitObject(ref traits, _) | TyKind::ImplTrait(_, ref traits) => {
                Some(Ty::TraitObject(TraitBounds::from_generic_bounds(
                    &traits,
                    scope.filepath.clone(),
                    scope.point.0 as i32,
                )))
            }
            TyKind::ImplicitSelf => Some(Ty::Self_(scope.clone())),
            _ => {
                trace!("unhandled Ty node: {:?}", ty.kind);
                None
            }
        }
    }

    pub(crate) fn from_lit(lit: &ast::Lit) -> Option<Ty> {
        let make_match = |kind: PrimKind| kind.to_module_match().map(Ty::Match);
        match lit.kind {
            LitKind::Str(_, _) => make_match(PrimKind::Str),
            LitKind::ByteStr(ref bytes) => make_match(PrimKind::U8)
                .map(|ty| Ty::Array(Box::new(ty), format!("{}", bytes.len()))),
            LitKind::Byte(_) => make_match(PrimKind::U8),
            LitKind::Char(_) => make_match(PrimKind::Char),
            LitKind::Int(_, int_ty) => make_match(PrimKind::from_litint(int_ty)),
            LitKind::Float(_, ast::LitFloatType::Unsuffixed) => make_match(PrimKind::F32),
            LitKind::Float(_, ast::LitFloatType::Suffixed(float_ty)) => match float_ty {
                ast::FloatTy::F32 => make_match(PrimKind::F32),
                ast::FloatTy::F64 => make_match(PrimKind::F64),
            },
            LitKind::Bool(_) => make_match(PrimKind::Bool),
            LitKind::Err(_) => None,
        }
    }
    fn resolve_common(self, session: &Session<'_>) -> Option<Match> {
        match self {
            Ty::Match(m) => Some(m),
            Ty::PathSearch(paths) => {
                find_type_match(&paths.path, &paths.filepath, paths.point, session)
            }
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
                    Some(Ty::Match(m)) => Some(m),
                    _ => None,
                }
            }
            _ => None,
        }
    }
    pub(crate) fn resolve_as_field_match(self, session: &Session<'_>) -> Option<Match> {
        match self {
            Ty::RefPtr(ty, _) => ty.resolve_as_field_match(session),
            Ty::Array(_, _) | Ty::Slice(_) => primitive::PrimKind::Slice.to_module_match(),
            _ => self.resolve_common(session),
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Ty::Match(ref m) => write!(f, "{}", m.matchstr),
            Ty::PathSearch(ref p) => write!(f, "{}", p.path),
            Ty::Tuple(ref vec) => {
                write!(f, "(")?;
                for (i, field) in vec.iter().enumerate() {
                    if i != 0 {
                        write!(f, ", ")?;
                    }
                    if let Some(field) = field {
                        write!(f, "{}", field)?;
                    } else {
                        write!(f, "UNKNOWN")?;
                    }
                }
                write!(f, ")")
            }
            Ty::Array(ref ty, ref expr) => {
                write!(f, "[")?;
                write!(f, "{}", ty)?;
                write!(f, "; ")?;
                write!(f, "{}", expr)?;
                write!(f, "]")
            }
            Ty::Slice(ref ty) => {
                write!(f, "[")?;
                write!(f, "{}", ty)?;
                write!(f, "]")
            }
            Ty::RefPtr(ref ty, mutab) => match mutab {
                Mutability::Not => write!(f, "&{}", ty),
                Mutability::Mut => write!(f, "&mut {}", ty),
            },
            Ty::Ptr(ref ty, mutab) => match mutab {
                Mutability::Not => write!(f, "*const {}", ty),
                Mutability::Mut => write!(f, "*mut {}", ty),
            },
            Ty::TraitObject(ref bounds) => {
                write!(f, "<")?;
                let last = bounds.len() - 1;
                for (i, ps) in bounds.iter().enumerate() {
                    if i == last {
                        write!(f, "{}", ps.path)?;
                    } else {
                        write!(f, "{},", ps.path)?;
                    }
                }
                write!(f, ">")
            }
            Ty::Self_(_) => write!(f, "Self"),
            Ty::Future(ref ty, _) => write!(f, "impl Future<Output={}>", ty),
            Ty::Never => write!(f, "!"),
            Ty::Default => write!(f, "()"),
            Ty::Unsupported => write!(f, "_"),
        }
    }
}

/// Compatible type for syntax::ast::PatKind
/// but currently doesn't support all kinds
#[derive(Clone, Debug, PartialEq)]
pub enum Pat {
    Wild,
    Ident(BindingMode, String),
    Struct(Path, Vec<FieldPat>),
    TupleStruct(Path, Vec<Pat>),
    Path(Path),
    Tuple(Vec<Pat>),
    Box,
    Ref(Box<Pat>, Mutability),
    Lit,
    Range,
    Slice,
    Mac,
    Rest,
    Or,
}

impl Pat {
    pub(crate) fn search_by_name(&self, sname: &str, stype: SearchType) -> Option<String> {
        match self {
            Pat::Wild => None,
            Pat::Ident(_, name) => {
                if util::symbol_matches(stype, sname, name) {
                    Some(name.clone())
                } else {
                    None
                }
            }
            Pat::Struct(_, pats) => pats
                .iter()
                .filter_map(|pat| pat.pat.search_by_name(sname, stype))
                .next(),
            Pat::TupleStruct(_, pats) | Pat::Tuple(pats) => pats
                .iter()
                .filter_map(|pat| pat.search_by_name(sname, stype))
                .next(),
            Pat::Ref(pat, _) => pat.search_by_name(sname, stype),
            _ => None,
        }
    }
    pub(crate) fn from_ast(pat: &PatKind, scope: &Scope) -> Self {
        match pat {
            PatKind::Wild => Pat::Wild,
            PatKind::Ident(bi, ident, _) => Pat::Ident(*bi, ident.to_string()),
            PatKind::Struct(_, path, fields, _) => {
                let path = Path::from_ast(path, scope);
                let fields = fields
                    .iter()
                    .map(|fld| FieldPat::from_ast(&fld, scope))
                    .collect();
                Pat::Struct(path, fields)
            }
            PatKind::TupleStruct(_, path, pats) => {
                let path = Path::from_ast(path, scope);
                let pats = pats
                    .iter()
                    .map(|pat| Pat::from_ast(&pat.kind, scope))
                    .collect();
                Pat::TupleStruct(path, pats)
            }
            PatKind::Path(_, path) => Pat::Path(Path::from_ast(&path, scope)),
            PatKind::Tuple(pats) => {
                let pats = pats
                    .iter()
                    .map(|pat| Pat::from_ast(&pat.kind, scope))
                    .collect();
                Pat::Tuple(pats)
            }
            PatKind::Box(_) => Pat::Box,
            PatKind::Ref(pat, mut_) => Pat::Ref(Box::new(Pat::from_ast(&pat.kind, scope)), *mut_),
            PatKind::Lit(_) => Pat::Lit,
            PatKind::Range(..) => Pat::Range,
            PatKind::Slice(..) => Pat::Slice,
            // ignore paren
            PatKind::Paren(pat) => Pat::from_ast(&pat.kind, scope),
            PatKind::MacCall(_) => Pat::Mac,
            PatKind::Rest => Pat::Rest,
            PatKind::Or(_) => Pat::Or,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldPat {
    pub field_name: String,
    pub pat: Box<Pat>,
}

impl FieldPat {
    pub fn from_ast(fpat: &ast::PatField, scope: &Scope) -> Self {
        FieldPat {
            field_name: fpat.ident.to_string(),
            pat: Box::new(Pat::from_ast(&fpat.pat.kind, scope)),
        }
    }
}

/// Prefix of path.
/// e.g. for path `::std` => Global
///      for path `self::abc` => Self_
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathPrefix {
    Crate,
    Super,
    Self_,
    Global,
}

impl PathPrefix {
    pub(crate) fn from_str(s: &str) -> Option<PathPrefix> {
        match s {
            "crate" => Some(PathPrefix::Crate),
            "super" => Some(PathPrefix::Super),
            "self" => Some(PathPrefix::Self_),
            "{{root}}" => Some(PathPrefix::Global),
            _ => None,
        }
    }
}

// The racer implementation of an ast::Path. Difference is that it is Send-able
#[derive(Clone, PartialEq)]
pub struct Path {
    pub prefix: Option<PathPrefix>,
    pub segments: Vec<PathSegment>,
}

impl Path {
    pub(crate) fn replace_by_bounds(&mut self, gen: &GenericsArgs) {
        self.segments.iter_mut().for_each(|segment| {
            segment
                .generics
                .iter_mut()
                .for_each(|generics| match generics {
                    Ty::PathSearch(ref mut ps) => {
                        if let Some((_, param)) = gen.search_param_by_path(&ps.path) {
                            if let Some(resolved) = param.resolved() {
                                *generics = resolved.to_owned();
                            } else {
                                *generics = Ty::Match(param.clone().into_match());
                            }
                        } else {
                            ps.path.replace_by_bounds(gen);
                        }
                    }
                    _ => {}
                })
        })
    }

    pub fn is_single(&self) -> bool {
        self.segments.len() == 1
    }

    pub fn from_ast_nogen(path: &ast::Path) -> Path {
        let mut segments = Vec::new();
        for seg in path.segments.iter() {
            let name = seg.ident.name.to_string();
            // used right now in use tree
            segments.push(PathSegment::new(name, vec![], None));
        }
        Path {
            prefix: None,
            segments,
        }
    }

    pub fn from_ast(path: &ast::Path, scope: &Scope) -> Path {
        let mut segments = Vec::new();
        for seg in path.segments.iter() {
            let name = seg.ident.name.to_string();
            let mut types = Vec::new();
            let mut output = None;

            if let Some(ref params) = seg.args {
                if let ast::GenericArgs::AngleBracketed(ref angle_args) = **params {
                    angle_args.args.iter().for_each(|arg| {
                        if let ast::AngleBracketedArg::Arg(ast::GenericArg::Type(ty)) = arg {
                            if let Some(ty) = Ty::from_ast(ty, scope) {
                                types.push(ty);
                            }
                        }
                    })
                }
                // TODO: support inputs in GenericArgs::Parenthesized (A path like `Foo(A,B) -> C`)
                if let ast::GenericArgs::Parenthesized(ref paren_args) = **params {
                    if let ast::FnRetTy::Ty(ref ty) = paren_args.output {
                        output = Ty::from_ast(&*ty, scope);
                    }
                }
            }

            segments.push(PathSegment::new(name, types, output));
        }
        Path {
            prefix: None,
            segments,
        }
    }

    pub fn generic_types(&self) -> impl Iterator<Item = &Ty> {
        self.segments[self.segments.len() - 1].generics.iter()
    }

    pub fn single(seg: PathSegment) -> Path {
        Path {
            prefix: None,
            segments: vec![seg],
        }
    }

    pub fn set_prefix(&mut self) {
        if self.prefix.is_some() {
            return;
        }
        self.prefix = self
            .segments
            .first()
            .and_then(|seg| PathPrefix::from_str(&seg.name));
        if self.prefix.is_some() {
            self.segments.remove(0);
        }
    }

    pub fn from_vec(global: bool, v: Vec<&str>) -> Path {
        Self::from_iter(global, v.into_iter().map(|s| s.to_owned()))
    }

    pub fn from_svec(global: bool, v: Vec<String>) -> Path {
        Self::from_iter(global, v.into_iter())
    }

    pub fn from_iter(global: bool, iter: impl Iterator<Item = String>) -> Path {
        let mut prefix = if global {
            Some(PathPrefix::Global)
        } else {
            None
        };
        let segments: Vec<_> = iter
            .enumerate()
            .filter_map(|(i, s)| {
                if i == 0 && prefix.is_none() {
                    if let Some(pre) = PathPrefix::from_str(&s) {
                        prefix = Some(pre);
                        return None;
                    }
                }
                Some(PathSegment::from(s))
            })
            .collect();
        Path { prefix, segments }
    }

    pub fn extend(&mut self, path: Path) -> &mut Self {
        self.segments.extend(path.segments);
        self
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn name(&self) -> Option<&str> {
        self.segments.last().map(|seg| &*seg.name)
    }
}

impl fmt::Debug for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P[")?;
        let mut first = true;
        for seg in &self.segments {
            if first {
                write!(f, "{}", seg.name)?;
                first = false;
            } else {
                write!(f, "::{}", seg.name)?;
            }

            if !seg.output.is_none() {
                write!(f, "(")?;
            }

            if !seg.generics.is_empty() {
                if seg.output.is_none() {
                    write!(f, "<")?;
                }
                for (i, ty) in seg.generics.iter().enumerate() {
                    if i == 0 {
                        write!(f, "{:?}", ty)?;
                    } else {
                        write!(f, ",{:?}", ty)?
                    }
                }
                if seg.output.is_none() {
                    write!(f, ">")?;
                }
            }
            if !seg.output.is_none() {
                write!(f, ")->{:?}", seg.output.as_ref().unwrap())?;
            }
        }
        write!(f, "]")
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for seg in &self.segments {
            if first {
                write!(f, "{}", seg.name)?;
                first = false;
            } else {
                write!(f, "::{}", seg.name)?;
            }

            if !seg.generics.is_empty() {
                write!(f, "<")?;
                for (i, ty) in seg.generics.iter().enumerate() {
                    if i == 0 {
                        write!(f, "{}", ty)?;
                    } else {
                        write!(f, ", {}", ty)?
                    }
                }
                write!(f, ">")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathSegment {
    pub name: String,
    pub generics: Vec<Ty>,
    /// If this path segment is a closure, it's return type
    pub output: Option<Ty>,
}

impl PathSegment {
    pub fn new(name: String, generics: Vec<Ty>, output: Option<Ty>) -> Self {
        PathSegment {
            name,
            generics,
            output,
        }
    }
}

impl From<String> for PathSegment {
    fn from(name: String) -> Self {
        PathSegment {
            name,
            generics: Vec::new(),
            output: None,
        }
    }
}

/// Information about generic types in a match
#[derive(Clone, PartialEq)]
pub struct PathSearch {
    pub path: Path,
    pub filepath: PathBuf,
    pub point: BytePos,
}

impl PathSearch {
    pub fn new(path: Path, scope: Scope) -> Self {
        let Scope { filepath, point } = scope;
        PathSearch {
            path,
            filepath,
            point,
        }
    }
    pub(crate) fn resolve_as_match(&self, session: &Session<'_>) -> Option<Match> {
        find_type_match(&self.path, &self.filepath, self.point, session)
    }
}

impl fmt::Debug for PathSearch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Search [{:?}, {:?}, {:?}]",
            self.path,
            self.filepath.display(),
            self.point
        )
    }
}

/// Wrapper struct for representing trait bounds.
/// Its usages are
/// - for generic types like T: Debug + Clone
/// - for trait inheritance like trait A: Debug + Clone
/// - for impl_trait like fn f(a: impl Debug + Clone)
/// - for dynamic traits(dyn_trait) like Box<Debug + Clone> or Box<dyn Debug + Clone>
#[derive(Clone, Debug, PartialEq)]
pub struct TraitBounds(Vec<PathSearch>);

impl TraitBounds {
    /// checks if it contains a trait, whick its name is 'name'
    pub fn find_by_name(&self, name: &str) -> Option<&PathSearch> {
        self.find_by_names(&[name])
    }
    pub fn find_by_name_mut(&mut self, name: &str) -> Option<&mut PathSearch> {
        self.find_by_names_mut(&[name])
    }
    /// checks if it contains a trait, whick its name is 'name'
    pub fn find_by_names(&self, names: &[&str]) -> Option<&PathSearch> {
        self.0.iter().find(|path_search| {
            let seg = &path_search.path.segments;
            seg.len() == 1 && names.contains(&&*seg[0].name)
        })
    }
    pub fn find_by_names_mut(&mut self, names: &[&str]) -> Option<&mut PathSearch> {
        self.0.iter_mut().find(|path_search| {
            let seg = &path_search.path.segments;
            seg.len() == 1 && names.contains(&&*seg[0].name)
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = &PathSearch> {
        self.0.iter()
    }

    pub fn into_iter(self) -> impl Iterator<Item = PathSearch> {
        self.0.into_iter()
    }
    /// Search traits included in bounds and return Matches
    pub fn get_traits(&self, session: &Session<'_>) -> Vec<Match> {
        self.0
            .iter()
            .filter_map(|ps| {
                nameres::resolve_path(
                    &ps.path,
                    &ps.filepath,
                    ps.point,
                    core::SearchType::ExactMatch,
                    core::Namespace::Trait,
                    session,
                    &ImportInfo::default(),
                )
                .into_iter()
                .nth(0)
            })
            .collect()
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn has_closure(&self) -> bool {
        self.find_by_names(&["Fn", "FnMut", "FnOnce"]).is_some()
    }

    pub fn get_closure(&self) -> Option<&PathSearch> {
        self.find_by_names(&["Fn", "FnMut", "FnOnce"])
    }

    pub fn get_closure_mut(&mut self) -> Option<&mut PathSearch> {
        self.find_by_names_mut(&["Fn", "FnMut", "FnOnce"])
    }

    pub(crate) fn from_generic_bounds<P: AsRef<FilePath>>(
        bounds: &GenericBounds,
        filepath: P,
        offset: i32,
    ) -> TraitBounds {
        let vec = bounds
            .iter()
            .filter_map(|bound| {
                if let GenericBound::Trait(ref ptrait_ref, _) = *bound {
                    let ast_path = &ptrait_ref.trait_ref.path;
                    let source_map::BytePos(point) = ast_path.span.lo();
                    let scope = Scope::new(
                        filepath.as_ref().to_path_buf(),
                        BytePos::from((point as i32 + offset) as u32),
                    );
                    let path = Path::from_ast(&ast_path, &scope);
                    let path_search = PathSearch::new(path, scope);
                    Some(path_search)
                } else {
                    None
                }
            })
            .collect();
        TraitBounds(vec)
    }
    fn extend(&mut self, other: Self) {
        self.0.extend(other.0)
    }
}

/// Argument of generics like T: From<String>
/// It's intended to use this type only for declaration of type parameter.
// TODO: impl trait's name
// TODO: it has too many PathBuf
#[derive(Clone, Debug, PartialEq)]
pub struct TypeParameter {
    /// the name of type parameter declared in generics, like 'T'
    pub name: String,
    /// The point 'T' appears
    pub point: BytePos,
    /// file path
    pub filepath: PathBuf,
    /// bounds
    pub bounds: TraitBounds,
    /// Resolved Type
    pub resolved: Option<Ty>,
}

impl TypeParameter {
    pub fn name(&self) -> &str {
        &(*self.name)
    }
    pub(crate) fn into_match(self) -> Match {
        // TODO: contextstr, local
        Match {
            matchstr: self.name,
            filepath: self.filepath,
            point: self.point,
            coords: None,
            local: false,
            mtype: MatchType::TypeParameter(Box::new(self.bounds)),
            contextstr: String::new(),
            docs: String::new(),
        }
    }
    pub(crate) fn resolve(&mut self, ty: Ty) {
        self.resolved = Some(ty);
    }
    pub(crate) fn resolved(&self) -> Option<&Ty> {
        self.resolved.as_ref()
    }
    pub(crate) fn add_bound(&mut self, bound: TraitBounds) {
        let add_bounds: Vec<_> = bound
            .0
            .into_iter()
            .filter(|p| {
                if let Some(name) = p.path.name() {
                    self.bounds.find_by_name(name).is_none()
                } else {
                    true
                }
            })
            .collect();
        self.bounds.0.extend(add_bounds);
    }
}

/// List of Args in generics, e.g. <T: Clone, U, P>
/// Now it's intended to use only for type parameters
// TODO: should we extend this type enable to handle both type parameters and true types?
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GenericsArgs(pub Vec<TypeParameter>);

impl GenericsArgs {
    pub(crate) fn extend(&mut self, other: GenericsArgs) {
        self.0.extend(other.0);
    }
    pub(crate) fn from_generics<'a, P: AsRef<FilePath>>(
        generics: &'a ast::Generics,
        filepath: P,
        offset: i32,
    ) -> Self {
        let mut args = Vec::new();
        let mut closure_args = Vec::new();
        for param in generics.params.iter() {
            match param.kind {
                // TODO: lifetime support
                GenericParamKind::Lifetime => {}
                // TODO: should we handle default type here?
                GenericParamKind::Type { default: _ } => {
                    let param_name = param.ident.name.to_string();
                    let source_map::BytePos(point) = param.ident.span.lo();
                    let bounds = TraitBounds::from_generic_bounds(&param.bounds, &filepath, offset);
                    let type_param = TypeParameter {
                        name: param_name,
                        point: BytePos::from((point as i32 + offset) as u32),
                        filepath: filepath.as_ref().to_path_buf(),
                        bounds,
                        resolved: None,
                    };
                    if type_param.bounds.has_closure() {
                        closure_args.push(type_param);
                    } else {
                        args.push(type_param);
                    }
                }
                // TODO: Support const
                GenericParamKind::Const { ty: _, .. } => {}
            }
        }
        for pred in generics.where_clause.predicates.iter() {
            match pred {
                WherePredicate::BoundPredicate(bound) => match bound.bounded_ty.kind {
                    TyKind::Path(ref _qself, ref path) => {
                        if let Some(seg) = path.segments.get(0) {
                            let name = pprust::path_segment_to_string(&seg);
                            let bound =
                                TraitBounds::from_generic_bounds(&bound.bounds, &filepath, offset);
                            if let Some(tp) = args.iter_mut().find(|tp| tp.name == name) {
                                tp.bounds.extend(bound);
                                continue;
                            }
                            if let Some(tp) = closure_args.iter_mut().find(|tp| tp.name == name) {
                                tp.bounds.extend(bound);
                            }
                        }
                    }
                    // TODO 'self' support
                    TyKind::ImplicitSelf => {}
                    _ => {}
                },
                // TODO: lifetime support
                WherePredicate::RegionPredicate(_) => {}
                _ => {}
            }
        }

        // resolve the closure's return type into the containing function's type parameter
        fn replace_closure_output_with_matching_type_params_from_fn(
            tp: &mut TypeParameter,
            args: &GenericsArgs,
        ) {
            if let Some(ps) = tp.bounds.get_closure_mut() {
                for segment in ps.path.segments.iter_mut() {
                    segment.output = segment.output.take().map(|ty| match ty {
                        Ty::PathSearch(mut ps) => {
                            match args.get_tbound_match(&ps.path.segments[0].name) {
                                Some(m) => Ty::Match(m),
                                None => {
                                    // if output is a PathSearch, it may have generics
                                    // eg. Option<T> or Box<Vec<T>>, so recursively replace
                                    // them with that of the enclosing function's type params
                                    ps.path.replace_by_bounds(args);
                                    Ty::PathSearch(ps)
                                }
                            }
                        }
                        ty => ty,
                    });
                }
            }
        }

        let mut args = GenericsArgs(args);
        // closure's return types may be the generic args from the function's definition
        // like <K: Clone, F: Fn() -> K>, so handle closure types after processing
        // other type parameters
        for type_param in closure_args.iter_mut() {
            replace_closure_output_with_matching_type_params_from_fn(type_param, &args);
        }
        args.extend(GenericsArgs(closure_args));
        args
    }

    pub fn get_idents(&self) -> Vec<String> {
        self.0.iter().map(|g| g.name.clone()).collect()
    }
    pub fn args(&self) -> impl Iterator<Item = &TypeParameter> {
        self.0.iter()
    }
    pub fn args_mut(&mut self) -> impl Iterator<Item = &mut TypeParameter> {
        self.0.iter_mut()
    }
    pub fn search_param_by_path(&self, path: &Path) -> Option<(usize, &TypeParameter)> {
        if !path.is_single() {
            return None;
        }
        let query = &path.segments[0].name;
        for (i, typ) in self.0.iter().enumerate() {
            if typ.name() == query {
                return Some((i, typ));
            }
        }
        None
    }
    pub fn search_param_by_name(&self, name: &str) -> Option<(usize, &TypeParameter)> {
        for (i, typ) in self.0.iter().enumerate() {
            if typ.name() == name {
                return Some((i, typ));
            }
        }
        None
    }
    pub fn get_tbound_match(&self, name: &str) -> Option<Match> {
        Some(self.search_param_by_name(name)?.1.clone().into_match())
    }
    pub(crate) fn add_bound(&mut self, pos: usize, bound: TraitBounds) {
        if let Some(param) = self.0.get_mut(pos) {
            param.add_bound(bound);
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) fn apply_types(&mut self, other: &[Ty]) {
        for (l, r) in self.0.iter_mut().zip(other.iter()) {
            l.resolve(r.clone());
        }
    }
}

/// `Impl` information
#[derive(Clone, Debug, PartialEq)]
pub struct ImplHeader {
    self_path: Path,
    trait_path: Option<Path>,
    pub(crate) generics: GenericsArgs,
    filepath: PathBuf,
    // TODO: should be removed
    local: bool,
    impl_start: BytePos,
    block_start: BytePos,
}

impl ImplHeader {
    pub(crate) fn new(
        generics: &ast::Generics,
        path: &FilePath,
        otrait: &Option<TraitRef>,
        self_type: &ast::Ty,
        offset: BytePos,
        local: bool,
        impl_start: BytePos,
        block_start: BytePos,
    ) -> Option<Self> {
        let generics = GenericsArgs::from_generics(generics, path, offset.0 as i32);
        let scope = Scope::new(path.to_owned(), impl_start);
        let self_path = get_self_path(&self_type.kind, &scope)?;
        let trait_path = otrait
            .as_ref()
            .map(|tref| Path::from_ast(&tref.path, &scope));
        Some(ImplHeader {
            self_path,
            trait_path,
            generics,
            filepath: path.to_owned(),
            local,
            impl_start,
            block_start,
        })
    }
    pub(crate) fn self_path(&self) -> &Path {
        &self.self_path
    }
    pub(crate) fn trait_path(&self) -> Option<&Path> {
        self.trait_path.as_ref()
    }
    pub(crate) fn file_path(&self) -> &FilePath {
        self.filepath.as_ref()
    }
    pub(crate) fn generics(&self) -> &GenericsArgs {
        &self.generics
    }
    pub(crate) fn impl_start(&self) -> BytePos {
        self.impl_start
    }
    // TODO: should be removed
    pub(crate) fn is_local(&self) -> bool {
        self.local || self.trait_path.is_some()
    }
    pub(crate) fn is_trait(&self) -> bool {
        self.trait_path.is_some()
    }
    pub(crate) fn resolve_trait(
        &self,
        session: &Session<'_>,
        import_info: &ImportInfo<'_, '_>,
    ) -> Option<Match> {
        nameres::resolve_path(
            self.trait_path()?,
            self.file_path(),
            self.impl_start,
            core::SearchType::ExactMatch,
            core::Namespace::Trait,
            session,
            import_info,
        )
        .into_iter()
        .nth(0)
    }
    pub(crate) fn scope_start(&self) -> BytePos {
        self.block_start.increment()
    }
}

pub(crate) fn get_self_path(ty: &TyKind, scope: &Scope) -> Option<Path> {
    match ty {
        TyKind::Rptr(_, ref ty) => get_self_path(&ty.ty.kind, scope),
        TyKind::Path(_, ref path) => Some(Path::from_ast(path, &scope)),
        // HACK: treat slice as path
        TyKind::Slice(_) => Some(Path::single("[T]".to_owned().into())),
        _ => None,
    }
}
