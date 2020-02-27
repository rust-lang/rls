extern crate rustc;
extern crate rustc_ast_pretty;
extern crate rustc_codegen_utils;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_lint;
extern crate rustc_save_analysis;
extern crate rustc_session;
extern crate rustc_span;
extern crate syntax;

use rustc::middle::cstore::ExternCrate;
use rustc::middle::privacy::AccessLevels;
use rustc::ty::{self, DefIdTree, TyCtxt};
use rustc_ast_pretty::pprust::{self, param_to_string, ty_to_string};
use rustc_codegen_utils::link::{filename_for_metadata, out_filename};
use rustc_data_structures::fx::{FxHashSet, FxHashMap};
use rustc_driver::Callbacks;
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
// use rustc_interface::{Config, interface::Compiler, Queries};
use rustc_interface::Config;
use rustc_lint::{EarlyContext, EarlyLintPass, LateContext, LateLintPass, Level, Lint, LintContext};
use rustc_save_analysis::SaveContext;
use rustc_session::{
    config::{CrateType, Input, OutputType},
    declare_lint, impl_lint_pass,
};
use rustc_span::hygiene::{ExpnId, SyntaxContext};
use rustc_span::{sym, ExpnKind, FileName, MacroKind, SourceFile, Span, ExpnData};
use syntax::{ast, util::comments::strip_doc_comment_decoration, visit};
use rustc_hir::{
    def_id::DefIndex,
    BodyId, FnDecl, GenericArg, GenericBound, GenericParam, GenericParamKind, Generics, ImplItem, ImplItemKind, Item,
    ItemKind, Lifetime, LifetimeName, ParamName, QPath, TraitBoundModifier, TraitItem, TraitItemKind, TraitMethod, Ty,
    TyKind, WhereClause, WherePredicate, Expr, Stmt, HirId, Crate,
};

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::borrow::Cow;

use rls_data::{
    Analysis, Attribute, CompilationOptions, CratePreludeData, Def, DefKind, ExternalCrateData,
    GlobalCrateId, Id, MacroRef, Ref, RefKind, Signature, SpanData,
};
use rls_span as span;

use super::dumper::{Access, Dumper};

declare_lint! {
    pub MACRO_DOCS,
    Allow,
    "gathers documentation for macros",
    report_in_external_macro
}

#[derive(Debug, Clone)]
pub struct MacroDocRef {
    pub name: String,
    pub id: ast::NodeId,
    pub span: Span,
}

unsafe impl Send for MacroDocRef {}

impl MacroDocRef {
    pub fn lower(self, ctxt: &TyCtxt<'_>) -> Ref {
        let MacroDocRef { name, id, span } = self;

        Ref {
            kind: RefKind::Macro,
            span: span_from_span(ctxt, span),
            ref_id: id_from_node_id(id, ctxt),
        }
    }
}

#[derive(Default, Debug)]
pub struct MacroDoc {
    pub defs: Arc<Mutex<Vec<Def>>>,
}

impl MacroDoc {
    pub(crate) fn new(defs: Arc<Mutex<Vec<Def>>>) -> Self {
        Self { defs }
    }
}

impl_lint_pass!(MacroDoc => [MACRO_DOCS]);

impl EarlyLintPass for MacroDoc {
    fn check_item(&mut self, ectx: &EarlyContext<'_>, item: &ast::Item) {
        if let ast::ItemKind::MacroDef(_) = &item.kind {
            let docs = docs_for_attrs(true, &item.attrs);

            let sm = ectx.sess.source_map();
            let filename = sm.span_to_filename(item.span);
            let name = item.ident.to_string();
            let id = item.id;

            println!("lint pass id {:?}", item.id);

            self.defs.lock().unwrap().push(Def {
                kind: DefKind::Macro,
                id: Id,
                span: span_from_span(ctxt, span),
                name: name,
                qualname: String,
                value: String,
                parent: Option<Id>,
                children: Vec<Id>,
                decl_id: Option<Id>,
                docs: String,
                sig: Option<Signature>,
                attributes: Vec<Attribute>,
            });
        }
    }
}

