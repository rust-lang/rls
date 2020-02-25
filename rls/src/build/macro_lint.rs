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
use rustc_data_structures::fx::FxHashSet;
use rustc_driver::Callbacks;
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
// use rustc_interface::{Config, interface::Compiler, Queries};
use rustc_interface::Config;
use rustc_lint::{EarlyContext, EarlyLintPass};
use rustc_save_analysis::SaveContext;
use rustc_session::{
    config::{CrateType, Input, OutputType},
    declare_lint, impl_lint_pass,
};
use rustc_span::hygiene::{ExpnId, SyntaxContext};
use rustc_span::{sym, ExpnKind, FileName, MacroKind, SourceFile, Span};
use syntax::{ast, util::comments::strip_doc_comment_decoration, visit};

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
pub struct MacroData {
    pub docs: String,
    pub name: String,
    file_name: String,
    pub id: ast::NodeId,
    pub span: Span,
}
unsafe impl Send for MacroData {}
impl MacroData {
    pub fn lower(self, ctxt: &TyCtxt<'_>) -> Ref {
        let MacroData { docs, name, file_name, id, span } = self;

        Ref {
            kind: RefKind::Macro,
            span: span_from_span(ctxt, span),
            ref_id: id_from_node_id(id, ctxt),
        }
    }
}

#[derive(Default, Debug)]
pub struct MacroDoc {
    pub defs: Arc<Mutex<Vec<MacroData>>>,
}

impl MacroDoc {
    pub(crate) fn new(defs: Arc<Mutex<Vec<MacroData>>>) -> Self {
        Self { defs }
    }
}

impl_lint_pass!(MacroDoc => [MACRO_DOCS]);

impl EarlyLintPass for MacroDoc {
    fn check_item(&mut self, ectx: &EarlyContext<'_>, it: &ast::Item) {
        if let ast::ItemKind::MacroDef(_) = &it.kind {
            let docs = docs_for_attrs(true, &it.attrs);

            let sm = ectx.sess.source_map();
            let filename = sm.span_to_filename(it.span);
            let name = it.ident.to_string();
            let id = it.id;

            self.defs.lock().unwrap().push(MacroData {
                docs,
                name,
                file_name: filename.to_string(),
                id,
                span: it.span,
            });
        }
    }
}

pub struct MacroDocCtxt<'l, 'tcx> {
    pub defs: Arc<Mutex<Vec<MacroData>>>,
    pub refs: Arc<Mutex<Vec<MacroData>>>,
    pub dumper: Dumper,
    pub tcx: &'l TyCtxt<'tcx>,
    pub macro_calls: FxHashSet<Span>,
    pub macro_defs: FxHashSet<Span>,
}

impl<'l, 'tcx> fmt::Debug for MacroDocCtxt<'l, 'tcx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacroDocCtx")
            .field("defs", &self.defs)
            .field("dumper", &self.dumper)
            .field("context", &self.tcx.def_path_hash_to_def_id)
            .finish()
    }
}

impl<'l, 'tcx> MacroDocCtxt<'l, 'tcx> {
    pub(crate) fn new(
        defs: Arc<Mutex<Vec<MacroData>>>,
        refs: Arc<Mutex<Vec<MacroData>>>,
        tcx: &'l TyCtxt<'tcx>,
    ) -> Self {
        Self {
            defs,
            refs,
            dumper: Dumper::new(rls_data::Config::default()),
            tcx,
            macro_calls: FxHashSet::default(),
            macro_defs: FxHashSet::default(),
        }
    }

    pub fn dump_crate_info(&mut self, name: &str, krate: &ast::Crate) {
        let source_file = self.tcx.sess.local_crate_source_file.as_ref();
        let crate_root = source_file.map(|source_file| {
            let source_file = Path::new(source_file);
            match source_file.file_name() {
                Some(_) => source_file.parent().unwrap().display(),
                None => source_file.display(),
            }
            .to_string()
        });

        let data = CratePreludeData {
            crate_id: GlobalCrateId {
                name: name.into(),
                disambiguator: self
                    .tcx
                    .sess
                    .local_crate_disambiguator()
                    .to_fingerprint()
                    .as_value(),
            },
            crate_root: crate_root.unwrap_or_else(|| "<no source>".to_owned()),
            external_crates: get_external_crates(self.tcx),
            span: span_from_span(&self.tcx, krate.span),
        };

        self.dumper.crate_prelude(data);
    }

