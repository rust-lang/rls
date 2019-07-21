use std::ops::DerefMut;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::{env, fs};

use jsonrpc_core::{Error, ErrorCode, IoHandler, Result as RpcResult};
use jsonrpc_derive::rpc;
use jsonrpc_ipc_server::ServerBuilder;
use rls_vfs::{FileContents, Vfs};

lazy_static::lazy_static! {
    static ref IPC_SERVER: Arc<Mutex<Option<jsonrpc_ipc_server::Server>>> = Arc::default();
}

/// Spins up an IPC server in the background. Currently used for inter-process
/// VFS, which is required for out-of-process rustc compilation.
pub fn start(vfs: Arc<Vfs>) -> Result<PathBuf, ()> {
    let server = IPC_SERVER.lock().map_err(|_| ())?;
    if server.is_some() {
        log::trace!("Can't start IPC server twice");
        return Err(());
    }

    let endpoint_path = gen_endpoint_path();
    std::thread::spawn({
        let endpoint_path = endpoint_path.clone();
        move || {
            log::trace!("Attempting to spin up IPC server at {}", endpoint_path);
            let runtime = tokio::runtime::Builder::new()
                .core_threads(1)
                .build()
                .unwrap();
            #[allow(deprecated)] // Windows won't work with lazily bound reactor
            let (reactor, executor) = (runtime.reactor(), runtime.executor());

            let mut io = IoHandler::new();
            io.extend_with(vfs.to_delegate());

            let server = ServerBuilder::new(io)
                .event_loop_executor(executor)
                .event_loop_reactor(reactor.clone())
                .start(&endpoint_path)
                .map_err(|_| log::warn!("Couldn't open socket"))
                .unwrap();
            log::trace!("Started the IPC server at {}", endpoint_path);

            server.wait();
        }
    });

    Ok(endpoint_path.into())
}

#[allow(clippy::unit_arg)]
#[allow(dead_code)]
pub fn shutdown() -> Result<(), ()> {
    let mut server = IPC_SERVER.lock().map_err(|_| ())?;
    match server.deref_mut().take() {
        Some(server) => Ok(server.close()),
        None => Err(()),
    }
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

#[rpc]
pub trait FileLoaderRpc {
    /// Query the existence of a file.
    #[rpc(name = "file_exists")]
    fn file_exists(&self, path: PathBuf) -> RpcResult<bool>;

    /// Returns an absolute path to a file, if possible.
    #[rpc(name = "abs_path")]
    fn abs_path(&self, path: PathBuf) -> RpcResult<Option<PathBuf>>;

    /// Read the contents of an UTF-8 file into memory.
    #[rpc(name = "read_file")]
    fn read_file(&self, path: PathBuf) -> RpcResult<String>;
}

impl FileLoaderRpc for Arc<Vfs> {
    fn file_exists(&self, path: PathBuf) -> RpcResult<bool> {
        // Copied from syntax::source_map::RealFileLoader
        Ok(fs::metadata(path).is_ok())
    }
    fn abs_path(&self, path: PathBuf) -> RpcResult<Option<PathBuf>> {
        // Copied from syntax::source_map::RealFileLoader
        Ok(if path.is_absolute() {
            Some(path.to_path_buf())
        } else {
            env::current_dir().ok().map(|cwd| cwd.join(path))
        })
    }
    fn read_file(&self, path: PathBuf) -> RpcResult<String> {
        self.load_file(&path).map_err(|e| rpc_error(&e.to_string())).and_then(|contents| {
            match contents {
                FileContents::Text(text) => Ok(text),
                FileContents::Binary(..) => Err(rpc_error("File is binary")),
            }
        })
    }
}
