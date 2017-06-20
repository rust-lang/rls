// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use vfs::{self, Vfs, Change};

use std::collections::HashMap;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, Thread};
use std::time::Duration;

/// A queue for ensuring that changes happen in version order.
///
/// Assumptions:
/// * Each change comes on its own thread
/// * Version numbers are sequential
/// * Version numbers are per-file and independent
///
/// If a version number is missed, then we will wait for a few seconds and then
/// panic. The theory is that it is better to burn down the whole RLS than continue
/// with inconsistent state.
///
/// This is necessary because the RLS spawns a new thread for every message it
/// is sent. It is possible that a client sends multiple changes in order, but
/// basically at the same time (this is especially common when 'undo'ing). The
/// threads would then race to commit the changes to the VFS. This queue serialises
/// those changes.

const CHANGE_QUEUE_TIMEOUT: u64 = 5;

// We need a newtype because of public in private warnings :-(
pub struct ChangeQueue(ChangeQueue_);

impl ChangeQueue {
    pub fn new(vfs: Arc<Vfs>) -> ChangeQueue {
        ChangeQueue(ChangeQueue_::new(VfsSink(vfs)))
    }

    pub fn on_changes(&self, file_name: &Path, version: u64, changes: &[Change]) -> Result<(), vfs::Error> {
        self.0.on_changes(file_name, version, changes)
    }
}

struct ChangeQueue_<S = VfsSink> {
    sink: S,
    queues: Mutex<HashMap<PathBuf, Queue>>,
}

impl<S: ChangeSink> ChangeQueue_<S> {
    fn new(sink: S) -> ChangeQueue_<S> {
        ChangeQueue_ {
            sink,
            queues: Mutex::new(HashMap::new()),
        }
    }

    pub fn on_changes(&self, file_name: &Path, version: u64, changes: &[Change]) -> Result<(), vfs::Error> {
        trace!("on_changes: {} {:?}", version, changes);

        // It is important to hold the lock on self.queues for the whole time
        // from checking the current version until we are done making the change.
        // However, we must drop the lock if our thread suspends so that other
        // threads can make the changes we're blocked waiting for.
        let mut queues = self.queues.lock().unwrap();
        let cur_version = {
            let queue = queues.entry(file_name.to_owned()).or_insert(Queue::new());
            queue.cur_version
        };
        if cur_version.is_some() && Some(version) != cur_version {
            trace!("Blocking change {}, current: {:?}", version, cur_version);
            {
                let mut queue = queues.get_mut(file_name).unwrap();
                queue.queued.insert(version, thread::current());
            }
            mem::drop(queues);
            thread::park_timeout(Duration::from_secs(CHANGE_QUEUE_TIMEOUT));

            // We've been woken up - either because our change is next, or the timeout expired.
            queues = self.queues.lock().unwrap();
        }

        let mut queue = queues.get_mut(file_name).unwrap();
        // Fail if we timed-out rather than our thread was unparked.
        if cur_version.is_some() && Some(version) != queue.cur_version {
            eprintln!("Missing change, aborting. Found {}, expected {:?}", version, queue.cur_version);
            S::on_error();
        }

        queue.commit_change(version, changes, &self.sink)
    }
}

struct Queue {
    cur_version: Option<u64>,
    queued: HashMap<u64, Thread>,
}

impl Queue {
    fn new() -> Queue {
        Queue {
            cur_version: None,
            queued: HashMap::new(),
        }
    }

    fn commit_change<S: ChangeSink>(&mut self, version: u64, changes: &[Change], sink: &S) -> Result<(), vfs::Error> {
        trace!("commit_change {}, current: {:?}", version, self.cur_version);

        let result = sink.change(changes)?;
        let cur_version = version + 1;
        self.cur_version = Some(cur_version);

        if let Some(t) = self.queued.remove(&cur_version) {
            trace!("waking up change {}", cur_version);
            t.unpark();
        }

        Ok(result)
    }
}

