use super::*;
use std::sync::Arc;
use std::marker::PhantomData;
use mio::Token;
use serde::{Serialize, Deserialize};

struct InProcessVfsIpcChannel {
}

impl VfsIpcChannel for InProcessVfsIpcChannel {
    type ServerEndPoint = InProcessVfsIpcServerEndPoint;
    type ClientEndPoint = InProcessVfsIpcClientEndPoint;

    fn new_prefork() -> Self {
        unimplemented!();
    }
    fn into_server_end_point_postfork(self) -> Self::ServerEndPoint {
        unimplemented!();
    }
    fn into_client_end_point_postfork(self) -> Self::ClientEndPoint {
        unimplemented!();
    }
}

struct InProcessVfsIpcServer<U: Serialize> {
    _u: PhantomData<U>
}

impl<U: Serialize> VfsIpcServer<U> for InProcessVfsIpcServer<U> {
    type Channel = InProcessVfsIpcChannel;
    type ServerEndPoint = InProcessVfsIpcServerEndPoint;
    type ClientEndPoint = InProcessVfsIpcClientEndPoint;

    fn new(vfs: Arc<Vfs<U>>) -> std::io::Result<Self> {
        unimplemented!();
    }

    fn roll_the_loop(&mut self) -> std::io::Result<()> {
        unimplemented!();
    }

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> std::io::Result<Token> {
        unimplemented!();
    }

    fn remove_server_end_point(&mut self, e_ept: Token) -> std::io::Result<()> {
        unimplemented!();
    }
}

struct InProcessVfsIpcClientEndPoint {
}

impl VfsIpcClientEndPoint for InProcessVfsIpcClientEndPoint {
    fn request_file<U: Serialize>(path: &std::path::Path) -> (String, U) {
        unimplemented!();
    }
}

struct InProcessVfsIpcServerEndPoint {
}

impl VfsIpcServerEndPoint for InProcessVfsIpcServerEndPoint {
}

