// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.
use log::trace;
use rls_data::Analysis;
use rls_vfs::Vfs;

// FIXME: switch to something more ergonomic here, once available.
// (currently there is no way to opt into sysroot crates w/o `extern crate`)
#[allow(unused_extern_crates)]
extern crate getopts;
#[allow(unused_extern_crates)]
extern crate rustc;
#[allow(unused_extern_crates)]
extern crate rustc_codegen_utils;
#[allow(unused_extern_crates)]
extern crate rustc_driver;
#[allow(unused_extern_crates)]
extern crate rustc_errors;
#[allow(unused_extern_crates)]
extern crate rustc_metadata;
#[allow(unused_extern_crates)]
extern crate rustc_plugin;
#[allow(unused_extern_crates)]
extern crate rustc_resolve;
#[allow(unused_extern_crates)]
extern crate rustc_save_analysis;
#[allow(unused_extern_crates)]
extern crate syntax;
use self::rustc::session::config::{self, ErrorOutputType, Input};
use self::rustc::session::Session;
use self::rustc_codegen_utils::codegen_backend::CodegenBackend;
use self::rustc_driver::driver::CompileController;
use self::rustc_driver::{run, run_compiler, Compilation, CompilerCalls, RustcDefaultCalls};
use self::rustc_metadata::cstore::CStore;
use self::rustc_save_analysis as save;
use self::rustc_save_analysis::CallbackHandler;
use self::syntax::ast;
use self::syntax::source_map::{FileLoader, RealFileLoader};

