//! Tokio-based LSP client. The tokio `current_thread::Runtime` allows for a
//! cheap, single-threaded blocking until a certain message is received from the
//! server. It also allows enforcing timeouts, which are necessary for testing.
//!
//! More concretely, we couple spawned RLS handle with the Tokio runtime on
//! current thread. A message reader `Stream<Item = Value, ...>` future is
//! spawned on the runtime, which allows us to queue channel senders which can
//! be notified when resolving the reader future. On each message reception we
//! check if a channel sender was registered with an associated predicate being
//! true for the message received, and if so we send the message, notifying the
//! receiver (thus, implementing the Future<Item = Value> model).

use std::cell::{Ref, RefCell};
use std::process::{Command, Stdio};
use std::rc::Rc;

use futures::sink::Sink;
use futures::stream::Stream;
use futures::unsync::oneshot;
use futures::Future;
use lsp_codec::{LspDecoder, LspEncoder};
use lsp_types::PublishDiagnosticsParams;
use lsp_types::notification::{Notification, PublishDiagnostics};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::codec::{FramedRead, FramedWrite};
use tokio::runtime::current_thread::Runtime;
use tokio::util::FutureExt;
use tokio_process::{Child, ChildStdin, CommandExt};

use super::project_builder::Project;
use super::{rls_exe, rls_timeout};

// `Rc` because we share those in message reader stream and the RlsHandle.
// `RefCell` because borrows don't overlap. This is safe, because `process_msg`
// is only called (synchronously) when we execute some work on the runtime,
// however we only call `Runtime::block_on` and whenever we do it, there are no
// active borrows in scope.
type Messages = Rc<RefCell<Vec<Value>>>;
type Channels = Rc<RefCell<Vec<(Box<Fn(&Value) -> bool>, oneshot::Sender<Value>)>>>;

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

        let msgs = Messages::default();
        let chans = Channels::default();

        let reader = FramedRead::new(std::io::BufReader::new(stdout), LspDecoder::default())
            .map_err(|_| ())
            .for_each({
                let msgs = Rc::clone(&msgs);
                let chans = Rc::clone(&chans);
                move |msg| process_msg(msg, msgs.clone(), chans.clone())
            })
            .timeout(rls_timeout());

        let writer = Some(FramedWrite::new(stdin, LspEncoder));

        let mut rt = Runtime::new().unwrap();
        rt.spawn(reader.map_err(|_| ()));

        RlsHandle {
            writer,
            child,
            runtime: rt,
            messages: msgs,
            channels: chans,
        }
    }
}

fn process_msg(msg: Value, msgs: Messages, chans: Channels) -> Result<(), ()> {
    eprintln!("Processing message: {:?}", msg);

    let mut chans = chans.borrow_mut();

    if chans.len() > 0 {
        let mut idx = (chans.len() - 1) as isize;

        // Poor man's drain_filter. Iterates over entire collection starting
        // from the end, takes ownership over the element and the predicate is
        // true, then we consume the value; otherwise, we push it to the back,
        // effectively undoing swap_remove (post-swap). This is correct, because
        // on every iteration we decrease idx by 1, so we won't loop and we will
        // check every element.
        while idx >= 0 {
            let (pred, tx) = chans.swap_remove(idx as usize);
            if pred(&msg) {
                tx.send(msg.clone()).map_err(|_| ())?;
            } else {
                chans.push((pred, tx));
            }

            idx -= 1;
        }

        debug_assert!(chans.iter().all(|(pred, _)| !pred(&msg)));
    }

    msgs.borrow_mut().push(msg);

    Ok(())
}

