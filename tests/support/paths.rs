#![allow(unknown_lints)]

use std::cell::Cell;
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

static RLS_INTEGRATION_TEST_DIR: &str = "rlsit";
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

thread_local!(static TASK_ID: usize = NEXT_ID.fetch_add(1, Ordering::SeqCst));

fn init() {
    static GLOBAL_INIT: Once = Once::new();
    thread_local!(static LOCAL_INIT: Cell<bool> = Cell::new(false));
    GLOBAL_INIT.call_once(|| {
        global_root().mkdir_p();
    });
    LOCAL_INIT.with(|i| {
        if i.get() {
            return;
        }
        i.set(true);
        root().rm_rf();
    })
}

fn global_root() -> PathBuf {
    let mut path = env::current_exe().unwrap();
    path.pop(); // chop off exe name
    path.pop(); // chop off 'debug'

    // If `cargo test` is run manually then our path looks like
    // `target/debug/foo`, in which case our `path` is already pointing at
    // `target`. If, however, `cargo test --target $target` is used then the
    // output is `target/$target/debug/foo`, so our path is pointing at
    // `target/$target`. Here we conditionally pop the `$target` name.
    if path.file_name().and_then(OsStr::to_str) != Some("target") {
        path.pop();
    }

    path.join(RLS_INTEGRATION_TEST_DIR)
}

pub fn root() -> PathBuf {
    init();
    global_root().join(&TASK_ID.with(|my_id| format!("t{}", my_id)))
}

pub trait TestPathExt {
    fn rm_rf(&self);
    fn mkdir_p(&self);
}

#[allow(clippy::redundant_closure)] // &Path is not AsRef<Path>
impl TestPathExt for Path {
    /* Technically there is a potential race condition, but we don't
     * care all that much for our tests
     */
    fn rm_rf(&self) {
        if !self.exists() {
            return;
        }

        for file in fs::read_dir(self).unwrap() {
            let file = file.unwrap().path();

            if file.is_dir() {
                file.rm_rf();
            } else {
                // On windows we can't remove a readonly file, and git will
                // often clone files as readonly. As a result, we have some
                // special logic to remove readonly files on windows.
                do_op(&file, "remove file", |p| fs::remove_file(p));
            }
        }
        do_op(self, "remove dir", |p| fs::remove_dir(p));
    }

    fn mkdir_p(&self) {
        fs::create_dir_all(self)
            .unwrap_or_else(|e| panic!("failed to mkdir_p {}: {}", self.display(), e))
    }
}

fn do_op<F>(path: &Path, desc: &str, mut f: F)
where
    F: FnMut(&Path) -> io::Result<()>,
{
    match f(path) {
        Ok(()) => {}
        Err(ref e) if cfg!(windows) && e.kind() == ErrorKind::PermissionDenied => {
            let mut p = path.metadata().unwrap().permissions();
            p.set_readonly(false);
            fs::set_permissions(path, p).unwrap();
            f(path).unwrap_or_else(|e| {
                panic!("failed to {} {}: {}", desc, path.display(), e);
            })
        }
        Err(e) => {
            panic!("failed to {} {}: {}", desc, path.display(), e);
        }
    }
}
