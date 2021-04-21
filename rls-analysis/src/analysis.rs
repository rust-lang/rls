use fst;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::iter;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::raw::{CrateId, DefKind};
use crate::{Id, Span, SymbolQuery};
use span::{Column, Row, ZeroIndexed};

/// This is the main database that contains all the collected symbol information,
/// such as definitions, their mapping between spans, hierarchy and so on,
/// organized in a per-crate fashion.
pub(crate) struct Analysis {
    /// Contains lowered data with global inter-crate `Id`s per each crate.
    pub per_crate: HashMap<CrateId, PerCrateAnalysis>,

    // This is a bit of a hack and should be considered temporary. A def has an
    // entry if there exists an import of the def which aliases it. We use this
    // for find_all_refs with unique spans to ensure that clients don't rename a
    // definition when they only mean to rename an alias.
    //
    // In the future we should handle imports, in particular aliasing ones, more
    // explicitly and then this can be removed.
    pub(crate) aliased_imports: HashSet<Id>,

    // Maps a crate names to the crate ids for all crates with that name.
    pub(crate) crate_names: HashMap<String, Vec<CrateId>>,

    pub doc_url_base: String,
    pub src_url_base: String,
}

pub struct PerCrateAnalysis {
    // Map span to id of def (either because it is the span of the def, or of
    // the def for the ref).
    pub def_id_for_span: HashMap<Span, Ref>,
    pub defs: HashMap<Id, Def>,
    pub defs_per_file: HashMap<PathBuf, Vec<Id>>,
    pub children: HashMap<Id, HashSet<Id>>,
    pub def_names: HashMap<String, Vec<Id>>,

    // Index of all symbols that powers the search.
    // See `SymbolQuery`.
    pub def_fst: fst::Map<Vec<u8>>,
    pub def_fst_values: Vec<Vec<Id>>,

    pub ref_spans: HashMap<Id, Vec<Span>>,
    pub globs: HashMap<Span, Glob>,
    pub impls: HashMap<Id, Vec<Span>>,
    pub idents: HashMap<PathBuf, IdentsByLine>,

    pub root_id: Option<Id>,
    pub timestamp: SystemTime,
    pub path: Option<PathBuf>,
    // All definitions in this crate will include the global_crate_num. See
    // lowering::id_from_compiler_id for details of how.
    // global_crate_num is not available until after lowering.
    pub global_crate_num: u32,
}

#[derive(Debug, Clone)]
pub enum Ref {
    // The common case - a reference to a single definition.
    Id(Id),
    // Two defs contribute to a single reference, occurs in the field name
    // shorthand, maybe other places.
    Double(Id, Id),
    // Multiple ids, we record only the first def we found, plus a count of defs.
    // Should only happen due to generated code which has not been well-filtered
    // by the compiler.
    Multi(Id, usize),
}

impl Ref {
    pub fn some_id(&self) -> Id {
        match *self {
            Ref::Id(id) => id,
            Ref::Double(id, _) => id,
            Ref::Multi(id, _) => id,
        }
    }

