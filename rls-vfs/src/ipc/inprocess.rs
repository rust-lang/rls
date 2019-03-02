use super::*;
use std::sync::Arc;
use std::marker::PhantomData;
use mio::Token;

struct InProcessVfsIpcChannel<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcChannel<U> for InProcessVfsIpcChannel<U> {
    type ServerEndPoint = InProcessVfsIpcServerEndPoint<U>;
    type ClientEndPoint = InProcessVfsIpcClientEndPoint<U>;

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

struct InProcessVfsIpcServer<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcServer<U> for InProcessVfsIpcServer<U> {
    type Channel = InProcessVfsIpcChannel<U>;
    type ServerEndPoint = InProcessVfsIpcServerEndPoint<U>;
    type ClientEndPoint = InProcessVfsIpcClientEndPoint<U>;

    fn new(vfs: Arc<Vfs>) -> std::io::Result<Self> {
        unimplemented!();
    }

    fn poll(&mut self) {
        unimplemented!();
    }

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> Token {
        unimplemented!();
    }

    fn remove_server_end_point(&mut self, e_ept: Token) {
        unimplemented!();
    }
}

struct InProcessVfsIpcClientEndPoint<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcClientEndPoint<U> for InProcessVfsIpcClientEndPoint<U> {
    fn request_file(path: &std::path::Path) -> (String, U) {
        unimplemented!();
    }
}

struct InProcessVfsIpcServerEndPoint<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcServerEndPoint<U> for InProcessVfsIpcServerEndPoint<U> {
}

