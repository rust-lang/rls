use crate::ast_types::{GenericsArgs, ImplHeader, Pat, TraitBounds, Ty, TypeParameter};
use crate::codecleaner;
use crate::codeiter::StmtIndicesIter;
use crate::matchers::ImportInfo;
use crate::project_model::ProjectModelProvider;
use rls_span;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::io::Read;
use std::iter::{Fuse, Iterator};
use std::ops::{Deref, Range};
use std::rc::Rc;
use std::{fmt, vec};
use std::{path, str};
use rustc_span::source_map;

use crate::ast;
use crate::fileres;
use crate::nameres;
use crate::primitive::PrimKind;
use crate::scopes;
use crate::util;

/// Within a [`Match`], specifies what was matched
///
/// [`Match`]: struct.Match.html
#[derive(Clone, Debug, PartialEq)]
pub enum MatchType {
    Struct(Box<GenericsArgs>),
    Module,
    MatchArm,
    Function,
    Method(Option<Box<GenericsArgs>>),
    Crate,
    Let(BytePos),
    IfLet(BytePos),
    WhileLet(BytePos),
    For(BytePos),
    StructField,
    Enum(Box<GenericsArgs>),
    Union(Box<GenericsArgs>),
    /// EnumVariant needs to have Enum type to complete methods
    EnumVariant(Option<Box<Match>>),
    UseAlias(Box<Match>),
    AssocType,
    Type,
    FnArg(Box<(Pat, Option<Ty>)>),
    Trait,
    Const,
    Static,
    Macro,
    Builtin(PrimKind),
    /// fn f<T: Clone> or fn f(a: impl Clone) with its trait bounds
    TypeParameter(Box<TraitBounds>),
}

impl MatchType {
    pub fn is_function(&self) -> bool {
        match self {
            MatchType::Function | MatchType::Method(_) => true,
            _ => false,
        }
    }
    pub fn is_enum(&self) -> bool {
        match self {
            MatchType::Enum(_) => true,
            _ => false,
        }
    }
    pub fn is_struct(&self) -> bool {
        match self {
            MatchType::Struct(_) => true,
            _ => false,
        }
    }
}

impl fmt::Display for MatchType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MatchType::Struct(_) => write!(f, "Struct"),
            MatchType::Union(_) => write!(f, "Union"),
            MatchType::Method(_) => write!(f, "Method"),
            MatchType::IfLet(_) => write!(f, "IfLet"),
            MatchType::Let(_) => write!(f, "Let"),
            MatchType::WhileLet(_) => write!(f, "WhileLet"),
            MatchType::For(_) => write!(f, "For"),
            MatchType::Enum(_) => write!(f, "Enum"),
            MatchType::EnumVariant(_) => write!(f, "EnumVariant"),
            MatchType::TypeParameter(_) => write!(f, "TypeParameter"),
            MatchType::FnArg(_) => write!(f, "FnArg"),
            MatchType::Type => write!(f, "Type"),
            MatchType::UseAlias(_) => write!(f, "UseAlias"),
            _ => fmt::Debug::fmt(self, f),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchType {
    ExactMatch,
    StartsWith,
}

mod declare_namespace {
    // (kngwyu) I reserved Crate, Mod or other names for future usage(like for #830)
    // but, currently they're not used and... I'm not sure they're useful:)
    #![allow(non_upper_case_globals, unused)]
    bitflags! {
        /// Type context
        pub struct Namespace: u32 {
            const Crate     = 0b0000000000001;
            const Mod       = 0b0000000000010;
            const Space     = 0b0000000000011;
            const Enum      = 0b0000000000100;
            const Struct    = 0b0000000001000;
            const Union     = 0b0000000010000;
            const Trait     = 0b0000000100000;
            const TypeDef   = 0b0000001000000;
            const HasField  = 0b0000001011100;
            const Type      = 0b0000001111100;
            const PathParen = 0b0000001111111;
            const Const     = 0b0000010000000;
            const Static    = 0b0000100000000;
            const Func      = 0b0001000000000;
            // for use_extern_macros
            const Macro     = 0b0010000000000;
            const Impl      = 0b0001110000000;
            const PathChild = 0b0011110000000;
            const Path      = 0b0011111111111;
            const Primitive = 0b0100000000000;
            const StdMacro  = 0b1000000000000;
            const Global    = 0b1100000000000;
        }
    }
}
pub use self::declare_namespace::Namespace;

#[derive(Debug, Clone, Copy)]
pub enum CompletionType {
    Field,
    Path,
}

/// 0-based byte offset in a file.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Index,
    From,
    Add,
    Sub,
    AddAssign,
    SubAssign,
)]
pub struct BytePos(pub usize);

impl From<u32> for BytePos {
    fn from(u: u32) -> Self {
        BytePos(u as usize)
    }
}

impl BytePos {
    pub const ZERO: BytePos = BytePos(0);
    /// returns self - 1
    pub fn decrement(&self) -> Self {
        BytePos(self.0 - 1)
    }
    pub fn checked_sub(&self, sub: impl Into<Self>) -> Option<Self> {
        self.0.checked_sub(sub.into().0).map(BytePos)
    }
    pub fn try_decrement(&self) -> Option<Self> {
        self.0.checked_sub(1).map(BytePos)
    }
    /// returns self + 1
    pub fn increment(&self) -> Self {
        BytePos(self.0 + 1)
    }
}

