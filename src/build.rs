// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate rustc_driver;
extern crate syntax;

use cargo::core::{PackageId, MultiShell, Workspace};
use cargo::ops::{compile_with_exec, Executor, Context, CompileOptions, CompileMode, CompileFilter};
use cargo::util::{Config as CargoConfig, ProcessBuilder, ProcessError, homedir};
use cargo::util::important_paths::find_root_manifest_for_wd;

use vfs::Vfs;
use self::rustc_driver::{RustcDefaultCalls, run_compiler, run};
use self::syntax::codemap::{FileLoader, RealFileLoader};

use config::Config;

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;


/// Manages builds.
///
/// The IDE will request builds quickly (possibly on every keystroke), there is
/// no point running every one. We also avoid running more than one build at once.
/// We cannot cancel builds. It might be worth running builds in parallel or
/// cancelling a started build.
///
/// BuildPriority::Immediate builds are started straightaway. Normal builds are
/// started after a timeout. A new build request cancels any pending build requests.
///
/// From the client's point of view, a build request is not guaranteed to cause
/// a build. However, a build is guaranteed to happen and that build will begin
/// after the build request is received (no guarantee on how long after), and
/// that build is guaranteed to have finished before the build reqest returns.
///
/// There is no way for the client to specify that an individual request will
/// result in a build. However, you can tell from the result - if a build
/// was run, the build result will contain any errors or warnings and an indication
/// of success or failure. If the build was not run, the result indicates that
/// it was squashed.
pub struct BuildQueue {
    build_dir: Mutex<Option<PathBuf>>,
    cmd_line_args: Arc<Mutex<Vec<String>>>,
    cmd_line_envs: Arc<Mutex<HashMap<String, Option<OsString>>>>,
    // True if a build is running.
    // Note I have been conservative with Ordering when accessing this atomic,
    // we might be able to do better.
    running: AtomicBool,
    // A vec of channels to pending build threads.
    pending: Mutex<Vec<Sender<Signal>>>,
    vfs: Arc<Vfs>,
    config: Mutex<Config>,
}

#[derive(Debug, Serialize, Eq, PartialEq)]
pub enum BuildResult {
    // Build was succesful, argument is warnings.
    Success(Vec<String>),
    // Build finished with errors, argument is errors and warnings.
    Failure(Vec<String>),
    // Build was coelesced with another build.
    Squashed,
    // There was an error attempting to build.
    Err,
}

/// Priority for a build request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildPriority {
    /// Run this build as soon as possible (e.g., on save or explicit build request).
    Immediate,
    /// A regular build request (e.g., on a minor edit).
    Normal,
}

// Minimum time to wait before starting a `BuildPriority::Normal` build.
const WAIT_TO_BUILD: u64 = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Signal {
    Build,
    Skip,
}

impl BuildQueue {
    pub fn new(vfs: Arc<Vfs>) -> BuildQueue {
        BuildQueue {
            build_dir: Mutex::new(None),
            cmd_line_args: Arc::new(Mutex::new(vec![])),
            cmd_line_envs: Arc::new(Mutex::new(HashMap::new())),
            running: AtomicBool::new(false),
            pending: Mutex::new(vec![]),
            vfs: vfs,
            config: Mutex::new(Config::default()),
        }
    }