    pub fn dump_compilation_opts(&mut self, input: &Input, crate_name: &str) {
        // Apply possible `remap-path-prefix` remapping to the input source file
        // (and don't include remapping args anymore)
        let (program, arguments) = {
            let remap_arg_indices = {
                let mut indices = FxHashSet::default();
                // Args are guaranteed to be valid UTF-8 (checked early)
                for (i, e) in std::env::args().enumerate() {
                    if e.starts_with("--remap-path-prefix=") {
                        indices.insert(i);
                    } else if e == "--remap-path-prefix" {
                        indices.insert(i);
                        indices.insert(i + 1);
                    }
                }
                indices
            };

            let mut args = std::env::args()
                .enumerate()
                .filter(|(i, _)| !remap_arg_indices.contains(i))
                .map(|(_, arg)| match input {
                    Input::File(ref path) if path == Path::new(&arg) => {
                        let mapped = &self.tcx.sess.local_crate_source_file;
                        mapped.as_ref().unwrap().to_string_lossy().into()
                    }
                    _ => arg,
                });

            (args.next().unwrap(), args.collect())
        };

        let data = CompilationOptions {
            directory: self.tcx.sess.working_dir.0.clone(),
            program,
            arguments,
            output: compilation_output(self.tcx, crate_name),
        };

        self.dumper.compilation_opts(data);
    }

    pub fn get_macro_use_data(&self, span: Span) -> Option<(Span, MacroRef)> {
        // if !generated_code(span) {
        //     println!("LEAVING GEN CODE");
        //     return None;
        // }
        let msg = "failed to create ctxt callee span";
        // Note we take care to use the source callsite/callee, to handle
        // nested expansions and ensure we only generate data for source-visible
        // macro uses.
        let callsite = span.source_callsite();
        let callsite_span = span_from_span(self.tcx, callsite);
        // TODO how to find ExpnId
        let callee =
            span.with_def_site_ctxt(ExpnId::from_u32(2)).source_callee().expect(msg).def_site;
        let callee_span = span_from_span(self.tcx, callsite);

        let qualname = file_to_qualname(
            &callee_span.file_name.to_str().map(|s| s.to_string()).unwrap_or_default(),
        );
        // println!(
        //     "parent {}\ncallee {}\ndef site {:#?}",
        //     span.parent().is_some(),
        //     span.source_callee().is_some(),
        //     span.ctxt().remove_mark()
        // );

        // let mac_name = match callee.kind {
        //     ExpnKind::Macro(mac_kind, name) => match mac_kind {
        //         MacroKind::Bang => name,

        //         // Ignore attribute macros, their spans are usually mangled
        //         // FIXME(eddyb) is this really the case anymore?
        //         MacroKind::Attr | MacroKind::Derive => return None,
        //     },

        //     // These are not macros.
        //     // FIXME(eddyb) maybe there is a way to handle them usefully?
        //     ExpnKind::Root | ExpnKind::AstPass(_) | ExpnKind::Desugaring(_) => return None,
        // };

        // If the callee is an imported macro from an external crate, need to get
        // the source span and name from the session, as their spans are localized
        // when read in, and no longer correspond to the source.
        if let Some(mac) = self.tcx.sess.imported_macro_spans.borrow().get(&callsite) {
            println!("IMPORT MAC");
            let &(ref mac_name, mac_span) = mac;
            let mac_span = span_from_span(self.tcx, mac_span);
            return Some((
                callee,
                MacroRef { span: callsite_span, qualname, callee_span: mac_span },
            ));
        }
        Some((callee, MacroRef { span: callsite_span, qualname, callee_span }))
    }

