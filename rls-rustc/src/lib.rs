#![feature(rustc_private)]
#![feature(box_syntax)]

extern crate env_logger;
extern crate getopts;
extern crate rustc;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_metadata;
extern crate rustc_codegen_utils;
extern crate syntax;

use rustc_metadata::cstore::CStore;
use rustc::session::{Session, early_error};
use rustc::session::config::{self, ErrorOutputType, Input};
use rustc_codegen_utils::codegen_backend::CodegenBackend;
use rustc_driver::driver::CompileController;
use rustc_driver::{run_compiler, CompilerCalls, RustcDefaultCalls, Compilation, enable_save_analysis};
use syntax::ast;

use std::env;
use std::path::PathBuf;
use std::process;

pub fn run() {
    drop(env_logger::init());
    let result = rustc_driver::run(|| {
        let args = env::args_os().enumerate()
            .map(|(i, arg)| arg.into_string().unwrap_or_else(|arg| {
                early_error(ErrorOutputType::default(),
                            &format!("Argument {} is not valid Unicode: {:?}", i, arg))
            }))
            .collect::<Vec<_>>();

        run_compiler(&args,
                     Box::new(ShimCalls),
                     None,
                     None)
    });
    process::exit(result as i32);
}


struct ShimCalls;

impl<'a> CompilerCalls<'a> for ShimCalls {
    fn early_callback(&mut self,
                      a: &getopts::Matches,
                      b: &config::Options,
                      c: &ast::CrateConfig,
                      d: &rustc_errors::registry::Registry,
                      e: ErrorOutputType)
                      -> Compilation {
        RustcDefaultCalls.early_callback(a, b, c, d, e)
    }

    fn late_callback(&mut self,
                     a: &CodegenBackend,
                     b: &getopts::Matches,
                     c: &Session,
                     d: &CStore,
                     e: &Input,
                     f: &Option<PathBuf>,
                     g: &Option<PathBuf>)
                     -> Compilation {
        RustcDefaultCalls.late_callback(a, b, c, d, e, f, g)
    }

    fn some_input(&mut self,
                  a: Input,
                  b: Option<PathBuf>)
                  -> (Input, Option<PathBuf>) {
        RustcDefaultCalls.some_input(a, b)
    }

    fn no_input(&mut self,
                a: &getopts::Matches,
                b: &config::Options,
                c: &ast::CrateConfig,
                d: &Option<PathBuf>,
                e: &Option<PathBuf>,
                f: &rustc_errors::registry::Registry)
                -> Option<(Input, Option<PathBuf>)> {
        RustcDefaultCalls.no_input(a, b, c, d, e, f)
    }

    fn build_controller(self: Box<Self>, a: &Session, b: &getopts::Matches) -> CompileController<'a> {
        let mut result = Box::new(RustcDefaultCalls).build_controller(a, b);

        result.continue_parse_after_error = true;
        enable_save_analysis(&mut result);

        result
    }
}