    pub fn request_build(&self, build_dir: &Path, priority: BuildPriority) -> BuildResult {
        // println!("request_build, {:?} {:?}", build_dir, priority);
        // If there is a change in the project directory, then we can forget any
        // pending build and start straight with this new build.
        {
            let mut reset = false;
            let mut prev_build_dir = self.build_dir.lock().unwrap();
            if let Some(ref prev_build_dir) = *prev_build_dir {
                if prev_build_dir != build_dir {
                    reset = true;
                }
            }

            if reset || prev_build_dir.is_none() {
                *prev_build_dir = Some(build_dir.to_owned());
                self.cancel_pending();

                let mut config = self.config.lock().unwrap();
                *config = Config::from_path(build_dir);

                let mut cmd_line_args = self.cmd_line_args.lock().unwrap();
                *cmd_line_args = vec![];
            }
        }

        self.cancel_pending();

        match priority {
            BuildPriority::Immediate => {
                // There is a build running, wait for it to finish, then run.
                if self.running.load(Ordering::SeqCst) {
                    let (tx, rx) = channel();
                    self.pending.lock().unwrap().push(tx);
                    // Blocks.
                    // println!("blocked on build");
                    let signal = rx.recv().unwrap_or(Signal::Build);
                    if signal == Signal::Skip {
                        return BuildResult::Squashed;
                    }
                }
            }
            BuildPriority::Normal => {
                let (tx, rx) = channel();
                self.pending.lock().unwrap().push(tx);
                thread::sleep(Duration::from_millis(WAIT_TO_BUILD));

                if self.running.load(Ordering::SeqCst) {
                    // Blocks
                    // println!("blocked until wake up");
                    let signal = rx.recv().unwrap_or(Signal::Build);
                    if signal == Signal::Skip {
                        return BuildResult::Squashed;
                    }
                } else {
                    // Doesn't block.
                    if rx.try_recv().unwrap_or(Signal::Build) == Signal::Skip {
                        return BuildResult::Squashed;
                    }
                }
            }
        }

        // If another build has started already, we don't need to build
        // ourselves (it must have arrived after this request; so we don't add
        // to the pending list). But we do need to wait for that build to
        // finish.
        if self.running.swap(true, Ordering::SeqCst) {
            let mut wait = 100;
            while self.running.load(Ordering::SeqCst) && wait < 50000 {
                // println!("loop of death");
                thread::sleep(Duration::from_millis(wait));
                wait *= 2;
            }
            return BuildResult::Squashed;
        }

        let result = self.build();
        self.running.store(false, Ordering::SeqCst);

        // If there is a pending build, run it now.
        let mut pending = self.pending.lock().unwrap();
        let pending = mem::replace(&mut *pending, vec![]);
        if !pending.is_empty() {
            // Kick off one build, then skip the rest.
            let mut pending = pending.iter();
            loop {
                let next = pending.next();
                let next = match next {
                    Some(n) => n,
                    None => break,
                };
                if let Ok(_) = next.send(Signal::Build) {
                    break;
                }
            }
            for t in pending {
                let _ = t.send(Signal::Skip);
            }
        }

        result
    }

