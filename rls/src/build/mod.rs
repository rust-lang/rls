//! Running builds as-needed for the server to answer questions.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, info, trace};
use rls_data::Analysis;
use rls_vfs::Vfs;

use self::environment::EnvironmentLock;
use self::plan::{BuildGraph, BuildPlan, WorkStatus};
pub use self::plan::{Crate, Edition};
use crate::actions::post_build::PostBuildHandler;
use crate::actions::progress::{ProgressNotifier, ProgressUpdate};
use crate::config::Config;
use crate::lsp_data::Range;

mod cargo;
mod cargo_plan;
pub mod environment;
mod external;
#[cfg(feature = "ipc")]
mod ipc;
mod plan;
mod rustc;

/// Manages builds.
///
/// The IDE will request builds quickly (possibly on every keystroke), there is
/// no point running every one. We also avoid running more than one build at once.
/// We cannot cancel builds. It might be worth running builds in parallel or
/// canceling a started build.
///
/// High priority builds are started 'straightaway' (builds cannot be interrupted).
/// Normal builds are started after a timeout. A new build request cancels any
/// pending build requests.
///
/// From the client's point of view, a build request is not guaranteed to cause
/// a build. However, a build is guaranteed to happen and that build will begin
/// after the build request is received (no guarantee on how long after), and
/// that build is guaranteed to have finished before the build request returns.
///
/// There is no way for the client to specify that an individual request will
/// result in a build. However, you can tell from the result - if a build
/// was run, the build result will contain any errors or warnings and an indication
/// of success or failure. If the build was not run, the result indicates that
/// it was squashed.
///
/// The build queue should be used from the RLS main thread, it should not be
/// used from multiple threads. It will spawn threads itself as necessary.
//
// See comment on `request_build` for implementation notes.
#[derive(Clone)]
pub struct BuildQueue {
    internals: Arc<Internals>,
    // The build queue -- we only have one low and one high priority build waiting.
    // (low, high) priority builds.
    // This lock should only be held transiently.
    queued: Arc<Mutex<(Build, Build)>>,
}

/// Used when tracking modified files across different builds.
type FileVersion = u64;

// Information needed to run and configure builds.
struct Internals {
    // Arguments and environment with which we call rustc.
    // This can be further expanded for multi-crate target configuration.
    // This lock should only be held transiently.
    compilation_cx: Arc<Mutex<CompilationContext>>,
    env_lock: Arc<EnvironmentLock>,
    /// Set of files that were modified since last build.
    dirty_files: Arc<Mutex<HashMap<PathBuf, FileVersion>>>,
    vfs: Arc<Vfs>,
    // This lock should only be held transiently.
    config: Arc<Mutex<Config>>,
    building: AtomicBool,
    /// A list of threads blocked on the current build queue. They should be
    /// resumed when there are no builds to run.
    blocked: Mutex<Vec<thread::Thread>>,
    last_build_duration: RwLock<Option<Duration>>,
}

/// The result of a build request.
#[derive(Debug)]
pub enum BuildResult {
    /// Build was performed without any internal errors. The payload
    /// contains current directory at the time, emitted raw diagnostics,
    /// Analysis data and list of input files to the compilation.
    /// Final bool is true if and only if compiler's exit code would be 0.
    Success(PathBuf, Vec<String>, Vec<Analysis>, HashMap<PathBuf, HashSet<Crate>>, bool),
    /// Build was coalesced with another build.
    Squashed,
    /// There was an error attempting to build.
    /// 0: error cause
    /// 1: command which caused the error
    Err(String, Option<String>),
    /// Cargo failed.
    CargoError {
        error: anyhow::Error,
        stdout: String,
        manifest_path: Option<PathBuf>,
        manifest_error_range: Option<Range>,
    },
}

/// Priority for a build request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildPriority {
    /// Run this build as soon as possible (e.g., on save or explicit build request).
    /// (Not currently used.)
    Immediate,
    /// Immediate, plus re-run Cargo.
    Cargo,
    /// A regular build request (e.g., on a minor edit).
    Normal,
}

impl BuildPriority {
    fn is_cargo(self) -> bool {
        match self {
            BuildPriority::Cargo => true,
            _ => false,
        }
    }
}

