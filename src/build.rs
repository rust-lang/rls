extern crate rustc_driver;
extern crate syntax;

use vfs::Vfs;

use self::rustc_driver::{RustcDefaultCalls, run_compiler, run};
use self::syntax::codemap::{FileLoader, RealFileLoader};

use std::collections::HashMap;
use std::env;
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
    build_dir: Mutex<Option<String>>,
    cmd_line: Mutex<Option<String>>,
    // True if a build is running.
    // Note I have been conservative with Ordering when accessing this atomic,
    // we might be able to do better.
    running: AtomicBool,
    // A vec of channels to pending build threads.
    pending: Mutex<Vec<Sender<Signal>>>,
    vfs: Arc<Vfs>,
}

#[derive(Debug, Serialize)]
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
            cmd_line: Mutex::new(None),
            running: AtomicBool::new(false),
            pending: Mutex::new(vec![]),
            vfs: vfs,
        }
    }

    pub fn request_build(&self, build_dir: &str, priority: BuildPriority) -> BuildResult {
        //println!("request_build, {} {:?}", build_dir, priority);
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

                let mut cmd_line = self.cmd_line.lock().unwrap();
                *cmd_line = None;
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
                    //println!("blocked on build");
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
                    //println!("blocked until wake up");
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
                //println!("loop of death");
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

    // Runs a `cargo build`. Note that what we actually run is not at all like
    // `cargo build`, except in spirit.
    fn build(&self) -> BuildResult {
        // When we build we are emulating `cargo build`, but trying to do so as
        // quickly as possible (FIXME(#24) we should add an option to do a real `cargo
        // build` for when the user wants to actually run the program).
        //
        // We build with `no-trans` to avoid generating code, and `save-analysis`
        // to get the data we need to perform IDE stuff.
        //
        // `cargo build` first builds all dependent crates, then builds the current
        // crate. The tricky bit for us is that there may be unsaved edits to
        // the current crate. So we want to `cargo build` the dependent crates,
        // then run our own version of rustc on the current crate + our edits.
        //
        // In order to do this we use `cargo rustc`, this runs `cargo build` on
        // the dependent crates then applies the given arguments to rustc for
        // the last crate. We give a bogus argument which forces an error (so as
        // not to waste time building the on-disk crate), and run `cargo` in
        // verbose mode which gives us the command line cargo used. We can then
        // remove the bogus argument and run that command line ourselves.
        //
        // Finally, we can also save a little time by caching that command line
        // and not running Cargo at all. We assume the dependent crates don't
        // change, except in the IDE. If the IDE changes the project directory,
        // they might be editing a dependent crate and so we blow away the cached
        // command line and run Cargo on the next build.

        let mut cmd_line = self.cmd_line.lock().unwrap();
        let build_dir = &self.build_dir.lock().unwrap();
        let build_dir = build_dir.as_ref().unwrap();

        if cmd_line.is_none() {
            let mut cmd = Command::new("cargo");
            // Using rustc rather than build means we can set flags which are
            // used only on the last crate.
            cmd.arg("rustc");
            cmd.arg("-v");
            cmd.arg("--");
            // We add this argument so we know that the rustc call for the last
            // crate will fail to build (no point wasting time when we'll do it
            // ourselves later).
            cmd.arg("--aBogusArgument");
            cmd.env("RUSTFLAGS", "-Zunstable-options -Zsave-analysis --error-format=json \
                                  -Zcontinue-parse-after-error -Zno-trans");
            cmd.current_dir(build_dir);

            let mut new_cmd_line = match cmd.output() {
                Ok(output) => {
                    let out = String::from_utf8(output.stderr).unwrap();
                    // println!("output: `{}`", out);
                    let exit_str = "     Running `";
                    match out.rfind(exit_str) {
                        Some(i) => {
                            let remaining = &out[i + exit_str.len() ..];
                            let end = remaining.find('`').unwrap();
                            remaining[..end].to_owned()
                        }
                        None => {
                            // println!("Couldn't parse stderr: `{}`", out);
                            return BuildResult::Err;
                        }
                    }
                }
                Err(e) => {
                    // println!("Error waiting for Cargo process: {:?}", e);
                    return BuildResult::Err;
                }
            };

            new_cmd_line = new_cmd_line.replace("--aBogusArgument ", "");

            let sysroot = env::var("SYS_ROOT").expect("No SYS_ROOT env var given");
            new_cmd_line.push_str(&format!(" --sysroot {}", sysroot));

            *cmd_line = Some(new_cmd_line);
        }

        self.rustc(cmd_line.as_ref().unwrap(), build_dir)
    }

    // Runs a single instance of rustc. Runs in-process.
    fn rustc(&self, cmd_line: &str, build_dir: &str) -> BuildResult {
        //println!("cmd_line: `{}`", cmd_line);

        let args: Vec<String> = cmd_line.split(' ').map(|s| s.to_owned()).collect();
        let changed = self.vfs.get_changed_files();

        let _pwd = WorkingDir::push(&Path::new(build_dir));
        let buf = Arc::new(Mutex::new(vec![]));
        let err_buf = buf.clone();

        //println!("building {} ...", build_dir);
        let exit_code = ::std::panic::catch_unwind(|| run(move || {
            // Use this struct instead of stderr so we catch most errors.
            struct BufWriter(Arc<Mutex<Vec<u8>>>);

            impl Write for BufWriter {
                fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                    self.0.lock().unwrap().write(buf)
                }
                fn flush(&mut self) -> io::Result<()> {
                    self.0.lock().unwrap().flush()
                }
            }

            run_compiler(&args,
                         &mut RustcDefaultCalls,
                         Some(Box::new(ReplacedFileLoader::new(changed))),
                         Some(Box::new(BufWriter(buf))))
        }));

        // FIXME(#25) given that we are running the compiler directly, there is no need
        // to serialise either the error messages or save-analysis - we should pass
        // them both in memory, without using save-analysis.
        let stderr_json_msg =
            convert_message_to_json_strings(Arc::try_unwrap(err_buf).unwrap().into_inner().unwrap());

        match exit_code {
            Ok(0) => BuildResult::Success(stderr_json_msg),
            Ok(_) => BuildResult::Failure(stderr_json_msg),
            Err(_) => BuildResult::Failure(stderr_json_msg),
        }
    }
}

// An RAII helper to set and reset the current workind directory.
struct WorkingDir {
    old_dir: PathBuf,
}

impl WorkingDir {
    fn push(p: &Path) -> WorkingDir {
        let result = WorkingDir {
            old_dir: env::current_dir().unwrap(),
        };
        env::set_current_dir(p).unwrap();
        result
    }
}

impl Drop for WorkingDir {
    fn drop(&mut self) {
        env::set_current_dir(&self.old_dir).unwrap();
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