    // Cancels all pending builds without running any of them.
    fn cancel_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        let pending = mem::replace(&mut *pending, vec![]);
        for t in pending {
            let _ = t.send(Signal::Skip);
        }
    }

    // Build the project.
    fn build(&self) -> BuildResult {
        // When we change build directory (presumably because the IDE is
        // changing project), we must do a cargo build of the whole project.
        // Otherwise we just use rustc directly.
        //
        // The 'full cargo build' is a `cargo check` customised and run
        // in-process. Cargo will shell out to call rustc (this means the
        // the compiler available at runtime must match the compiler linked to
        // the RLS). All but the last crate are built as normal, we intercept
        // the call to the last crate and do our own rustc build. We cache the
        // command line args and environment so we can avoid running Cargo in
        // the future.
        //
        // Our 'short' rustc build runs rustc directly and in-process (we must
        // do this so we can load changed code from the VFS, rather than from
        // disk). We get the data we need by building with `-Zsave-analysis`.

        let needs_to_run_cargo = {
            let cmd_line_args = self.cmd_line_args.lock().unwrap();
            cmd_line_args.is_empty()
        };

        let build_dir = &self.build_dir.lock().unwrap();
        let build_dir = build_dir.as_ref().unwrap();

        if needs_to_run_cargo {
            if self.cargo(build_dir.clone()) == BuildResult::Err {
                return BuildResult::Err;
            }
        }

        let cmd_line_args = self.cmd_line_args.lock().unwrap();
        assert!(!cmd_line_args.is_empty());
        let cmd_line_envs = self.cmd_line_envs.lock().unwrap();
        self.rustc(&*cmd_line_args, &*cmd_line_envs, build_dir)
    }

    // Runs an in-process instance of Cargo.
    fn cargo(&self, build_dir: PathBuf) -> BuildResult {
        struct RlsExecutor {
            cmd_line_args: Arc<Mutex<Vec<String>>>,
            cmd_line_envs: Arc<Mutex<HashMap<String, Option<OsString>>>>,
            cur_package_id: Mutex<Option<PackageId>>,
            config: Config,
        }

        impl RlsExecutor {
            fn new(cmd_line_args: Arc<Mutex<Vec<String>>>,
                   cmd_line_envs: Arc<Mutex<HashMap<String, Option<OsString>>>>,
                   config: Config) -> RlsExecutor {
                RlsExecutor {
                    cmd_line_args: cmd_line_args,
                    cmd_line_envs: cmd_line_envs,
                    cur_package_id: Mutex::new(None),
                    config: config,
                }
            }
        }

        impl Executor for RlsExecutor {
            fn init(&self, cx: &Context) {
                let mut cur_package_id = self.cur_package_id.lock().unwrap();
                *cur_package_id = Some(cx.ws
                                         .current_opt()
                                         .expect("No current package in Cargo")
                                         .package_id()
                                         .clone());
            }

            fn exec(&self, cmd: ProcessBuilder, id: &PackageId) -> Result<(), ProcessError> {
                let is_primary_crate = {
                    let cur_package_id = self.cur_package_id.lock().unwrap();
                    id == cur_package_id.as_ref().expect("Executor has not been initialised")
                };
                if is_primary_crate {
                    let mut args: Vec<_> =
                        cmd.get_args().iter().map(|a| a.clone().into_string().unwrap()).collect();

                    // We end up taking this code path for build scripts, we don't
                    // want to do that, so we check here if the crate is actually
                    // being linked (c.f., emit=metadata) and if just call the
                    // usual rustc. This is clearly a bit fragile (if the emit
                    // string changes, we get screwed).
                    if args.contains(&"--emit=dep-info,link".to_owned()) {
                        trace!("rustc not intercepted (link)");
                        return cmd.exec();
                    }

                    trace!("intercepted rustc, args: {:?}", args);

                    // FIXME here and below should check $RUSTC before using rustc.
                    {
                        // Cargo is going to expect to get dep-info for this crate, so we shell out
                        // to rustc to get that. This is not really ideal, because we are going to
                        // compute this info anyway when we run rustc ourselves, but we don't do
                        // that before we return to Cargo.
                        // FIXME Don't do this. Instead either persuade Cargo that it doesn't need
                        // this info at all, or start our build here rather than on another thread
                        // so the dep-info is ready by the time we return from this callback.
                        let mut cmd_dep_info = Command::new("rustc");
                        for a in &args {
                            if a.starts_with("--emit") {
                                cmd_dep_info.arg("--emit=dep-info");
                            } else {
                                cmd_dep_info.arg(a);
                            }
                        }
                        if let Some(cwd) = cmd.get_cwd() {
                            cmd_dep_info.current_dir(cwd);
                        }
                        cmd_dep_info.status().expect("Couldn't execute rustc");
                    }

                    args.insert(0, "rustc".to_owned());
                    if self.config.cfg_test {
                        args.push("--test".to_owned());
                    }
                    args.push("--sysroot".to_owned());
                    let home = option_env!("RUSTUP_HOME").or(option_env!("MULTIRUST_HOME"));
                    let toolchain = option_env!("RUSTUP_TOOLCHAIN").or(option_env!("MULTIRUST_TOOLCHAIN"));
                    let sys_root = if let (Some(home), Some(toolchain)) = (home, toolchain) {
                        format!("{}/toolchains/{}", home, toolchain)
                    } else {
                        option_env!("SYSROOT")
                            .map(|s| s.to_owned())
                            .or(Command::new("rustc")
                                .arg("--print")
                                .arg("sysroot")
                                .output()
                                .ok()
                                .and_then(|out| String::from_utf8(out.stdout).ok())
                                .map(|s| s.trim().to_owned()))
                            .expect("need to specify SYSROOT env var, \
                                     or use rustup or multirust")
                    };
                    args.push(sys_root.to_owned());

                    let envs = cmd.get_envs();
                    trace!("envs: {:?}", envs);

                    {
                        let mut queue_args = self.cmd_line_args.lock().unwrap();
                        *queue_args = args.clone();
                    }
                    {
                        let mut queue_envs = self.cmd_line_envs.lock().unwrap();
                        *queue_envs = envs.clone();
                    }

                    Ok(())
                } else {
                    trace!("rustc not intercepted");
                    cmd.exec()
                }
            }
        }

        let rls_config = {
            let rls_config = self.config.lock().unwrap();
            rls_config.clone()
        };

        trace!("cargo - `{:?}`", build_dir);
        let exec = RlsExecutor::new(self.cmd_line_args.clone(),
                                    self.cmd_line_envs.clone(),
                                    rls_config.clone());

        let out = Arc::new(Mutex::new(vec![]));
        let err = Arc::new(Mutex::new(vec![]));
        let out_clone = out.clone();
        let err_clone = err.clone();

        // Cargo may or may not spawn threads to run the various builds, since
        // we may be in separate threads we need to block and wait our thread.
        // However, if Cargo doesn't run a separate thread, then we'll just wait
        // forever. Therefore, we spawn an extra thread here to be safe.
        let handle = thread::spawn(move || {
            env::set_var("CARGO_TARGET_DIR", &Path::new("target").join("rls"));
            env::set_var("RUSTFLAGS",
                         "-Zunstable-options -Zsave-analysis --error-format=json \
                          -Zcontinue-parse-after-error");
            let shell = MultiShell::from_write(Box::new(BufWriter(out.clone())),
                                               Box::new(BufWriter(err.clone())));
            let config = CargoConfig::new(shell,
                                          build_dir.clone(),
                                          homedir(&build_dir).unwrap());
            let root = find_root_manifest_for_wd(None, config.cwd()).expect("could not find root manifest");
            let ws = Workspace::new(&root, &config).expect("could not create cargo workspace");
            let mut opts = CompileOptions::default(&config, CompileMode::Check);
            if rls_config.build_lib {
                opts.filter = CompileFilter::new(true, &[], &[], &[], &[]);
            }
            compile_with_exec(&ws, &opts, Arc::new(exec)).expect("could not run cargo");
        });

        trace!("cargo stdout {}", String::from_utf8(out_clone.lock().unwrap().to_owned()).unwrap());
        trace!("cargo stderr {}", String::from_utf8(err_clone.lock().unwrap().to_owned()).unwrap());

        if let Err(_) = handle.join() {
            BuildResult::Err
        } else {
            BuildResult::Success(vec![])
        }
    }

    // Runs a single instance of rustc. Runs in-process.
    fn rustc(&self, args: &[String], envs: &HashMap<String, Option<OsString>>, build_dir: &Path) -> BuildResult {
        trace!("rustc - args: `{:?}`, envs: {:?}, build dir: {:?}", args, envs, build_dir);

        let changed = self.vfs.get_cached_files();

        let _pwd = Environment::push(&Path::new(build_dir), envs);
        let buf = Arc::new(Mutex::new(vec![]));
        let err_buf = buf.clone();
        let args = args.to_owned();

        let exit_code = ::std::panic::catch_unwind(|| {
            run(move || {
                // Replace stderr so we catch most errors.
                run_compiler(&args,
                             &mut RustcDefaultCalls,
                             Some(Box::new(ReplacedFileLoader::new(changed))),
                             Some(Box::new(BufWriter(buf))))
            })
        });

        // FIXME(#25) given that we are running the compiler directly, there is no need
        // to serialise either the error messages or save-analysis - we should pass
        // them both in memory, without using save-analysis.
        let stderr_json_msg = convert_message_to_json_strings(Arc::try_unwrap(err_buf)
            .unwrap()
            .into_inner()
            .unwrap());

        match exit_code {
            Ok(0) => BuildResult::Success(stderr_json_msg),
            Ok(_) => BuildResult::Failure(stderr_json_msg),
            Err(_) => BuildResult::Failure(stderr_json_msg),
        }
    }
}

