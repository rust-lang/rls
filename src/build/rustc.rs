// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate getopts;
extern crate rustc;
extern crate rustc_driver;
extern crate rustc_plugin;
extern crate rustc_errors as errors;
extern crate rustc_resolve;
extern crate rustc_save_analysis;
extern crate rustc_trans_utils;
#[cfg(feature = "clippy")]
extern crate clippy_lints;
extern crate syntax;

use self::rustc::middle::cstore::CrateStore;
use self::rustc::session::Session;
use self::rustc::session::config::{self, ErrorOutputType, Input};
use self::rustc_driver::{run, run_compiler, Compilation, CompilerCalls, RustcDefaultCalls};
use self::rustc_driver::driver::{CompileController, CompileState};
use self::rustc_save_analysis as save;
use self::rustc_save_analysis::CallbackHandler;
use self::rustc_trans_utils::trans_crate::TransCrate;
use self::syntax::ast;
use self::syntax::codemap::{FileLoader, RealFileLoader};

use config::Config;
use build::{BufWriter, BuildResult};
use build::environment::{Environment, EnvironmentLockFacade};
use data::Analysis;
use vfs::Vfs;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Runs a single instance of rustc. Runs in-process.
pub fn rustc(
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

    let (guard, _) = env_lock.lock();
    let restore_env = Environment::push_with_lock(&local_envs, cwd, guard);

    let buf = Arc::new(Mutex::new(vec![]));
    let err_buf = buf.clone();
    let args: Vec<_> = if cfg!(feature = "clippy") {
        args.iter()
            .map(|s| s.to_owned())
            .chain(vec![
                "-Aclippy".to_owned(),
                "--cfg".to_owned(),
                r#"feature="cargo-clippy""#.to_owned(),
            ])
            .collect()
    } else {
        args.to_owned()
    };

    let analysis = Arc::new(Mutex::new(None));

    let mut controller = RlsRustcCalls::new(analysis.clone());

    // rustc explicitly panics in run_compiler() on compile failure, regardless
    // if it encounters an ICE (internal compiler error) or not.
    // TODO: Change librustc_driver behaviour to distinguish between ICEs and
    // regular compilation failure with errors?
    let result = ::std::panic::catch_unwind(|| {
        run(move || {
            // Replace stderr so we catch most errors.
            run_compiler(
                &args,
                &mut controller,
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
    let analysis = analysis.map(|analysis| vec![analysis]).unwrap_or_else(Vec::new);

    let cwd = cwd.unwrap_or_else(|| restore_env.get_old_cwd()).to_path_buf();

    match result {
        Ok(_) => BuildResult::Success(cwd, stderr_json_msgs, analysis, true),
        Err(_) => BuildResult::Success(cwd, stderr_json_msgs, analysis, false),
    }
}

// Our compiler controller. We mostly delegate to the default rustc
// controller, but use our own callback for save-analysis.
#[derive(Clone)]
struct RlsRustcCalls {
    default_calls: RustcDefaultCalls,
    analysis: Arc<Mutex<Option<Analysis>>>,
}

impl RlsRustcCalls {
    fn new(analysis: Arc<Mutex<Option<Analysis>>>) -> RlsRustcCalls {
        RlsRustcCalls {
            default_calls: RustcDefaultCalls,
            analysis: analysis,
        }
    }
}

impl<'a> CompilerCalls<'a> for RlsRustcCalls {
    fn early_callback(
        &mut self,
        matches: &getopts::Matches,
        sopts: &config::Options,
        cfg: &ast::CrateConfig,
        descriptions: &errors::registry::Registry,
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
        descriptions: &errors::registry::Registry,
    ) -> Option<(Input, Option<PathBuf>)> {
        self.default_calls
            .no_input(matches, sopts, cfg, odir, ofile, descriptions)
    }

    fn late_callback(
        &mut self,
        trans_crate: &TransCrate,
        matches: &getopts::Matches,
        sess: &Session,
        cstore: &CrateStore,
        input: &Input,
        odir: &Option<PathBuf>,
        ofile: &Option<PathBuf>,
    ) -> Compilation {
        self.default_calls
            .late_callback(trans_crate, matches, sess, cstore, input, odir, ofile)
    }

    fn build_controller(
        &mut self,
        sess: &Session,
        matches: &getopts::Matches,
    ) -> CompileController<'a> {
        let mut result = self.default_calls.build_controller(sess, matches);
        result.keep_ast = true;
        let analysis = self.analysis.clone();

        #[cfg(feature = "clippy")]
        fn clippy(state: &mut CompileState) {
            let mut registry = rustc_plugin::registry::Registry::new(
                state.session,
                state
                    .krate
                    .as_ref()
                    .expect(
                        "at this compilation stage \
                            the crate must be parsed",
                    )
                    .span,
            );
            registry.args_hidden = Some(Vec::new());
            clippy_lints::register_plugins(&mut registry);

            let rustc_plugin::registry::Registry {
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

            for (name, to) in lint_groups {
                ls.register_group(Some(sess), true, name, to);
            }

            sess.plugin_llvm_passes.borrow_mut().extend(llvm_passes);
            sess.plugin_attributes.borrow_mut().extend(attributes);
        }
        #[cfg(not(feature = "clippy"))]
        fn clippy(_: &mut CompileState) {}
        result.after_parse.callback = Box::new(clippy);

        result.after_analysis.callback = Box::new(move |state| {
            // There are two ways to move the data from rustc to the RLS, either
            // directly or by serialising and deserialising. We only want to do
            // the latter when there are compatibility issues between crates.

            // This version passes via JSON, it is more easily backwards compatible.
            // save::process_crate(state.tcx.unwrap(),
            //                     state.expanded_crate.unwrap(),
            //                     state.analysis.unwrap(),
            //                     state.crate_name.unwrap(),
            //                     None,
            //                     save::DumpHandler::new(state.out_dir,
            //                                            state.crate_name.unwrap()));
            // This version passes directly, it is more efficient.
            save::process_crate(
                state.tcx.expect("missing tcx"),
                state.expanded_crate.expect("missing crate"),
                state.analysis.expect("missing analysis"),
                state.crate_name.expect("missing crate name"),
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
            replacements: replacements,
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
