#![warn(rust_2018_idioms)]

#[macro_use]
extern crate derive_new;
#[macro_use]
extern crate log;

extern crate rls_data as data;
extern crate rls_span as span;

mod analysis;
mod listings;
mod loader;
mod lowering;
mod raw;
mod symbol_query;
#[cfg(test)]
mod test;
mod util;

use analysis::Analysis;
pub use analysis::{Def, Ident, IdentKind, Ref};
pub use loader::{AnalysisLoader, CargoAnalysisLoader, SearchDirectory, Target};
pub use raw::{
    deserialize_crate_data, name_space_for_def_kind, read_analysis_from_files, read_crate_data,
    Crate, CrateId, DefKind,
};
pub use symbol_query::SymbolQuery;

use std::collections::HashMap;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};
use std::u64;

pub struct AnalysisHost<L: AnalysisLoader = CargoAnalysisLoader> {
    analysis: Mutex<Option<Analysis>>,
    master_crate_map: Mutex<HashMap<CrateId, u32>>,
    loader: Mutex<L>,
}

pub type AResult<T> = Result<T, AError>;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AError {
    MutexPoison,
    Unclassified,
}

#[derive(Debug, Clone)]
pub struct SymbolResult {
    pub id: Id,
    pub name: String,
    pub kind: raw::DefKind,
    pub span: Span,
    pub parent: Option<Id>,
}

impl SymbolResult {
    fn new(id: Id, def: &Def) -> SymbolResult {
        SymbolResult {
            id,
            name: def.name.clone(),
            span: def.span.clone(),
            kind: def.kind,
            parent: def.parent,
        }
    }
}

pub type Span = span::Span<span::ZeroIndexed>;

/// A common identifier for definitions, references etc. This is effectively a
/// `DefId` with globally unique crate number (instead of a compiler generated
/// crate-local number).
#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash, new)]
pub struct Id(u64);

impl Id {
    fn from_crate_and_local(crate_id: u32, local_id: u32) -> Id {
        // Use global crate number for high order bits,
        // then index for least significant bits.
        Id((u64::from(crate_id) << 32) | u64::from(local_id))
    }
}

/// Used to indicate a missing index in the Id.
pub const NULL: Id = Id(u64::MAX);

macro_rules! clone_field {
    ($field: ident) => {
        |x| x.$field.clone()
    };
}

macro_rules! def_span {
    ($analysis: expr, $id: expr) => {
        $analysis.with_defs_and_then($id, |def| Some(def.span.clone()))
    };
}

impl AnalysisHost<CargoAnalysisLoader> {
    pub fn new(target: Target) -> AnalysisHost {
        AnalysisHost {
            analysis: Mutex::new(None),
            master_crate_map: Mutex::new(HashMap::new()),
            loader: Mutex::new(CargoAnalysisLoader::new(target)),
        }
    }
}

impl<L: AnalysisLoader> AnalysisHost<L> {
    pub fn new_with_loader(loader: L) -> Self {
        Self {
            analysis: Mutex::new(None),
            master_crate_map: Mutex::new(HashMap::new()),
            loader: Mutex::new(loader),
        }
    }

    /// Reloads given data passed in `analysis`. This will first check and read
    /// on-disk data (just like `reload`). It then imports the data we're
    /// passing in directly.
    pub fn reload_from_analysis(
        &self,
        analysis: Vec<data::Analysis>,
        path_prefix: &Path,
        base_dir: &Path,
        blacklist: &[impl AsRef<str> + Debug],
    ) -> AResult<()> {
        self.reload_with_blacklist(path_prefix, base_dir, blacklist)?;

        let crates: Vec<_> = analysis
            .into_iter()
            .map(|analysis| raw::Crate::new(analysis, SystemTime::now(), None, None))
            .collect();

        lowering::lower(crates, base_dir, self, |host, per_crate, id| {
            let mut a = host.analysis.lock()?;
            a.as_mut().unwrap().update(id, per_crate);
            Ok(())
        })
    }

    pub fn reload(&self, path_prefix: &Path, base_dir: &Path) -> AResult<()> {
        self.reload_with_blacklist(path_prefix, base_dir, &[] as &[&str])
    }

    pub fn reload_with_blacklist(
        &self,
        path_prefix: &Path,
        base_dir: &Path,
        blacklist: &[impl AsRef<str> + Debug],
    ) -> AResult<()> {
        trace!("reload_with_blacklist {:?} {:?} {:?}", path_prefix, base_dir, blacklist);
        let empty = self.analysis.lock()?.is_none();
        if empty || self.loader.lock()?.needs_hard_reload(path_prefix) {
            return self.hard_reload_with_blacklist(path_prefix, base_dir, blacklist);
        }

        let timestamps = self.analysis.lock()?.as_ref().unwrap().timestamps();
        let raw_analysis = {
            let loader = self.loader.lock()?;
            read_analysis_from_files(&*loader, timestamps, blacklist)
        };

        lowering::lower(raw_analysis, base_dir, self, |host, per_crate, id| {
            let mut a = host.analysis.lock()?;
            a.as_mut().unwrap().update(id, per_crate);
            Ok(())
        })
    }

