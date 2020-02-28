// FIXME: switch to something more ergonomic here, once available.
// (Currently, there is no way to opt into sysroot crates without `extern crate`.)
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
extern crate rustc_interface;
#[allow(unused_extern_crates)]
extern crate rustc_metadata;
#[allow(unused_extern_crates)]
extern crate rustc_resolve;
#[allow(unused_extern_crates)]
extern crate rustc_save_analysis;
#[allow(unused_extern_crates)]
extern crate rustc_span;
extern crate syntax;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::io;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use log::trace;
use rls_data::Analysis;
use rls_vfs::Vfs;

use self::rustc::session::config::Input;
use self::rustc::session::Session;
use self::rustc::ty::TyCtxt;
use self::rustc_driver::{run_compiler, Compilation};
use self::rustc_interface::interface;
use self::rustc_interface::Queries;
use self::rustc_save_analysis as save;
use self::rustc_save_analysis::CallbackHandler;
use self::rustc_span::{BytePos, Span};
use self::rustc_span::edition::Edition as RustcEdition;
use self::rustc_span::source_map::{FileLoader, RealFileLoader};
use self::syntax::{ast, visit};
use crate::build::environment::{Environment, EnvironmentLockFacade};
use crate::build::macro_lint::{
    LateMacroDocs, MacroDef, MacroDoc, MacroDocRef, LATE_MACRO_DOCS, MACRO_DOCS, id_from_node_id,
};
use crate::build::plan::{Crate, Edition};
use crate::build::{BufWriter, BuildResult};
use crate::config::{ClippyPreference, Config};

// Runs a single instance of Rustc.
pub(crate) fn rustc(
    vfs: &Vfs,
    args: &[String],
    envs: &BTreeMap<String, Option<OsString>>,
    cwd: Option<&Path>,
    build_dir: &Path,
    rls_config: Arc<Mutex<Config>>,
    env_lock: &EnvironmentLockFacade,
) -> BuildResult {
    trace!(
        "rustc - args: `{:?}`, envs: {:?}, cwd: {:?}, build dir: {:?}",
        args,
        envs,
        cwd,
        build_dir
    );

    let changed = vfs.get_cached_files();

    let mut envs = envs.clone();

    let clippy_preference = {
        let config = rls_config.lock().unwrap();
        if config.clear_env_rust_log {
            envs.insert(String::from("RUST_LOG"), None);
        }

        config.clippy_preference
    };

    let lock_environment = |envs, cwd| {
        let (guard, _) = env_lock.lock();
        Environment::push_with_lock(envs, cwd, guard)
    };

    let CompilationResult { result, stderr, analysis, input_files } = match std::env::var(
        "RLS_OUT_OF_PROCESS",
    ) {
        #[cfg(feature = "ipc")]
        Ok(..) => run_out_of_process(changed.clone(), &args, &envs, clippy_preference)
            .unwrap_or_else(|_| {
                run_in_process(changed, &args, clippy_preference, lock_environment(&envs, cwd))
            }),
        #[cfg(not(feature = "ipc"))]
        Ok(..) => {
            trace!("Support for out-of-process compilation was not compiled. Rebuild with 'ipc' feature enabled");
            run_in_process(changed, &args, clippy_preference, lock_environment(&envs, cwd))
        }
        Err(..) => run_in_process(changed, &args, clippy_preference, lock_environment(&envs, cwd)),
    };

    let stderr = String::from_utf8(stderr).unwrap();
    log::debug!("rustc - stderr: {}", &stderr);
    let stderr_json_msgs: Vec<_> = stderr.lines().map(String::from).collect();

    let analysis = analysis.map(|analysis| vec![analysis]).unwrap_or_else(Vec::new);
    log::debug!("rustc: analysis read successfully?: {}", !analysis.is_empty());

    let cwd = cwd.unwrap_or_else(|| Path::new(".")).to_path_buf();

    BuildResult::Success(cwd, stderr_json_msgs, analysis, input_files, result.is_ok())
}

/// Resulting data from compiling a crate (in the rustc sense)
pub struct CompilationResult {
    /// Whether compilation was succesful
    result: Result<(), ()>,
    stderr: Vec<u8>,
    analysis: Option<Analysis>,
    // TODO: Move to Vec<PathBuf>
    input_files: HashMap<PathBuf, HashSet<Crate>>,
}