    pub fn add_id(&self, def_id: Id) -> Ref {
        match *self {
            Ref::Id(id) => Ref::Double(id, def_id),
            Ref::Double(id, _) => Ref::Multi(id, 3),
            Ref::Multi(id, n) => Ref::Multi(id, n + 1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Def {
    pub kind: DefKind,
    pub span: Span,
    pub name: String,
    pub qualname: String,
    pub distro_crate: bool,
    pub parent: Option<Id>,
    pub value: String,
    pub docs: String,
    // pub sig: Option<Signature>,
}

pub type IdentsByLine = BTreeMap<Row<ZeroIndexed>, IdentsByColumn>;
pub type IdentsByColumn = BTreeMap<Column<ZeroIndexed>, IdentBound>;

/// We store the identifiers for a file in a BTreeMap ordered by starting index.
/// This struct contains the rest of the information we need to create an `Ident`.
///
/// We're optimising for space, rather than speed (of getting an Ident), because
/// we have to build the whole index for every file (which is a lot for a large
/// project), whereas we only get idents a few at a time and not very often.
#[derive(new, Clone, Debug)]
pub struct IdentBound {
    pub column_end: Column<ZeroIndexed>,
    pub id: Id,
    pub kind: IdentKind,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IdentKind {
    Def,
    Ref,
}

/// An identifier (either a reference or definition).
///
/// This struct represents the syntactic name, use the `id` to look up semantic
/// information.
#[derive(new, Clone, Debug)]
pub struct Ident {
    pub span: Span,
    pub id: Id,
    pub kind: IdentKind,
}

#[derive(Debug, Clone)]
pub struct Signature {
    pub span: Span,
    pub text: String,
    pub ident_start: u32,
    pub ident_end: u32,
    pub defs: Vec<SigElement>,
    pub refs: Vec<SigElement>,
}

#[derive(Debug, Clone)]
pub struct SigElement {
    pub id: Id,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct Glob {
    pub value: String,
}

impl PerCrateAnalysis {
    pub fn new(timestamp: SystemTime, path: Option<PathBuf>) -> PerCrateAnalysis {
        let empty_fst = fst::Map::from_iter(iter::empty::<(String, u64)>()).unwrap();
        PerCrateAnalysis {
            def_id_for_span: HashMap::new(),
            defs: HashMap::new(),
            defs_per_file: HashMap::new(),
            children: HashMap::new(),
            def_names: HashMap::new(),
            def_fst: empty_fst,
            def_fst_values: Vec::new(),
            ref_spans: HashMap::new(),
            globs: HashMap::new(),
            impls: HashMap::new(),
            idents: HashMap::new(),
            root_id: None,
            timestamp,
            path,
            global_crate_num: 0,
        }
    }

    // Returns true if there is a def in this crate with the same crate-local id
    // and span as `def`.
    pub(crate) fn has_congruent_def(&self, local_id: u32, span: &Span) -> bool {
        let id = Id::from_crate_and_local(self.global_crate_num, local_id);
        match self.defs.get(&id) {
            Some(existing) => span == &existing.span,
            None => false,
        }
    }

    // Returns all identifiers which overlap with `span`. There is no guarantee about
    // the ordering of identifiers in the result, but they will probably be roughly
    // in order of appearance.
    #[cfg(feature = "idents")]
    fn idents(&self, span: &Span) -> Vec<Ident> {
        self.idents
            .get(&span.file)
            .map(|by_line| {
                (span.range.row_start..=span.range.row_end)
                    .flat_map(|line| {
                        let vec = by_line
                            .get(&line)
                            .iter()
                            .flat_map(|by_col| {
                                by_col.into_iter().filter_map(|(col_start, id)| {
                                    if col_start <= &span.range.col_end
                                        && id.column_end >= span.range.col_start
                                    {
                                        Some(Ident::new(
                                            Span::new(
                                                line,
                                                line,
                                                *col_start,
                                                id.column_end,
                                                span.file.clone(),
                                            ),
                                            id.id,
                                            id.kind,
                                        ))
                                    } else {
                                        None
                                    }
                                })
                            })
                            .collect::<Vec<Ident>>();
                        vec.into_iter()
                    })
                    .collect::<Vec<Ident>>()
            })
            .unwrap_or_else(Vec::new)
    }
}

impl Analysis {
    pub fn new() -> Analysis {
        Analysis {
            per_crate: HashMap::new(),
            aliased_imports: HashSet::new(),
            crate_names: HashMap::new(),
            // TODO don't hardcode these
            doc_url_base: "https://doc.rust-lang.org/nightly".to_owned(),
            src_url_base: "https://github.com/rust-lang/rust/blob/master".to_owned(),
        }
    }

    pub fn timestamps(&self) -> HashMap<PathBuf, SystemTime> {
        self.per_crate
            .values()
            .filter(|c| c.path.is_some())
            .map(|c| (c.path.as_ref().unwrap().clone(), c.timestamp))
            .collect()
    }

    pub fn update(&mut self, crate_id: CrateId, per_crate: PerCrateAnalysis) {
        self.per_crate.insert(crate_id, per_crate);
    }

    pub fn has_def(&self, id: Id) -> bool {
        self.per_crate.values().any(|c| c.defs.contains_key(&id))
    }

    pub fn for_each_crate<F, T>(&self, f: F) -> Option<T>
    where
        F: Fn(&PerCrateAnalysis) -> Option<T>,
    {
        let mut result = vec![];
        for per_crate in self.per_crate.values() {
            if let Some(t) = f(per_crate) {
                result.push(t);
            }
        }

        // This assertion is sometimes helpful for debugging, but also can cause
        // problems where otherwise there are none.
        // FIXME - might be worth investigating some common causes.
        // assert!(
        //     result.len() <= 1,
        //     "error in for_each_crate, found {} results, expected 0 or 1",
        //     result.len(),
        // );
        result.into_iter().nth(0)
    }

    pub fn for_all_crates<F, T>(&self, f: F) -> Vec<T>
    where
        F: Fn(&PerCrateAnalysis) -> Option<Vec<T>>,
    {
        let mut result = vec![];
        for per_crate in self.per_crate.values() {
            if let Some(this_crate) = f(per_crate) {
                result.extend(this_crate);
            }
        }

        result
    }

    pub fn def_id_for_span(&self, span: &Span) -> Option<Id> {
        self.ref_for_span(span).map(|r| r.some_id())
    }

    pub fn ref_for_span(&self, span: &Span) -> Option<Ref> {
        self.for_each_crate(|c| c.def_id_for_span.get(span).cloned())
    }

    // Like def_id_for_span, but will only return a def_id if it is in the same
    // crate.
    pub fn local_def_id_for_span(&self, span: &Span) -> Option<Id> {
        self.for_each_crate(|c| {
            c.def_id_for_span.get(span).map(Ref::some_id).and_then(|id| {
                if c.defs.contains_key(&id) {
                    Some(id)
                } else {
                    None
                }
            })
        })
    }

    pub fn with_defs<F, T>(&self, id: Id, f: F) -> Option<T>
    where
        F: Fn(&Def) -> T,
    {
        self.for_each_crate(|c| c.defs.get(&id).map(&f))
    }

    pub fn with_defs_and_then<F, T>(&self, id: Id, f: F) -> Option<T>
    where
        F: Fn(&Def) -> Option<T>,
    {
        self.for_each_crate(|c| c.defs.get(&id).and_then(&f))
    }

    pub fn with_globs<F, T>(&self, span: &Span, f: F) -> Option<T>
    where
        F: Fn(&Glob) -> T,
    {
        self.for_each_crate(|c| c.globs.get(span).map(&f))
    }

    pub fn for_each_child<F, T>(&self, id: Id, mut f: F) -> Option<Vec<T>>
    where
        F: FnMut(Id, &Def) -> T,
    {
        for per_crate in self.per_crate.values() {
            if let Some(children) = per_crate.children.get(&id) {
                return Some(
                    children
                        .iter()
                        .filter_map(|id| {
                            let def = per_crate.defs.get(id);
                            if def.is_none() {
                                info!("def not found for {}", id);
                            }
                            def.map(|def| f(*id, def))
                        })
                        .collect(),
                );
            }
        }

        Some(vec![])
    }

    pub fn with_ref_spans<F, T>(&self, id: Id, f: F) -> Option<T>
    where
        F: Fn(&Vec<Span>) -> Option<T>,
    {
        self.for_each_crate(|c| c.ref_spans.get(&id).and_then(&f))
    }

    pub fn with_defs_per_file<F, T>(&self, file: &Path, f: F) -> Option<T>
    where
        F: Fn(&Vec<Id>) -> T,
    {
        self.for_each_crate(|c| c.defs_per_file.get(file).map(&f))
    }

    #[cfg(feature = "idents")]
    pub fn idents(&self, span: &Span) -> Vec<Ident> {
        self.for_each_crate(|c| {
            let result = c.idents(span);
            if result.is_empty() {
                None
            } else {
                Some(result)
            }
        })
        .unwrap_or_else(Vec::new)
    }

    pub fn query_defs(&self, query: SymbolQuery) -> Vec<Def> {
        let mut crates = Vec::with_capacity(self.per_crate.len());
        let stream = query.build_stream(self.per_crate.values().map(|c| {
            crates.push(c);
            &c.def_fst
        }));

        query.search_stream(stream, |acc, e| {
            let c = &crates[e.index];
            let ids = &c.def_fst_values[e.value as usize];
            acc.extend(ids.iter().flat_map(|id| c.defs.get(id)).cloned());
        })
    }

    pub fn with_def_names<F, T>(&self, name: &str, f: F) -> Vec<T>
    where
        F: Fn(&Vec<Id>) -> Vec<T>,
    {
        self.for_all_crates(|c| c.def_names.get(name).map(&f))
    }
}
