use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fmt::{self, Write};
use std::fs::{read_dir, remove_file};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use cargo::core::compiler::{BuildConfig, CompileMode, Context, Executor, Unit};
use cargo::core::resolver::{CliFeatures, ResolveError};
use cargo::core::Package;
use cargo::core::{PackageId, Shell, Target, TargetKind, Verbosity, Workspace};
use cargo::ops::{compile_with_exec, CompileFilter, CompileOptions, Packages};
use cargo::util::{
    config as cargo_config, errors::ManifestError, homedir, important_paths, CargoResult,
    ConfigValue,
};
use cargo_util::ProcessBuilder;
use log::{debug, trace, warn};
use rls_data::Analysis;
use rls_vfs::Vfs;

use crate::actions::progress::ProgressUpdate;
use crate::build::cargo_plan::CargoPlan;
use crate::build::environment::{self, Environment, EnvironmentLock};
use crate::build::plan::{BuildPlan, Crate};
use crate::build::{BufWriter, BuildResult, CompilationContext, Internals, PackageArg};
use crate::config::Config;
use crate::lsp_data::{Position, Range};

// Runs an in-process instance of Cargo.
pub(super) fn cargo(
    internals: &Internals,
    package_arg: PackageArg,
    progress_sender: Sender<ProgressUpdate>,
) -> BuildResult {
    let compilation_cx = Arc::clone(&internals.compilation_cx);
    let config = Arc::clone(&internals.config);
    let vfs = Arc::clone(&internals.vfs);
    let env_lock = Arc::clone(&internals.env_lock);

    let diagnostics = Arc::default();
    let analysis = Arc::default();
    let input_files = Arc::default();
    let out = Arc::default();

    // Cargo may or may not spawn threads to run the various builds, since
    // we may be in separate threads we need to block and wait our thread.
    // However, if Cargo doesn't run a separate thread, then we'll just wait
    // forever. Therefore, we spawn an extra thread here to be safe.
    let handle = thread::spawn({
        let diagnostics = Arc::clone(&diagnostics);
        let analysis = Arc::clone(&analysis);
        let input_files = Arc::clone(&input_files);
        let out = Arc::clone(&out);
        || {
            run_cargo(
                compilation_cx,
                package_arg,
                config,
                vfs,
                env_lock,
                diagnostics,
                analysis,
                input_files,
                out,
                progress_sender,
            )
        }
    });

    match handle.join().map_err(|_| anyhow::Error::msg("thread panicked")).and_then(|res| res) {
        Ok(ref cwd) => {
            let diagnostics = Arc::try_unwrap(diagnostics).unwrap().into_inner().unwrap();
            let analysis = Arc::try_unwrap(analysis).unwrap().into_inner().unwrap();
            let input_files = Arc::try_unwrap(input_files).unwrap().into_inner().unwrap();
            BuildResult::Success(cwd.clone(), diagnostics, analysis, input_files, true)
        }
        Err(error) => {
            let stdout = String::from_utf8(out.lock().unwrap().to_owned()).unwrap();

            let (manifest_path, manifest_error_range) = {
                let mae = error.downcast_ref::<ManifestAwareError>();
                (mae.map(|e| e.manifest_path().clone()), mae.and_then(|e| e.manifest_error_range()))
            };
            BuildResult::CargoError { error, stdout, manifest_path, manifest_error_range }
        }
    }
}