#[cfg(feature = "ipc")]
fn run_out_of_process(
    changed: HashMap<PathBuf, String>,
    args: &[String],
    envs: &HashMap<String, Option<OsString>>,
    clippy_preference: ClippyPreference,
) -> Result<CompilationResult, ()> {
    let analysis = Arc::default();
    let input_files = Arc::default();

    let ipc_server =
        super::ipc::start_with_all(changed, Arc::clone(&analysis), Arc::clone(&input_files))?;

    // Compiling out of process is only supported by our own shim
    let rustc_shim = env::current_exe()
        .ok()
        .and_then(|x| x.to_str().map(String::from))
        .expect("Couldn't set executable for RLS rustc shim");

    let output = Command::new(rustc_shim)
        .env(crate::RUSTC_SHIM_ENV_VAR_NAME, "1")
        .env("RLS_IPC_ENDPOINT", ipc_server.endpoint())
        .env("RLS_CLIPPY_PREFERENCE", clippy_preference.to_string())
        .args(args.iter().skip(1))
        .envs(envs.iter().filter_map(|(k, v)| v.as_ref().map(|v| (k, v))))
        .output()
        .map_err(|_| ());

    let result = match &output {
        Ok(output) if output.status.code() == Some(0) => Ok(()),
        _ => Err(()),
    };
    // NOTE: Make sure that we pass JSON error format
    let stderr = output.map(|out| out.stderr).unwrap_or_default();

    ipc_server.close();

    let input_files = unwrap_shared(input_files, "Other ref dropped by closed IPC server");
    let analysis = unwrap_shared(analysis, "Other ref dropped by closed IPC server");
    // FIXME(#25): given that we are running the compiler directly, there is no need
    // to serialize the error messages -- we should pass them in memory.
    Ok(CompilationResult { result, stderr, analysis, input_files })
}

