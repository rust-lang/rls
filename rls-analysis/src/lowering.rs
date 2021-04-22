//! For processing the raw save-analysis data from rustc into the rls
//! in-memory representation.

use crate::analysis::{Def, Glob, PerCrateAnalysis, Ref};
#[cfg(feature = "idents")]
use crate::analysis::{IdentBound, IdentKind, IdentsByColumn, IdentsByLine};
use crate::loader::AnalysisLoader;
use crate::raw::{self, CrateId, DefKind, RelationKind};
use crate::util;
use crate::{AResult, AnalysisHost, Id, Span, NULL};

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::iter::Extend;
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::u32;

use fst;
use itertools::Itertools;

// f is a function used to record the lowered crate into analysis.
pub fn lower<F, L>(
    raw_analysis: Vec<raw::Crate>,
    base_dir: &Path,
    analysis: &AnalysisHost<L>,
    mut f: F,
) -> AResult<()>
where
    F: FnMut(&AnalysisHost<L>, PerCrateAnalysis, CrateId) -> AResult<()>,
    L: AnalysisLoader,
{
    let rss = util::get_resident().unwrap_or(0);
    let t_start = Instant::now();

    // Keep a queue of crates that we are yet to overwrite as part of the lowering
    // process (to know which already-existing defs we can overwrite and lower)
    let mut invalidated_crates: Vec<_> = raw_analysis.iter().map(|c| c.id.clone()).collect();

    for c in raw_analysis {
        let t_start = Instant::now();

        let (per_crate, id) = CrateReader::read_crate(analysis, c, base_dir, &invalidated_crates);
        invalidated_crates.retain(|elem| *elem != id);

        let time = t_start.elapsed();
        info!(
            "Lowering {} in {:.2}s",
            format!("{} ({:?})", id.name, id.disambiguator),
            time.as_secs() as f64 + f64::from(time.subsec_nanos()) / 1_000_000_000.0
        );
        info!("    defs:  {}", per_crate.defs.len());
        info!("    refs:  {}", per_crate.ref_spans.len());
        info!("    globs: {}", per_crate.globs.len());

        f(analysis, per_crate, id)?;
    }

    let time = t_start.elapsed();
    let rss = util::get_resident().unwrap_or(0) as isize - rss as isize;
    info!(
        "Total lowering time: {:.2}s",
        time.as_secs() as f64 + f64::from(time.subsec_nanos()) / 1_000_000_000.0
    );
    info!("Diff in rss: {:.2}KB", rss as f64 / 1000.0);

    Ok(())
}

fn lower_span(raw_span: &raw::SpanData, base_dir: &Path, path_rewrite: &Option<PathBuf>) -> Span {
    let file_name = &raw_span.file_name;

    // Go from relative to absolute paths.
    let file_name = if let Some(ref prefix) = *path_rewrite {
        // Invariant: !file_name.is_absolute()
        // We don't assert this because better to have an incorrect span than to
        // panic.
        let prefix = &Path::new(prefix);
        prefix.join(file_name)
    } else if file_name.is_absolute() {
        file_name.to_owned()
    } else {
        base_dir.join(file_name)
    };

    // Rustc uses 1-indexed rows and columns, the RLS uses 0-indexed.
    span::Span::new(
        raw_span.line_start.zero_indexed(),
        raw_span.line_end.zero_indexed(),
        raw_span.column_start.zero_indexed(),
        raw_span.column_end.zero_indexed(),
        file_name,
    )
}

/// Responsible for processing the raw `data::Analysis`, including translating
/// from local crate ids to global crate ids, and creating lowered
/// `PerCrateAnalysis`.
struct CrateReader<'a> {
    /// This is effectively a map from local crate id -> global crate id, where
    /// local crate id are indices 0...external_crate_count.
    crate_map: Vec<u32>,
    base_dir: PathBuf,
    crate_name: String,
    path_rewrite: Option<PathBuf>,
    crate_homonyms: Vec<CrateId>,
    /// List of crates that are invalidated (replaced) as part of the current
    /// lowering process. These will be overriden and their definitions should
    /// not be taken into account when checking if we need to ignore duplicated
    /// item.
    invalidated_crates: &'a [CrateId],
}

