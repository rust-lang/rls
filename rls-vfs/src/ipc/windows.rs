use super::*;
use std::sync::Arc;
use std::marker::PhantomData;

struct WindowsVfsIpcChannel<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcChannel<U> for WindowsVfsIpcChannel<U> {
    type ServerEndPoint = WindowsVfsIpcServerEndPoint<U>;
    type ClientEndPoint = WindowsVfsIpcClientEndPoint<U>;

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

struct WindowsVfsIpcServer<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcServer<U> for WindowsVfsIpcServer<U> {
    type Channel = WindowsVfsIpcChannel<U>;
    type ServerEndPoint = WindowsVfsIpcServerEndPoint<U>;
    type ClientEndPoint = WindowsVfsIpcClientEndPoint<U>;
    type ServerEndPointToken = WindowsVfsIpcServerEndPointToken;

    fn new(vfs: Arc<Vfs>) -> Self {
        unimplemented!();
    }

    fn poll(&mut self) {
        unimplemented!();
    }

    fn add_server_end_point(&mut self, s_ep: Self::ServerEndPoint) -> Self::ServerEndPointToken {
        unimplemented!();
    }

    fn remove_server_end_point(&mut self, e_ept: Self::ServerEndPointToken) {
        unimplemented!();
    }
}

struct WindowsVfsIpcClientEndPoint<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcClientEndPoint<U> for WindowsVfsIpcClientEndPoint<U> {
    fn request_file(path: &std::path::Path) -> (String, U) {
        unimplemented!();
    }
}

struct WindowsVfsIpcServerEndPoint<U> {
    _u: PhantomData<U>
}

impl<U> VfsIpcServerEndPoint<U> for WindowsVfsIpcServerEndPoint<U> {
}

struct WindowsVfsIpcServerEndPointToken;

impl VfsIpcServerEndPointToken for WindowsVfsIpcServerEndPointToken {
}