// A threadsafe buffer for writing.
struct BufWriter(Arc<Mutex<Vec<u8>>>);

impl Write for BufWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

// An RAII helper to set and reset the current working directory and env vars.
struct Environment {
    old_dir: PathBuf,
    old_vars: HashMap<String, Option<OsString>>,
}

impl Environment {
    fn push(p: &Path, envs: &HashMap<String, Option<OsString>>) -> Environment {
        let mut result = Environment {
            old_dir: env::current_dir().unwrap(),
            old_vars: HashMap::new(),
        };

        env::set_current_dir(p).unwrap();
        for (k, v) in envs {
            result.old_vars.insert(k.to_owned(), env::var_os(k));
            match *v {
                Some(ref v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
        result
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        env::set_current_dir(&self.old_dir).unwrap();

        for (k, v) in &self.old_vars {
            match *v {
                Some(ref v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
    }
}

fn convert_message_to_json_strings(input: Vec<u8>) -> Vec<String> {
    let mut output = vec![];

    // FIXME: this is *so gross*  Trying to work around cargo not supporting json messages
    let it = input.into_iter();

    let mut read_iter = it.skip_while(|&x| x != b'{');

    let mut _msg = String::new();
    loop {
        match read_iter.next() {
            Some(b'\n') => {
                output.push(_msg);
                _msg = String::new();
                while let Some(res) = read_iter.next() {
                    if res == b'{' {
                        _msg.push('{');
                        break;
                    }
                }
            }
            Some(x) => {
                _msg.push(x as char);
            }
            None => {
                break;
            }
        }
    }

    output
}

/// Tries to read a file from a list of replacements, and if the file is not
/// there, then reads it from disk, by delegating to RealFileLoader.
pub struct ReplacedFileLoader {
    replacements: HashMap<PathBuf, String>,
    real_file_loader: RealFileLoader,
}

impl ReplacedFileLoader {
    pub fn new(replacements: HashMap<PathBuf, String>) -> ReplacedFileLoader {
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