/// Holds the handle to a spawned RLS child process and allows to send and
/// receive messages to and from the process.
pub struct RlsHandle {
    /// Asynchronous LSP writer for the spawned process.
    writer: Option<FramedWrite<ChildStdin, LspEncoder>>,
    /// Handle to the spawned child.
    child: Child,
    /// Tokio single-thread runtime onto which LSP message reading stream has
    /// been spawned. Allows to synchronously write messages via `writer` and
    /// block on received messages matching an enqueued predicate in `channels`.
    runtime: Runtime,
    /// Handle to all of the received LSP messages.
    messages: Messages,
    /// Handle to enqueued channel senders, used to notify when a given message
    /// has been received.
    channels: Channels,
}

impl RlsHandle {
    /// Returns messages received until the moment of the call.
    pub fn messages(&self) -> Ref<Vec<Value>> {
        self.messages.borrow()
    }

    /// Block on returned, associated future with a timeout.
    pub fn block_on<F: Future>(&mut self, f: F) -> Result<F::Item, tokio_timer::timeout::Error<F::Error>> {
        let future_with_timeout = f.timeout(rls_timeout());

        self.runtime.block_on(future_with_timeout)
    }

    /// Send a request to the RLS and block until we receive the message.
    /// Note that between sending and receiving the response *another* messages
    /// can be received.
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

        let msg = self.wait_for_message(move |val| val["id"] == id && val.get("result").is_some());

        R::Result::deserialize(&msg["result"])
            .unwrap_or_else(|_| panic!("Can't deserialize results: {:?}", msg))
    }

    /// Synchronously sends a notification to the RLS.
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
        eprintln!("Sending: {:?}", msg);

        let writer = self.writer.take().unwrap();

        let fut = writer.send(msg);

        self.writer = Some(self.block_on(fut).unwrap());
    }

    /// Enqueues a channel that is notified and consumed when a given predicate
    /// `f` is true for a received message.
    fn future_msg(
        &mut self,
        f: impl Fn(&Value) -> bool + 'static,
    ) -> impl Future<Item = Value, Error = oneshot::Canceled> {
        let (tx, rx) = oneshot::channel();

        self.channels.borrow_mut().push((Box::new(f), tx));

        rx
    }

    // Returns a future diagnostic message for a given file path suffix.
    #[rustfmt::skip]
    pub fn future_diagnostics(
        &mut self,
        path: impl AsRef<str> + 'static,
    ) -> impl Future<Item = PublishDiagnosticsParams, Error = oneshot::Canceled> {
        self.future_msg(move |msg|
            msg["method"] == PublishDiagnostics::METHOD &&
            msg["params"]["uri"].as_str().unwrap().ends_with(path.as_ref())
        )
        .and_then(|msg| Ok(PublishDiagnosticsParams::deserialize(&msg["params"]).unwrap()))
    }

    /// Blocks until a message, for which predicate `f` returns true, is received.
    pub fn wait_for_message(&mut self, f: impl Fn(&Value) -> bool + 'static) -> Value {
        let fut = self.future_msg(f);

        self.block_on(fut).unwrap()
    }

    /// Blocks until the processing (building + indexing) is done by the RLS.
    pub fn wait_for_indexing(&mut self) {
        self.wait_for_message(|msg| {
            msg["params"]["title"] == "Indexing" && msg["params"]["done"] == true
        });
    }

    /// Blocks until a "textDocument/publishDiagnostics" message is received.
    pub fn wait_for_diagnostics(&mut self) -> lsp_types::PublishDiagnosticsParams {
        let msg = self.wait_for_message(|msg| msg["method"] == PublishDiagnostics::METHOD);

        lsp_types::PublishDiagnosticsParams::deserialize(&msg["params"])
            .unwrap_or_else(|_| panic!("Can't deserialize params: {:?}", msg))
    }

    /// Requests the RLS to shut down and waits (with a timeout) until the child
    /// process is terminated.
    pub fn shutdown(mut self) {
        self.request::<lsp_types::request::Shutdown>(99999, ());
        self.notify::<lsp_types::notification::Exit>(());

        let fut = self.child.wait_with_output().timeout(rls_timeout());

        self.runtime.block_on(fut).unwrap();
    }
}
