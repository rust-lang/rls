use jsonrpc_core::types::params::Params;
use jsonrpc_core_client::transports::duplex;
use jsonrpc_core_client::RpcError;
use std::io;
use std::path::{Path, PathBuf};

use crate::syntax::source_map::FileLoader;
use futures::Future;
use parity_tokio_ipc::IpcConnection;

use futures::sink::Sink;
use futures::stream::Stream;
use tokio::codec::Decoder;

pub struct IpcFileLoader;

impl IpcFileLoader {
    pub fn new(path: String) -> io::Result<Self> {
        let mut runtime = tokio::runtime::Runtime::new().unwrap();
        let handle = runtime.reactor();

        eprintln!("ipc: Attempting to connect to {}", path);
        let connection = IpcConnection::connect(path, handle)?;
        let codec = jsonrpc_server_utils::codecs::StreamCodec::stream_incoming();

        let (sink, stream) = codec.framed(connection).split();
        let sink = sink.sink_map_err(|e| RpcError::Other(e.into()));
        let stream = stream.map_err(|e| RpcError::Other(e.into()));
        eprintln!("ipc: Client connected");

        eprintln!("ipc: Setting up duplex");
        let (client, sender) = duplex(sink, stream);

        let raw = jsonrpc_core_client::RawClient::from(sender);
        eprintln!("ipc: Call say_hello");
        let result = raw
            .call_method("say_hello", Params::None)
            .map(|val| {
                eprintln!("ipc: Result of say_hello method: {:?}", val);
            })
            .map_err(|e| eprintln!("ipc: Called method say_hello failed with: {:?}", e));
        let client = client.map_err(|e| {
            eprintln!("Err: {:?}", e);
            panic!();
        });
        runtime.spawn(client);
        dbg!(&runtime.block_on(result));

        Ok(IpcFileLoader)
    }
}

impl FileLoader for IpcFileLoader {
    fn file_exists(&self, _path: &Path) -> bool {
        unimplemented!()
    }

    fn abs_path(&self, _path: &Path) -> Option<PathBuf> {
        unimplemented!()
    }

    fn read_file(&self, _path: &Path) -> io::Result<String> {
        unimplemented!()
    }
}
