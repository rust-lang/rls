use std::{
    thread,
    sync::{Arc, Mutex, Condvar},
};

/// `ConcurrentJob` is a handle for some long-running computation
/// off the main thread. It can be used, indirectly, to wait for
/// the completion of the said computation.
///
/// All `ConncurrentJob`s must eventually be stored in a `Jobs` table.
///
/// All concurrent activities, like spawning a thread or pushing
/// a work item to a job queue, should be covered by `ConcurrentJob`.
/// This way, the set of `Jobs` table will give a complete overview of
/// concurrency in the system, and it will be possinle to wait for all
/// jobs to finish, which helps tremendously with making tests deterministic.
///
/// `JobToken` is the worker-side counterpart of `ConcurrentJob`. Dropping
/// a `JobToken` signals that the corresponding job has finished.
#[must_use]
pub struct ConcurrentJob {
    is_done: Arc<AtomicFlag>,
}

pub struct JobToken {
    is_done: Arc<AtomicFlag>
}

impl Drop for JobToken {
    fn drop(&mut self) {
        self.is_done.set()
    }
}

pub struct Jobs {
    jobs: Vec<ConcurrentJob>,
}

impl Jobs {
    pub fn new() -> Jobs {
        Jobs { jobs: Vec::new() }
    }

    pub fn add(&mut self, job: ConcurrentJob) {
        self.gc();
        self.jobs.push(job);
    }

    /// Blocks the current thread until all pending jobs are finished.
    pub fn wait_for_all(&mut self) {
        for job in self.jobs.drain(..) {
            job.wait();
        }
    }

    fn gc(&mut self) {
        self.jobs.retain(|job| !job.is_completed())
    }
}

impl ConcurrentJob {
    pub fn new() -> (ConcurrentJob, JobToken) {
        let is_done = Arc::new(AtomicFlag::new());
        let job = ConcurrentJob { is_done: is_done.clone() };
        let token = JobToken { is_done };
        (job, token)
    }

    fn wait(&self) {
        self.is_done.wait()
    }

    fn is_completed(&self) -> bool {
        self.is_done.is_set()
    }
}

impl Drop for ConcurrentJob {
    fn drop(&mut self) {
        if self.is_completed() || thread::panicking() {
            return;
        }
        panic!("orphaned concurrent job");
    }
}

struct AtomicFlag {
    flag: Mutex<bool>,
    cvar: Condvar,
}

impl AtomicFlag {
    fn new() -> AtomicFlag {
        AtomicFlag {
            flag: Mutex::new(false),
            cvar: Condvar::new(),
        }
    }

    fn is_set(&self) -> bool {
        *self.flag.lock().unwrap()
    }

    fn set(&self) {
        *self.flag.lock().unwrap() = true;
        self.cvar.notify_all();
    }

    fn wait(&self) {
        let mut is_set = self.flag.lock().unwrap();
        while !*is_set {
            is_set = self.cvar.wait(is_set).unwrap();
        }
    }
}