impl<'a> CrateReader<'a> {
    fn from_prelude(
        mut prelude: raw::CratePreludeData,
        master_crate_map: &mut HashMap<CrateId, u32>,
        base_dir: &Path,
        path_rewrite: Option<PathBuf>,
        invalidated_crates: &'a [CrateId],
    ) -> CrateReader<'a> {
        fn fetch_crate_index(map: &mut HashMap<CrateId, u32>, id: CrateId) -> u32 {
            let next = map.len() as u32;
            *map.entry(id).or_insert(next)
        }
        // When reading a local crate and its external crates, we need to:
        // 1. Update a global crate id map if we encounter any new crate
        // 2. Prepare a local crate id -> global crate id map, so we can easily
        // map those when lowering symbols with local crate ids into global registry
        // It's worth noting, that we assume that local crate id is 0, whereas
        // the external crates will have num in 1..count contiguous range.
        let crate_id = prelude.crate_id;
        trace!("building crate map for {:?}", crate_id);
        let index = fetch_crate_index(master_crate_map, crate_id.clone());
        let mut crate_map = vec![index];
        trace!("  {} -> {}", crate_id.name, master_crate_map[&crate_id]);

        prelude.external_crates.sort_by(|a, b| a.num.cmp(&b.num));
        for c in prelude.external_crates {
            assert!(c.num == crate_map.len() as u32);
            let index = fetch_crate_index(master_crate_map, c.id.clone());
            crate_map.push(index);
            trace!("  {} -> {}", c.id.name, master_crate_map[&c.id]);
        }

