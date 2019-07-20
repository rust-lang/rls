// TODO: Remove me, this is only here for demonstration purposes how to set up
// a server.

use std::process::Command;

use futures::{stream::Stream, sync::oneshot, Future};
use parity_tokio_ipc::{dummy_endpoint, Endpoint, SecurityAttributes};
use tokio;
use tokio::io::{self, AsyncRead};

use jsonrpc_ipc_server::jsonrpc_core::*;
use jsonrpc_ipc_server::ServerBuilder;

fn main() {
    env_logger::init();

    let endpoint_path = dummy_endpoint();

    // let mut runtime = tokio::runtime::Runtime::new().unwrap();
    let reactor = tokio::reactor::Reactor::new().expect("Can't construct an I/O reactor");
    // let handle = runtime.reactor();

    let mut io = IoHandler::new();
    io.add_method("say_hello", |_params| {
        eprintln!("ipc_tester: At long fucking last");
        Ok(serde_json::Value::String("No eloszka".into()))
    });
    let builder = ServerBuilder::new(io).event_loop_reactor(reactor.handle());
    let server = builder.start(&endpoint_path).expect("Couldn't open socket");

    // let endpoint = Endpoint::new(endpoint_path.clone());
    // let server = endpoint
    //     .incoming(handle)
    //     .unwrap()
    //     .for_each(|(connection, _)| {
    //         eprintln!("ipc_tester: Client connected!");
    //         let (reader, writer) = connection.split();
    //         // TODO: Try reading using a StreamCodec instead of buffers
    //         let buf = [0u8; 5];
    //         io::read_exact(reader, buf)
    //             .map(|(_, buf)| {
    //                 let buf = String::from_utf8_lossy(&buf);
    //                 eprintln!("Read some: `{:?}`", buf)
    //             })
    //             .map_err(|e| {
    //                 eprintln!("io error: {:?}", e);
    //                 e
    //             })
    //     })
    //     .map_err(|_| ());
    // runtime.spawn(server);
    eprintln!("ipc_tester: Started an IPC server");

    std::thread::sleep_ms(1000);

    let mut child = Command::new("cargo")
        .args(&["run", "--bin", "rustc"])
        // .env_remove("RUST_LOG")
        .env("RLS_IPC_ENDPOINT", endpoint_path)
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();

    std::thread::sleep_ms(1000);
    // FIXME: It seems that the closing polls the inner future actually executing it...
    // Couldn't do it otherwise.
    // server.close();

    let exit = child.wait().unwrap();
    dbg!(exit);
}
