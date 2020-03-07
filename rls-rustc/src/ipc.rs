use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use failure::Fail;
use futures::Future;

use rls_ipc::client::{Client as JointClient, RpcChannel, RpcError};
use rls_ipc::rpc::callbacks::Client as CallbacksClient;
use rls_ipc::rpc::file_loader::Client as FileLoaderClient;

pub use rls_ipc::client::connect;

#[derive(Clone)]
pub struct Client(JointClient);

impl From<RpcChannel> for Client {
    fn from(channel: RpcChannel) -> Self {
        Client(channel.into())
    }
}

#[derive(Clone)]
pub struct IpcFileLoader(FileLoaderClient);

impl IpcFileLoader {
    pub fn into_boxed(self) -> Option<Box<dyn rustc_span::source_map::FileLoader + Send + Sync>> {
        Some(Box::new(self))
    }
}

impl rustc_span::source_map::FileLoader for IpcFileLoader {
    fn file_exists(&self, path: &Path) -> bool {
        self.0.file_exists(path.to_owned()).wait().unwrap()
    }

    fn abs_path(&self, path: &Path) -> Option<PathBuf> {
        self.0.abs_path(path.to_owned()).wait().ok()?
    }

    fn read_file(&self, path: &Path) -> io::Result<String> {
        self.0
            .read_file(path.to_owned())
            .wait()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.compat()))
    }
}

#[derive(Clone)]
pub struct IpcCallbacks(CallbacksClient);

impl IpcCallbacks {
    pub fn complete_analysis(
        &self,
        analysis: rls_data::Analysis,
    ) -> impl Future<Item = (), Error = RpcError> {
        self.0.complete_analysis(analysis)
    }

    pub fn input_files(
        &self,
        input_files: HashMap<PathBuf, HashSet<rls_ipc::rpc::Crate>>,
    ) -> impl Future<Item = (), Error = RpcError> {
        self.0.input_files(input_files)
    }
}

impl Client {
    pub fn split(self) -> (IpcFileLoader, IpcCallbacks) {
        let JointClient { file_loader, callbacks } = self.0;
        (IpcFileLoader(file_loader), IpcCallbacks(callbacks))
    }
}