        CrateReader {
            crate_map,
            base_dir: base_dir.to_owned(),
            crate_homonyms: master_crate_map
                .keys()
                .filter(|cid| cid.name == crate_id.name)
                .cloned()
                .collect(),
            crate_name: crate_id.name,
            path_rewrite,
            invalidated_crates,
        }
    }

    /// Lowers a given `raw::Crate` into `AnalysisHost`.
    fn read_crate<L: AnalysisLoader>(
        project_analysis: &AnalysisHost<L>,
        krate: raw::Crate,
        base_dir: &Path,
        invalidated_crates: &[CrateId],
    ) -> (PerCrateAnalysis, CrateId) {
        let reader = CrateReader::from_prelude(
            krate.analysis.prelude.unwrap(),
            &mut project_analysis.master_crate_map.lock().unwrap(),
            base_dir,
            krate.path_rewrite,
            invalidated_crates,
        );

        let mut per_crate = PerCrateAnalysis::new(krate.timestamp, krate.path);

        let is_distro_crate = krate.analysis.config.distro_crate;
        reader.read_defs(krate.analysis.defs, &mut per_crate, is_distro_crate, project_analysis);
        reader.read_imports(krate.analysis.imports, &mut per_crate, project_analysis);
        reader.read_refs(krate.analysis.refs, &mut per_crate, project_analysis);
        reader.read_impls(krate.analysis.relations, &mut per_crate, project_analysis);
        per_crate.global_crate_num = reader.crate_map[0];

        {
            let analysis = &mut project_analysis.analysis.lock().unwrap();
            analysis
                .as_mut()
                .unwrap()
                .crate_names
                .entry(krate.id.name.clone())
                .or_insert_with(Vec::new)
                .push(krate.id.clone());
        }

        (per_crate, krate.id)
    }

    fn read_imports<L: AnalysisLoader>(
        &self,
        imports: Vec<raw::Import>,
        analysis: &mut PerCrateAnalysis,
        project_analysis: &AnalysisHost<L>,
    ) {
        for i in imports {
            let span = lower_span(&i.span, &self.base_dir, &self.path_rewrite);
            if !i.value.is_empty() {
                // A glob import.
                if !self.has_congruent_glob(&span, project_analysis) {
                    let glob = Glob { value: i.value };
                    trace!("record glob {:?} {:?}", span, glob);
                    analysis.globs.insert(span, glob);
                }
            } else if let Some(ref ref_id) = i.ref_id {
                // Import where we know the referred def.
                let def_id = self.id_from_compiler_id(*ref_id);
                self.record_ref(def_id, span, analysis, project_analysis);
                if let Some(alias_span) = i.alias_span {
                    let alias_span = lower_span(&alias_span, &self.base_dir, &self.path_rewrite);
                    self.record_ref(def_id, alias_span, analysis, project_analysis);
                    let mut analysis = project_analysis.analysis.lock().unwrap();
                    analysis.as_mut().unwrap().aliased_imports.insert(def_id);
                }
            }
        }
    }

    fn record_ref<L: AnalysisLoader>(
        &self,
        def_id: Id,
        span: Span,
        analysis: &mut PerCrateAnalysis,
        project_analysis: &AnalysisHost<L>,
    ) {
        if def_id != NULL
            && (project_analysis.has_def(def_id) || analysis.defs.contains_key(&def_id))
        {
            trace!("record_ref {:?} {}", span, def_id);
            match analysis.def_id_for_span.entry(span.clone()) {
                Entry::Occupied(mut oe) => {
                    let new = oe.get().add_id(def_id);
                    oe.insert(new);
                }
                Entry::Vacant(ve) => {
                    ve.insert(Ref::Id(def_id));
                }
            }

            #[cfg(feature = "idents")]
            {
                Self::record_ident(analysis, &span, def_id, IdentKind::Ref);
            }
            analysis.ref_spans.entry(def_id).or_insert_with(Vec::new).push(span);
        }
    }

    #[cfg(feature = "idents")]
    fn record_ident(analysis: &mut PerCrateAnalysis, span: &Span, id: Id, kind: IdentKind) {
        let row_start = span.range.row_start;
        let col_start = span.range.col_start;
        let col_end = span.range.col_end;
        analysis
            .idents
            .entry(span.file.clone())
            .or_insert_with(IdentsByLine::new)
            .entry(row_start)
            .or_insert_with(IdentsByColumn::new)
            .entry(col_start)
            .or_insert_with(|| IdentBound::new(col_end, id, kind));
    }

    // We are sometimes asked to analyze the same crate twice. This can happen due to duplicate data,
    // but more frequently is due to compiling it twice with different Cargo targets (e.g., bin and test).
    // In that case there will be two crates with the same names, but different disambiguators. We
    // want to ensure that we only record defs once, even if the defintion is in multiple crates.
    // So we compare the crate-local id and span and skip any subsequent defs which match already
    // present defs.
    fn has_congruent_def<L: AnalysisLoader>(
        &self,
        local_id: u32,
        span: &Span,
        project_analysis: &AnalysisHost<L>,
    ) -> bool {
        self.has_congruent_item(project_analysis, |per_crate| {
            per_crate.has_congruent_def(local_id, span)
        })
    }

    fn has_congruent_glob<L: AnalysisLoader>(
        &self,
        span: &Span,
        project_analysis: &AnalysisHost<L>,
    ) -> bool {
        self.has_congruent_item(project_analysis, |per_crate| per_crate.globs.contains_key(span))
    }

    fn has_congruent_item<L, P>(&self, project_analysis: &AnalysisHost<L>, pred: P) -> bool
    where
        L: AnalysisLoader,
        P: Fn(&PerCrateAnalysis) -> bool,
    {
        if self.crate_homonyms.is_empty() {
            return false;
        }

        let project_analysis = project_analysis.analysis.lock().unwrap();
        let project_analysis = project_analysis.as_ref().unwrap();

        // Don't take into account crates that we are about to replace as part
        // of the lowering. This often happens when we reload definitions for
        // the same crate. Naturally most of the definitions will stay the same
        // for incremental changes but will be overwritten - don't ignore them!
        let homonyms_to_consider =
            self.crate_homonyms.iter().filter(|c| !self.invalidated_crates.contains(c));

        homonyms_to_consider.filter_map(|ch| project_analysis.per_crate.get(ch)).any(pred)
    }

    fn read_defs<L: AnalysisLoader>(
        &self,
        defs: Vec<raw::Def>,
        analysis: &mut PerCrateAnalysis,
        distro_crate: bool,
        project_analysis: &AnalysisHost<L>,
    ) {
        let mut defs_to_index = Vec::new();
        for d in defs {
            if bad_span(&d.span, d.kind == DefKind::Mod) {
                continue;
            }
            let span = lower_span(&d.span, &self.base_dir, &self.path_rewrite);
            if self.has_congruent_def(d.id.index, &span, project_analysis) {
                trace!("read_defs: has_congruent_def({}, {:?}), skipping", d.id.index, span);
                continue;
            }

            let id = self.id_from_compiler_id(d.id);
            if id != NULL && !analysis.defs.contains_key(&id) {
                let file_name = span.file.clone();
                analysis.defs_per_file.entry(file_name).or_insert_with(Vec::new).push(id);
                let decl_id = match d.decl_id {
                    Some(ref decl_id) => {
                        let def_id = self.id_from_compiler_id(*decl_id);
                        analysis
                            .ref_spans
                            .entry(def_id)
                            .or_insert_with(Vec::new)
                            .push(span.clone());
                        Ref::Id(def_id)
                    }
                    None => Ref::Id(id),
                };
                match analysis.def_id_for_span.entry(span.clone()) {
                    Entry::Occupied(_) => {
                        debug!("def already exists at span: {:?} {:?}", span, d);
                    }
                    Entry::Vacant(ve) => {
                        ve.insert(decl_id);
                    }
                }

                analysis.def_names.entry(d.name.clone()).or_insert_with(Vec::new).push(id);

                // NOTE not every Def will have a name, e.g. test_data/hello/src/main is analyzed with an implicit module
                // that's fine, but no need to index in def_trie
                if d.name != "" {
                    defs_to_index.push((d.name.to_lowercase(), id));
                }

                let parent = d.parent.map(|id| self.id_from_compiler_id(id));
                if let Some(parent) = parent {
                    let children = analysis.children.entry(parent).or_insert_with(HashSet::new);
                    children.insert(id);
                }
                if !d.children.is_empty() {
                    let children_for_id = analysis.children.entry(id).or_insert_with(HashSet::new);
                    children_for_id
                        .extend(d.children.iter().map(|id| self.id_from_compiler_id(*id)));
                }

                #[cfg(feature = "idents")]
                {
                    Self::record_ident(analysis, &span, id, IdentKind::Def);
                }

                let def = Def {
                    kind: d.kind,
                    span,
                    name: d.name,
                    value: d.value,
                    qualname: format!("{}{}", self.crate_name, d.qualname),
                    distro_crate,
                    parent,
                    docs: d.docs,
                    // sig: d.sig.map(|ref s| self.lower_sig(s, &self.base_dir)),
                };
                trace!(
                    "record def: {:?}/{:?} ({}): {:?}",
                    id,
                    d.id,
                    self.crate_map[d.id.krate as usize],
                    def
                );

                if d.kind == super::raw::DefKind::Mod && def.name == "" {
                    assert!(analysis.root_id.is_none());
                    analysis.root_id = Some(id);
                }

                analysis.defs.insert(id, def);
            }
        }

        let (def_fst, def_fst_values) = build_index(defs_to_index);
        analysis.def_fst = def_fst;
        analysis.def_fst_values = def_fst_values;

        // We must now run a pass over the defs setting parents, because
        // save-analysis often omits parent info.
        for (parent, children) in &analysis.children {
            for c in children {
                if let Some(def) = analysis.defs.get_mut(c) {
                    def.parent = Some(*parent);
                }
            }
        }
    }

    fn read_refs<L: AnalysisLoader>(
        &self,
        refs: Vec<raw::Ref>,
        analysis: &mut PerCrateAnalysis,
        project_analysis: &AnalysisHost<L>,
    ) {
        for r in refs {
            if r.span.file_name.to_str().map(|s| s.ends_with('>')).unwrap_or(true) {
                continue;
            }
            let def_id = self.id_from_compiler_id(r.ref_id);
            let span = lower_span(&r.span, &self.base_dir, &self.path_rewrite);
            self.record_ref(def_id, span, analysis, project_analysis);
        }
    }

    fn read_impls<L: AnalysisLoader>(
        &self,
        relations: Vec<raw::Relation>,
        analysis: &mut PerCrateAnalysis,
        project_analysis: &AnalysisHost<L>,
    ) {
        for r in relations {
            match r.kind {
                RelationKind::Impl { .. } => {}
                _ => continue,
            }
            let self_id = self.id_from_compiler_id(r.from);
            let trait_id = self.id_from_compiler_id(r.to);
            let span = lower_span(&r.span, &self.base_dir, &self.path_rewrite);
            if self_id != NULL {
                if let Some(self_id) = abs_ref_id(self_id, analysis, project_analysis) {
                    trace!("record impl for self type {:?} {}", span, self_id);
                    analysis.impls.entry(self_id).or_insert_with(Vec::new).push(span.clone());
                }
            }
            if trait_id != NULL {
                if let Some(trait_id) = abs_ref_id(trait_id, analysis, project_analysis) {
                    trace!("record impl for trait {:?} {}", span, trait_id);
                    analysis.impls.entry(trait_id).or_insert_with(Vec::new).push(span);
                }
            }
        }
    }

    // fn lower_sig(&self, raw_sig: &raw::Signature, base_dir: &Path) -> Signature {
    //     Signature {
    //         span: lower_span(&raw_sig.span, base_dir, &self.path_rewrite),
    //         text: raw_sig.text.clone(),
    //         ident_start: raw_sig.ident_start as u32,
    //         ident_end: raw_sig.ident_end as u32,
    //         defs: raw_sig.defs.iter().map(|se| self.lower_sig_element(se)).collect(),
    //         refs: raw_sig.refs.iter().map(|se| self.lower_sig_element(se)).collect(),
    //     }
    // }

    // fn lower_sig_element(&self, raw_se: &raw::SigElement) -> SigElement {
    //     SigElement {
    //         id: self.id_from_compiler_id(raw_se.id),
    //         start: raw_se.start,
    //         end: raw_se.end,
    //     }
    // }

    /// Recreates resulting crate-local (`u32`, `u32`) id from compiler
    /// to a global `u64` `Id`, mapping from a local to global crate id.
    fn id_from_compiler_id(&self, id: data::Id) -> Id {
        if id.krate == u32::MAX || id.index == u32::MAX {
            return NULL;
        }

        let krate = self.crate_map[id.krate as usize];
        Id::from_crate_and_local(krate, id.index)
    }
}

fn abs_ref_id<L: AnalysisLoader>(
    id: Id,
    analysis: &PerCrateAnalysis,
    project_analysis: &AnalysisHost<L>,
) -> Option<Id> {
    if project_analysis.has_def(id) || analysis.defs.contains_key(&id) {
        return Some(id);
    }

    // TODO
    None
}

fn build_index(mut defs: Vec<(String, Id)>) -> (fst::Map<Vec<u8>>, Vec<Vec<Id>>) {
    defs.sort_by(|(n1, _), (n2, _)| n1.cmp(n2));
    let by_name = defs.into_iter().group_by(|(n, _)| n.clone());

    let mut values: Vec<Vec<Id>> = Vec::new();
    let fst = {
        let defs = by_name.into_iter().enumerate().map(|(i, (name, defs))| {
            values.push(defs.map(|(_, id)| id).collect());
            (name, i as u64)
        });
        fst::Map::from_iter(defs).expect("defs are sorted by lowercase name")
    };
    (fst, values)
}

fn bad_span(span: &raw::SpanData, is_mod: bool) -> bool {
    span.file_name.to_str().map(|s| s.ends_with('>')).unwrap_or(true)
        || (!is_mod && span.byte_start == 0 && span.byte_end == 0)
}