fn run_cargo(
    compilation_cx: Arc<Mutex<CompilationContext>>,
    package_arg: PackageArg,
    rls_config: Arc<Mutex<Config>>,
    vfs: Arc<Vfs>,
    env_lock: Arc<EnvironmentLock>,
    compiler_messages: Arc<Mutex<Vec<String>>>,
    analysis: Arc<Mutex<Vec<Analysis>>>,
    input_files: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
    out: Arc<Mutex<Vec<u8>>>,
    progress_sender: Sender<ProgressUpdate>,
) -> Result<PathBuf, anyhow::Error> {
    // Lock early to guarantee synchronized access to env var for the scope of Cargo routine.
    // Additionally we need to pass inner lock to `RlsExecutor`, since it needs to hand it down
    // during `exec()` callback when calling linked compiler in parallel, for which we need to
    // guarantee consistent environment variables.
    let (lock_guard, inner_lock) = env_lock.lock();
    let restore_env = Environment::push_with_lock(&BTreeMap::new(), None, lock_guard);

    let build_dir = compilation_cx.lock().unwrap().build_dir.clone().unwrap();

    // Note that this may not be equal build_dir when inside a workspace member
    let manifest_path = important_paths::find_root_manifest_for_wd(&build_dir)?;
    trace!("root manifest_path: {:?}", &manifest_path);

    // Cargo constructs relative paths from the manifest dir, so we have to pop "Cargo.toml"
    let manifest_dir = manifest_path.parent().unwrap();

    let mut shell = Shell::from_write(Box::new(BufWriter(Arc::clone(&out))));
    shell.set_verbosity(Verbosity::Quiet);

    let mut config = {
        let rls_config = rls_config.lock().unwrap();

        let target_dir = rls_config.target_dir.as_ref().as_ref().map(|p| p as &Path);
        make_cargo_config(manifest_dir, target_dir, restore_env.get_old_cwd(), shell)
    };
    config.nightly_features_allowed = true;

    let ws = Workspace::new(&manifest_path, &config)
        .map_err(|err| ManifestAwareError::new(err, &manifest_path, None))?;

    run_cargo_ws(
        compilation_cx,
        package_arg,
        rls_config,
        vfs,
        compiler_messages,
        analysis,
        input_files,
        progress_sender,
        inner_lock,
        restore_env,
        &manifest_path,
        &config,
        &ws,
    )
    .map_err(|err| ManifestAwareError::new(err, &manifest_path, Some(&ws)).into())
}

fn run_cargo_ws(
    compilation_cx: Arc<Mutex<CompilationContext>>,
    package_arg: PackageArg,
    rls_config: Arc<Mutex<Config>>,
    vfs: Arc<Vfs>,
    compiler_messages: Arc<Mutex<Vec<String>>>,
    analysis: Arc<Mutex<Vec<Analysis>>>,
    input_files: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
    progress_sender: Sender<ProgressUpdate>,
    inner_lock: environment::InnerLock,
    mut restore_env: Environment<'_>,
    manifest_path: &PathBuf,
    config: &cargo_config::Config,
    ws: &Workspace<'_>,
) -> CargoResult<PathBuf> {
    let (all, packages) = match package_arg {
        PackageArg::Default => (false, vec![]),
        PackageArg::Packages(pkgs) => (false, pkgs.into_iter().collect()),
    };

    // TODO: it might be feasible to keep this `CargoOptions` structure cached and regenerate
    // it on every relevant configuration change.
    let (opts, rustflags, clear_env_rust_log, cfg_test) = {
        // We mustn't lock configuration for the whole build process
        let rls_config = rls_config.lock().unwrap();

        let opts = CargoOptions::new(&rls_config);
        trace!("Cargo compilation options:\n{:?}", opts);
        let rustflags = prepare_cargo_rustflags(&rls_config);

        for package in &packages {
            if ws.members().find(|x| *x.name() == *package).is_none() {
                warn!(
                    "couldn't find member package `{}` specified in `analyze_package` \
                     configuration",
                    package
                );
            }
        }

        (opts, rustflags, rls_config.clear_env_rust_log, rls_config.cfg_test)
    };

    let spec = Packages::from_flags(all, Vec::new(), packages)?;

    let pkg_names = spec
        .to_package_id_specs(&ws)?
        .iter()
        .map(|pkg_spec| pkg_spec.name().as_str().to_owned())
        .collect();
    trace!("specified packages to be built by Cargo: {:#?}", pkg_names);

    // Since the Cargo build routine will try to regenerate the unit dep graph,
    // we need to clear the existing dep graph.
    compilation_cx.lock().unwrap().build_plan =
        BuildPlan::Cargo(CargoPlan::with_packages(manifest_path, pkg_names));

    let compile_opts = CompileOptions {
        spec,
        filter: CompileFilter::from_raw_arguments(
            opts.lib,
            opts.bin,
            opts.bins,
            // TODO: support more crate target types.
            Vec::new(),
            // Check all integration tests under `tests/`.
            cfg_test,
            Vec::new(),
            false,
            Vec::new(),
            false,
            opts.all_targets,
        ),
        build_config: BuildConfig::new(
            &config,
            opts.jobs,
            opts.target.iter().map(|x| x.into()).collect::<Vec<String>>().as_slice(),
            CompileMode::Check { test: cfg_test },
        )?,
        cli_features: CliFeatures::from_command_line(
            &opts.features,
            opts.all_features,
            !opts.no_default_features,
        )?,
        ..CompileOptions::new(&config, CompileMode::Check { test: cfg_test })?
    };

    // Create a custom environment for running cargo, the environment is reset
    // afterwards automatically.
    restore_env.push_var("RUSTFLAGS", &Some(rustflags.into()));

    if clear_env_rust_log {
        restore_env.push_var("RUST_LOG", &None);
    }

    let reached_primary = Arc::new(AtomicBool::new(false));

    let exec = RlsExecutor::new(
        &ws,
        Arc::clone(&compilation_cx),
        rls_config,
        inner_lock,
        vfs,
        compiler_messages,
        analysis,
        input_files,
        progress_sender,
        Arc::clone(&reached_primary),
    );

    // Cargo excludes target/ from backups since rust-lang/cargo@cf3bfc9/rust-lang/cargo#8378 but
    // it does so if and only if the directory does not exist and it's about to create it.
    // rls runs cargo internally with target directory set to target/rls/ so, if target/ doesn't
    // exist yet and rls runs, target/ and target/rls/ will be created. While target/rls/ will be
    // excluded from backups target/ won't be (as from our perspective it's not the target
    // directory but its parent) and, when user runs "cargo build" themselves cargo will see
    // target/ existing already and won't exclude it from backups. We can work around that by
    // attempting to create a backup-excluded target/ ourelves using cargo paths:: machinery.
    cargo_util::paths::create_dir_all_excluded_from_backups_atomic(
        config.target_dir().unwrap().unwrap().as_path_unlocked().parent().unwrap(),
    )?;
    let exec = Arc::new(exec) as Arc<dyn Executor>;
    match compile_with_exec(&ws, &compile_opts, &exec) {
        Ok(_) => {
            trace!(
                "created build plan after Cargo compilation routine: {:?}",
                compilation_cx.lock().unwrap().build_plan
            );
        }
        Err(e) => {
            if !reached_primary.load(Ordering::SeqCst) {
                debug!("error running `compile_with_exec`: {:?}", e);
                return Err(e);
            } else {
                warn!("ignoring error running `compile_with_exec`: {:?}", e);
            }
        }
    }

    if !reached_primary.load(Ordering::SeqCst) {
        return Err(anyhow::format_err!("error compiling dependent crate"));
    }

    Ok(compilation_cx
        .lock()
        .unwrap()
        .cwd
        .clone()
        .unwrap_or_else(|| restore_env.get_old_cwd().to_path_buf()))
}

