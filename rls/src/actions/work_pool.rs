use crate::server::DEFAULT_REQUEST_TIMEOUT;
use lazy_static::lazy_static;
use log::{info, warn};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};
use std::{fmt, panic};

/// Description of work on the request work pool. Equality implies two pieces of work are the same
/// kind of thing. The `str` should be human readable for logging (e.g., the language server
/// protocol request message name or similar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkDescription(pub &'static str);

impl fmt::Display for WorkDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

lazy_static! {
    /// Maximum total concurrent working tasks
    static ref NUM_THREADS: usize = ::num_cpus::get();

    /// Duration of work after which we should warn something is taking a long time
    static ref WARN_TASK_DURATION: Duration = DEFAULT_REQUEST_TIMEOUT * 5;

    /// Current work descriptions active on the work pool
    static ref WORK: Mutex<Vec<WorkDescription>> = Mutex::new(vec![]);

    /// Thread pool for request execution allowing concurrent request processing.
    static ref WORK_POOL: rayon::ThreadPool = rayon::ThreadPoolBuilder::new()
        .num_threads(*NUM_THREADS)
        .thread_name(|num| format!("request-worker-{}", num))
        .build()
        .unwrap();
}

/// Maximum concurrent working tasks of the same type (equal `WorkDescription`)
/// Note: `2` allows a single task to run immediately after a similar task has timed out.
/// Once multiple similar tasks have timed out but remain running we start refusing to start new
/// ones.
const MAX_SIMILAR_CONCURRENT_WORK: usize = 2;

/// Runs work in a new thread on the `WORK_POOL` returning a result `Receiver`
///
/// Panicking work will receive `Err(RecvError)` / `Err(RecvTimeoutError::Disconnected)`
///
/// If too many tasks are already running the work will not be done and the receiver will
/// immediately return `Err(RecvTimeoutError::Disconnected)`
pub fn receive_from_thread<T, F>(work_fn: F, description: WorkDescription) -> mpsc::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + panic::UnwindSafe + 'static,
{
    let (sender, receiver) = mpsc::channel();

    {
        let mut work = WORK.lock().unwrap();
        if work.len() >= *NUM_THREADS {
            // there are already N ongoing tasks, that may or may not have timed out
            // don't add yet more to the queue fail fast to allow the work pool to recover
            warn!("Could not start `{}` as at work capacity, {:?} in progress", description, *work,);
            return receiver;
        }
        if work.iter().filter(|desc| *desc == &description).count() >= MAX_SIMILAR_CONCURRENT_WORK {
            // this type of work is already filling max proportion of the work pool, so fail
            // new requests of this kind until some/all the ongoing work finishes
            info!(
                "Could not start `{}` as same work-type is filling half capacity, {:?} in progress",
                description, *work,
            );
            return receiver;
        }
        work.push(description);
    }

    WORK_POOL.spawn(move || {
        let start = Instant::now();

        // panic details will be on stderr, otherwise ignore the work panic as it
        // will already cause a mpsc disconnect-error & there isn't anything else to log
        if let Ok(work_result) = panic::catch_unwind(work_fn) {
            // an error here simply means the work took too long and the receiver has been dropped
            let _ = sender.send(work_result);
        }

        let mut work = WORK.lock().unwrap();
        if let Some(index) = work.iter().position(|desc| desc == &description) {
            work.swap_remove(index);
        }

        let elapsed = start.elapsed();
        if elapsed >= *WARN_TASK_DURATION {
            let secs =
                elapsed.as_secs() as f64 + f64::from(elapsed.subsec_nanos()) / 1_000_000_000_f64;
            warn!("`{}` took {:.1}s", description, secs);
        }
    });
    receiver
}
