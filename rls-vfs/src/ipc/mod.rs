#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

/*
#[cfg(target_os = "windows")]
pub mod windows; 
#[cfg(target_os = "windows")]
pub use self::windows::*;
*/

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
    type ReadBuffer;
    type WriteBuffer;
    // predicate: this can only be called with a blocking underlying fd
    fn blocking_request_file<U: Serialize + DeserializeOwned + Clone>(&mut self, path: &std::path::Path, rbuf: &mut Self::ReadBuffer, wbuf: &mut Self::WriteBuffer) -> Result<(Self::FileHandle, Option<U>), Self::Error> {
        let req = VfsRequestMsg::OpenFile(path.to_owned());
        self.blocking_write_request(&req, wbuf)?;
        let rep = self.blocking_read_reply::<U>(rbuf)?;
        let handle = self.reply_to_file_handle(&rep)?;
        Ok((handle, rep.user_data))
    }
    // flush the wbuf and write a request
    fn blocking_write_request(&mut self, req: &VfsRequestMsg, wbuf: &mut Self::WriteBuffer) -> Result<(), Self::Error>;
    // read a reply message from the rbuf and remote
    fn blocking_read_reply<U: Serialize + DeserializeOwned + Clone>(&mut self, rbuf: &mut Self::ReadBuffer) -> Result<VfsReplyMsg<U>, Self::Error>;
    fn reply_to_file_handle<U: Serialize + DeserializeOwned + Clone>(&mut self, rep: &VfsReplyMsg<U>) -> Result<Self::FileHandle, Self::Error>;
}

trait VfsIpcServerEndPoint {
    type Error: std::error::Error;
    type ReadBuffer;
    type WriteBuffer;
    fn blocking_read_request(&mut self, rbuf: &mut Self::ReadBuffer) -> Result<VfsRequestMsg, Self::Error>;
    fn blocking_write_reply<U: Serialize + DeserializeOwned + Clone>(&mut self, rep: &VfsReplyMsg<U>, wbuf: &mut Self::WriteBuffer) -> Result<(), Self::Error>;
}

trait VfsIpcFileHandle {
    type Error: std::error::Error;
    fn get_file_ref(&self) -> Result<&str, Self::Error>;
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Debug)]
pub enum VfsRequestMsg {
    OpenFile(std::path::PathBuf),
    CloseFile(std::path::PathBuf),
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct VfsReplyMsg<U> {
    // NB: make sure path is null-terminated
    path: String,
    // Save the client from calling fstat
    length: u32,
    user_data: Option<U>,
}