declare_lint! {
    pub LATE_MACRO_DOCS,
    Allow,
    "gathers documentation for macros",
    report_in_external_macro
}

#[derive(Clone, Debug, Default)]
pub struct LateMacroDocs {
    pub macro_map: FxHashMap<DefIndex, (HirId, String)>,
    pub defs: Arc<Mutex<Vec<Def>>>,
    pub refs: Arc<Mutex<Vec<MacroDocRef>>>,
}

impl LateMacroDocs {
    pub fn new(defs: Arc<Mutex<Vec<Def>>>, refs: Arc<Mutex<Vec<MacroDocRef>>>) -> Self {
        Self {
            macro_map: FxHashMap::default(),
            defs,
            refs,
        }
    }
}
impl_lint_pass!(LateMacroDocs => [LATE_MACRO_DOCS]);

impl<'a, 'tcx> LateLintPass<'a, 'tcx> for LateMacroDocs {
    // fn check_item(&mut self, ecx: &LateContext<'a, 'tcx>, item: &'tcx Item<'tcx>) {
    //     if in_macro(item.span) {
    //         println!("LATE PASS ITEM {:#?}", item);
    //     }
    // }

    fn check_expr(&mut self, lcx: &LateContext<'a, 'tcx>, expr: &'tcx Expr<'_>) {
        if in_macro(expr.span) && !self.macro_map.contains_key(&expr.hir_id.owner) {
            let mac_ref = MacroDocRef {
                name,
                span,
                id,
            };
            self.refs.lock().unwrap().push();
            println!("SOURCE {:#?}", expr.span.source_callee());
            println!("PARENT {:#?}", lcx.tcx.hir().get_parent_item(expr.hir_id));
            println!("SNIP {:#?}", snippet_with_macro_callsite(lcx, expr.span, "oops"));
            println!("GLOB {:#?}", lcx.tcx.names_imported_by_glob_use(expr.hir_id.owner_def_id()));
            println!("DEF PATH {:?}", lcx.get_def_path(expr.hir_id.owner_def_id()));
            let parent = lcx.tcx.hir().get_parent_item(expr.hir_id);
            // println!(
            //     "{:?}",
            //     snippet_with_macro_callsite(
            //         lcx,
            //         expr.span.with_def_site_ctxt(expr.span.source_callee().unwrap().parent),
            //         "oops",
            //     )
            // );
            if let Some(expnd) = expr.span.source_callee() {
                if let ExpnKind::Macro(kind, sym) = expnd.kind {
                    self.macro_map.insert(expr.hir_id.owner, (expr.hir_id, sym.to_string()));
                } else {
                    self.macro_map.insert(expr.hir_id.owner, (expr.hir_id, String::from("<unknown macro>")));
                }
            } else {
                self.macro_map.insert(expr.hir_id.owner, (expr.hir_id, String::from("<unknown macro>")));
            }
        }
    }
    // fn check_stmt(&mut self, lcx: &LateContext<'a, 'tcx>, stmt: &'tcx Stmt<'_>) {
    //     if in_macro(stmt.span) {
    //         println!("{:#?}", stmt);
    //     }
    // }

    fn check_crate_post(&mut self, lcx: &LateContext<'a, 'tcx>, k: &'tcx Crate<'tcx>) {
        let lkd_defs = self.defs.lock().unwrap();
        if !lkd_defs.is_empty() {
            for (hir_index, (hirid, name)) in self.macro_map.iter() {
                // println!("{:?}", lcx.tcx.hir().def_path_from_hir_id(*hirid));
                // println!("{:?}", snippet_with_macro_callsite(lcx, lcx.tcx.hir().span(*hirid), "oops"));
                // println!(
                //     "{:#?}",
                //     lcx.tcx.hir().span(*hirid).source_callee().unwrap()
                // );
            }
            for mac_def in lkd_defs.iter() {
                println!("MAC DEFS {:#?}", mac_def);
            }
            // println!("{:#?}", lcx.tcx.names_imported_by_glob_use(k.hir_id.owner_def_id()));
        }
    }
}

