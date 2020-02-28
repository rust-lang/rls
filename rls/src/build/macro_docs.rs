extern crate rustc;
extern crate rustc_ast_pretty;
extern crate rustc_data_structures;
extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_session;
extern crate rustc_span;
extern crate syntax;

use rustc::ty::TyCtxt;
use rustc_ast_pretty::pprust;
use rustc_data_structures::fx::FxHashSet;
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
// use rustc_interface::{Config, interface::Compiler, Queries};
use rustc_hir::{Expr, Item, Stmt};
use rustc_lint::{EarlyContext, EarlyLintPass, LateContext, LateLintPass};
use rustc_session::{declare_lint, impl_lint_pass};
use rustc_span::{sym, ExpnKind, Span};
use syntax::{ast, util::comments::strip_doc_comment_decoration};

use std::sync::{Arc, Mutex};

use rls_data::{Def, DefKind, Ref, RefKind, SpanData};

declare_lint! {
    pub MACRO_DOCS,
    Allow,
    "gathers documentation for macros",
    report_in_external_macro
}

#[derive(Debug, Clone)]
pub struct MacroDef {
    pub docs: String,
    pub name: String,
    file_name: String,
    pub id: ast::NodeId,
    pub span: Span,
    pub attrs: Vec<ast::Attribute>,
}

unsafe impl Send for MacroDef {}

impl MacroDef {
    pub fn lower(self, ctxt: TyCtxt<'_>) -> Def {
        let MacroDef { docs, name, file_name, id, span, attrs } = self;

        let qualname = format!("::{}", file_to_qualname(&file_name));
        let span = span_from_span(ctxt, span);
        let data_id = id_from_node_id(id, ctxt);

        Def {
            kind: DefKind::Macro,
            id: data_id,
            name: name.to_string(),
            qualname,
            span,
            value: format!("macro_rules! {} (args...)", name),
            children: Vec::default(),
            parent: None,
            decl_id: None,
            docs,
            sig: None,
            attributes: lower_attributes(attrs, ctxt),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MacroDocRef {
    pub name: String,
    pub id: rls_data::Id,
    pub span: Span,
    pub def_span: Span,
}

unsafe impl Send for MacroDocRef {}

impl MacroDocRef {
    pub fn lower(self, ctxt: TyCtxt<'_>) -> Ref {
        let MacroDocRef { id, span, .. } = self;

        Ref { kind: RefKind::Macro, span: span_from_span(ctxt, span), ref_id: id }
    }
}

#[derive(Default, Debug)]
pub struct MacroDoc {
    pub defs: Arc<Mutex<Vec<MacroDef>>>,
}

impl MacroDoc {
    pub(crate) fn new(defs: Arc<Mutex<Vec<MacroDef>>>) -> Self {
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

            self.defs.lock().unwrap().push(MacroDef {
                docs,
                name,
                file_name: filename.to_string(),
                id,
                span: item.span,
                attrs: item.attrs.clone(),
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
    pub macro_map: FxHashSet<Span>,
    pub defs: Arc<Mutex<Vec<MacroDef>>>,
    pub refs: Arc<Mutex<Vec<MacroDocRef>>>,
}

impl LateMacroDocs {
    pub fn new(defs: Arc<Mutex<Vec<MacroDef>>>, refs: Arc<Mutex<Vec<MacroDocRef>>>) -> Self {
        Self { macro_map: FxHashSet::default(), defs, refs }
    }
}
impl_lint_pass!(LateMacroDocs => [LATE_MACRO_DOCS]);

impl<'a, 'tcx> LateLintPass<'a, 'tcx> for LateMacroDocs {
    fn check_item(&mut self, _lcx: &LateContext<'a, 'tcx>, item: &'tcx Item<'tcx>) {
        if let Some(expn_data) = item.span.source_callee() {
            if in_macro(item.span) && !self.macro_map.contains(&expn_data.call_site) {
                self.macro_map.insert(expn_data.call_site);
                if let ExpnKind::Macro(_delim, name) = expn_data.kind {
                    let mac_ref = MacroDocRef {
                        name: name.to_string(),
                        span: expn_data.call_site,
                        id: null_id(),
                        def_span: expn_data.def_site,
                    };
                    self.refs.lock().unwrap().push(mac_ref);
                }
            }
        }
    }

    fn check_expr(&mut self, _lcx: &LateContext<'a, 'tcx>, expr: &'tcx Expr<'_>) {
        if let Some(expn_data) = expr.span.source_callee() {
            if in_macro(expr.span) && !self.macro_map.contains(&expn_data.call_site) {
                self.macro_map.insert(expn_data.call_site);
                if let ExpnKind::Macro(_delim, name) = expn_data.kind {
                    let mac_ref = MacroDocRef {
                        name: name.to_string(),
                        span: expn_data.call_site,
                        id: null_id(),
                        def_span: expn_data.def_site,
                    };
                    self.refs.lock().unwrap().push(mac_ref);
                }
            }
        }
    }
    fn check_stmt(&mut self, _lcx: &LateContext<'a, 'tcx>, stmt: &'tcx Stmt<'_>) {
        if let Some(expn_data) = stmt.span.source_callee() {
            if in_macro(stmt.span) && !self.macro_map.contains(&expn_data.call_site) {
                self.macro_map.insert(expn_data.call_site);
                if let ExpnKind::Macro(_delim, name) = expn_data.kind {
                    let mac_ref = MacroDocRef {
                        name: name.to_string(),
                        span: expn_data.call_site,
                        id: null_id(),
                        def_span: expn_data.def_site,
                    };
                    self.refs.lock().unwrap().push(mac_ref);
                }
            }
        }
    }
}

// Taken directly from `librustc_save_analysis` and `clippy_lints::utils`
//
//

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

/// DefId::index is a newtype and so the JSON serialisation is ugly. Therefore
/// we use our own Id which is the same, but without the newtype.
pub fn id_from_def_id(id: DefId) -> rls_data::Id {
    rls_data::Id { krate: id.krate.as_u32(), index: id.index.as_u32() }
}

pub fn id_from_node_id(id: ast::NodeId, tcx: TyCtxt<'_>) -> rls_data::Id {
    let def_id = tcx.hir().opt_local_def_id_from_node_id(id);
    def_id.map(|id| id_from_def_id(id)).unwrap_or_else(|| {
        // Create a *fake* `DefId` out of a `NodeId` by subtracting the `NodeId`
        // out of the maximum u32 value. This will work unless you have *billions*
        // of definitions in a single crate (very unlikely to actually happen).
        rls_data::Id { krate: LOCAL_CRATE.as_u32(), index: !id.as_u32() }
    })
}

pub fn null_id() -> rls_data::Id {
    rls_data::Id { krate: u32::max_value(), index: u32::max_value() }
}

pub fn lower_attributes(attrs: Vec<ast::Attribute>, tcx: TyCtxt<'_>) -> Vec<rls_data::Attribute> {
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
pub fn span_from_span(tcx: TyCtxt<'_>, span: Span) -> SpanData {
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
