use std::{thread};

use crossbeam_channel::{bounded, Receiver, Sender};
use crossbeam_channel::{select, __crossbeam_channel_parse, __crossbeam_channel_codegen};

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
    chan: Receiver<Never>,
}

pub struct JobToken {
    _chan: Sender<Never>,
}

#[derive(Default)]
pub struct Jobs {
    jobs: Vec<ConcurrentJob>,
}

impl Jobs {
    pub fn add(&mut self, job: ConcurrentJob) {
        self.gc();
        self.jobs.push(job);
    }

    /// Blocks the current thread until all pending jobs are finished.
    pub fn wait_for_all(&mut self) {
        while !self.jobs.is_empty() {
            let done: usize = {
                let chans = self.jobs.iter().map(|j| &j.chan);
                select! {
                    recv(chans, msg, from) => {
                        assert!(msg.is_none());
                        self.jobs.iter().position(|j| &j.chan == from).unwrap()
                    }
                }
            };
            drop(self.jobs.swap_remove(done));
        }
    }

    fn gc(&mut self) {
        self.jobs.retain(|job| !job.is_completed())
    }
}

impl ConcurrentJob {
    pub fn new() -> (ConcurrentJob, JobToken) {
        let (tx, rx) = bounded(0);
        let job = ConcurrentJob { chan: rx };
        let token = JobToken { _chan: tx };
        (job, token)
    }

    fn is_completed(&self) -> bool {
        is_closed(&self.chan)
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

// We don't actually send messages through the channels,
// and instead just check if the channel is closed,
// so we use uninhabited enum as a message type
enum Never {}

/// Nonblocking
fn is_closed(chan: &Receiver<Never>) -> bool {
    select! {
        recv(chan, msg) => match msg {
            None => true,
            Some(never) => match never {}
        }
        default => false,
    }
}