// Taken directly from `librustc_save_analysis`
//
//

/// Helper function to escape quotes in a string
fn escape(s: String) -> String {
    s.replace("\"", "\"\"")
}

pub fn in_macro(span: Span) -> bool {
    if span.from_expansion() {
        if let ExpnKind::Desugaring(..) = span.ctxt().outer_expn_data().kind {
            false
        } else {
            true
        }
    } else {
        false
    }
}

/// Converts a span to a code snippet if available, otherwise use default.
///
/// This is useful if you want to provide suggestions for your lint or more generally, if you want
/// to convert a given `Span` to a `str`.
///
/// # Example
/// ```rust,ignore
/// snippet(cx, expr.span, "..")
/// ```
pub fn snippet<'a, T: LintContext>(cx: &T, span: Span, default: &'a str) -> Cow<'a, str> {
    snippet_opt(cx, span).map_or_else(|| Cow::Borrowed(default), From::from)
}

/// Same as `snippet`, but should only be used when it's clear that the input span is
/// not a macro argument.
pub fn snippet_with_macro_callsite<'a, T: LintContext>(cx: &T, span: Span, default: &'a str) -> Cow<'a, str> {
    snippet(cx, span.source_callsite(), default)
}

/// Converts a span to a code snippet. Returns `None` if not available.
pub fn snippet_opt<T: LintContext>(cx: &T, span: Span) -> Option<String> {
    cx.sess().source_map().span_to_snippet(span).ok()
}

/// Helper function to determine if a span came from a
/// macro expansion or syntax extension.
fn generated_code(span: Span) -> bool {
    span.from_expansion() || span.is_dummy()
}
/// DefId::index is a newtype and so the JSON serialisation is ugly. Therefore
/// we use our own Id which is the same, but without the newtype.
fn id_from_def_id(id: DefId) -> rls_data::Id {
    rls_data::Id { krate: id.krate.as_u32(), index: id.index.as_u32() }
}

fn id_from_node_id(id: ast::NodeId, tcx: &TyCtxt<'_>) -> rls_data::Id {
    let def_id = tcx.hir().opt_local_def_id_from_node_id(id);
    def_id.map(|id| id_from_def_id(id)).unwrap_or_else(|| {
        // Create a *fake* `DefId` out of a `NodeId` by subtracting the `NodeId`
        // out of the maximum u32 value. This will work unless you have *billions*
        // of definitions in a single crate (very unlikely to actually happen).
        rls_data::Id { krate: LOCAL_CRATE.as_u32(), index: !id.as_u32() }
    })
}

fn null_id() -> rls_data::Id {
    rls_data::Id { krate: u32::max_value(), index: u32::max_value() }
}

fn lower_attributes(attrs: Vec<ast::Attribute>, tcx: &TyCtxt<'_>) -> Vec<rls_data::Attribute> {
    attrs
        .into_iter()
        // Only retain real attributes. Doc comments are lowered separately.
        .filter(|attr| !attr.has_name(sym::doc))
        .map(|mut attr| {
            // Remove the surrounding '#[..]' or '#![..]' of the pretty printed
            // attribute. First normalize all inner attribute (#![..]) to outer
            // ones (#[..]), then remove the two leading and the one trailing character.
            attr.style = ast::AttrStyle::Outer;
            let value = pprust::attribute_to_string(&attr);
            // This str slicing works correctly, because the leading and trailing characters
            // are in the ASCII range and thus exactly one byte each.
            let value = value[2..value.len() - 1].to_string();

            rls_data::Attribute { value, span: span_from_span(tcx, attr.span) }
        })
        .collect()
}

/// rustc::Span to rls::SpanData
fn span_from_span(tcx: &TyCtxt<'_>, span: Span) -> SpanData {
    use rls_span::{Column, Row};

    let cm = tcx.sess.source_map();
    let start = cm.lookup_char_pos(span.lo());
    let end = cm.lookup_char_pos(span.hi());

    SpanData {
        file_name: start.file.name.to_string().into(),
        byte_start: span.lo().0,
        byte_end: span.hi().0,
        line_start: Row::new_one_indexed(start.line as u32),
        line_end: Row::new_one_indexed(end.line as u32),
        column_start: Column::new_one_indexed(start.col.0 as u32 + 1),
        column_end: Column::new_one_indexed(end.col.0 as u32 + 1),
    }
}