impl fmt::Display for BytePos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 0-based byte range in a file.
#[derive(Clone, Copy, Default, Eq, PartialEq, Hash)]
pub struct ByteRange {
    /// start of byte position in codes(inclusive)
    pub start: BytePos,
    /// end of byte position in codes(exclusive)
    pub end: BytePos,
}

impl ByteRange {
    /// returns new ByteRange from start and end
    pub fn new<P: Into<BytePos>>(start: P, end: P) -> Self {
        ByteRange {
            start: start.into(),
            end: end.into(),
        }
    }

    /// returns the length of the range
    #[inline]
    pub fn len(&self) -> usize {
        (self.end - self.start).0
    }

    /// returns if the range contains `point` or not
    #[inline]
    pub fn contains(&self, point: BytePos) -> bool {
        self.start <= point && point < self.end
    }

    /// returns if the range contains `point` (except its start point)
    #[inline]
    pub fn contains_exclusive(&self, point: BytePos) -> bool {
        self.start < point && point < self.end
    }

    /// returns the new range with which its start is `self.start + shift`,
    /// its end is `self.end + shift`
    #[inline]
    pub fn shift<P: Into<BytePos>>(&self, shift: P) -> Self {
        let shift = shift.into();
        ByteRange {
            start: self.start + shift,
            end: self.end + shift,
        }
    }

    /// convert the range to `std::ops::Range`
    #[inline]
    pub fn to_range(&self) -> Range<usize> {
        self.start.0..self.end.0
    }
}

impl PartialEq<BytePos> for ByteRange {
    fn eq(&self, other: &BytePos) -> bool {
        self.contains(*other)
    }
}

impl PartialOrd<BytePos> for ByteRange {
    fn partial_cmp(&self, other: &BytePos) -> Option<Ordering> {
        if *other < self.start {
            Some(Ordering::Greater)
        } else if *other >= self.end {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Equal)
        }
    }
}

impl From<source_map::Span> for ByteRange {
    fn from(span: source_map::Span) -> Self {
        let (lo, hi) = ast::destruct_span(span);
        ByteRange::new(lo, hi)
    }
}

impl fmt::Debug for ByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ByteRange({}..{})", self.start.0, self.end.0)
    }
}

/// Row and Column position in a file
// for backward compatibility, we use 1-index row and 0-indexed column here
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Coordinate {
    pub row: rls_span::Row<rls_span::OneIndexed>,
    pub col: rls_span::Column<rls_span::ZeroIndexed>,
}

impl Coordinate {
    /// construct new Coordinate
    pub fn new(row: u32, col: u32) -> Self {
        Coordinate {
            row: rls_span::Row::<rls_span::OneIndexed>::new_one_indexed(row),
            col: rls_span::Column::<rls_span::ZeroIndexed>::new_zero_indexed(col),
        }
    }
    /// start point of the file
    pub fn start() -> Self {
        Coordinate::new(1, 0)
    }
}

/// Context, source, and etc. for detected completion or definition
#[derive(Clone, PartialEq)]
pub struct Match {
    pub matchstr: String,
    pub filepath: path::PathBuf,
    pub point: BytePos,
    pub coords: Option<Coordinate>,
    pub local: bool,
    pub mtype: MatchType,
    pub contextstr: String,
    pub docs: String,
}

impl Match {
    /// Checks if two matches can be considered the same for deduplication purposes.
    ///
    /// This could be the basis for a `PartialEq` implementation in the future,
    /// but in the interest of minimizing the crate's public API surface it's exposed
    /// as a private method for now.
    fn is_same_as(&self, other: &Match) -> bool {
        self.point == other.point
            && self.matchstr == other.matchstr
            && self.filepath == other.filepath
    }
    pub(crate) fn to_generics(&self) -> Option<&GenericsArgs> {
        match &self.mtype {
            MatchType::Struct(gen_arg) | MatchType::Enum(gen_arg) => Some(gen_arg.as_ref()),
            MatchType::Method(gen_arg) => gen_arg.as_ref().map(AsRef::as_ref),
            _ => None,
        }
    }
    pub(crate) fn into_generics(self) -> Option<GenericsArgs> {
        match self.mtype {
            MatchType::Struct(gen_arg) | MatchType::Enum(gen_arg) => Some(*gen_arg),
            MatchType::Method(gen_arg) => gen_arg.map(|x| *x),
            _ => None,
        }
    }
    pub(crate) fn generics(&self) -> impl Iterator<Item = &TypeParameter> {
        let opt = match self.mtype {
            MatchType::Struct(ref gen_arg) | MatchType::Enum(ref gen_arg) => Some(gen_arg),
            MatchType::Method(ref gen_arg) => gen_arg.as_ref(),
            _ => None,
        };
        opt.into_iter().flat_map(|gen_arg| gen_arg.args())
    }
    pub(crate) fn resolved_generics(&self) -> impl Iterator<Item = &Ty> {
        let opt = match self.mtype {
            MatchType::Struct(ref gen_arg) | MatchType::Enum(ref gen_arg) => Some(gen_arg),
            MatchType::Method(ref gen_arg) => gen_arg.as_ref(),
            _ => None,
        };
        opt.into_iter()
            .flat_map(|gen_arg| gen_arg.args())
            .filter_map(|ty_param| ty_param.resolved.as_ref())
    }
    pub(crate) fn resolve_generics(&mut self, types: &[Ty]) {
        match self.mtype {
            MatchType::Struct(ref mut gen_arg) | MatchType::Enum(ref mut gen_arg) => {
                gen_arg.apply_types(types);
            }
            _ => {}
        };
    }
    // currently we can't resolve method's type parameter
    pub(crate) fn generics_mut(&mut self) -> impl Iterator<Item = &mut TypeParameter> {
        let opt = match &mut self.mtype {
            MatchType::Struct(gen_arg) | MatchType::Enum(gen_arg) => Some(&mut **gen_arg),
            _ => None,
        };
        opt.into_iter().flat_map(|gen_arg| gen_arg.args_mut())
    }
}