use crate::build::environment::{Environment, EnvironmentLockFacade};
use crate::build::{BufWriter, BuildResult};
use crate::config::{ClippyPreference, Config};

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Runs a single instance of rustc. Runs in-process.
crate fn rustc(
    vfs: &Vfs,
    args: &[String],
    envs: &HashMap<String, Option<OsString>>,
    cwd: Option<&Path>,
    build_dir: &Path,
    rls_config: Arc<Mutex<Config>>,
    env_lock: &EnvironmentLockFacade,
) -> BuildResult {
    trace!(
        "rustc - args: `{:?}`, envs: {:?}, build dir: {:?}",
        args,
        envs,
        build_dir
    );

    let changed = vfs.get_cached_files();

    let mut local_envs = envs.clone();

    if rls_config.lock().unwrap().clear_env_rust_log {
        local_envs.insert(String::from("RUST_LOG"), None);
    }

    let clippy_pref = rls_config.lock().unwrap().clippy_preference;
    let (guard, _) = env_lock.lock();
    let restore_env = Environment::push_with_lock(&local_envs, cwd, guard);

    let buf = Arc::new(Mutex::new(vec![]));
    let err_buf = buf.clone();
    let args: Vec<_> = if cfg!(feature = "clippy") && clippy_pref != ClippyPreference::Off {
        // Allow feature gating in the same way as `cargo clippy`
        let mut clippy_args = vec!["--cfg".to_owned(), r#"feature="cargo-clippy""#.to_owned()];

        if clippy_pref == ClippyPreference::OptIn {
            // `OptIn`: Require explicit `#![warn(clippy::all)]` annotation in each workspace crate
            clippy_args.push("-A".to_owned());
            clippy_args.push("clippy::all".to_owned());
        }

        args.iter()
            .map(|s| s.to_owned())
            .chain(clippy_args)
            .collect()
    } else {
        args.to_owned()
    };

    let analysis = Arc::new(Mutex::new(None));
    let controller = Box::new(RlsRustcCalls::new(Arc::clone(&analysis), clippy_pref));

    // rustc explicitly panics in run_compiler() on compile failure, regardless
    // if it encounters an ICE (internal compiler error) or not.
    // TODO: Change librustc_driver behaviour to distinguish between ICEs and
    // regular compilation failure with errors?
    let result = ::std::panic::catch_unwind(|| {
        run(move || {
            // Replace stderr so we catch most errors.
            run_compiler(
                &args,
                controller,
                Some(Box::new(ReplacedFileLoader::new(changed))),
                Some(Box::new(BufWriter(buf))),
            )
        })
    });

    // FIXME(#25) given that we are running the compiler directly, there is no need
    // to serialize the error messages - we should pass them in memory.
    let err_buf = Arc::try_unwrap(err_buf).unwrap().into_inner().unwrap();
    let err_buf = String::from_utf8(err_buf).unwrap();
    let stderr_json_msgs: Vec<_> = err_buf.lines().map(String::from).collect();

    let analysis = analysis.lock().unwrap().clone();
    let analysis = analysis
        .map(|analysis| vec![analysis])
        .unwrap_or_else(Vec::new);

    let cwd = cwd
        .unwrap_or_else(|| restore_env.get_old_cwd())
        .to_path_buf();

    match result {
        Ok(_) => BuildResult::Success(cwd, stderr_json_msgs, analysis, true),
        Err(_) => BuildResult::Success(cwd, stderr_json_msgs, analysis, false),
    }
}

// Our compiler controller. We mostly delegate to the default rustc
// controller, but use our own callback for save-analysis.
#[derive(Clone)]
struct RlsRustcCalls {
    default_calls: Box<RustcDefaultCalls>,
    analysis: Arc<Mutex<Option<Analysis>>>,
    clippy_preference: ClippyPreference,
}

impl RlsRustcCalls {
    fn new(
        analysis: Arc<Mutex<Option<Analysis>>>,
        clippy_preference: ClippyPreference,
    ) -> RlsRustcCalls {
        RlsRustcCalls {
            default_calls: Box::new(RustcDefaultCalls),
            analysis,
            clippy_preference,
        }
    }
}

#[cfg(feature = "clippy")]
fn clippy_after_parse_callback(state: &mut rustc_driver::driver::CompileState<'_, '_>) {
    use self::rustc_plugin::registry::Registry;

    let mut registry = Registry::new(
        state.session,
        state
            .krate
            .as_ref()
            .expect(
                "at this compilation stage \
                 the crate must be parsed",
            ).span,
    );
    registry.args_hidden = Some(Vec::new());

    let conf = clippy_lints::read_conf(&registry);
    clippy_lints::register_plugins(&mut registry, &conf);

    let Registry {
        early_lint_passes,
        late_lint_passes,
        lint_groups,
        llvm_passes,
        attributes,
        ..
    } = registry;
    let sess = &state.session;
    let mut ls = sess.lint_store.borrow_mut();
    for pass in early_lint_passes {
        ls.register_early_pass(Some(sess), true, pass);
    }
    for pass in late_lint_passes {
        ls.register_late_pass(Some(sess), true, pass);
    }

    for (name, (to, deprecated_name)) in lint_groups {
        ls.register_group(Some(sess), true, name, deprecated_name, to);
    }

    sess.plugin_llvm_passes.borrow_mut().extend(llvm_passes);
    sess.plugin_attributes.borrow_mut().extend(attributes);
}

impl<'a> CompilerCalls<'a> for RlsRustcCalls {
    fn early_callback(
        &mut self,
        matches: &getopts::Matches,
        sopts: &config::Options,
        cfg: &ast::CrateConfig,
        descriptions: &rustc_errors::registry::Registry,
        output: ErrorOutputType,
    ) -> Compilation {
        self.default_calls
            .early_callback(matches, sopts, cfg, descriptions, output)
    }

    fn no_input(
        &mut self,
        matches: &getopts::Matches,
        sopts: &config::Options,
        cfg: &ast::CrateConfig,
        odir: &Option<PathBuf>,
        ofile: &Option<PathBuf>,
        descriptions: &rustc_errors::registry::Registry,
    ) -> Option<(Input, Option<PathBuf>)> {
        self.default_calls
            .no_input(matches, sopts, cfg, odir, ofile, descriptions)
    }

    fn late_callback(
        &mut self,
        codegen_backend: &dyn CodegenBackend,
        matches: &getopts::Matches,
        sess: &Session,
        cstore: &CStore,
        input: &Input,
        odir: &Option<PathBuf>,
        ofile: &Option<PathBuf>,
    ) -> Compilation {
        self.default_calls
            .late_callback(codegen_backend, matches, sess, cstore, input, odir, ofile)
    }

    #[allow(clippy::boxed_local)] // https://github.com/rust-lang-nursery/rust-clippy/issues/1123
    fn build_controller(
        self: Box<Self>,
        sess: &Session,
        matches: &getopts::Matches,
    ) -> CompileController<'a> {
        let analysis = self.analysis.clone();
        #[cfg(feature = "clippy")]
        let clippy_preference = self.clippy_preference;
        let mut result = self.default_calls.build_controller(sess, matches);
        result.keep_ast = true;

        #[cfg(feature = "clippy")]
        {
            if clippy_preference != ClippyPreference::Off {
                result.after_parse.callback = Box::new(clippy_after_parse_callback);
            }
        }

        result.after_analysis.callback = Box::new(move |state| {
            // There are two ways to move the data from rustc to the RLS, either
            // directly or by serialising and deserialising. We only want to do
            // the latter when there are compatibility issues between crates.

            // This version passes via JSON, it is more easily backwards compatible.
            // save::process_crate(state.tcx.unwrap(),
            //                     state.expanded_crate.unwrap(),
            //                     state.analysis.unwrap(),
            //                     state.crate_name.unwrap(),
            //                     state.input,
            //                     None,
            //                     save::DumpHandler::new(state.out_dir,
            //                                            state.crate_name.unwrap()));
            // This version passes directly, it is more efficient.
            save::process_crate(
                state.tcx.expect("missing tcx"),
                state.expanded_crate.expect("missing crate"),
                state.analysis.expect("missing analysis"),
                state.crate_name.expect("missing crate name"),
                state.input,
                None,
                CallbackHandler {
                    callback: &mut |a| {
                        let mut analysis = analysis.lock().unwrap();
                        let a = unsafe { ::std::mem::transmute(a.clone()) };
                        *analysis = Some(a);
                    },
                },
            );
        });
        result.after_analysis.run_callback_on_error = true;
        result.make_glob_map = rustc_resolve::MakeGlobMap::Yes;

        result
    }
}

/// Tries to read a file from a list of replacements, and if the file is not
/// there, then reads it from disk, by delegating to `RealFileLoader`.
struct ReplacedFileLoader {
    replacements: HashMap<PathBuf, String>,
    real_file_loader: RealFileLoader,
}

impl ReplacedFileLoader {
    fn new(replacements: HashMap<PathBuf, String>) -> ReplacedFileLoader {
        ReplacedFileLoader {
            replacements,
            real_file_loader: RealFileLoader,
        }
    }
}

impl FileLoader for ReplacedFileLoader {
    fn file_exists(&self, path: &Path) -> bool {
        self.real_file_loader.file_exists(path)
    }

    fn abs_path(&self, path: &Path) -> Option<PathBuf> {
        self.real_file_loader.abs_path(path)
    }

    fn read_file(&self, path: &Path) -> io::Result<String> {
        if let Some(abs_path) = self.abs_path(path) {
            if self.replacements.contains_key(&abs_path) {
                return Ok(self.replacements[&abs_path].clone());
            }
        }
        self.real_file_loader.read_file(path)
    }
}