    /// Reloads the entire project's analysis data.
    pub fn hard_reload(&self, path_prefix: &Path, base_dir: &Path) -> AResult<()> {
        self.hard_reload_with_blacklist(path_prefix, base_dir, &[] as &[&str])
    }

    pub fn hard_reload_with_blacklist(
        &self,
        path_prefix: &Path,
        base_dir: &Path,
        blacklist: &[impl AsRef<str> + Debug],
    ) -> AResult<()> {
        trace!("hard_reload {:?} {:?}", path_prefix, base_dir);
        // We're going to create a dummy AnalysisHost that we will fill with data,
        // then once we're done, we'll swap its data into self.
        let mut fresh_host = self.loader.lock()?.fresh_host();
        fresh_host.analysis = Mutex::new(Some(Analysis::new()));

        {
            let mut fresh_loader = fresh_host.loader.lock().unwrap();
            fresh_loader.set_path_prefix(path_prefix); // TODO: Needed?

            let raw_analysis = read_analysis_from_files(&*fresh_loader, HashMap::new(), blacklist);
            lowering::lower(raw_analysis, base_dir, &fresh_host, |host, per_crate, id| {
                let mut a = host.analysis.lock()?;
                a.as_mut().unwrap().update(id, per_crate);
                Ok(())
            })?;
        }

        // To guarantee a consistent state and no corruption in case an error
        // happens during reloading, we need to swap data with a dummy host in
        // a single atomic step. We can't lock and swap every member at a time,
        // as this can possibly lead to inconsistent state, but now this can possibly
        // deadlock, which isn't that good. Ideally we should have guaranteed
        // exclusive access to AnalysisHost as a whole to perform a reliable swap.
        macro_rules! swap_mutex_fields {
            ($($name:ident),*) => {
                // First, we need exclusive access to every field before swapping
                $(let mut $name = self.$name.lock()?;)*
                // Then, we can swap every field
                $(*$name = fresh_host.$name.into_inner().unwrap();)*
            };
        }

        swap_mutex_fields!(analysis, master_crate_map, loader);

        Ok(())
    }

    /// Note that `self.has_def()` =/> `self.goto_def().is_ok()`, since if the
    /// Def is in an api crate, there is no reasonable Span to jump to.
    pub fn has_def(&self, id: Id) -> bool {
        match self.analysis.lock() {
            Ok(a) => a.as_ref().unwrap().has_def(id),
            _ => false,
        }
    }

    pub fn get_def(&self, id: Id) -> AResult<Def> {
        self.with_analysis(|a| a.with_defs(id, Clone::clone))
    }

    pub fn goto_def(&self, span: &Span) -> AResult<Span> {
        self.with_analysis(|a| a.def_id_for_span(span).and_then(|id| def_span!(a, id)))
    }

    pub fn for_each_child_def<F, T>(&self, id: Id, f: F) -> AResult<Vec<T>>
    where
        F: FnMut(Id, &Def) -> T,
    {
        self.with_analysis(|a| a.for_each_child(id, f))
    }

    pub fn def_parents(&self, id: Id) -> AResult<Vec<(Id, String)>> {
        self.with_analysis(|a| {
            let mut result = vec![];
            let mut next = id;
            loop {
                match a.with_defs_and_then(next, |def| {
                    def.parent.and_then(|p| a.with_defs(p, |def| (p, def.name.clone())))
                }) {
                    Some((id, name)) => {
                        result.insert(0, (id, name));
                        next = id;
                    }
                    None => {
                        return Some(result);
                    }
                }
            }
        })
    }

    /// Returns the name of each crate in the program and the id of the root
    /// module of that crate.
    pub fn def_roots(&self) -> AResult<Vec<(Id, String)>> {
        self.with_analysis(|a| {
            Some(
                a.per_crate
                    .iter()
                    .filter_map(|(crate_id, data)| {
                        data.root_id.map(|id| (id, crate_id.name.clone()))
                    })
                    .collect(),
            )
        })
    }

    pub fn id(&self, span: &Span) -> AResult<Id> {
        self.with_analysis(|a| a.def_id_for_span(span))
    }

    /// Like id, but will only return a value if it is in the same crate as span.
    pub fn crate_local_id(&self, span: &Span) -> AResult<Id> {
        self.with_analysis(|a| a.local_def_id_for_span(span))
    }