/// The cursor position used by public search methods
#[derive(Debug, Clone, Copy)]
pub enum Location {
    /// A byte offset in the file
    Point(BytePos),
    /// 1-based line and column indices.
    Coords(Coordinate),
}

impl From<BytePos> for Location {
    fn from(val: BytePos) -> Location {
        Location::Point(val)
    }
}

impl From<usize> for Location {
    fn from(val: usize) -> Location {
        Location::Point(BytePos(val))
    }
}

impl From<Coordinate> for Location {
    fn from(val: Coordinate) -> Location {
        Location::Coords(val)
    }
}

/// Internal cursor methods
pub trait LocationExt {
    fn to_point(&self, src: &RawSource) -> Option<BytePos>;
    fn to_coords(&self, src: &RawSource) -> Option<Coordinate>;
}

impl LocationExt for Location {
    fn to_point(&self, src: &RawSource) -> Option<BytePos> {
        match *self {
            Location::Point(val) => Some(val),
            Location::Coords(ref coords) => src.coords_to_point(coords),
        }
    }

    fn to_coords(&self, src: &RawSource) -> Option<Coordinate> {
        match *self {
            Location::Coords(val) => Some(val),
            Location::Point(point) => src.point_to_coords(point),
        }
    }
}

impl fmt::Debug for Match {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Match [{:?}, {:?}, {:?}, {:?}, {:?}, |{}|]",
            self.matchstr,
            self.filepath.display(),
            self.point,
            self.local,
            self.mtype,
            self.contextstr
        )
    }
}

#[derive(Clone, PartialEq)]
pub struct Scope {
    pub filepath: path::PathBuf,
    pub point: BytePos,
}

impl Scope {
    pub fn new(path: path::PathBuf, pos: BytePos) -> Self {
        Scope {
            filepath: path,
            point: pos,
        }
    }

    pub fn from_match(m: &Match) -> Scope {
        Scope {
            filepath: m.filepath.clone(),
            point: m.point,
        }
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Scope [{:?}, {:?}]", self.filepath.display(), self.point)
    }
}

#[derive(Clone, Debug)]
pub struct RawSource {
    pub code: String,
    pub lines: RefCell<Vec<ByteRange>>,
}

impl RawSource {
    pub fn new(s: String) -> Self {
        RawSource {
            code: s,
            lines: Default::default(),
        }
    }

    fn cache_lineoffsets(&self) {
        if self.lines.borrow().len() != 0 {
            return;
        }
        let mut before = 0;
        *self.lines.borrow_mut() = self
            .code
            .split('\n')
            .map(|line| {
                let len = line.len() + 1;
                let res = ByteRange::new(before, before + len);
                before += len;
                res
            })
            .collect();
    }

    pub fn coords_to_point(&self, coords: &Coordinate) -> Option<BytePos> {
        self.cache_lineoffsets();
        self.lines
            .borrow()
            .get(coords.row.zero_indexed().0 as usize)
            .and_then(|&range| {
                let col = coords.col.0 as usize;
                if col < range.len() {
                    Some(range.start + col.into())
                } else {
                    None
                }
            })
    }

    pub fn point_to_coords(&self, point: BytePos) -> Option<Coordinate> {
        self.cache_lineoffsets();
        let lines = self.lines.borrow();
        lines
            .binary_search_by(|range| range.partial_cmp(&point).unwrap())
            .ok()
            .map(|idx| Coordinate::new(idx as u32 + 1, (point - lines[idx].start).0 as u32))
    }
}

#[derive(Clone, Debug)]
pub struct MaskedSource {
    pub code: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Src<'c> {
    pub src: &'c MaskedSource,
    pub range: ByteRange,
}

impl MaskedSource {
    pub fn new(src: &str) -> MaskedSource {
        let idx: Vec<_> = codecleaner::code_chunks(&src).collect();
        let code = scopes::mask_comments(src, &idx);
        MaskedSource { code }
    }