// A wrapper around the VFS so we can test easily.
trait ChangeSink {
    // Make a change to the VFS (or mock the change).
    fn change(&self, changes: &[Change]) -> Result<(), vfs::Error>;
    // How to handle a sequencing error.
    fn on_error() -> !;
}

struct VfsSink(Arc<Vfs>);

impl ChangeSink for VfsSink {
    fn change(&self, changes: &[Change]) -> Result<(), vfs::Error> {
        self.0.on_changes(changes)
    }

    // Burn down the whole RLS.
    fn on_error() -> ! {
        ::std::process::abort();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::sync::{Mutex, Arc};
    use std::path::PathBuf;

    struct TestSink {
        expected: Mutex<HashMap<PathBuf, u64>>,
    }

    impl TestSink {
        fn new() -> TestSink {
            TestSink {
                expected: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ChangeSink for TestSink {
        fn change(&self, changes: &[Change]) -> Result<(), vfs::Error> {
            if let Change::AddFile { ref text, ref file } = changes[0] {
                let index: u64 = text.parse().unwrap();
                let mut expected = self.expected.lock().unwrap();
                let expected = expected.entry(file.to_owned()).or_insert(0);
                assert_eq!(*expected, index);
                *expected = index + 1;
                Ok(())
            } else {
                unreachable!();
            }
        }

        fn on_error() -> ! {
            panic!();
        }
    }

    #[test]
    fn test_queue_seq() {
        // Sanity test that checks we get the expected behaviour with no threading.

        let queue = ChangeQueue_::new(TestSink::new());
        queue.on_changes(Path::new("foo"), 0, &[Change::AddFile { file: PathBuf::new(), text: "0".to_owned() }]).unwrap();
        queue.on_changes(Path::new("foo"), 1, &[Change::AddFile { file: PathBuf::new(), text: "1".to_owned() }]).unwrap();
        queue.on_changes(Path::new("foo"), 2, &[Change::AddFile { file: PathBuf::new(), text: "2".to_owned() }]).unwrap();
        queue.on_changes(Path::new("foo"), 3, &[Change::AddFile { file: PathBuf::new(), text: "3".to_owned() }]).unwrap();
    }

    #[test]
    fn test_queue_concurrent() {
        let queue = Arc::new(ChangeQueue_::new(TestSink::new()));
        let mut threads = vec![];
        let foo = Path::new("foo");
        let bar = Path::new("bar");

        // Get the first changes in early. Otherwise a later change can land before
        // the first one which throws the testing off.
        queue.on_changes(foo, 2, &[Change::AddFile { file: foo.to_owned(), text: 0.to_string() }]).unwrap();
        queue.on_changes(bar, 2, &[Change::AddFile { file: bar.to_owned(), text: 0.to_string() }]).unwrap();
        for i in 3..100 {
            let queue_ = queue.clone();
            threads.push(thread::spawn(move || {
                queue_.on_changes(foo, i, &[Change::AddFile { file: foo.to_owned(), text: (i-2).to_string() }]).unwrap();
            }));

            let queue_ = queue.clone();
            threads.push(thread::spawn(move || {
                queue_.on_changes(bar, i, &[Change::AddFile { file: bar.to_owned(), text: (i-2).to_string() }]).unwrap();
            }));
        }

        for h in threads {
            h.join().unwrap();
        }
    }

    #[test]
    #[should_panic]
    fn test_queue_skip() {
        // Skip a change - the queue should panic rather than loop forever.
        let queue = Arc::new(ChangeQueue_::new(TestSink::new()));
        let mut threads = vec![];
        for i in 0..100 {
            if i == 45 {
                continue;
            }
            let queue = queue.clone();
            threads.push(thread::spawn(move || {
                queue.on_changes(Path::new("foo"), i, &[Change::AddFile { file: PathBuf::new(), text: i.to_string() }]).unwrap();
            }));
        }

        for h in threads {
            h.join().unwrap();
        }
    }
}