    /// Extracts macro use and definition information from the AST node defined
    /// by the given NodeId, using the expansion information from the node's
    /// span.
    ///
    /// If the span is not macro-generated, do nothing, else use callee and
    /// callsite spans to record macro definition and use data, using the
    /// mac_uses and mac_defs sets to prevent multiples.
    fn process_macro_use(&mut self, span: Span) {
        use std::hash::Hash;
        use std::hash::Hasher;
        // FIXME if we're not dumping the defs (see below), there is no point
        // dumping refs either.
        let source_span = span.source_callsite();
        if !self.macro_calls.insert(source_span) {
            println!("BAIL OUT");
            return;
        }

        let (callee, data) = match self.get_macro_use_data(span) {
            None => {
                println!("NO USE DAtA");
                return;
            }
            Some(data) => data,
        };
        // println!("MAC SPAN {:#?}", data);

        let mut hasher = DefaultHasher::new();
        (data.span.byte_end, data.callee_span.byte_start).hash(&mut hasher);
        let hash = hasher.finish();

        let qualname = format!("{}::{}", data.qualname, hash);
        println!("NAME {:?}", qualname);
        // Don't write macro definition for imported macros
        if !self.macro_defs.contains(&span.source_callsite()) {
            self.macro_defs.insert(span.source_callsite());
        }
        self.dumper.macro_use(data);

        let sm = self.tcx.sess.source_map();
        let filename = sm.span_to_filename(span);
        let (docs, name) = self
            .defs
            .lock()
            .unwrap()
            .iter()
            .find(|def| def.span == callee)
            .map(|mac| (mac.docs.to_string(), mac.name.to_string()))
            .unwrap_or_else(|| (String::new(), String::default()));

        let mac_ref = MacroData {
            docs,
            name,
            file_name: filename.to_string(),
            id: ast::NodeId::from_u32(0),
            span,
        };
        self.refs.lock().unwrap().push(mac_ref.clone());
        self.dumper.dump_ref(mac_ref.lower(self.tcx))
    }
}

impl<'a, 'tcx> visit::Visitor<'a> for MacroDocCtxt<'a, 'tcx> {
    fn visit_item(&mut self, i: &'a ast::Item) {
        match i.kind {
            ast::ItemKind::MacroDef(ref mac) => {
                // TODO local_def_id_from_node_id still panics with crazy i.id
                // are these stable throughout compilation??
                //
                // let qualname =
                //     format!("::{}", self.tcx.def_path_str(self.tcx.hir().local_def_id_from_node_id(i.id)));

                let sm = self.tcx.sess.source_map();
                let filename = sm.span_to_filename(i.span);
                let qualname = format!("::{}", file_to_qualname(&filename.to_string()));

                let data_id = id_from_node_id(i.id, &self.tcx);
                let span = span_from_span(&self.tcx, i.span);
                let docs = self
                    .defs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|mac| mac.span == i.span)
                    .map(|mac| mac.docs.clone())
                    .unwrap_or_default();

                self.dumper.dump_def(
                    &Access { public: true, reachable: true },
                    Def {
                        kind: DefKind::Macro,
                        id: data_id,
                        name: i.ident.to_string(),
                        qualname,
                        span,
                        value: format!("macro_rules! {} (args...)", i.ident.to_string()),
                        children: Vec::default(),
                        parent: None,
                        decl_id: None,
                        docs,
                        sig: None,
                        attributes: lower_attributes(i.attrs.to_owned(), &self.tcx),
                    },
                );
                self.macro_defs.insert(i.span);
            }
            _ => visit::walk_item(self, i),
        }
    }
    // fn visit_expr(&mut self, expr: &'a ast::Expr) {
    //     println!("EXPR");
    //     self.process_macro_use(expr.span);
    //     visit::walk_expr(self, expr)
    // }
    // fn visit_stmt(&mut self, stmt: &'a ast::Stmt) {
    //     println!("STMT");
    //     // self.process_macro_use(stmt.span);
    //     visit::walk_stmt(self, stmt)
    // }
    fn visit_mac(&mut self, mac: &'a ast::Mac) {
        // println!("VISIT MAC {:?}", mac);
        self.process_macro_use(mac.span());
        // visit::walk_mac(self, mac);
    }
}

// Taken directly from `librustc_save_analysis`
//
//

/// Helper function to escape quotes in a string
fn escape(s: String) -> String {
    s.replace("\"", "\"\"")
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