    pub fn as_src(&self) -> Src<'_> {
        self.get_src_from_start(BytePos::ZERO)
    }

    pub fn get_src_from_start(&self, new_start: BytePos) -> Src<'_> {
        Src {
            src: self,
            range: ByteRange::new(new_start, self.len().into()),
        }
    }
}

pub struct MatchIter<'c> {
    session: &'c Session<'c>,
    matches: vec::IntoIter<Match>,
}

impl<'c> Iterator for MatchIter<'c> {
    type Item = Match;

    fn next(&mut self) -> Option<Match> {
        self.matches.next().map(|mut m| {
            if m.coords.is_none() {
                let point = m.point;
                let src = self.session.load_raw_file(m.filepath.as_path());
                m.coords = src.point_to_coords(point);
            }
            m
        })
    }
}

#[test]
fn coords_to_point_works() {
    let src = "
fn myfn() {
    let a = 3;
    print(a);
}";
    let src = RawSource::new(src.into());
    assert_eq!(
        src.coords_to_point(&Coordinate::new(3, 5)),
        Some(BytePos(18))
    );
}

#[test]
fn coords_to_point_lf_newline() {
    let src = "\n\
               fn myfn() {\n\
               let a = 3;\n\
               print(a);\n\
               }\n";
    let src = RawSource::new(src.into());
    assert_eq!(
        src.coords_to_point(&Coordinate::new(3, 5)),
        Some(BytePos(18))
    );
}

#[test]
fn coords_to_point_crlf_newline() {
    let src = "\r\n\
               fn myfn() {\r\n\
               let a = 3;\r\n\
               print(a);\r\n\
               }\r\n";
    let src = RawSource::new(src.into());
    assert_eq!(
        src.coords_to_point(&Coordinate::new(3, 5)),
        Some(BytePos(20))
    );
}

#[test]
fn test_point_to_coords() {
    let src = "
fn myfn(b:usize) {
   let a = 3;
   if b == 12 {
       let a = 24;
       do_something_with(a);
   }
   do_something_with(a);
}
";
    fn round_trip_point_and_coords(src: &str, lineno: usize, charno: usize) {
        let raw_src = RawSource::new(src.to_owned());
        let point = raw_src
            .coords_to_point(&Coordinate::new(lineno as u32, charno as u32))
            .unwrap();
        let coords = raw_src.point_to_coords(point).unwrap();
        assert_eq!(coords, Coordinate::new(lineno as u32, charno as u32));
    }
    round_trip_point_and_coords(src, 4, 5);
}

impl<'c> Src<'c> {
    pub fn iter_stmts(&self) -> Fuse<StmtIndicesIter<'_>> {
        StmtIndicesIter::from_parts(self)
    }

    pub fn shift_start(&self, shift: BytePos) -> Src<'c> {
        Src {
            src: self.src,
            range: ByteRange::new(self.range.start + shift, self.range.end),
        }
    }

    pub fn change_length(&self, new_length: BytePos) -> Src<'c> {
        Src {
            src: self.src,
            range: ByteRange::new(self.range.start, self.range.start + new_length),
        }
    }

    pub fn shift_range(&self, new_range: ByteRange) -> Src<'c> {
        Src {
            src: self.src,
            range: new_range.shift(self.range.start),
        }
    }
}

pub struct RangedRawSrc {
    inner: Rc<RawSource>,
    range: ByteRange,
}

impl Deref for RangedRawSrc {
    type Target = str;
    fn deref(&self) -> &str {
        &self.inner.code[self.range.to_range()]
    }
}

impl Deref for RawSource {
    type Target = str;
    fn deref(&self) -> &str {
        &self.code
    }
}

impl Deref for MaskedSource {
    type Target = str;
    fn deref(&self) -> &str {
        &self.code
    }
}

impl<'c> Deref for Src<'c> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.src.code[self.range.to_range()]
    }
}

/// Caches file contents for re-use between sessions.
///
/// The file cache is an opaque blob outside of racer which contains maps of loaded and masked
/// files.
pub struct FileCache {
    /// raw source for cached files
    raw_map: RefCell<HashMap<path::PathBuf, Rc<RawSource>>>,

    /// masked source for cached files
    ///
    /// a version with comments and strings replaced by spaces, so that they
    /// aren't found when scanning the source for signatures.
    masked_map: RefCell<HashMap<path::PathBuf, Rc<MaskedSource>>>,

    /// The file loader
    pub(crate) loader: Box<dyn FileLoader>,
}

/// Used by the FileCache for loading files
///
/// Implement one of these and pass it to `FileCache::new()` to override Racer's
/// file loading behavior.
pub trait FileLoader {
    /// Load a single file
    fn load_file(&self, path: &path::Path) -> io::Result<String>;
}

/// Provide a blanket impl for Arc<T> since Rls uses that
impl<T: FileLoader> FileLoader for ::std::sync::Arc<T> {
    fn load_file(&self, path: &path::Path) -> io::Result<String> {
        (&self as &T).load_file(path)
    }
}

/// The default file loader
///
/// Private since this shouldn't be needed outside of racer
struct DefaultFileLoader;

