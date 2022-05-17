use crate::core::{BytePos, Match, MatchType, Namespace, SearchType, Session};
use crate::matchers::ImportInfo;
use crate::nameres::{self, RUST_SRC_PATH};
use rustc_ast::ast::{IntTy, LitIntType, UintTy};
use std::path::PathBuf;

const PRIM_DOC: &str = "std/src/primitive_docs.rs";
const KEY_DOC: &str = "std/src/keyword_docs.rs";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimKind {
    Bool,
    Never,
    Char,
    Unit,
    Pointer,
    Array,
    Slice,
    Str,
    Tuple,
    F32,
    F64,
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    Isize,
    Usize,
    Ref,
    Fn,
    Await,
}

const PRIM_MATCHES: [PrimKind; 17] = [
    PrimKind::Bool,
    PrimKind::Char,
    PrimKind::Str,
    PrimKind::F32,
    PrimKind::F64,
    PrimKind::I8,
    PrimKind::I16,
    PrimKind::I32,
    PrimKind::I64,
    PrimKind::I128,
    PrimKind::U8,
    PrimKind::U16,
    PrimKind::U32,
    PrimKind::U64,
    PrimKind::U128,
    PrimKind::Isize,
    PrimKind::Usize,
];

impl PrimKind {
    pub(crate) fn from_litint(lit: LitIntType) -> Self {
        match lit {
            LitIntType::Signed(i) => match i {
                IntTy::I8 => PrimKind::I8,
                IntTy::I16 => PrimKind::I16,
                IntTy::I32 => PrimKind::I32,
                IntTy::I64 => PrimKind::I64,
                IntTy::I128 => PrimKind::I128,
                IntTy::Isize => PrimKind::Isize,
            },
            LitIntType::Unsigned(u) => match u {
                UintTy::U8 => PrimKind::U8,
                UintTy::U16 => PrimKind::U16,
                UintTy::U32 => PrimKind::U32,
                UintTy::U64 => PrimKind::U64,
                UintTy::U128 => PrimKind::U128,
                UintTy::Usize => PrimKind::Usize,
            },
            LitIntType::Unsuffixed => PrimKind::U32,
        }
    }
    fn impl_files(self) -> Option<&'static [&'static str]> {
        match self {
            PrimKind::Bool => None,
            PrimKind::Never => None,
            PrimKind::Char => Some(&["core/src/char/methods.rs"]),
            PrimKind::Unit => None,
            PrimKind::Pointer => Some(&["core/src/ptr.rs"]),
            PrimKind::Array => None,
            PrimKind::Slice => Some(&["core/src/slice/mod.rs", "alloc/src/slice.rs"]),
            PrimKind::Str => Some(&["core/src/str/mod.rs", "alloc/src/str.rs"]),
            PrimKind::Tuple => None,
            PrimKind::F32 => Some(&["std/src/f32.rs", "core/src/num/f32.rs"]),
            PrimKind::F64 => Some(&["std/src/f64.rs", "core/src/num/f64.rs"]),
            PrimKind::I8 => Some(&["core/src/num/mod.rs"]),
            PrimKind::I16 => Some(&["core/src/num/mod.rs"]),
            PrimKind::I32 => Some(&["core/src/num/mod.rs"]),
            PrimKind::I64 => Some(&["core/src/num/mod.rs"]),
            PrimKind::I128 => Some(&["core/src/num/mod.rs"]),
            PrimKind::U8 => Some(&["core/src/num/mod.rs"]),
            PrimKind::U16 => Some(&["core/src/num/mod.rs"]),
            PrimKind::U32 => Some(&["core/src/num/mod.rs"]),
            PrimKind::U64 => Some(&["core/src/num/mod.rs"]),
            PrimKind::U128 => Some(&["core/src/num/mod.rs"]),
            PrimKind::Isize => Some(&["core/src/num/mod.rs"]),
            PrimKind::Usize => Some(&["core/src/num/mod.rs"]),
            PrimKind::Ref => None,
            PrimKind::Fn => None,
            PrimKind::Await => None,
        }
    }
    fn is_keyword(self) -> bool {
        match self {
            PrimKind::Await => true,
            _ => false,
        }
    }
    fn match_name(self) -> &'static str {
        match self {
            PrimKind::Bool => "bool",
            PrimKind::Never => "never",
            PrimKind::Char => "char",
            PrimKind::Unit => "unit",
            PrimKind::Pointer => "pointer",
            PrimKind::Array => "array",
            PrimKind::Slice => "slice",
            PrimKind::Str => "str",
            PrimKind::Tuple => "tuple",
            PrimKind::F32 => "f32",
            PrimKind::F64 => "f64",
            PrimKind::I8 => "i8",
            PrimKind::I16 => "i16",
            PrimKind::I32 => "i32",
            PrimKind::I64 => "i64",
            PrimKind::I128 => "i128",
            PrimKind::U8 => "u8",
            PrimKind::U16 => "u16",
            PrimKind::U32 => "u32",
            PrimKind::U64 => "u64",
            PrimKind::U128 => "u128",
            PrimKind::Isize => "isize",
            PrimKind::Usize => "usize",
            PrimKind::Ref => "ref",
            PrimKind::Fn => "fn",
            PrimKind::Await => "await",
        }
    }
    pub(crate) fn get_impl_files(&self) -> Option<Vec<PathBuf>> {
        let src_path = RUST_SRC_PATH.as_ref()?;
        let impls = self.impl_files()?;
        Some(impls.iter().map(|file| src_path.join(file)).collect())
    }
    pub fn to_module_match(self) -> Option<Match> {
        let _impl_files = self.impl_files()?;
        Some(Match {
            matchstr: self.match_name().to_owned(),
            filepath: PathBuf::new(),
            point: BytePos::ZERO,
            coords: None,
            local: false,
            mtype: MatchType::Builtin(self),
            contextstr: String::new(),
            docs: String::new(),
        })
    }
    pub fn to_doc_match(self, session: &Session<'_>) -> Option<Match> {
        let src_path = RUST_SRC_PATH.as_ref()?;
        let (path, seg) = if self.is_keyword() {
            (
                src_path.join(KEY_DOC),
                format!("{}_keyword", self.match_name()),
            )
        } else {
            (
                src_path.join(PRIM_DOC),
                format!("prim_{}", self.match_name()),
            )
        };
        let mut m = nameres::resolve_name(
            &seg.into(),
            &path,
            BytePos::ZERO,
            SearchType::ExactMatch,
            Namespace::Mod,
            session,
            &ImportInfo::default(),
        )
        .into_iter()
        .next()?;
        m.mtype = MatchType::Builtin(self);
        m.matchstr = self.match_name().to_owned();
        Some(m)
    }
}

pub fn get_primitive_docs(
    searchstr: &str,
    stype: SearchType,
    session: &Session<'_>,
    out: &mut Vec<Match>,
) {
    for prim in PRIM_MATCHES.iter() {
        let prim_str = prim.match_name();
        if (stype == SearchType::StartsWith && prim_str.starts_with(searchstr))
            || (stype == SearchType::ExactMatch && prim_str == searchstr)
        {
            if let Some(m) = prim.to_doc_match(session) {
                out.push(m);
                if stype == SearchType::ExactMatch {
                    return;
                }
            }
        }
    }
}

pub fn get_primitive_mods(searchstr: &str, stype: SearchType, out: &mut Vec<Match>) {
    for prim in PRIM_MATCHES.iter() {
        let prim_str = prim.match_name();
        if (stype == SearchType::StartsWith && prim_str.starts_with(searchstr))
            || (stype == SearchType::ExactMatch && prim_str == searchstr)
        {
            if let Some(matches) = prim.to_module_match() {
                out.push(matches);
                if stype == SearchType::ExactMatch {
                    return;
                }
            }
        }
    }
}
