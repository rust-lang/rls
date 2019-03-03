#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

#[cfg(target_os = "windows")]
mod windows; 
#[cfg(target_os = "windows")]
pub use self::windows::*;

//mod inprocess;

use std::result::Result;
use serde::{Serialize, Deserialize, de::DeserializeOwned};
use std::sync::Arc;
use std::clone::Clone;
//pub use self::inprocess::*;

use super::Vfs;

trait VfsIpcChannel: Sized {
    type ServerEndPoint: VfsIpcServerEndPoint;
    type ClientEndPoint: VfsIpcClientEndPoint;
    type Error: std::error::Error;

    fn new_prefork() -> Result<Self, Self::Error>;
    fn into_server_end_point_postfork(self) -> Result<Self::ServerEndPoint, Self::Error>;
    fn into_client_end_point_postfork(self) -> Result<Self::ClientEndPoint, Self::Error>;
}

trait VfsIpcServer<U: Serialize + Clone> : Sized {
    type Channel: VfsIpcChannel;
    type ServerEndPoint: VfsIpcServerEndPoint;
    type ClientEndPoint: VfsIpcClientEndPoint;
    type Error: std::error::Error;

    fn new(vfs: Arc<Vfs<U>>) -> Result<Self, Self::Error>;

    fn roll_the_loop(&mut self) -> Result<(), Self::Error>;

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> Result<mio::Token, Self::Error>;

    fn remove_server_end_point(&mut self, tok: mio::Token) -> Result<(), Self::Error>;
}

trait VfsIpcClientEndPoint {
    type Error: std::error::Error;
    type FileHandle: VfsIpcFileHandle;
    fn request_file<U: Serialize + DeserializeOwned + Clone>(&mut self, path: &std::path::Path) -> Result<(Self::FileHandle, U), Self::Error>;
}

trait VfsIpcServerEndPoint {
}

trait VfsIpcFileHandle {
    type Error: std::error::Error;
    fn get_file_ref(&self) -> Result<&str, Self::Error>;
}

#[derive(Serialize, Deserialize)]
pub enum VfsRequestMsg {
    OpenFile(std::path::PathBuf),
    CloseFile(std::path::PathBuf),
}

#[derive(Serialize, Deserialize)]
pub struct VfsReplyMsg<U> {
    // NB: make sure path is null-terminated
    path: String,
    // Save the client from calling fstat
    length: u32,
    user_data: U,
}