impl FileLoader for DefaultFileLoader {
    fn load_file(&self, path: &path::Path) -> io::Result<String> {
        let mut rawbytes = Vec::new();
        let mut f = File::open(path)?;
        f.read_to_end(&mut rawbytes)?;

        // skip BOM bytes, if present
        if rawbytes.len() > 2 && rawbytes[0..3] == [0xEF, 0xBB, 0xBF] {
            str::from_utf8(&rawbytes[3..])
                .map(|s| s.to_owned())
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
        } else {
            String::from_utf8(rawbytes).map_err(|err| io::Error::new(io::ErrorKind::Other, err))
        }
    }
}

impl Default for FileCache {
    fn default() -> FileCache {
        FileCache::new(DefaultFileLoader)
    }
}

impl FileCache {
    /// Create a new file cache
    ///
    /// In order to load files into the cache, please see
    /// [`Session::cache_file_contents()`]
    ///
    /// [`Session::cache_file_contents()`]: struct.Session.html#method.cache_file_contents
    pub fn new<L: FileLoader + 'static>(loader: L) -> FileCache {
        FileCache {
            raw_map: RefCell::new(HashMap::new()),
            masked_map: RefCell::new(HashMap::new()),
            loader: Box::new(loader),
        }
    }

    /// Remove specific files from the cache
    ///
    /// Returns true if a file was removed
    pub fn remove_file<P: AsRef<path::Path>>(&self, path: &P) -> bool {
        let path = path.as_ref();
        let mut raw = self.raw_map.borrow_mut();
        let mut masked = self.masked_map.borrow_mut();
        raw.remove(path).is_some() || masked.remove(path).is_some()
    }

    /// Add/Replace a file in both versions.
    fn cache_file_contents<P, T>(&self, filepath: P, buf: T)
    where
        T: Into<String>,
        P: Into<path::PathBuf>,
    {
        let pathbuf = filepath.into();
        let src = buf.into();
        let masked_src = MaskedSource::new(&src);
        self.raw_map
            .borrow_mut()
            .insert(pathbuf.clone(), Rc::new(RawSource::new(src)));
        self.masked_map
            .borrow_mut()
            .insert(pathbuf, Rc::new(masked_src));
    }

    fn load_file(&self, filepath: &path::Path) -> Rc<RawSource> {
        if let Some(src) = self.raw_map.borrow().get(filepath) {
            return src.clone();
        }

        // nothing found, insert into cache
        // Ugh, really need handle results on all these methods :(
        let source = self
            .loader
            .load_file(filepath)
            .expect(&format!("Failed load file {:?}", filepath));
        let source = Rc::new(RawSource::new(source));
        self.raw_map
            .borrow_mut()
            .insert(filepath.to_path_buf(), Rc::clone(&source));
        source
    }

    fn load_file_and_mask_comments(&self, filepath: &path::Path) -> Rc<MaskedSource> {
        if let Some(src) = self.masked_map.borrow().get(filepath) {
            return src.clone();
        }
        // nothing found, insert into cache
        let src = self.load_file(filepath);
        let msrc = Rc::new(MaskedSource::new(&src.code));
        self.masked_map
            .borrow_mut()
            .insert(filepath.to_path_buf(), msrc.clone());
        msrc
    }
}

/// Private methods for the Session type
pub trait SessionExt {
    /// Request that a file is loaded into the cache
    ///
    /// This API is unstable and should not be used outside of Racer
    fn load_raw_file(&self, _: &path::Path) -> Rc<RawSource>;

    /// ranged version of load_raw_file
    fn load_raw_src_ranged(&self, src: &Src<'_>, _: &path::Path) -> RangedRawSrc;

    /// Request that a file is loaded into the cache with comments masked
    ///
    /// This API is unstable and should not be used outside of Racer
    fn load_source_file(&self, _: &path::Path) -> Rc<MaskedSource>;
}

/// Context for a Racer operation
pub struct Session<'c> {
    /// Cache for files
    ///
    /// The file cache is used within a session to prevent multiple reads. It is
    /// borrowed here in order to support reuse across Racer operations.
    cache: &'c FileCache,
    /// Cache for generic impls
    pub generic_impls: RefCell<HashMap<(path::PathBuf, BytePos), Vec<Rc<ImplHeader>>>>,
    pub project_model: Box<dyn ProjectModelProvider + 'c>,
}

impl<'c> fmt::Debug for Session<'c> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Session {{ .. }}")
    }
}

