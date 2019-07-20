// TODO: Remove me, this is only here for demonstration purposes how to set up
// a server.
#![cfg(feature = "ipc")]

use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

use jsonrpc_core::Result as RpcResult;
use jsonrpc_derive::rpc;
use jsonrpc_ipc_server::jsonrpc_core::*;
use jsonrpc_ipc_server::ServerBuilder;
use tokio::runtime::Runtime;

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

struct FileLoaderRpcImpl;
impl FileLoaderRpc for FileLoaderRpcImpl {
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
    fn read_file(&self, _path: PathBuf) -> RpcResult<String> {
        unimplemented!()
    }
}

fn main() {
    let endpoint_path = {
        let num: u64 = rand::Rng::gen(&mut rand::thread_rng());
        if cfg!(windows) {
            format!(r"\\.\pipe\ipc-pipe-{}", num)
        } else {
            format!(r"/tmp/ipc-uds-{}", num)
        }
    };

    let runtime = Runtime::new().unwrap();
    #[allow(deprecated)] // Windows won't work with lazily bound reactor
    let reactor = runtime.reactor();
    let executor = runtime.executor();

    let mut io = IoHandler::new();
    io.extend_with(FileLoaderRpcImpl.to_delegate());

    let builder =
        ServerBuilder::new(io).event_loop_executor(executor).event_loop_reactor(reactor.clone());
    let server = builder.start(&endpoint_path).expect("Couldn't open socket");
    eprintln!("ipc_tester: Started an IPC server");

    let mut child = Command::new("cargo")
        .args(&["run", "--bin", "rustc"])
        .env("RLS_IPC_ENDPOINT", endpoint_path)
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();

    let exit = child.wait().unwrap();
    dbg!(exit);

    server.wait();
}
