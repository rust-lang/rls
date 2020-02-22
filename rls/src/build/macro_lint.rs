
#[allow(unused_extern_crates)]
extern crate rustc_driver;
extern crate rustc_lint;
extern crate rustc_span;
extern crate rustc_interface;
extern crate rustc_session;
extern crate syntax;

// use rustc_driver::{Callbacks, Compilation};
use rustc_driver::Callbacks;
// use rustc_interface::{Config, interface::Compiler, Queries};
use rustc_interface::Config;
use rustc_lint::{
    EarlyContext,
    EarlyLintPass,
};
use rustc_span::hygiene::{SyntaxContext};
use rustc_span::Span;
use rustc_session::{declare_lint, impl_lint_pass};
use syntax::{ast, visit};

use std::sync::{Arc, Mutex};
use std::path::PathBuf;

use rls_data::{Analysis, Def, DefKind, SpanData, Id, Signature, Attribute};
use rls_span as span;

declare_lint! {
    pub MACRO_DOCS,
    Allow,
    "gathers documentation for macros",
    report_in_external_macro
}

#[derive(Debug)]
pub struct Comments {
    span: (u32, u32, SyntaxContext),
    text: String,
}

impl Comments {
    pub fn new(span: Span, text: String) -> Self {
        let data = span.data();
        Self {
            span: (data.lo.0, data.hi.0, data.ctxt),
            text,
        }
    }
}

#[derive(Debug, Default)]
pub struct MacroDoc {
    pub defs: Arc<Mutex<Vec<Def>>>,
}

impl MacroDoc {
    pub(crate) fn new(defs: Arc<Mutex<Vec<Def>>>) -> Self {
        Self { defs, }
    }
}

impl_lint_pass!(MacroDoc => [MACRO_DOCS]);

impl EarlyLintPass for MacroDoc {
    fn check_item(&mut self, ecx: &EarlyContext, it: &ast::Item) {
        if let ast::ItemKind::MacroDef(_) = &it.kind {
            let mut width = 0;
            let docs = it.attrs
                .iter()
                .filter(|attr| attr.is_doc_comment())
                .flat_map(|attr| attr.doc_str())
                .map(|sym| {
                    let doc = sym.as_str().chars()
                        .filter(|c| c != &'/')
                        .collect::<String>();
                    if doc.len() > width {
                        width = doc.len();
                    }
                    doc
                })
                .collect::<Vec<_>>()
                .join("\n");
            
            let name = it.ident.to_string();
            let file_name = ecx.sess.local_crate_source_file.clone().unwrap_or_default();

            let mut attributes = Vec::default();
            for attr in &it.attrs {
                let span = SpanData {
                    file_name: file_name.clone(),
                    byte_start: attr.span.lo().0,
                    byte_end: attr.span.hi().0,
                    line_start: span::Row::new_one_indexed(0),
                    line_end: span::Row::new_one_indexed(0),
                    // Character offset.
                    column_start: span::Column::new_one_indexed(0),
                    column_end: span::Column::new_one_indexed(0),
                };
                match &attr.kind {
                    ast::AttrKind::DocComment(_) => {
                        attributes.push(Attribute {
                            value: attr.doc_str().unwrap().to_string(),
                            span,
                        })
                    },
                    ast::AttrKind::Normal(item) => {
                        attributes.push(Attribute {
                            value: format!("{:?}", item),
                            span,
                        })
                    },
                }
            }

            let id = Id { krate: 0, index: 0, };
            let span = SpanData {
                file_name: file_name.clone(),
                byte_start: it.span.lo().0,
                byte_end: it.span.hi().0,
                line_start: span::Row::new_one_indexed(0),
                line_end: span::Row::new_one_indexed(0),
                // Character offset.
                column_start: span::Column::new_one_indexed(0),
                column_end: span::Column::new_one_indexed(0),
            };
            self.defs.lock().unwrap().push(Def {
                kind: DefKind::Macro,
                id,
                span,
                name: name.clone(),
                qualname: format!("{}", file_name.to_str().unwrap()),
                value: name.clone(),
                parent: None,
                children: Vec::default(),
                decl_id: None,
                docs,
                sig: Some(Signature {
                    text: format!("macro_rules! {} (args...)", name),
                    defs: Vec::default(),
                    refs: Vec::default(),
                }),
                attributes,
            });

            // println!("{:#?}", self.defs)
        }
    }
}

// visit::Visitor is the generic trait for walking an AST.
impl<'a> visit::Visitor<'a> for MacroDoc {
    // We found an item, could be a function.
    fn visit_item(&mut self, i: &ast::Item) {
        println!("VISIT ITEM");
        if let ast::ItemKind::Fn(ref decl, ref gen, ref blk) = i.kind {
            // record the number of args
        }
        if let ast::ItemKind::MacroDef(ref mac) = i.kind {
            println!("{:#?}", mac);
        }
        // Keep walking.
        visit::walk_item(self, i)
    }

    fn visit_mac(&mut self, mac: &'a ast::Mac) {
        println!("MACRO {:#?}", mac);
        visit::walk_mac(self, mac);
    }
    fn visit_mac_def(&mut self, mac: &'a ast::MacroDef, _id: ast::NodeId) {
        println!("MACRO DEF {:#?}", mac);

    }
}