    // `include_decl` means the declaration will be included as the first result.
    // `force_unique_spans` means that if any reference is a reference to multiple
    // defs, then we return an empty vector (in which case, even if include_decl
    // is true, the result will be empty).
    // Note that for large numbers of refs, if `force_unique_spans` is true, then
    // this function might take significantly longer to execute.
    pub fn find_all_refs(
        &self,
        span: &Span,
        include_decl: bool,
        force_unique_spans: bool,
    ) -> AResult<Vec<Span>> {
        let t_start = Instant::now();
        let result = self.with_analysis(|a| {
            a.def_id_for_span(span).map(|id| {
                if force_unique_spans && a.aliased_imports.contains(&id) {
                    return vec![];
                }
                let decl = if include_decl { def_span!(a, id) } else { None };
                let refs = a.with_ref_spans(id, |refs| {
                    if force_unique_spans {
                        for r in refs.iter() {
                            match a.ref_for_span(r) {
                                Some(Ref::Id(_)) => {}
                                _ => return None,
                            }
                        }
                    }
                    Some(refs.clone())
                });
                refs.map(|refs| decl.into_iter().chain(refs.into_iter()).collect::<Vec<_>>())
                    .unwrap_or_else(|| vec![])
            })
        });

        let time = t_start.elapsed();
        info!(
            "find_all_refs: {}s",
            time.as_secs() as f64 + f64::from(time.subsec_nanos()) / 1_000_000_000.0
        );
        result
    }

    pub fn show_type(&self, span: &Span) -> AResult<String> {
        self.with_analysis(|a| {
            a.def_id_for_span(span)
                .and_then(|id| a.with_defs(id, clone_field!(value)))
                .or_else(|| a.with_globs(span, clone_field!(value)))
        })
    }

    pub fn docs(&self, span: &Span) -> AResult<String> {
        self.with_analysis(|a| {
            a.def_id_for_span(span).and_then(|id| a.with_defs(id, clone_field!(docs)))
        })
    }

    /// Finds Defs with names that starting with (ignoring case) `stem`
    pub fn matching_defs(&self, stem: &str) -> AResult<Vec<Def>> {
        self.query_defs(SymbolQuery::prefix(stem))
    }

    pub fn query_defs(&self, query: SymbolQuery) -> AResult<Vec<Def>> {
        let t_start = Instant::now();
        let result = self.with_analysis(move |a| {
            let defs = a.query_defs(query);
            info!("query_defs {:?}", &defs);
            Some(defs)
        });

        let time = t_start.elapsed();
        info!(
            "query_defs: {}",
            time.as_secs() as f64 + f64::from(time.subsec_nanos()) / 1_000_000_000.0
        );

        result
    }