/// Information passed to Cargo/rustc to build.
#[derive(Debug)]
struct CompilationContext {
    cwd: Option<PathBuf>,
    /// The build directory is supplied by the client and passed to Cargo.
    build_dir: Option<PathBuf>,
    /// `true` if we need to perform a Cargo rebuild.
    needs_rebuild: bool,
    /// Build plan, which should know all the inter-package/target dependencies
    /// along with args/envs.
    build_plan: BuildPlan,
}

impl CompilationContext {
    fn new() -> CompilationContext {
        CompilationContext {
            cwd: None,
            build_dir: None,
            needs_rebuild: true,
            build_plan: BuildPlan::new(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
/// Specified set of packages to be built by Cargo.
pub enum PackageArg {
    Default,
    Packages(HashSet<String>),
}

/// Status of the build queue.
///
/// Pending should only be replaced if it is built or squashed. `InProgress` can be
/// replaced by None or Pending when appropriate. That is, Pending means something
/// is ready and something else may or may not be being built.
enum Build {
    // A build is in progress.
    InProgress,
    // A build is queued.
    Pending(Box<PendingBuild>),
    // No build.
    None,
}

/// Represents a queued build.
struct PendingBuild {
    build_dir: PathBuf,
    priority: BuildPriority,
    built_files: HashMap<PathBuf, FileVersion>,
    notifier: Box<dyn ProgressNotifier>,
    pbh: PostBuildHandler,
}

impl Build {
    fn is_pending(&self) -> bool {
        match *self {
            Build::Pending(_) => true,
            _ => false,
        }
    }

    // Returns `true` if the build is waiting and where it should be impossible for one to
    // be in progress.
    fn is_pending_fresh(&self) -> bool {
        match *self {
            Build::Pending(_) => true,
            Build::InProgress => unreachable!(),
            Build::None => false,
        }
    }

    fn try_into_pending(self) -> Result<PendingBuild, ()> {
        match self {
            Build::Pending(b) => Ok(*b),
            _ => Err(()),
        }
    }
}

impl BuildQueue {
    /// Constructs a new build queue.
    pub fn new(vfs: Arc<Vfs>, config: Arc<Mutex<Config>>) -> BuildQueue {
        BuildQueue {
            internals: Arc::new(Internals::new(vfs, config)),
            queued: Arc::new(Mutex::new((Build::None, Build::None))),
        }
    }

    /// Requests a build (see comments on `BuildQueue` for what that means).
    ///
    /// Now for the complicated bits. Not all builds are equal - they might have
    /// different arguments, build directory, etc. Lets call all such things the
    /// context for the build. We don't try and compare contexts but rely on
    /// some invariants:
    ///
    /// * Context can only change if the build priority is `Cargo` or the
    ///   `build_dir` changes (in the latter case we upgrade the priority to
    ///   `Cargo`).
    ///
    /// * If the context changes, all previous build requests can be ignored
    ///   (even if they change the context themselves).
    ///
    /// * If there are multiple requests with the same context, we can skip all
    ///   but the most recent.
    ///
    /// * A pending request is obsolete (and may be discarded) if a more recent
    ///   request has happened.
    ///
    /// ## Implementation
    ///
    /// This layer of the build queue is single-threaded and we aim to return
    /// quickly. A single build thread is spawned to do any building (we never
    /// do parallel builds so that we don't hog the CPU, we might want to change
    /// that in the future).
    ///
    /// There is never any point in queuing more than one build of each priority
    /// (we might want to do a high priority build, then a low priority one). So
    /// our build queue is just a single slot (for each priority). We record if
    /// a build is waiting and if not, if a build is running.
    pub fn request_build(
        &self,
        new_build_dir: &Path,
        mut priority: BuildPriority,
        notifier: Box<dyn ProgressNotifier>,
        pbh: PostBuildHandler,
    ) {
        trace!("request_build {:?}", priority);
        if self.internals.compilation_cx.lock().unwrap().needs_rebuild {
            priority = BuildPriority::Cargo;
        }
        let build = PendingBuild {
            build_dir: new_build_dir.to_owned(),
            built_files: self.internals.dirty_files.lock().unwrap().clone(),
            priority,
            notifier,
            pbh,
        };

        let mut queued = self.queued.lock().unwrap();
        Self::push_build(&mut queued, build);

        // Need to spawn while holding the lock on queued so that we don't race.
        if !self.internals.building.swap(true, Ordering::SeqCst) {
            thread::spawn({
                let queued = Arc::clone(&self.queued);
                let internals = Arc::clone(&self.internals);
                move || {
                    BuildQueue::run_thread(queued, &internals);
                    let building = internals.building.swap(false, Ordering::SeqCst);
                    assert!(building);
                }
            });
        }
    }

    /// Blocks until any currently queued builds are complete.
    ///
    /// Since an incoming build can squash a pending or executing one, we wait
    /// for all builds to complete, i.e., if any build is running when called,
    /// this function will not return until that build or a more recent one has
    /// completed. This means that if build requests keep coming, this function
    /// will never return. The caller should therefore block the dispatching
    /// thread (i.e., should be called from the same thread as `request_build`).
    pub fn block_on_build(&self) {
        loop {
            if !self.internals.building.load(Ordering::SeqCst) {
                return;
            }
            {
                let mut blocked = self.internals.blocked.lock().unwrap();
                blocked.push(thread::current());
            }
            thread::park();
        }
    }

    /// Essentially this is the opposite of 'would block' (see `block_on_build`). If this is
    /// true, then it is safe to rely on data from the build.
    pub fn build_ready(&self) -> bool {
        !self.internals.building.load(Ordering::SeqCst)
    }

    // Takes the unlocked build queue and pushes an incoming build onto it.
    fn push_build(queued: &mut (Build, Build), build: PendingBuild) {
        if build.priority == BuildPriority::Normal {
            Self::squash_build(&mut queued.0);
            queued.0 = Build::Pending(build.into());
        } else {
            Self::squash_build(&mut queued.0);
            Self::squash_build(&mut queued.1);
            queued.1 = Build::Pending(build.into());
        }
    }

    // Takes a reference to a build in the queue in preparation for pushing a
    // new build into the queue. The build is removed (if it exists) and its
    // closure is notified that the build is squashed.
    fn squash_build(build: &mut Build) {
        let mut old_build = Build::None;
        mem::swap(build, &mut old_build);
        if let Build::Pending(build) = old_build {
            build.pbh.handle(BuildResult::Squashed);
        }
    }

    // Run the build thread. This thread will keep going until the build queue is
    // empty, then terminate.
    fn run_thread(queued: Arc<Mutex<(Build, Build)>>, internals: &Internals) {
        loop {
            // Find the next build to run, or terminate if there are no builds.
            let build = {
                let mut queued = queued.lock().unwrap();
                if queued.1.is_pending_fresh() {
                    let mut build = Build::InProgress;
                    mem::swap(&mut queued.1, &mut build);
                    build.try_into_pending().unwrap()
                } else if queued.0.is_pending_fresh() {
                    let mut build = Build::InProgress;
                    mem::swap(&mut queued.0, &mut build);
                    build.try_into_pending().unwrap()
                } else {
                    return;
                }
            };

            // Normal priority threads sleep before starting up.
            if build.priority == BuildPriority::Normal {
                let build_wait = internals.build_wait();
                debug!("sleeping {:.1?}", build_wait);
                thread::sleep(build_wait);
                trace!("waking");

                // Check if a new build arrived while we were sleeping.
                let interrupt = {
                    let queued = queued.lock().unwrap();
                    queued.0.is_pending() || queued.1.is_pending()
                };
                if interrupt {
                    build.pbh.handle(BuildResult::Squashed);
                    continue;
                }
            }

            // Channel to get progress updates out for the async build.
            let (progress_sender, progress_receiver) = channel::<ProgressUpdate>();

            // Notifier of window/progress.
            let notifier = build.notifier;

            // Use this thread to propagate the progress messages until the sender is dropped.
            let progress_thread = thread::Builder::new()
                .name("progress-notifier".into())
                .spawn(move || {
                    // Window/progress notification that we are about to build.
                    notifier.notify_begin_progress();
                    while let Ok(progress) = progress_receiver.recv() {
                        notifier.notify_progress(progress);
                    }
                    notifier.notify_end_progress();
                })
                .expect("Failed to start progress-notifier thread");

            // Run the build.
            let result = internals.run_build(
                &build.build_dir,
                build.priority,
                &build.built_files,
                progress_sender,
            );
            // Assert that the build was not squashed.
            if let BuildResult::Squashed = result {
                unreachable!();
            }

            let mut pbh = build.pbh;
            {
                let mut blocked = internals.blocked.lock().unwrap();
                pbh.blocked_threads.extend(blocked.drain(..));
            }

            // wait for progress to complete before starting analysis
            progress_thread.join().expect("progress-notifier panicked!");
            pbh.handle(result);

            // Remove the in-progress marker from the build queue.
            let mut queued = queued.lock().unwrap();
            if let Build::InProgress = queued.1 {
                queued.1 = Build::None;
            } else if let Build::InProgress = queued.0 {
                queued.0 = Build::None;
            }
        }
    }

    /// Marks a given versioned file as dirty since last build. The dirty flag
    /// will be cleared by a successful build that builds this or a more recent
    /// version of this file.
    pub fn mark_file_dirty(&self, file: PathBuf, version: FileVersion) {
        trace!("Marking file as dirty: {:?} ({})", file, version);
        self.internals.dirty_files.lock().unwrap().insert(file, version);
    }
}

impl Internals {
    fn new(vfs: Arc<Vfs>, config: Arc<Mutex<Config>>) -> Internals {
        Internals {
            compilation_cx: Arc::new(Mutex::new(CompilationContext::new())),
            vfs,
            config,
            dirty_files: Arc::new(Mutex::new(HashMap::new())),
            // Since environment is global mutable state and we can run multiple server
            // instances, be sure to use a global lock to ensure env var consistency
            env_lock: EnvironmentLock::get(),
            building: AtomicBool::new(false),
            blocked: Mutex::new(vec![]),
            last_build_duration: RwLock::default(),
        }
    }

    // Entry point method for building.
    fn run_build(
        &self,
        new_build_dir: &Path,
        priority: BuildPriority,
        built_files: &HashMap<PathBuf, FileVersion>,
        progress_sender: Sender<ProgressUpdate>,
    ) -> BuildResult {
        trace!("run_build, {:?} {:?}", new_build_dir, priority);

        // Check if the build directory changed and update it.
        {
            let mut compilation_cx = self.compilation_cx.lock().unwrap();
            if compilation_cx.build_dir.as_ref().map_or(true, |dir| dir != new_build_dir) {
                // We'll need to re-run cargo in this case.
                assert!(priority.is_cargo());
                (*compilation_cx).build_dir = Some(new_build_dir.to_owned());
            }

            compilation_cx.needs_rebuild = priority.is_cargo();
        }

        let result = self.build(progress_sender);
        // On a successful build, clear dirty files that were successfully built
        // now. It's possible that a build was scheduled with given files, but
        // user later changed them. These should still be left as dirty (not built).
        if let BuildResult::Success(..) = result {
            let mut dirty_files = self.dirty_files.lock().unwrap();
            dirty_files.retain(|file, dirty_version| {
                built_files
                    .get(file)
                    .map(|built_version| built_version < dirty_version)
                    .unwrap_or(false)
            });
            trace!("Files still dirty after the build: {:?}", *dirty_files);
        }
        result
    }

    // Build the project.
    fn build(&self, progress_sender: Sender<ProgressUpdate>) -> BuildResult {
        trace!("running build");
        let start = Instant::now();
        // When we change build directory (presumably because the IDE is
        // changing project), we must do a cargo build of the whole project.
        // Otherwise we just use rustc directly.
        //
        // The 'full Cargo build' is a `cargo check` customised and run
        // in-process. Cargo will shell out to call rustc (this means the
        // the compiler available at runtime must match the compiler linked to
        // the RLS). All but the last crate are built as normal, we intercept
        // the call to the last crate and do our own rustc build. We cache the
        // command line args and environment so we can avoid running Cargo in
        // the future.
        //
        // Our 'short' rustc build runs rustc directly and in-process (we must
        // do this so we can load changed code from the VFS, rather than from
        // disk).

        // If the build plan has already been cached, use it, unless Cargo
        // has to be specifically rerun (e.g., when build scripts changed).
        let work = {
            let modified: Vec<_> = self.dirty_files.lock().unwrap().keys().cloned().collect();

            let mut cx = self.compilation_cx.lock().unwrap();
            let build_dir = cx.build_dir.clone().unwrap();
            let needs_rebuild = cx.needs_rebuild;

            // Check if an external build command was provided and execute that, instead.
            if let Some(cmd) = self.config.lock().unwrap().build_command.clone() {
                match (needs_rebuild, &cx.build_plan) {
                    (false, BuildPlan::External(ref plan)) => plan.prepare_work(&modified),
                    // We need to rebuild; regenerate the build plan if possible.
                    _ => match external::build_with_external_cmd(cmd, build_dir) {
                        (result, Err(_)) => return result,
                        (result, Ok(plan)) => {
                            cx.needs_rebuild = false;
                            cx.build_plan = BuildPlan::External(plan);
                            // Since we don't support diagnostics in external
                            // builds it might be worth rerunning the commands
                            // ourselves again to get both analysis *and* diagnostics.
                            return result;
                        }
                    },
                }
            // Fall back to Cargo.
            } else {
                // Cargo plan is recreated and `needs_rebuild` reset if we run `cargo::cargo()`.
                match cx.build_plan {
                    BuildPlan::External(_) => WorkStatus::NeedsCargo(PackageArg::Default),
                    BuildPlan::Cargo(ref plan) => {
                        match plan.prepare_work(&modified) {
                            // Don't reuse the plan if we need to rebuild.
                            WorkStatus::Execute(_) if needs_rebuild => {
                                WorkStatus::NeedsCargo(PackageArg::Default)
                            }
                            work => work,
                        }
                    }
                }
            }
        };
        trace!("specified work: {:#?}", work);

        let result = match work {
            WorkStatus::NeedsCargo(package_arg) => cargo::cargo(self, package_arg, progress_sender),
            WorkStatus::Execute(job_queue) => job_queue.execute(self, progress_sender),
        };

        if let BuildResult::Success(.., true) = result {
            let elapsed = start.elapsed();
            *self.last_build_duration.write().unwrap() = Some(elapsed);
            info!("build finished in {:.1?}", elapsed);
        }

        result
    }

    /// Returns a pre-build wait time facilitating build debouncing.
    ///
    /// Uses client configured value, or attempts to infer an appropriate duration.
    fn build_wait(&self) -> Duration {
        self.config.lock().unwrap().wait_to_build.map(Duration::from_millis).unwrap_or_else(|| {
            match *self.last_build_duration.read().unwrap() {
                Some(build_duration) if build_duration < Duration::from_secs(5) => {
                    if build_duration < Duration::from_millis(300) {
                        Duration::from_millis(0)
                    } else if build_duration < Duration::from_secs(1) {
                        Duration::from_millis(200)
                    } else {
                        Duration::from_millis(500)
                    }
                }
                _ => Duration::from_millis(1500),
            }
        })
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

#[test]
fn auto_tune_build_wait_no_config() {
    let i = Internals::new(Arc::new(Vfs::new()), Arc::default());

    // Pessimistic if no information.
    assert_eq!(i.build_wait(), Duration::from_millis(1500));

    // Very fast builds like hello world.
    *i.last_build_duration.write().unwrap() = Some(Duration::from_millis(70));
    assert_eq!(i.build_wait(), Duration::from_millis(0));

    // Somewhat fast builds should have a minimally impacting debounce for typing.
    *i.last_build_duration.write().unwrap() = Some(Duration::from_millis(850));
    assert_eq!(i.build_wait(), Duration::from_millis(200));

    // Medium builds should have a medium debounce time.
    *i.last_build_duration.write().unwrap() = Some(Duration::from_secs(4));
    assert_eq!(i.build_wait(), Duration::from_millis(500));

    // Slow builds. Lets wait just a bit longer, maybe they'll type something else?
    *i.last_build_duration.write().unwrap() = Some(Duration::from_secs(12));
    assert_eq!(i.build_wait(), Duration::from_millis(1500));
}

#[test]
fn dont_auto_tune_build_wait_configured() {
    let i = Internals::new(Arc::new(Vfs::new()), Arc::default());
    i.config.lock().unwrap().wait_to_build = Some(350);

    // Always use configured build wait if available.
    assert_eq!(i.build_wait(), Duration::from_millis(350));

    *i.last_build_duration.write().unwrap() = Some(Duration::from_millis(70));
    assert_eq!(i.build_wait(), Duration::from_millis(350));
}
