use std::io;
use std::path::{Path, PathBuf};

use crate::syntax::source_map::FileLoader;
use futures::Future;
use parity_tokio_ipc::IpcConnection;

pub struct IpcFileLoader;

impl IpcFileLoader {
    pub fn new(path: String) -> io::Result<Self> {
        let mut runtime = tokio::runtime::Runtime::new().expect("Error creating tokio runtime");

        let connection = IpcConnection::connect(path, &Default::default())?;

        let rx_buf2 = vec![0u8; 5];
        let fut = tokio::io::read_exact(connection, rx_buf2)
            .map(|(_, buf)| buf)
            .map_err(|err| panic!("Client 1 read error: {:?}", err));

        let test_buf = runtime.block_on(fut).unwrap();
        eprintln!("Read from IPC: `{}`", String::from_utf8(test_buf).unwrap());

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