impl<'c> Session<'c> {
    /// Create a Session for use in Racer operations
    ///
    /// * `cache` is a reference to a `FileCache`. It's take by reference for
    ///   use across racer operations.
    ///
    /// # Examples
    ///
    /// ```
    /// extern crate racer;
    ///
    /// let cache = racer::FileCache::default();
    /// let session = racer::Session::new(&cache, None);
    /// ```
    ///
    /// [`FileCache`]: struct.FileCache.html
    #[cfg(feature = "metadata")]
    pub fn new(cache: &'c FileCache, project_path: Option<&path::Path>) -> Session<'c> {
        let project_model = crate::metadata::project_model(project_path);
        Session::with_project_model(cache, project_model)
    }

    pub fn with_project_model(
        cache: &'c FileCache,
        project_model: Box<dyn ProjectModelProvider + 'c>,
    ) -> Session<'c> {
        Session {
            cache,
            generic_impls: Default::default(),
            project_model,
        }
    }
    /// Specify the contents of a file to be used in completion operations
    ///
    /// The path to the file and the file's contents must both be specified.
    ///
    /// # Examples
    ///
    /// ```
    /// extern crate racer;
    ///
    /// let cache = racer::FileCache::default();
    /// let session = racer::Session::new(&cache, None);
    ///
    /// session.cache_file_contents("foo.rs", "pub struct Foo;\\n");
    /// ```
    pub fn cache_file_contents<T, P>(&self, filepath: P, buf: T)
    where
        T: Into<String>,
        P: Into<path::PathBuf>,
    {
        self.cache.cache_file_contents(filepath, buf);
    }

    pub fn contains_file<P: AsRef<path::Path>>(&self, path: P) -> bool {
        let path = path.as_ref();
        let raw = self.cache.raw_map.borrow();
        let masked = self.cache.masked_map.borrow();
        raw.contains_key(path) && masked.contains_key(path)
    }
}

impl<'c> SessionExt for Session<'c> {
    fn load_raw_file(&self, filepath: &path::Path) -> Rc<RawSource> {
        self.cache.load_file(filepath)
    }

    fn load_raw_src_ranged(&self, src: &Src<'_>, filepath: &path::Path) -> RangedRawSrc {
        let inner = self.cache.load_file(filepath);
        RangedRawSrc {
            inner,
            range: src.range,
        }
    }

    fn load_source_file(&self, filepath: &path::Path) -> Rc<MaskedSource> {
        self.cache.load_file_and_mask_comments(filepath)
    }
}

/// Get the racer point of a line/character number pair for a file.
pub fn to_point<P>(coords: Coordinate, path: P, session: &Session<'_>) -> Option<BytePos>
where
    P: AsRef<path::Path>,
{
    Location::from(coords).to_point(&session.load_raw_file(path.as_ref()))
}

/// Get the racer point of a line/character number pair for a file.
pub fn to_coords<P>(point: BytePos, path: P, session: &Session<'_>) -> Option<Coordinate>
where
    P: AsRef<path::Path>,
{
    Location::from(point).to_coords(&session.load_raw_file(path.as_ref()))
}

/// Find completions for a fully qualified name like `std::io::`
///
/// Searchs are started relative to `path`.
///
/// * `query` - is the fqn to search for
/// * `path` - the directory to start searching in
/// * `session` - reference to a racer::Session
///
/// ```no_run
/// extern crate racer;
///
/// let path = std::path::Path::new(".");
/// let cache = racer::FileCache::default();
/// let session = racer::Session::new(&cache, Some(path));
///
/// let m = racer::complete_fully_qualified_name(
///     "std::fs::canon",
///     &path,
///     &session
/// ).next().unwrap();
///
/// assert_eq!(&m.matchstr[..], "canonicalize");
/// assert_eq!(m.mtype, racer::MatchType::Function);
/// ```
#[inline]
pub fn complete_fully_qualified_name<'c, S, P>(
    query: S,
    path: P,
    session: &'c Session<'_>,
) -> MatchIter<'c>
where
    S: AsRef<str>,
    P: AsRef<path::Path>,
{
    let mut matches = complete_fully_qualified_name_(query.as_ref(), path.as_ref(), session);
    matches.dedup_by(|a, b| a.is_same_as(b));

    MatchIter {
        matches: matches.into_iter(),
        session,
    }
}

/// Actual implementation without generic bounds
fn complete_fully_qualified_name_(
    query: &str,
    path: &path::Path,
    session: &Session<'_>,
) -> Vec<Match> {
    let p: Vec<&str> = query.split("::").collect();

    let mut matches = Vec::new();

    for m in nameres::do_file_search(p[0], path, session) {
        if p.len() == 1 {
            matches.push(m);
        } else {
            let external_search_matches = nameres::do_external_search(
                &p[1..],
                &m.filepath,
                m.point,
                SearchType::StartsWith,
                Namespace::Path,
                &session,
            );

            for m in external_search_matches {
                matches.push(m);
            }
        }
    }

    matches
}

/// Search for completion at position in a file
///
/// * `src` - the file contents to search in
/// * `filepath` - path to file containing `src`
/// * `pos` - byte offset in file with path/expr to complete
/// * `session` - a racer::Session
///
/// # Examples
///
/// ```
/// extern crate racer;
///
/// # fn main() {
/// let src = "
/// fn apple() {
/// }
///
/// fn main() {
///     let b = ap
/// }";
///
/// println!("{:?}", src);
///
/// let cache = racer::FileCache::default();
/// let session = racer::Session::new(&cache, None);
///
/// session.cache_file_contents("lib.rs", src);
///
/// let got = racer::complete_from_file("lib.rs", racer::Location::from(43), &session)
///     .nth(0).unwrap();
/// assert_eq!("apple", got.matchstr);
/// assert_eq!(got.mtype, racer::MatchType::Function);
///
/// # }
/// ```
pub fn complete_from_file<'c, P, C>(
    filepath: P,
    cursor: C,
    session: &'c Session<'_>,
) -> MatchIter<'c>
where
    P: AsRef<path::Path>,
    C: Into<Location>,
{
    let mut matches = complete_from_file_(filepath.as_ref(), cursor.into(), session);
    matches.sort_by(|a, b| a.matchstr.cmp(&b.matchstr).then(a.point.cmp(&b.point)));
    matches.dedup_by(|a, b| a.is_same_as(b));

    MatchIter {
        matches: matches.into_iter(),
        session,
    }
}