fn run_in_process(
    changed: HashMap<PathBuf, String>,
    args: &[String],
    clippy_preference: ClippyPreference,
    environment_lock: Environment<'_>,
) -> CompilationResult {
    let mut callbacks = RlsRustcCalls { clippy_preference, ..Default::default() };
    let input_files = Arc::clone(&callbacks.input_files);
    let analysis = Arc::clone(&callbacks.analysis);

    let args: Vec<_> = if cfg!(feature = "clippy") && clippy_preference != ClippyPreference::Off {
        // Allow feature gating in the same way as `cargo clippy`
        let mut clippy_args = vec!["--cfg".to_owned(), r#"feature="cargo-clippy""#.to_owned()];

        if clippy_preference == ClippyPreference::OptIn {
            // `OptIn`: Require explicit `#![warn(clippy::all)]` annotation in each workspace crate
            clippy_args.push("-A".to_owned());
            clippy_args.push("clippy::all".to_owned());
        }

        args.iter().map(ToOwned::to_owned).chain(clippy_args).collect()
    } else {
        args.to_owned()
    };

    // rustc explicitly panics in `run_compiler()` on compile failure, regardless
    // of whether it encounters an ICE (internal compiler error) or not.
    // TODO: Change librustc_driver behaviour to distinguish between ICEs and
    // regular compilation failure with errors?
    let stderr = Arc::default();
    let result = std::panic::catch_unwind({
        let stderr = Arc::clone(&stderr);
        || {
            rustc_driver::catch_fatal_errors(move || {
                // Replace stderr so we catch most errors.
                run_compiler(
                    &args,
                    &mut callbacks,
                    Some(Box::new(ReplacedFileLoader::new(changed))),
                    Some(Box::new(BufWriter(stderr))),
                )
            })
        }
    })
    .map(|_| ())
    .map_err(|_| ());
    // Explicitly drop the global environment lock
    mem::drop(environment_lock);

    let stderr = unwrap_shared(stderr, "Other ref dropped by scoped compilation");
    let input_files = unwrap_shared(input_files, "Other ref dropped by scoped compilation");
    let analysis = unwrap_shared(analysis, "Other ref dropped by scoped compilation");

    CompilationResult { result, stderr, analysis, input_files }
}

// Our compiler controller. We mostly delegate to the default rustc
// controller, but use our own callback for save-analysis.
#[derive(Clone, Default)]
struct RlsRustcCalls {
    analysis: Arc<Mutex<Option<Analysis>>>,
    input_files: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
    clippy_preference: ClippyPreference,
    mac_defs: Arc<Mutex<Vec<MacroDef>>>,
    mac_refs: Arc<Mutex<Vec<MacroDocRef>>>,
}

impl RlsRustcCalls {
    /// Sets valid `Id`s on each `Ref` otherwise they are `NULL`.
    /// 
    /// This matches each `RlsRustCalls.mac_ref` to the corresponding `mac_def` id
    /// when the ref is found by span it returns its definition's `rls_data::Id`.
    pub fn add_def_id(&mut self, ctxt: TyCtxt<'_>) {
        let lkd_defs = self.mac_defs.lock().unwrap();
        let mut lkd_refs = self.mac_refs.lock().unwrap();
        
        for ref_ in lkd_refs.iter_mut() {
            if let Some(def) = lkd_defs.iter().find(|d| d.span == ref_.def_span) {
                println!("FOUND MATCHING DEF SPAN");
                ref_.id = id_from_node_id(def.id, &ctxt);
                let data = ref_.span.data();
                println!("{:?} {:?}", data.lo, data.hi);
                let end = BytePos(data.lo.0 + def.name.chars().count() as u32);
                ref_.span = Span::new(data.lo, end, data.ctxt);
            }
            println!("ADD DEFS {:#?}", ref_);
        }
    }
}

impl rustc_driver::Callbacks for RlsRustcCalls {
    fn config(&mut self, config: &mut interface::Config) {
        // This also prevents the compiler from dropping expanded AST, which we
        // still need in the `after_analysis` callback in order to process and
        // pass the computed analysis in-memory.
        config.opts.debugging_opts.save_analysis = true;

        #[cfg(feature = "clippy")]
        {
            if self.clippy_preference != ClippyPreference::Off {
                clippy_config(config);
            }
        }

        // TODO I'm sure there is a better way to do the registering such as above
        let previous = config.register_lints.take();
        let macro_defs = Arc::clone(&self.mac_defs);
        let macro_refs = Arc::clone(&self.mac_refs);
        config.register_lints = Some(Box::new(move |sess, lint_store| {
            // technically we're ~guaranteed that this is none but might as well call anything that
            // is there already. Certainly it can't hurt.
            if let Some(previous) = &previous {
                (previous)(sess, lint_store);
            }

            let macro_defs = Arc::clone(&macro_defs);
            let macro_defs2 = Arc::clone(&macro_defs);
            let macro_refs = Arc::clone(&macro_refs);
            lint_store.register_lints(&[&MACRO_DOCS, LATE_MACRO_DOCS]);
            lint_store
                .register_early_pass(move || Box::new(MacroDoc::new(Arc::clone(&macro_defs))));
            lint_store.register_late_pass(move || {
                Box::new(LateMacroDocs::new(Arc::clone(&macro_defs2), Arc::clone(&macro_refs)))
            });
        }));
    }

    fn after_parsing<'tcx>(
        &mut self,
        compiler: &interface::Compiler,
        queries: &'tcx Queries<'tcx>,
    ) -> Compilation {
        Compilation::Continue
    }

    fn after_expansion<'tcx>(
        &mut self,
        compiler: &interface::Compiler,
        queries: &'tcx Queries<'tcx>,
    ) -> Compilation {
        let sess = compiler.session();
        let input = compiler.input();
        let crate_name = queries.crate_name().unwrap().peek().clone();

        let cwd = &sess.working_dir.0;

        let src_path = match input {
            Input::File(ref name) => Some(name.to_path_buf()),
            Input::Str { .. } => None,
        }
        .and_then(|path| src_path(Some(cwd), path));

        let krate = Crate {
            name: crate_name.to_owned(),
            src_path,
            disambiguator: sess.local_crate_disambiguator().to_fingerprint().as_value(),
            edition: match sess.edition() {
                RustcEdition::Edition2015 => Edition::Edition2015,
                RustcEdition::Edition2018 => Edition::Edition2018,
            },
        };

        // We populate the file -> edition mapping only after expansion since it
        // can pull additional input files
        let mut input_files = self.input_files.lock().unwrap();
        for file in fetch_input_files(sess) {
            input_files.entry(file).or_default().insert(krate.clone());
        }
        Compilation::Continue
    }

    fn after_analysis<'tcx>(
        &mut self,
        compiler: &interface::Compiler,
        queries: &'tcx Queries<'tcx>,
    ) -> Compilation {
        let input = compiler.input();
        let crate_name = queries.crate_name().unwrap().peek().clone();

        let krate = queries.parse().expect("no Result<Query<Crate>> found").take();
        // ...and walks the AST, collecting stats.

        // Guaranteed to not be dropped yet in the pipeline thanks to the
        // `config.opts.debugging_opts.save_analysis` value being set to `true`.
        let expanded_crate = &queries.expansion().unwrap().peek().0;
        queries.global_ctxt().unwrap().peek_mut().enter(|tcx| {
            save::process_crate(
                tcx,
                &expanded_crate,
                &crate_name,
                &input,
                None,
                CallbackHandler {
                    callback: &mut |a| {
                        let mut analysis = self.analysis.lock().unwrap();
                        let a = unsafe { mem::transmute(a.clone()) };
                        *analysis = Some(a);
                    },
                },
            );

            // This must be called to attach the correct rls_data::Id to
            // the macro refs
            self.add_def_id(tcx);

            self.analysis
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .defs
                .extend(self.mac_defs.lock().unwrap().drain(..).map(|mac| mac.lower(&tcx)));
            self.analysis
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .refs
                .extend(self.mac_refs.lock().unwrap().drain(..).map(|rmac| rmac.lower(&tcx)));
        });
        Compilation::Continue
    }
}

