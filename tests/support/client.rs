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

        let reader = FramedRead::new(std::io::BufReader::new(stdout), LspDecoder::default());
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
    reader: FramedRead<std::io::BufReader<ChildStdout>, LspDecoder>,
    /// ASynchronous LSP writer for the spawned process
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
    fn receive_messages(&mut self, msgs: Vec<Value>) {
        for msg in &msgs {
            eprintln!("Received: {:?}", msg);
        }

        self.messages.extend(msgs);
    }

    /// Synchronously sends a message to the RLS.
    pub fn send(&mut self, msg: Value) {
        let writer = self.writer.take().unwrap();

        let fut = writer.send(msg);

        self.writer = Some(self.runtime.block_on(fut).unwrap());
    }

    /// Consumes messages in blocking manner until `f` predicate returns true
    /// for a received message from the stream.
    pub fn take_messages_until(&mut self, f: impl Fn(&Value) -> bool) -> &[Value] {
        let stream = self.reader.by_ref();
        let old_msg_len = self.messages.len();

        let msgs = stream.take_while(|msg| Ok(!f(msg))).collect();
        let msgs = self.runtime.block_on(msgs).unwrap();

        self.receive_messages(msgs);

        &self.messages[old_msg_len..]
    }

    /// Blocks until the processing (building + indexing) is done by the RLS.
    pub fn wait_for_indexing(&mut self) {
        self.take_messages_until(|msg| {
            msg["params"]["title"] == "Indexing" && msg["params"]["done"] == true
        });
    }

    /// Blocks until RLS responds with a message with a given `id`.
    pub fn wait_for_id(&mut self, id: u64) {
        self.take_messages_until(|msg| msg["id"] == id);
    }

    /// Requests the RLS to shut down and waits (with a timeout) until the child
    /// process is terminated.
    pub fn shutdown(mut self) {
        self.send(json!({"jsonrpc": "2.0", "id": 99999, "method": "shutdown"}));
        self.send(json!({"jsonrpc": "2.0", "method": "exit"}));

        let rt = &mut self.runtime;

        let fut = self.child.wait_with_output();
        let fut = Timeout::new(fut, Duration::from_secs(15));

        rt.block_on(fut).unwrap();
    }
}
