use std::io;
use std::path::{Path, PathBuf};

use failure::Fail;
use futures::sink::Sink;
use futures::stream::Stream;
use futures::Future;
use jsonrpc_core_client::transports::duplex;
use jsonrpc_core_client::{RpcChannel, RpcError, TypedClient};
use jsonrpc_server_utils::codecs::StreamCodec;
use parity_tokio_ipc::IpcConnection;
use tokio::codec::Decoder;

/// Connect to a JSON-RPC IPC server.
pub fn connect<Client: From<RpcChannel>>(
    path: PathBuf,
    reactor: tokio::reactor::Handle,
) -> impl Future<Item = Client, Error = io::Error> {
    log::trace!("ipc: Attempting to connect to {}", path.display());
    let connection = IpcConnection::connect(path, &reactor).unwrap();

    futures::lazy(move || {
        let (sink, stream) = StreamCodec::stream_incoming().framed(connection).split();
        let sink = sink.sink_map_err(|e| RpcError::Other(e.into()));
        let stream = stream.map_err(|e| RpcError::Other(e.into()));

        let (client, sender) = duplex(sink, stream);

        tokio::spawn(client.map_err(|e| log::warn!("IPC client error: {:?}", e)));
        Ok(sender.into())
    })
}

#[derive(Clone)]
pub struct FileLoader(TypedClient);

impl From<RpcChannel> for FileLoader {
    fn from(channel: RpcChannel) -> Self {
        FileLoader(channel.into())
    }
}

impl FileLoader {
    pub fn spawn(path: PathBuf, runtime: &mut tokio::runtime::Runtime) -> io::Result<Self> {
        #[allow(deprecated)] // Windows doesn't work with lazily-bound reactors
        let reactor = runtime.reactor().clone();

        Ok(runtime.block_on(connect(path, reactor))?)
    }

    pub fn into_boxed(self) -> Option<Box<dyn syntax::source_map::FileLoader + Send + Sync>> {
        Some(Box::new(self))
    }
}

impl FileLoader {
    pub fn file_exists(&self, path: PathBuf) -> impl Future<Item = bool, Error = RpcError> {
        self.0.call_method("file_exists", "bool", (path,))
    }

    pub fn abs_path(&self, path: PathBuf) -> impl Future<Item = Option<PathBuf>, Error = RpcError> {
        self.0.call_method("abs_path", "Option<PathBuf>", (path,))
    }

    pub fn read_file(&self, path: PathBuf) -> impl Future<Item = String, Error = RpcError> {
        self.0.call_method("read_file", "String", (path,))
    }
}

impl syntax::source_map::FileLoader for FileLoader {
    fn file_exists(&self, path: &Path) -> bool {
        self.file_exists(path.to_owned()).wait().unwrap()
    }

    fn abs_path(&self, path: &Path) -> Option<PathBuf> {
        self.abs_path(path.to_owned()).wait().ok()?
    }

    fn read_file(&self, path: &Path) -> io::Result<String> {
        self.read_file(path.to_owned())
            .wait()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.compat()))
    }
}