#[cfg(feature = "clippy")]
fn clippy_config(config: &mut interface::Config) {
    let previous = config.register_lints.take();
    config.register_lints = Some(Box::new(move |sess, mut lint_store| {
        // technically we're ~guaranteed that this is none but might as well call anything that
        // is there already. Certainly it can't hurt.
        if let Some(previous) = &previous {
            (previous)(sess, lint_store);
        }

        let conf = clippy_lints::read_conf(&[], &sess);
        clippy_lints::register_plugins(&mut lint_store, &sess, &conf);
        clippy_lints::register_pre_expansion_lints(&mut lint_store, &conf);
        clippy_lints::register_renamed(&mut lint_store);
    }));
}

fn fetch_input_files(sess: &Session) -> Vec<PathBuf> {
    let cwd = &sess.working_dir.0;

    sess.source_map()
        .files()
        .iter()
        .filter(|fmap| fmap.is_real_file())
        .filter(|fmap| !fmap.is_imported())
        .map(|fmap| fmap.name.to_string())
        .map(|fmap| src_path(Some(cwd), fmap).unwrap())
        .collect()
}

/// Tries to read a file from a list of replacements, and if the file is not
/// there, then reads it from disk, by delegating to `RealFileLoader`.
struct ReplacedFileLoader {
    replacements: HashMap<PathBuf, String>,
    real_file_loader: RealFileLoader,
}

impl ReplacedFileLoader {
    fn new(replacements: HashMap<PathBuf, String>) -> ReplacedFileLoader {
        ReplacedFileLoader { replacements, real_file_loader: RealFileLoader }
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

pub(super) fn current_sysroot() -> Option<String> {
    let home = env::var("RUSTUP_HOME").or_else(|_| env::var("MULTIRUST_HOME"));
    let toolchain = env::var("RUSTUP_TOOLCHAIN").or_else(|_| env::var("MULTIRUST_TOOLCHAIN"));
    if let (Ok(home), Ok(toolchain)) = (home, toolchain) {
        Some(format!("{}/toolchains/{}", home, toolchain))
    } else {
        let rustc_exe = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
        env::var("SYSROOT").ok().or_else(|| {
            Command::new(rustc_exe)
                .arg("--print")
                .arg("sysroot")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_owned())
        })
    }
}

pub fn src_path(cwd: Option<&Path>, path: impl AsRef<Path>) -> Option<PathBuf> {
    let path = path.as_ref();

    Some(match (cwd, path.is_absolute()) {
        (_, true) => path.to_owned(),
        (Some(cwd), _) => cwd.join(path),
        (None, _) => std::env::current_dir().ok()?.join(path),
    })
}

fn unwrap_shared<T: std::fmt::Debug>(shared: Arc<Mutex<T>>, msg: &'static str) -> T {
    Arc::try_unwrap(shared).expect(msg).into_inner().unwrap()
}