    /// Search for a symbol name, returns a list of spans matching defs and refs
    /// for that name.
    pub fn search(&self, name: &str) -> AResult<Vec<Span>> {
        let t_start = Instant::now();
        let result = self.with_analysis(|a| {
            Some(a.with_def_names(name, |defs| {
                info!("defs: {:?}", defs);
                defs.iter()
                    .flat_map(|id| {
                        a.with_ref_spans(*id, |refs| {
                            Some(
                                def_span!(a, *id)
                                    .into_iter()
                                    .chain(refs.iter().cloned())
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .or_else(|| def_span!(a, *id).map(|s| vec![s]))
                        .unwrap_or_else(Vec::new)
                        .into_iter()
                    })
                    .collect::<Vec<Span>>()
            }))
        });

        let time = t_start.elapsed();
        info!(
            "search: {}s",
            time.as_secs() as f64 + f64::from(time.subsec_nanos()) / 1_000_000_000.0
        );
        result
    }

    // TODO refactor search and find_all_refs to use this
    // Includes all references and the def, the def is always first.
    pub fn find_all_refs_by_id(&self, id: Id) -> AResult<Vec<Span>> {
        let t_start = Instant::now();
        let result = self.with_analysis(|a| {
            a.with_ref_spans(id, |refs| {
                Some(def_span!(a, id).into_iter().chain(refs.iter().cloned()).collect::<Vec<_>>())
            })
            .or_else(|| def_span!(a, id).map(|s| vec![s]))
        });

        let time = t_start.elapsed();
        info!(
            "find_all_refs_by_id: {}s",
            time.as_secs() as f64 + f64::from(time.subsec_nanos()) / 1_000_000_000.0
        );
        result
    }

    pub fn find_impls(&self, id: Id) -> AResult<Vec<Span>> {
        self.with_analysis(|a| Some(a.for_all_crates(|c| c.impls.get(&id).cloned())))
    }

    /// Search for a symbol name, returning a list of def_ids for that name.
    pub fn search_for_id(&self, name: &str) -> AResult<Vec<Id>> {
        self.with_analysis(|a| Some(a.with_def_names(name, Clone::clone)))
    }

    /// Returns all identifiers which overlap the given span.
    #[cfg(feature = "idents")]
    pub fn idents(&self, span: &Span) -> AResult<Vec<Ident>> {
        self.with_analysis(|a| Some(a.idents(span)))
    }

    pub fn symbols(&self, file_name: &Path) -> AResult<Vec<SymbolResult>> {
        self.with_analysis(|a| {
            a.with_defs_per_file(file_name, |ids| {
                ids.iter()
                    .map(|id| a.with_defs(*id, |def| SymbolResult::new(*id, def)).unwrap())
                    .collect()
            })
        })
    }

    pub fn doc_url(&self, span: &Span) -> AResult<String> {
        // e.g., https://doc.rust-lang.org/nightly/std/string/String.t.html
        self.with_analysis(|a| {
            a.def_id_for_span(span).and_then(|id| {
                a.with_defs_and_then(id, |def| AnalysisHost::<L>::mk_doc_url(def, a))
            })
        })
    }

    // e.g., https://github.com/rust-lang/rust/blob/master/src/liballoc/string.rs#L261-L263
    pub fn src_url(&self, span: &Span) -> AResult<String> {
        // FIXME would be nice not to do this every time.
        let path_prefix = self.loader.lock().unwrap().abs_path_prefix();

        self.with_analysis(|a| {
            a.def_id_for_span(span).and_then(|id| {
                a.with_defs_and_then(id, |def| {
                    AnalysisHost::<L>::mk_src_url(def, path_prefix.as_ref(), a)
                })
            })
        })
    }

    fn with_analysis<F, T>(&self, f: F) -> AResult<T>
    where
        F: FnOnce(&Analysis) -> Option<T>,
    {
        let a = self.analysis.lock()?;
        if let Some(ref a) = *a {
            f(a).ok_or(AError::Unclassified)
        } else {
            Err(AError::Unclassified)
        }
    }

    fn mk_doc_url(def: &Def, analysis: &Analysis) -> Option<String> {
        if !def.distro_crate {
            return None;
        }

        if def.parent.is_none() && def.qualname.contains('<') {
            debug!("mk_doc_url, bailing, found generic qualname: `{}`", def.qualname);
            return None;
        }

        match def.parent {
            Some(p) => analysis.with_defs(p, |parent| match def.kind {
                DefKind::Field
                | DefKind::Method
                | DefKind::Tuple
                | DefKind::TupleVariant
                | DefKind::StructVariant => {
                    let ns = name_space_for_def_kind(def.kind);
                    let mut res = AnalysisHost::<L>::mk_doc_url(parent, analysis)
                        .unwrap_or_else(|| "".into());
                    res.push_str(&format!("#{}.{}", def.name, ns));
                    res
                }
                DefKind::Mod => {
                    let parent_qualpath = parent.qualname.replace("::", "/");
                    format!(
                        "{}/{}/{}/",
                        analysis.doc_url_base,
                        parent_qualpath.trim_end_matches('/'),
                        def.name,
                    )
                }
                _ => {
                    let parent_qualpath = parent.qualname.replace("::", "/");
                    let ns = name_space_for_def_kind(def.kind);
                    format!(
                        "{}/{}/{}.{}.html",
                        analysis.doc_url_base, parent_qualpath, def.name, ns,
                    )
                }
            }),
            None => {
                let qualpath = def.qualname.replace("::", "/");
                let ns = name_space_for_def_kind(def.kind);
                Some(format!("{}/{}.{}.html", analysis.doc_url_base, qualpath, ns,))
            }
        }
    }

    fn mk_src_url(def: &Def, path_prefix: Option<&PathBuf>, analysis: &Analysis) -> Option<String> {
        if !def.distro_crate {
            return None;
        }

        let file_path = &def.span.file;
        let file_path = file_path.strip_prefix(path_prefix?).ok()?;

        Some(format!(
            "{}/{}#L{}-L{}",
            analysis.src_url_base,
            file_path.to_str().unwrap(),
            def.span.range.row_start.one_indexed().0,
            def.span.range.row_end.one_indexed().0
        ))
    }
}

impl ::std::fmt::Display for Id {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        ::std::fmt::Display::fmt(&self.0, f)
    }
}

impl ::std::error::Error for AError {}

impl ::std::fmt::Display for AError {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        let description = match self {
            AError::MutexPoison => "poison error in a mutex (usually a secondary error)",
            AError::Unclassified => "unknown error",
        };
        write!(f, "{}", description)
    }
}

impl<T> From<::std::sync::PoisonError<T>> for AError {
    fn from(_: ::std::sync::PoisonError<T>) -> AError {
        AError::MutexPoison
    }
}