struct RlsExecutor {
    compilation_cx: Arc<Mutex<CompilationContext>>,
    config: Arc<Mutex<Config>>,
    /// Because of the Cargo API design, we first acquire outer lock before creating the executor
    /// and calling the compilation function. This, resulting, inner lock is used to synchronize
    /// env var access during underlying `rustc()` calls during parallel `exec()` callback threads.
    env_lock: environment::InnerLock,
    vfs: Arc<Vfs>,
    analysis: Arc<Mutex<Vec<Analysis>>>,
    /// Packages which are directly a member of the workspace, for which
    /// analysis and diagnostics will be provided.
    member_packages: Mutex<HashSet<PackageId>>,
    input_files: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
    /// JSON compiler messages emitted for each primary compiled crate.
    compiler_messages: Arc<Mutex<Vec<String>>>,
    progress_sender: Mutex<Sender<ProgressUpdate>>,
    /// Set to true if attempt to compile a primary crate. If we don't track
    /// this then errors which prevent giving type info won't be shown to the
    /// user. This feels a bit hacky, but I can't see how to otherwise
    /// distinguish compile errors on dependent crates from the primary crate
    /// (which are handled directly by the RLS).
    reached_primary: Arc<AtomicBool>,
}

impl RlsExecutor {
    fn new(
        ws: &Workspace<'_>,
        compilation_cx: Arc<Mutex<CompilationContext>>,
        config: Arc<Mutex<Config>>,
        env_lock: environment::InnerLock,
        vfs: Arc<Vfs>,
        compiler_messages: Arc<Mutex<Vec<String>>>,
        analysis: Arc<Mutex<Vec<Analysis>>>,
        input_files: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
        progress_sender: Sender<ProgressUpdate>,
        reached_primary: Arc<AtomicBool>,
    ) -> RlsExecutor {
        let member_packages = ws.members().map(Package::package_id).collect();

        RlsExecutor {
            compilation_cx,
            config,
            env_lock,
            vfs,
            analysis,
            input_files,
            member_packages: Mutex::new(member_packages),
            compiler_messages,
            progress_sender: Mutex::new(progress_sender),
            reached_primary,
        }
    }

