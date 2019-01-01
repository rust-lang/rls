//! Tokio-based LSP client. The tokio `current_thread::Runtime` allows for a
//! cheap, single-threaded blocking until a certain message is received from the
//! server. It also allows enforcing timeouts, which are necessary for testing.

use std::process::{Command, Stdio};
use std::time::Duration;

use futures::sink::Sink;
use futures::stream::Stream;
use lsp_codec::{LspDecoder, LspEncoder};
use serde_json::{json, Value};
use tokio::codec::{FramedRead, FramedWrite};
use tokio::runtime::current_thread::Runtime;
use tokio_process::{Child, ChildStdin, ChildStdout, CommandExt};
use tokio_timer::Timeout;

use serde::Deserialize;

use super::project_builder::Project;
use super::rls_exe;

impl Project {
    pub fn spawn_rls_async(&self) -> RlsHandle {
        let mut cmd = Command::new(rls_exe());
        cmd.current_dir(self.root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = cmd.spawn_async().expect("Couldn't spawn RLS");
        let stdin = child.stdin().take().unwrap();
        let stdout = child.stdout().take().unwrap();

        let reader = Some(FramedRead::new(
            std::io::BufReader::new(stdout),
            LspDecoder::default(),
        ));
        let writer = Some(FramedWrite::new(stdin, LspEncoder));

        RlsHandle {
            reader,
            writer,
            child,
            runtime: Runtime::new().unwrap(),
            messages: Vec::new(),
        }
    }
}

/// Holds the handle to a spawned RLS child process and allows to send and
/// receive messages to and from the process.
pub struct RlsHandle {
    /// Asynchronous LSP reader for the spawned process
    reader: Option<FramedRead<std::io::BufReader<ChildStdout>, LspDecoder>>,
    /// Asynchronous LSP writer for the spawned process
    writer: Option<FramedWrite<ChildStdin, LspEncoder>>,
    /// Handle to the spawned child
    child: Child,
    /// Tokio single-thread runtime required for interaction with async-based
    /// `reader` and `writer`
    runtime: Runtime,
    /// LSP Messages received from the stream and processed
    messages: Vec<Value>,
}

impl RlsHandle {
    /// Returns messages received until the moment of the call.
    pub fn messages(&self) -> &[Value] {
        &self.messages
    }

    // TODO: Notify on every message received?
    fn receive_message(&mut self, msg: Value) {
        eprintln!("Received: {:?}", msg);

        self.messages.push(msg);
    }

    // TODO:
    pub fn request<R>(&mut self, id: u64, params: R::Params) -> R::Result
    where
        R: rls::lsp_data::LSPRequest,
        R::Params: serde::Serialize,
        R::Result: serde::de::DeserializeOwned,
    {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": R::METHOD,
            "params": params,
        }));

        let msg = self.wait_for_message(|val| val["id"] == id && val.get("result").is_some());
        let msg = &msg["result"];

        R::Result::deserialize(msg).unwrap_or_else(|_| panic!("Can't deserialize results: {:?}", msg))
    }

    pub fn notify<R>(&mut self, params: R::Params)
    where
        R: rls::lsp_data::LSPNotification,
        R::Params: serde::Serialize,
    {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": R::METHOD,
            "params": params,
        }));
    }

    /// Synchronously sends a message to the RLS.
    pub fn send(&mut self, msg: Value) {
        let writer = self.writer.take().unwrap();

        eprintln!("Sending: {:?}", msg);
        let fut = writer.send(msg);

        self.writer = Some(self.runtime.block_on(fut).unwrap());
        eprintln!("Finished Sending");
    }

    /// Consumes messages in blocking manner until `f` predicate returns true
    /// for a received message from the stream, additionally including the first
    /// message for which the predicate returned false.
    pub fn take_messages_until_inclusive(&mut self, f: impl Fn(&Value) -> bool) -> &[Value] {
        // let stream = self.reader.by_ref();
        let old_msg_len = self.messages.len();

        // Fugly workaround to synchronously take items from stream *including*
        // the one for which `f` returns false.
        // Straightforward implementation of using `by_ref` and then doing
        // `take_while(|x| Ok(!f(x))).collect()` doesn't work since it seems
        // that the last element for which `f` is false is consumed from the
        // inner stream and there's no way to retrieve it afterwards.
        loop {
            let reader = self.reader.take().unwrap();

            match self.runtime.block_on(reader.into_future()) {
                Ok((item, stream)) => {
                    if let Some(item) = item {
                        self.receive_message(item);
                    }

                    self.reader = Some(stream);
                },
                Err(..) => panic!("Can't read LSP message from stream"),
            }

            let last = self.messages.last().unwrap();
            // *Do* include the last message for which `f` was false.
            if f(last) {
                break;
            }
        }

        &self.messages[old_msg_len..]
    }

    pub fn wait_for_message(&mut self, f: impl Fn(&Value) -> bool) -> &Value {
        self.take_messages_until_inclusive(f);

        self.messages.last().unwrap()
    }

    /// Blocks until the processing (building + indexing) is done by the RLS.
    pub fn wait_for_indexing(&mut self) {
        self.wait_for_message(|msg| {
            msg["params"]["title"] == "Indexing" && msg["params"]["done"] == true
        });
    }

    /// Blocks until RLS responds with a message with a given `id`.
    pub fn wait_for_id(&mut self, id: u64) {
        self.wait_for_message(|msg| msg["id"] == id);
    }

    /// Requests the RLS to shut down and waits (with a timeout) until the child
    /// process is terminated.
    pub fn shutdown(mut self) {
        self.request::<languageserver_types::request::Shutdown>(99999, ());
        self.notify::<languageserver_types::notification::Exit>(());

        let rt = &mut self.runtime;

        let fut = self.child.wait_with_output();
        let fut = Timeout::new(fut, Duration::from_secs(15));

        rt.block_on(fut).unwrap();
    }
}
