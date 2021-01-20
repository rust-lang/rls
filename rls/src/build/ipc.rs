use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::{env, fs};

use jsonrpc_core::{ErrorCode, IoHandler};

use crate::build::plan::Crate;

use rls_ipc::rpc::{self, Error, Result as RpcResult};
use rls_ipc::server::{CloseHandle, ServerBuilder};

/// An IPC server spawned on a different thread.
pub struct Server {
    endpoint: PathBuf,
    join_handle: std::thread::JoinHandle<()>,
    close_handle: CloseHandle,
}

impl Server {
    /// Returns an endpoint on which the server is listening.
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Shuts down the IPC server and waits on the thread it was spawned on.
    pub fn close(self) {
        self.close_handle.close();
        let _ = self.join_handle.join();
    }
}

/// Starts an IPC server in the background supporting both VFS requests and data
/// callbacks used by rustc for the out-of-process compilation.
pub fn start_with_all(
    changed_files: HashMap<PathBuf, String>,
    analysis: Arc<Mutex<Option<rls_data::Analysis>>>,
    input_files: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
) -> Result<Server, ()> {
    use rls_ipc::rpc::callbacks::Server as _;
    use rls_ipc::rpc::file_loader::Server as _;

    let mut io = IoHandler::new();
    io.extend_with(ChangedFiles(changed_files).to_delegate());
    io.extend_with(callbacks::CallbackHandler { analysis, input_files }.to_delegate());

    self::start_with_handler(io)
}

/// Spins up an IPC server in the background.
pub fn start_with_handler(io: IoHandler) -> Result<Server, ()> {
    let endpoint_path = gen_endpoint_path();
    let (tx, rx) = std::sync::mpsc::channel();
    let join_handle = std::thread::spawn({
        let endpoint_path = endpoint_path.clone();
        move || {
            log::trace!("Attempting to spin up IPC server at {}", endpoint_path);
            let server = ServerBuilder::new(io)
                .start(&endpoint_path)
                .map_err(|_| log::warn!("Couldn't open socket"))
                .unwrap();
            log::trace!("Started the IPC server at {}", endpoint_path);

            tx.send(server.close_handle()).unwrap();
            server.wait();
        }
    });

    rx.recv_timeout(Duration::from_secs(5))
        .map(|close_handle| Server { endpoint: endpoint_path.into(), join_handle, close_handle })
        .map_err(|_| ())
}

fn gen_endpoint_path() -> String {
    let num: u64 = rand::Rng::gen(&mut rand::thread_rng());
    if cfg!(windows) {
        format!(r"\\.\pipe\ipc-pipe-{}", num)
    } else {
        format!(r"/tmp/ipc-uds-{}", num)
    }
}

fn rpc_error(msg: &str) -> Error {
    Error { code: ErrorCode::InternalError, message: msg.to_owned(), data: None }
}

mod callbacks {
    use super::PathBuf;
    use super::{rpc, RpcResult};
    use super::{Arc, Mutex};
    use super::{HashMap, HashSet};

    impl From<rls_ipc::rpc::Crate> for crate::build::plan::Crate {
        fn from(krate: rls_ipc::rpc::Crate) -> Self {
            Self {
                name: krate.name,
                src_path: krate.src_path,
                edition: match krate.edition {
                    rls_ipc::rpc::Edition::Edition2015 => crate::build::plan::Edition::Edition2015,
                    rls_ipc::rpc::Edition::Edition2018 => crate::build::plan::Edition::Edition2018,
                    rls_ipc::rpc::Edition::Edition2021 => crate::build::plan::Edition::Edition2021,
                },
                disambiguator: krate.disambiguator,
            }
        }
    }

    pub struct CallbackHandler {
        pub analysis: Arc<Mutex<Option<rls_data::Analysis>>>,
        pub input_files: Arc<Mutex<HashMap<PathBuf, HashSet<crate::build::plan::Crate>>>>,
    }

    impl rpc::callbacks::Rpc for CallbackHandler {
        fn complete_analysis(&self, analysis: rls_data::Analysis) -> RpcResult<()> {
            *self.analysis.lock().unwrap() = Some(analysis);
            Ok(())
        }

        fn input_files(
            &self,
            input_files: HashMap<PathBuf, HashSet<rls_ipc::rpc::Crate>>,
        ) -> RpcResult<()> {
            let mut current_files = self.input_files.lock().unwrap();
            for (file, crates) in input_files {
                current_files.entry(file).or_default().extend(crates.into_iter().map(From::from));
            }
            Ok(())
        }
    }
}

pub struct ChangedFiles(HashMap<PathBuf, String>);

impl rpc::file_loader::Rpc for ChangedFiles {
    fn file_exists(&self, path: PathBuf) -> RpcResult<bool> {
        Ok(fs::metadata(path).is_ok())
    }

    fn read_file(&self, path: PathBuf) -> RpcResult<String> {
        if let Some(contents) = abs_path(&path).and_then(|x| self.0.get(&x)) {
            return Ok(contents.clone());
        }

        fs::read_to_string(path).map_err(|e| rpc_error(&e.to_string()))
    }
}

fn abs_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}