fn complete_from_file_(
    filepath: &path::Path,
    cursor: Location,
    session: &Session<'_>,
) -> Vec<Match> {
    let src = session.load_source_file(filepath);
    let raw_src = session.load_raw_file(filepath);
    let src_text = &src.as_src()[..];
    // TODO return result
    let pos = match cursor.to_point(&raw_src) {
        Some(pos) => pos,
        None => {
            debug!("Failed to convert cursor to point");
            return Vec::new();
        }
    };
    let start = scopes::get_start_of_search_expr(src_text, pos);
    let expr = &src_text[start.0..pos.0];
    let (contextstr, searchstr, completetype) = scopes::split_into_context_and_completion(expr);

    debug!(
        "{:?}: contextstr is |{}|, searchstr is |{}|",
        completetype, contextstr, searchstr
    );

    let mut out = Vec::new();

    match completetype {
        CompletionType::Path => {
            let (stmtstart, stmt) = &scopes::get_current_stmt(src.as_src(), pos);
            debug!("Complete path with stmt: {:?}", stmt);
            // when in the function ident position, only look for methods
            // from a trait to complete.
            if util::in_fn_name(stmt) {
                trace!("Path is in fn declaration: `{}`", expr);
                return nameres::resolve_method(
                    pos,
                    src.as_src(),
                    expr,
                    filepath,
                    SearchType::StartsWith,
                    session,
                    &ImportInfo::default(),
                );
            }
            let (path, namespace) = if let Some(use_start) = scopes::use_stmt_start(stmt) {
                let path = scopes::construct_path_from_use_tree(&stmt[use_start.0..]);
                (path, Namespace::Path)
            } else if scopes::is_extern_crate(stmt) {
                return fileres::search_crate_names(
                    searchstr,
                    SearchType::StartsWith,
                    filepath,
                    false,
                    session,
                );
            } else if let Some(str_path) = scopes::is_in_struct_ctor(src.as_src(), *stmtstart, pos)
            {
                let path = scopes::expr_to_path(&src[str_path.to_range()]).0;
                return nameres::get_struct_fields(
                    &path,
                    searchstr,
                    filepath,
                    pos,
                    SearchType::StartsWith,
                    session,
                );
            } else {
                scopes::expr_to_path(expr)
            };
            debug!("path: {:?}, prefix: {:?}", path, path.prefix);
            out.extend(nameres::resolve_path(
                &path,
                filepath,
                pos,
                SearchType::StartsWith,
                namespace,
                session,
                &ImportInfo::default(),
            ));
        }
        CompletionType::Field => {
            let context = ast::get_type_of(contextstr.to_owned(), filepath, pos, session);
            debug!("complete_from_file context is {:?}", context);
            if let Some(ty) = context {
                out.extend(nameres::get_field_matches_from_ty(
                    ty,
                    searchstr,
                    SearchType::StartsWith,
                    session,
                ));
            }
        }
    }

    out
}

