
use std::mem;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::thread::{self, Thread};
use std::time::Duration;

#[derive(Debug, Serialize)]
pub enum BuildResult {
    Success(Vec<String>),
    Failure(Vec<String>),
    Squashed,
    Err,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildPriority {
    Immediate,
    Normal,
}

// TODO
/*

if there is a build in progress, wait
on change - wait for a running build - if previous build requests, skip them
on save or want info => if there is a queued build => cancel running build and run the queued one, if there is no queued build => no op
    - although is it better to have out of date info sooner, or new info later?
*/

// Builds will not return before a fresh build has completed, i.e., by
// by the time we return we will be at least as up to date as when the build was
// requested.

// However, there is no guarantee that any particular build will actually run.

// If we block on a channel, when we are woken we must no longer be on the pending list.

// If you add yourself to the pending list when a build is running, that build must wake you. If you add yourself when no build is running, you may or may not get woken.
pub struct BuildQueue {
    build_dir: Mutex<Option<String>>,
    // TODO can we not block on builds? Can we cancel them (or forget them)?
    // Note I have been conservative with Ordering, we might be able to do much better.
    running: AtomicBool,

    pending: Mutex<Vec<Sender<Signal>>>,
}

const WAIT_TO_BUILD: u64 = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Signal {
    Build,
    Skip,
}

impl BuildQueue {
    pub fn new() -> BuildQueue {
        BuildQueue {
            build_dir: Mutex::new(None),
            running: AtomicBool::new(false),
            pending: Mutex::new(vec![]),
        }
    }

    pub fn request_build(&self, build_dir: &str, priority: BuildPriority) -> BuildResult {
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
            }
        }

        // An immediate build gets started straightaway. Otherwise, we wait a
        // beat in case we get another build (e.g., while the user is typing).
        match priority {
            BuildPriority::Immediate => {
                // There is a build running, wait for it to finish, then run.
                if self.running.load(Ordering::SeqCst) {
                    let (tx, rx) = channel();
                    self.pending.lock().unwrap().push(tx);
                    // Blocks.
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
        // ourselves (so we don't add to the pending list). But we
        // do need to wait for that build to finish.
        if self.running.swap(true, Ordering::SeqCst) {
            let mut wait = 100;
            while self.running.load(Ordering::SeqCst) && wait < 50000 {
                thread::sleep(Duration::from_millis(wait));
                wait *= 2;
            }
            return BuildResult::Squashed;
        }

        self.cancel_pending();

        let result = self.build();
        self.running.store(false, Ordering::SeqCst);

        // If there is a pending build, run it now.
        let mut pending = self.pending.lock().unwrap();
        let pending = mem::replace(&mut *pending, vec![]);
        if !pending.is_empty() {
            pending[0].send(Signal::Build);
            for t in &pending[1..] {
                t.send(Signal::Skip);
            }
        }

        result
    }

    // Cancels all pending builds without running any of them.
    fn cancel_pending(&self) {
        let mut pending = self.pending.lock().unwrap();
        let pending = mem::replace(&mut *pending, vec![]);
        for t in pending {
            t.send(Signal::Skip);
        }
    }

    fn build(&self) -> BuildResult {
        use std::env;
        use std::process::Command;

        let mut cmd = Command::new("cargo");
        cmd.arg("rustc");
        cmd.arg("--");
        cmd.arg("-Zno-trans");
        cmd.env("RUSTFLAGS", "-Zunstable-options -Zsave-analysis --error-format=json \
                              -Zcontinue-parse-after-error");
        if let Ok(rls_rustc) = env::var("RLS_RUSTC") {
            cmd.env("RUSTC", &rls_rustc);
        }
        let build_dir = &self.build_dir.lock().unwrap();
        let build_dir = build_dir.as_ref().unwrap();
        cmd.current_dir(build_dir);
        println!("building {} ...", build_dir);
        match cmd.output() {
            Ok(x) => {
                let stderr_json_msg = convert_message_to_json_strings(x.stderr);
                match x.status.code() {
                    Some(0) => {
                        BuildResult::Success(stderr_json_msg)
                    }
                    Some(_) => {
                        BuildResult::Failure(stderr_json_msg)
                    }
                    None => BuildResult::Err
                }
            }
            Err(_) => {
                BuildResult::Err
            }
        }
    }
}


fn convert_message_to_json_strings(input: Vec<u8>) -> Vec<String> {
    let mut output = vec![];

    //FIXME: this is *so gross*  Trying to work around cargo not supporting json messages
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
