// TODO: Remove me, this is only here for demonstration purposes how to set up
// a server.

use std::process::Command;

use futures::{stream::Stream, Future};
use parity_tokio_ipc::{dummy_endpoint, Endpoint};
use tokio;
use tokio::io::{self, AsyncRead};

fn main() {
    let endpoint = dummy_endpoint();

    std::thread::spawn({
        let endpoint = endpoint.clone();
        move || {
            let mut runtime = tokio::runtime::Runtime::new().expect("Can't create Runtime");

            let endpoint = Endpoint::new(endpoint);
            let connections = endpoint
                .incoming(&Default::default())
                .expect("failed to open up a new pipe/socket");
            let server = connections
                .for_each(|(stream, _)| {
                    eprintln!("Connected!");
                    let (_, writer) = stream.split();
                    io::write_all(writer, b"Hello!").map(|_| ())
                })
                .map_err(|_| ());
            runtime.block_on(server).unwrap();
        }
    });

    let output = Command::new("cargo")
        .args(&["run", "--bin", "rustc"])
        .env("RLS_IPC_ENDPOINT", endpoint)
        .output()
        .unwrap();
    dbg!(&output);
}
