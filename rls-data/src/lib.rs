use rls_span as span;

use std::path::PathBuf;

#[cfg(feature = "derive")]
use serde::{Deserialize, Serialize};

pub mod config;
pub use config::Config;

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
#[repr(C)]
pub struct Analysis {
    /// The Config used to generate this analysis data.
    pub config: Config,
    pub version: Option<String>,
    pub compilation: Option<CompilationOptions>,
    pub prelude: Option<CratePreludeData>,
    pub imports: Vec<Import>,
    pub defs: Vec<Def>,
    pub impls: Vec<Impl>,
    pub refs: Vec<Ref>,
    pub macro_refs: Vec<MacroRef>,
    pub relations: Vec<Relation>,
}

impl Analysis {
    /// Returns an initialized `Analysis` struct with `config` and also
    /// `version` field to Cargo package version.
    pub fn new(config: Config) -> Analysis {
        Analysis {
            config,
            version: option_env!("CARGO_PKG_VERSION").map(ToString::to_string),
            ..Analysis::default()
        }
    }
}

// DefId::index is a newtype and so the JSON serialisation is ugly. Therefore
// we use our own Id which is the same, but without the newtype.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Id {
    pub krate: u32,
    pub index: u32,
}

/// Crate name, along with its disambiguator (128-bit hash) represents a globally
/// unique crate identifier, which should allow for differentiation between
/// different crate targets or versions and should point to the same crate when
/// pulled by different other, dependent crates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct GlobalCrateId {
    pub name: String,
    pub disambiguator: (u64, u64),
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct SpanData {
    pub file_name: PathBuf,
    pub byte_start: u32,
    pub byte_end: u32,
    pub line_start: span::Row<span::OneIndexed>,
    pub line_end: span::Row<span::OneIndexed>,
    // Character offset.
    pub column_start: span::Column<span::OneIndexed>,
    pub column_end: span::Column<span::OneIndexed>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct CompilationOptions {
    pub directory: PathBuf,
    pub program: String,
    pub arguments: Vec<String>,
    pub output: PathBuf,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct CratePreludeData {
    pub crate_id: GlobalCrateId,
    pub crate_root: String,
    pub external_crates: Vec<ExternalCrateData>,
    pub span: SpanData,
}

/// Data for external crates in the prelude of a crate.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct ExternalCrateData {
    /// Source file where the external crate is declared.
    pub file_name: String,
    /// A crate-local crate index of an external crate. Local crate index is
    /// always 0, so these should start from 1 and range should be contiguous,
    /// e.g. from 1 to n for n external crates.
    pub num: u32,
    pub id: GlobalCrateId,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Import {
    pub kind: ImportKind,
    pub ref_id: Option<Id>,
    pub span: SpanData,
    pub alias_span: Option<SpanData>,
    pub name: String,
    pub value: String,
    pub parent: Option<Id>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub enum ImportKind {
    ExternCrate,
    Use,
    GlobUse,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Def {
    pub kind: DefKind,
    pub id: Id,
    pub span: SpanData,
    pub name: String,
    pub qualname: String,
    pub value: String,
    pub parent: Option<Id>,
    pub children: Vec<Id>,
    pub decl_id: Option<Id>,
    pub docs: String,
    pub sig: Option<Signature>,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub enum DefKind {
    // value = variant names
    Enum,
    // value = enum name + variant name + types
    TupleVariant,
    // value = enum name + name + fields
    StructVariant,
    // value = variant name + types
    Tuple,
    // value = name + fields
    Struct,
    Union,
    // value = signature
    Trait,
    // value = type + generics
    Function,
    ForeignFunction,
    // value = type + generics
    Method,
    // No id, no value.
    Macro,
    // value = file_name
    Mod,
    // value = aliased type
    Type,
    // value = type and init expression (for all variable kinds).
    Local,
    Static,
    ForeignStatic,
    Const,
    Field,
    // no value
    ExternType,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Impl {
    pub id: u32,
    pub kind: ImplKind,
    pub span: SpanData,
    pub value: String,
    pub parent: Option<Id>,
    pub children: Vec<Id>,
    pub docs: String,
    pub sig: Option<Signature>,
    pub attributes: Vec<Attribute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub enum ImplKind {
    // impl Foo { ... }
    Inherent,
    // impl Bar for Foo { ... }
    Direct,
    // impl Bar for &Foo { ... }
    Indirect,
    // impl<T: Baz> Bar for T { ... }
    //   where Foo: Baz
    Blanket,
    // impl Bar for Baz { ... } or impl Baz { ... }, etc.
    //   where Foo: Deref<Target = Baz>
    // Args are name and id of Baz
    Deref(String, Id),
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Attribute {
    pub value: String,
    pub span: SpanData,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Ref {
    pub kind: RefKind,
    pub span: SpanData,
    pub ref_id: Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub enum RefKind {
    Function,
    Mod,
    Type,
    Variable,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct MacroRef {
    pub span: SpanData,
    pub qualname: String,
    pub callee_span: SpanData,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Relation {
    pub span: SpanData,
    pub kind: RelationKind,
    pub from: Id,
    pub to: Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub enum RelationKind {
    Impl { id: u32 },
    SuperTrait,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct Signature {
    pub text: String,
    pub defs: Vec<SigElement>,
    pub refs: Vec<SigElement>,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "derive", derive(Serialize, Deserialize))]
pub struct SigElement {
    pub id: Id,
    pub start: usize,
    pub end: usize,
}
