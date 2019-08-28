//! Allows to connect to an IPC server.

use crate::rpc::callbacks::gen_client::Client as CallbacksClient;
use crate::rpc::file_loader::gen_client::Client as FileLoaderClient;

pub use jsonrpc_core_client::transports::ipc::connect;
pub use jsonrpc_core_client::{RpcChannel, RpcError};

/// Joint IPC client.
#[derive(Clone)]
pub struct Client {
    /// File loader interface
    pub file_loader: FileLoaderClient,
    /// Callbacks interface
    pub callbacks: CallbacksClient,
}

impl From<RpcChannel> for Client {
    fn from(channel: RpcChannel) -> Self {
        Client {
            file_loader: FileLoaderClient::from(channel.clone()),
            callbacks: CallbacksClient::from(channel),
        }
    }
}