/// Finds if the statement where cursor lies is a `use` statement.
///
/// # Examples
///
/// ```
/// extern crate racer;
/// extern crate env_logger;
///
///
/// # fn main() {
/// let _ = env_logger::init();
/// let cache = racer::FileCache::default();
/// let session = racer::Session::new(&cache, None);
///
/// // This is the file where we request completion from
/// let src = stringify! {
///    use sub::foo;
///    use sub::{
///         bar
///    };
///    pub(crate) use sub::baz;
/// };
///
/// // Load files into cache to prevent trying to read from disk
/// session.cache_file_contents("lib.rs", src);
///
/// assert_eq!(racer::is_use_stmt("lib.rs", racer::Location::from(9), &session), true);
/// assert_eq!(racer::is_use_stmt("lib.rs", racer::Location::from(28), &session), true);
/// assert_eq!(racer::is_use_stmt("lib.rs", racer::Location::from(5000), &session), false);
/// # }
/// ```
pub fn is_use_stmt<P, C>(file_path: P, cursor: C, session: &Session<'_>) -> bool
where
    P: AsRef<path::Path>,
    C: Into<Location>,
{
    let file_path = file_path.as_ref();
    let src = session.load_source_file(file_path);
    let raw_src = session.load_raw_file(file_path);
    let pos = match cursor.into().to_point(&raw_src) {
        Some(pos) => pos,
        None => return false,
    };

    if src.bytes().len() <= pos.0 {
        return false;
    }

    let line = &scopes::get_current_stmt(src.as_src(), pos).1;
    scopes::use_stmt_start(line).is_some()
}

/// Find the definition for item at given a file, source, and cursor index
///
/// # Examples
///
/// ```
/// extern crate racer;
/// extern crate env_logger;
///
/// use std::path::Path;
///
/// # fn main() {
/// let _ = env_logger::init();
/// let cache = racer::FileCache::default();
/// let session = racer::Session::new(&cache, None);
///
/// // This is the file where we request completion from
/// let src = r"
///    mod sub;
///    use sub::foo;
///    fn main() {
///        foo();
///    };
/// ";
///
/// // This is the submodule where the definition is found
/// let sub = r"pub fn foo() {}";
///
/// // Load files into cache to prevent trying to read from disk
/// session.cache_file_contents("sub.rs", sub);
/// session.cache_file_contents("lib.rs", src);
///
/// // Search for the definition. 52 is the byte offset in `src`.
/// // Specifically, this asks for the definition of `foo()`.
/// let m = racer::find_definition("lib.rs", racer::Location::from(52), &session)
///               .expect("find definition returns a match");
///
/// // Should have found definition in the "sub.rs" file
/// assert_eq!(m.filepath, Path::new("sub.rs"));
/// // The definition should be for foo
/// assert_eq!(&m.matchstr[..], "foo");
/// // The definition should be a function
/// assert_eq!(m.mtype, racer::MatchType::Function);
/// # }
/// ```
pub fn find_definition<P, C>(filepath: P, cursor: C, session: &Session<'_>) -> Option<Match>
where
    P: AsRef<path::Path>,
    C: Into<Location>,
{
    find_definition_(filepath.as_ref(), cursor.into(), session).map(|mut m| {
        if m.coords.is_none() {
            let point = m.point;
            let src = session.load_raw_file(m.filepath.as_path());
            m.coords = src.point_to_coords(point);
        }
        m
    })
}

pub fn find_definition_(
    filepath: &path::Path,
    cursor: Location,
    session: &Session<'_>,
) -> Option<Match> {
    let src = session.load_source_file(filepath);
    let src_txt = &src[..];
    // TODO return result
    let pos = match cursor.to_point(&session.load_raw_file(filepath)) {
        Some(pos) => pos,
        None => {
            debug!("Failed to convert cursor to point");
            return None;
        }
    };

    // Make sure `src` is in the cache
    let range = scopes::expand_search_expr(src_txt, pos);
    let expr = &src[range.to_range()];
    let (contextstr, searchstr, completetype) = scopes::split_into_context_and_completion(expr);
    debug!(
        "find_definition_ for |{:?}| |{:?}| {:?}",
        contextstr, searchstr, completetype
    );

    match completetype {
        CompletionType::Path => {
            let (stmtstart, stmt) = &scopes::get_current_stmt(src.as_src(), range.end);
            let (path, namespace) = if let Some(use_start) = scopes::use_stmt_start(stmt) {
                let path = scopes::construct_path_from_use_tree(&stmt[use_start.0..]);
                (path, Namespace::Path)
            } else if let Some(str_path) = scopes::is_in_struct_ctor(src.as_src(), *stmtstart, pos)
            {
                let path = scopes::expr_to_path(&src[str_path.to_range()]).0;
                return nameres::get_struct_fields(
                    &path,
                    searchstr,
                    filepath,
                    pos,
                    SearchType::StartsWith,
                    session,
                )
                .into_iter()
                .next();
            } else {
                scopes::expr_to_path(expr)
            };
            debug!("[find_definition_] Path: {:?}", path);
            nameres::resolve_path(
                &path,
                filepath,
                pos,
                SearchType::ExactMatch,
                namespace,
                session,
                &ImportInfo::default(),
            )
            .into_iter()
            .nth(0)
        }
        CompletionType::Field => {
            let context = ast::get_type_of(contextstr.to_owned(), filepath, pos, session);
            debug!("context is {:?}", context);
            let only_method = src[range.end.0..].starts_with('(');
            context.and_then(|ty| {
                nameres::get_field_matches_from_ty(ty, searchstr, SearchType::ExactMatch, session)
                    .into_iter()
                    .filter(|m| !only_method || m.mtype.is_function())
                    .next()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FileCache;
    use super::{Session, SessionExt};
    use std::path::Path;

    #[test]
    fn overwriting_cached_files() {
        let src1 = "src1";
        let src2 = "src2";
        let src3 = "src3";
        let src4 = "src4";

        // Need session and path to cache files
        let path = Path::new("not_on_disk");
        let cache = FileCache::default();

        // Cache contents for a file and assert that load_file and load_file_and_mask_comments return
        // the newly cached contents.
        macro_rules! cache_and_assert {
            ($src: ident) => {{
                let session = Session::new(&cache, Some(path));
                session.cache_file_contents(path, $src);
                assert_eq!($src, &session.load_raw_file(path)[..]);
                assert_eq!($src, &session.load_source_file(path).code[..]);
            }};
        }

        // Check for all srcN
        cache_and_assert!(src1);
        cache_and_assert!(src2);
        cache_and_assert!(src3);
        cache_and_assert!(src4);
    }
}