/// Returns path to the compilation output (e.g., libfoo-12345678.rmeta)
pub fn compilation_output(tcx: &TyCtxt<'_>, crate_name: &str) -> PathBuf {
    let sess = &tcx.sess;
    // Save-analysis is emitted per whole session, not per each crate type
    let crate_type = sess.crate_types.borrow()[0];
    let outputs = &*tcx.output_filenames(LOCAL_CRATE);

    if outputs.outputs.contains_key(&OutputType::Metadata) {
        filename_for_metadata(sess, crate_name, outputs)
    } else if outputs.outputs.should_codegen() {
        out_filename(sess, crate_type, outputs, crate_name)
    } else {
        // Otherwise it's only a DepInfo, in which case we return early and
        // not even reach the analysis stage.
        unreachable!()
    }
}

/// List external crates used by the current crate.
pub fn get_external_crates(tcx: &TyCtxt<'_>) -> Vec<ExternalCrateData> {
    let mut result = Vec::with_capacity(tcx.crates().len());

    for &n in tcx.crates().iter() {
        let span = match tcx.extern_crate(n.as_def_id()) {
            Some(&ExternCrate { span, .. }) => span,
            None => {
                println!("skipping crate {}, no data", n);
                continue;
            }
        };
        let lo_loc = tcx.sess.source_map().lookup_char_pos(span.lo());
        result.push(ExternalCrateData {
            // FIXME: change file_name field to PathBuf in rls-data
            // https://github.com/nrc/rls-data/issues/7
            file_name: make_filename_string(tcx, &lo_loc.file),
            num: n.as_u32(),
            id: GlobalCrateId {
                name: tcx.crate_name(n).to_string(),
                disambiguator: tcx.crate_disambiguator(n).to_fingerprint().as_value(),
            },
        });
    }

    result
}

pub fn file_to_qualname(filename: &str) -> String {
    filename
        .to_string()
        .split('/')
        .last()
        .map(|s| {
            s.split('.')
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
                .first()
                .cloned()
                .unwrap_or_default()
        })
        .unwrap_or_else(|| String::new())
}

pub fn make_filename_string(tcx: &TyCtxt<'_>, file: &SourceFile) -> String {
    match &file.name {
        FileName::Real(path) if !file.name_was_remapped => {
            if path.is_absolute() {
                tcx.sess
                    .source_map()
                    .path_mapping()
                    .map_prefix(path.clone())
                    .0
                    .display()
                    .to_string()
            } else {
                tcx.sess.working_dir.0.join(&path).display().to_string()
            }
        }
        // If the file name is already remapped, we assume the user
        // configured it the way they wanted to, so use that directly
        filename => filename.to_string(),
    }
}

fn docs_for_attrs(full_docs: bool, attrs: &[ast::Attribute]) -> String {
    let mut result = String::new();

    for attr in attrs {
        if let Some(val) = attr.doc_str() {
            if attr.is_doc_comment() {
                result.push_str(&strip_doc_comment_decoration(&val.as_str()));
            } else {
                result.push_str(&val.as_str());
            }
            result.push('\n');
        } else if attr.check_name(sym::doc) {
            if let Some(meta_list) = attr.meta_item_list() {
                meta_list
                    .into_iter()
                    .filter(|it| it.check_name(sym::include))
                    .filter_map(|it| it.meta_item_list().map(|l| l.to_owned()))
                    .flat_map(|it| it)
                    .filter(|meta| meta.check_name(sym::contents))
                    .filter_map(|meta| meta.value_str())
                    .for_each(|val| {
                        result.push_str(&val.as_str());
                        result.push('\n');
                    });
            }
        }
    }

    if !full_docs {
        if let Some(index) = result.find("\n\n") {
            result.truncate(index);
        }
    }

    result
}
