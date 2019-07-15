//! Inter-process communication (IPC) layer between RLS and rustc.

#![deny(missing_docs)]

#[cfg(feature = "client")]
pub mod client;
pub mod rpc;
#[cfg(feature = "server")]
pub mod server;