    /// Returns `true if a given package is a primary one (every member of the
    /// workspace is considered as such). Used to determine whether the RLS
    /// should cache invocations for these packages and rebuild them on changes.
    fn is_primary_package(&self, id: PackageId) -> bool {
        id.source_id().is_path() || self.member_packages.lock().unwrap().contains(&id)
    }
}

impl Executor for RlsExecutor {
    /// Called after a rustc process invocation is prepared up-front for a given
    /// unit of work (may still be modified for runtime-known dependencies, when
    /// the work is actually executed). This is called even for a target that
    /// is fresh and won't be compiled.
    fn init<'a>(&self, cx: &Context<'a, '_>, unit: &Unit) {
        let mut compilation_cx = self.compilation_cx.lock().unwrap();
        let plan = compilation_cx
            .build_plan
            .as_cargo_mut()
            .expect("build plan should be properly initialized before running Cargo");

        let only_primary = |unit: &Unit| self.is_primary_package(unit.pkg.package_id());

        plan.emplace_dep_with_filter(unit, cx, &only_primary);
    }

    fn force_rebuild(&self, unit: &Unit) -> bool {
        // We need to force rebuild every package in the
        // workspace, even if it's not dirty at a time, to cache compiler
        // invocations in the build plan.
        // We only do a cargo build if we want to force rebuild the last
        // crate (e.g., because some args changed). Therefore, we should
        // always force rebuild the primary crate.
        let id = unit.pkg.package_id();
        // FIXME: build scripts -- this will force rebuild build scripts as
        // well as the primary crate. But this is not too bad -- it means
        // we will rarely rebuild more than we have to.
        self.is_primary_package(id)
    }

    fn exec(
        &self,
        cargo_cmd: &ProcessBuilder,
        id: PackageId,
        target: &Target,
        mode: CompileMode,
        _on_stdout_line: &mut dyn FnMut(&str) -> CargoResult<()>,
        _on_stderr_line: &mut dyn FnMut(&str) -> CargoResult<()>,
    ) -> CargoResult<()> {
        let mut cargo_cmd = cargo_cmd.clone();
        // Enforce JSON output so that we can parse the rustc output by
        // stripping --error-format if it was specified (e.g. Cargo pipelined
        // build)
        let filtered_args = filter_arg(cargo_cmd.get_args(), "--error-format");
        cargo_cmd.args_replace(&filtered_args);
        cargo_cmd.arg("--error-format=json");
        // Delete any stale data. We try and remove any json files with
        // the same crate name as Cargo would emit. This includes files
        // with the same crate name but different hashes, e.g., those
        // made with a different compiler.
        let cargo_args = cargo_cmd.get_args();
        let crate_name =
            parse_arg(cargo_args, "--crate-name").expect("no crate-name in rustc command line");
        let cfg_test = cargo_args.iter().any(|arg| arg == "--test");
        trace!("exec: {} {:?}", crate_name, cargo_cmd);

        // Send off a window/progress notification for this compile target.
        // At the moment, we don't know the number of things cargo is going to compile,
        // so we just send the name of each thing we find.
        {
            let progress_sender = self.progress_sender.lock().unwrap();
            progress_sender
                .send(ProgressUpdate::Message(if cfg_test {
                    format!("{} cfg(test)", crate_name)
                } else {
                    crate_name.clone()
                }))
                .expect("failed to send progress update");
        }

        let out_dir = parse_arg(cargo_args, "--out-dir").expect("no out-dir in rustc command line");
        let analysis_dir = Path::new(&out_dir).join("save-analysis");
        if let Ok(dir_contents) = read_dir(&analysis_dir) {
            let lib_crate_name = "lib".to_owned() + &crate_name;
            for entry in dir_contents {
                let entry = entry.expect("unexpected error reading save-analysis directory");
                let name = entry.file_name();
                let name = name.to_str().unwrap();
                if (name.starts_with(&crate_name) || name.starts_with(&lib_crate_name))
                    && name.ends_with(".json")
                {
                    if let Err(e) = remove_file(entry.path()) {
                        debug!("error deleting file, {}: {}", name, e);
                    }
                }
            }
        }

        // Prepare our own call to `rustc` as follows:
        // 1. Use `$RUSTC` wrapper if specified, otherwise use RLS executable
        //    as an rustc shim (needed to distribute via the stable channel)
        // 2. For non-primary packages or build scripts, execute the call
        // 3. Otherwise, we'll want to use the compilation to drive the analysis:
        //    i.  Modify arguments to account for the RLS settings (e.g.,
        //        compiling under `cfg(test)` mode or passing a custom sysroot),
        //    ii. Execute the call and store the final args/envs to be used for
        //        later in-process execution of the compiler.
        let mut cmd = cargo_cmd.clone();

        // RLS executable can be spawned in a different directory than the one
        // that Cargo was spawned in, so be sure to use absolute RLS path (which
        // `env::current_exe()` returns) for the shim.
        let rustc_shim = env::var("RUSTC")
            .ok()
            .or_else(|| env::current_exe().ok().and_then(|x| x.to_str().map(String::from)))
            .expect("Couldn't set executable for RLS rustc shim");
        cmd.program(rustc_shim);
        // In case RLS is set as the rustc shim signal to `main_inner()` to call
        // `rls_rustc::run()`.
        cmd.env(crate::RUSTC_SHIM_ENV_VAR_NAME, "1");

        // Add args and envs to cmd.
        let mut args: Vec<_> =
            cargo_args.iter().map(|a| a.clone().into_string().unwrap()).collect();
        let envs = cargo_cmd.get_envs().clone();

        let sysroot = super::rustc::current_sysroot()
            .expect("need to specify `SYSROOT` env var or use rustup or multirust");

        {
            let config = self.config.lock().unwrap();
            if config.sysroot.is_none() {
                args.push("--sysroot".to_owned());
                args.push(sysroot);
            }
        }
        cmd.args_replace(&args);
        for (k, v) in &envs {
            if let Some(v) = v {
                cmd.env(k, v);
            }
        }

        // We only want to intercept rustc call targeting current crate to cache
        // args/envs generated by cargo so we can run only rustc later ourselves
        // Currently we don't cache nor modify build script args
        let is_build_script = *target.kind() == TargetKind::CustomBuild;
        if !self.is_primary_package(id) || is_build_script {
            let build_script_notice = if is_build_script { " (build script)" } else { "" };
            trace!(
                "rustc not intercepted - {}{} - args: {:?} envs: {:?}",
                id.name(),
                build_script_notice,
                cmd.get_args(),
                cmd.get_envs(),
            );

            let (crate_blacklist, full_docs) = {
                let config = self.config.lock().unwrap();
                (config.crate_blacklist.clone(), *config.full_docs.clone().as_ref())
            };
            if crate_blacklist.as_ref().0.contains(&crate_name) {
                // By running the original command (rather than using our shim), we
                // avoid producing save-analysis data.
                trace!("crate is blacklisted");
                return cargo_cmd.exec();
            }
            // Only include public symbols in externally compiled deps data
            let save_config = serde_json::to_string(&rls_data::config::Config {
                pub_only: true,
                reachable_only: true,
                full_docs,
                ..Default::default()
            })?;
            cmd.env("RUST_SAVE_ANALYSIS_CONFIG", &OsString::from(save_config));

            return cmd.exec();
        }

        trace!("rustc intercepted - args: {:?} envs: {:?}", args, envs,);

        self.reached_primary.store(true, Ordering::SeqCst);

        // Cache executed command for the build plan.
        {
            let mut cx = self.compilation_cx.lock().unwrap();
            let plan = cx.build_plan.as_cargo_mut().unwrap();
            plan.cache_compiler_job(id, target, mode, &cmd);
        }

        // Prepare modified cargo-generated args/envs for future rustc calls.
        let rustc = cargo_cmd.get_program().to_owned().into_string().unwrap();
        args.insert(0, rustc);

        // Store the modified cargo-generated args/envs for future rustc calls.
        {
            let mut compilation_cx = self.compilation_cx.lock().unwrap();
            compilation_cx.needs_rebuild = false;
            compilation_cx.cwd = cargo_cmd.get_cwd().map(ToOwned::to_owned);
        }

        let build_dir = {
            let cx = self.compilation_cx.lock().unwrap();
            cx.build_dir.clone().unwrap()
        };

        if let BuildResult::Success(_, mut messages, mut analysis, input_files, success) =
            super::rustc::rustc(
                &self.vfs,
                &args,
                &envs,
                cargo_cmd.get_cwd(),
                &build_dir,
                Arc::clone(&self.config),
                &self.env_lock.as_facade(),
            )
        {
            self.compiler_messages.lock().unwrap().append(&mut messages);
            self.analysis.lock().unwrap().append(&mut analysis);

            // Cache calculated input files for a given rustc invocation.
            {
                let mut cx = self.compilation_cx.lock().unwrap();
                let plan = cx.build_plan.as_cargo_mut().unwrap();
                let input_files = input_files.keys().cloned().collect();
                plan.cache_input_files(id, target, mode, input_files, cargo_cmd.get_cwd());
            }

            let mut self_input_files = self.input_files.lock().unwrap();
            for (file, inputs) in input_files {
                self_input_files.entry(file).or_default().extend(inputs);
            }

            if !success {
                return Err(anyhow::format_err!("Build error"));
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
struct CargoOptions {
    target: Option<String>,
    lib: bool,
    bin: Vec<String>,
    bins: bool,
    all_features: bool,
    no_default_features: bool,
    features: Vec<String>,
    jobs: Option<u32>,
    all_targets: bool,
}

impl Default for CargoOptions {
    fn default() -> CargoOptions {
        CargoOptions {
            target: None,
            lib: false,
            bin: vec![],
            bins: false,
            all_features: false,
            no_default_features: false,
            features: vec![],
            jobs: None,
            all_targets: false,
        }
    }
}

impl CargoOptions {
    fn new(config: &Config) -> CargoOptions {
        CargoOptions {
            target: config.target.clone(),
            features: config.features.clone(),
            all_features: config.all_features,
            no_default_features: config.no_default_features,
            jobs: config.jobs,
            all_targets: config.all_targets,
            ..CargoOptions::default()
        }
    }
}

fn prepare_cargo_rustflags(config: &Config) -> String {
    let mut flags = env::var("RUSTFLAGS").unwrap_or_else(|_| String::new());

    if let Some(config_flags) = &config.rustflags {
        write!(flags, " {}", config_flags.as_str()).unwrap();
    }

    if let Some(sysroot) = &config.sysroot {
        write!(flags, " --sysroot {}", sysroot).unwrap();
    }

    dedup_flags(&flags)
}

/// Constructs a cargo configuration for the given build and target directories
/// and shell.
pub fn make_cargo_config(
    build_dir: &Path,
    target_dir: Option<&Path>,
    cwd: &Path,
    shell: Shell,
) -> cargo_config::Config {
    let config = cargo_config::Config::new(shell, cwd.to_path_buf(), homedir(build_dir).unwrap());

    // Cargo is expecting the config to come from a config file and keeps
    // track of the path to that file. We'll make one up, it shouldn't be
    // used for much. Cargo does use it for finding a root path. Since
    // we pass an absolute path for the build directory, that doesn't
    // matter too much. However, Cargo still takes the grandparent of this
    // path, so we need to have at least two path elements.
    let config_path = build_dir.join("config").join("rls-config.toml");

    let mut config_value_map = config.load_values().unwrap();
    {
        let build_value = config_value_map.entry("build".to_owned()).or_insert_with(|| {
            let def = cargo_config::Definition::Path(config_path.clone());
            ConfigValue::Table(HashMap::new(), def)
        });

        let target_dir = target_dir.map(|d| d.to_str().unwrap().to_owned()).unwrap_or_else(|| {
            // Try to use .cargo/config build.target-dir + "/rls"
            let cargo_target = build_value
                .table("build")
                .ok()
                .and_then(|(build, _)| build.get("target-dir"))
                .and_then(|td| td.string("target-dir").ok())
                .map(|(target, _)| {
                    let t_path = Path::new(target);
                    if t_path.is_absolute() {
                        t_path.into()
                    } else {
                        build_dir.join(t_path)
                    }
                })
                .unwrap_or_else(|| build_dir.join("target"));

            cargo_target.join("rls").to_str().unwrap().to_owned()
        });

        let def = cargo_config::Definition::Path(config_path);
        let td_value = ConfigValue::String(target_dir, def);
        if let ConfigValue::Table(ref mut build_table, _) = *build_value {
            build_table.insert("target-dir".to_owned(), td_value);
        } else {
            unreachable!();
        }
    }

    config.set_values(config_value_map).unwrap();
    config
}

fn parse_arg(args: &[OsString], arg: &str) -> Option<String> {
    for (i, a) in args.iter().enumerate() {
        if a == arg {
            return Some(args[i + 1].clone().into_string().unwrap());
        }
    }
    None
}

/// Removes any duplicate flags from `flag_str` (a string of command line args for Rust).
fn dedup_flags(flag_str: &str) -> String {
    // The basic strategy here is that we split `flag_str` into a set of keys and
    // values and dedup any duplicate keys, using the last value in `flag_str`.
    // This is a bit complicated because of the variety of ways args can be specified.

    // Retain flags order to prevent complete project rebuild due to `RUSTFLAGS` fingerprint change.
    let mut flags = BTreeMap::new();
    let mut bits = flag_str.split_whitespace().peekable();

    while let Some(bit) = bits.next() {
        let mut bit = bit.to_owned();
        // Handle `-Z foo` the same way as `-Zfoo`.
        if bit.len() == 2 && bits.peek().is_some() && !bits.peek().unwrap().starts_with('-') {
            let bit_clone = bit.clone();
            let mut bit_chars = bit_clone.chars();
            if bit_chars.next().unwrap() == '-' && bit_chars.next().unwrap() != '-' {
                bit.push_str(bits.next().unwrap());
            }
        }

        if bit.starts_with('-') {
            if bit.contains('=') {
                // Split only on the first equals sign (there may be more than one).
                let bits: Vec<_> = bit.splitn(2, '=').collect();
                assert!(bits.len() == 2);
                flags.insert(bits[0].to_owned() + "=", bits[1].to_owned());
            } else if bits.peek().is_some() && !bits.peek().unwrap().starts_with('-') {
                flags.insert(bit, bits.next().unwrap().to_owned());
            } else {
                flags.insert(bit, String::new());
            }
        } else {
            // A standalone arg with no flag, no deduplication to do. We merge these
            // together, which is probably not ideal, but is simple.
            flags.entry(String::new()).or_insert_with(String::new).push_str(&format!(" {}", bit));
        }
    }

    // Put the map back together as a string.
    let mut result = String::new();
    for (k, v) in &flags {
        if k.is_empty() {
            result.push_str(v);
        } else {
            result.push(' ');
            result.push_str(k);
            if !v.is_empty() {
                if !k.ends_with('=') {
                    result.push(' ');
                }
                result.push_str(v);
            }
        }
    }
    result
}

/// Removes a selected flag of a `--flag=VALUE` or `--flag VALUE` shape from `args` (command line args for Rust).
fn filter_arg(args: &[OsString], key: &str) -> Vec<String> {
    let key_as_prefix = key.to_owned() + "=";
    let mut ret = vec![];

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let first = arg.to_str().unwrap();

        if first == key {
            iter.next();
        } else if first.starts_with(&key_as_prefix) {
            // no-op
        } else {
            ret.push(first.to_owned());
        }
    }

    ret
}

/// Error wrapper that tries to figure out which manifest the cause best relates to in the project
#[derive(Debug)]
pub struct ManifestAwareError {
    cause: anyhow::Error,
    /// The path to a manifest file within the project that seems the closest to the error's origin.
    nearest_project_manifest: PathBuf,
    manifest_error_range: Option<Range>,
}

impl ManifestAwareError {
    fn new(cause: anyhow::Error, root_manifest: &Path, ws: Option<&Workspace<'_>>) -> Self {
        let project_dir = root_manifest.parent().unwrap();
        let mut err_path = root_manifest;
        // Cover whole manifest if we haven't any better idea.
        let mut err_range = None;

        if let Some(manifest_err) = cause.downcast_ref::<ManifestError>() {
            // Scan through any manifest errors to pin the error more precisely.
            let is_project_manifest =
                |path: &PathBuf| path.is_file() && path.starts_with(project_dir);

            let last_cause = manifest_err.manifest_causes().last().unwrap_or(manifest_err);
            if is_project_manifest(last_cause.manifest_path()) {
                // Manifest with the issue is inside the project.
                err_path = last_cause.manifest_path().as_path();
                // This can be replaced by Error::chain when it is stabilized.
                fn find_toml_error(
                    err: &(dyn std::error::Error + 'static),
                ) -> Option<(usize, usize)> {
                    if let Some(toml_err) = err.downcast_ref::<toml_edit::TomlError>() {
                        toml_err.line_col()
                    } else if let Some(toml_err) = err.downcast_ref::<toml_edit::de::Error>() {
                        toml_err.line_col()
                    } else if let Some(toml_err) = err.downcast_ref::<toml::de::Error>() {
                        toml_err.line_col()
                    } else {
                        find_toml_error(err.source()?)
                    }
                }
                if let Some((line, col)) = find_toml_error(last_cause) {
                    let line = line as _;
                    let start_col = col as _;
                    let end_col = start_col + 1;
                    // Use TOML deserializiation error position.
                    err_range = Some(Range {
                        start: Position::new(line, start_col),
                        end: Position::new(line, end_col),
                    });
                }
            } else {
                let nearest_cause = manifest_err
                    .manifest_causes()
                    .filter(|e| is_project_manifest(e.manifest_path()))
                    .last();
                if let Some(nearest) = nearest_cause {
                    // Not the root cause, but the nearest manifest to it in the project.
                    err_path = nearest.manifest_path().as_path();
                }
            }
        } else if let (Some(ws), Some(resolve_err)) = (ws, cause.downcast_ref::<ResolveError>()) {
            // If the resolve error leads to a workspace member, we should use that manifest.
            if let Some(member) = resolve_err
                .package_path()
                .iter()
                .filter_map(|pkg| ws.members().find(|m| m.package_id() == *pkg))
                .next()
            {
                err_path = member.manifest_path();
            }
        }

        let nearest_project_manifest = err_path.to_path_buf();
        Self { cause, nearest_project_manifest, manifest_error_range: err_range }
    }

    pub fn manifest_path(&self) -> &PathBuf {
        &self.nearest_project_manifest
    }

    pub fn manifest_error_range(&self) -> Option<Range> {
        self.manifest_error_range
    }
}
impl fmt::Display for ManifestAwareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.cause.fmt(f)
    }
}
impl std::error::Error for ManifestAwareError {}

#[cfg(test)]
mod test {
    use super::{dedup_flags, filter_arg};

    #[test]
    fn test_dedup_flags() {
        // These should all be preserved.
        assert!(dedup_flags("") == "");
        assert!(dedup_flags("-Zfoo") == " -Zfoo");
        assert!(dedup_flags("-Z foo") == " -Zfoo");
        assert!(dedup_flags("-Zfoo bar") == " -Zfoo bar");
        let result = dedup_flags("-Z foo foo bar");
        assert!(result.matches("foo").count() == 2);
        assert!(result.matches("bar").count() == 1);

        // These should get deduplicated.
        assert!(dedup_flags("-Zfoo -Zfoo") == " -Zfoo");
        assert!(dedup_flags("-Zfoo -Zfoo -Zfoo") == " -Zfoo");
        let result = dedup_flags("-Zfoo -Zfoo -Zbar");
        assert!(result.matches("foo").count() == 1);
        assert!(result.matches("bar").count() == 1);
        let result = dedup_flags("-Zfoo -Zbar -Zfoo -Zbar -Zbar");
        assert!(result.matches("foo").count() == 1);
        assert!(result.matches("bar").count() == 1);
        assert!(dedup_flags("-Zfoo -Z foo") == " -Zfoo");

        assert!(dedup_flags("--error-format=json --error-format=json") == " --error-format=json");
        assert!(dedup_flags("--error-format=foo --error-format=json") == " --error-format=json");

        assert!(
            dedup_flags(
                "-C link-args=-fuse-ld=gold -C target-cpu=native -C link-args=-fuse-ld=gold"
            ) == " -Clink-args=-fuse-ld=gold -Ctarget-cpu=native"
        );
    }

    #[test]
    fn test_filter_arg() {
        use std::ffi::OsString;

        fn args(input: &str) -> Vec<OsString> {
            input.split_whitespace().map(OsString::from).collect()
        }

        assert!(filter_arg(&args("--error-format=json"), "--error-format").is_empty());
        assert!(filter_arg(&args("--error-format json"), "--error-format").is_empty());
        assert_eq!(filter_arg(&args("-a --error-format=json"), "--error-format"), ["-a"]);
        assert_eq!(filter_arg(&args("-a --error-format json"), "--error-format"), ["-a"]);
        assert_eq!(filter_arg(&args("-a --error-format=json -b"), "--error-format"), ["-a", "-b"]);
        assert_eq!(filter_arg(&args("-a --error-format json -b"), "--error-format"), ["-a", "-b"]);
        assert_eq!(filter_arg(&args("-a -b -x"), "--error-format"), ["-a", "-b", "-x"]);
    }
}
