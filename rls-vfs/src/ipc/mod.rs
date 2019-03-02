#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

#[cfg(target_os = "windows")]
mod windows; 
#[cfg(target_os = "windows")]
pub use self::windows::*;

mod inprocess;

use serde::{Serialize, Deserialize};
pub use self::inprocess::*;

use super::Vfs;
use std::sync::Arc;

trait VfsIpcChannel<U> {
    type ServerEndPoint: VfsIpcServerEndPoint<U>;
    type ClientEndPoint: VfsIpcClientEndPoint<U>;

    fn new_prefork() -> Self;
    fn into_server_end_point_postfork(self) -> Self::ServerEndPoint;
    fn into_client_end_point_postfork(self) -> Self::ClientEndPoint;
}

trait VfsIpcServer<U>: Sized {
    type Channel: VfsIpcChannel<U>;
    type ServerEndPoint: VfsIpcServerEndPoint<U>;
    type ClientEndPoint: VfsIpcClientEndPoint<U>;

    fn new(vfs: Arc<Vfs>) -> std::io::Result<Self>;

    fn roll_the_loop(&mut self) -> std::io::Result<()>;

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> std::io::Result<mio::Token>;

    fn remove_server_end_point(&mut self, tok: mio::Token) -> std::io::Result<()>;
}

trait VfsIpcClientEndPoint<U> {
    fn request_file(path: &std::path::Path) -> (String, U);
}

trait VfsIpcServerEndPoint<U> {
}

trait VfsIpcFileHandle<U> {
    fn get_file_ref(&self) -> &str;
    fn get_user_data_ref(&self) -> &U;
}

#[derive(Serialize, Deserialize)]
enum VfsRequestMsg {
    OpenFile(std::path::PathBuf),
    CloseFile(std::path::PathBuf),
}

#[derive(Serialize, Deserialize)]
struct VfsReplyMsg<U> {
    // NB: make sure path is null-terminated
    path: String,
    // Save the client from calling fstat
    length: usize,
    user_data: U,
}
