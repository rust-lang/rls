use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jsonrpc_core::types::ErrorCode;
use log::debug;

use crate::actions::work_pool;
use crate::actions::work_pool::WorkDescription;
use crate::actions::InitActionContext;
use crate::concurrency::{ConcurrentJob, JobToken};
use crate::lsp_data::LSPRequest;
use crate::server;
use crate::server::io::Output;
use crate::server::message::ResponseError;
use crate::server::{Request, Response};

use super::requests::*;

/// Timeout time for request responses. By default a LSP client request not
/// responded to after this duration will return a fallback response.
#[cfg(not(test))]
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(1500);

// Timeout lengthened to "never" for potentially very slow CI boxes
#[cfg(test)]
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(3_600_000);

/// Macro enum `DispatchRequest` packing in various similar `Request` types
macro_rules! define_dispatch_request_enum {
    ($($request_type:ident),*$(,)*) => {
        // Seems ok for a short-lived macro-enum.
        #[allow(clippy::large_enum_variant)]
        pub(crate) enum DispatchRequest {
            $(
                $request_type(Request<$request_type>),
            )*
        }

        $(
            impl From<Request<$request_type>> for DispatchRequest {
                fn from(req: Request<$request_type>) -> Self {
                    DispatchRequest::$request_type(req)
                }
            }
        )*

        impl DispatchRequest {
            fn handle<O: Output>(self, ctx: InitActionContext, out: &O) {
                match self {
                $(
                    DispatchRequest::$request_type(req) => {
                        let Request { id, params, received, .. } = req;
                        let timeout = $request_type::timeout();

                        let receiver = work_pool::receive_from_thread(move || {
                            // Checking timeout here can prevent starting expensive work that has
                            // already timed out due to previous long running requests.
                            // Note: done here on the threadpool as pool scheduling may incur
                            // a further delay.
                            if received.elapsed() >= timeout {
                                $request_type::fallback_response()
                            }
                            else {
                                $request_type::handle(ctx, params)
                            }
                        }, WorkDescription($request_type::METHOD));

                        match receiver.recv_timeout(timeout)
                            .unwrap_or_else(|_| $request_type::fallback_response()) {
                            Ok(response) => response.send(id, out),
                            Err(ResponseError::Empty) => {
                                out.failure_message(id, ErrorCode::InternalError, "An unknown error occurred")
                            }
                            Err(ResponseError::Message(code, msg)) => {
                                out.failure_message(id, code, msg)
                            }
                        }
                    }
                )*
                }
            }
        }
    }
}

define_dispatch_request_enum!(
    Completion,
    Definition,
    References,
    WorkspaceSymbol,
    Symbols,
    Hover,
    Implementation,
    DocumentHighlight,
    Rename,
    CodeAction,
    ResolveCompletion,
    Formatting,
    RangeFormatting,
    ExecuteCommand,
    CodeLensRequest,
);

/// Provides ability to dispatch requests to a worker thread that will
/// handle the requests sequentially, without blocking stdin.
/// Requests dispatched this way are automatically timed out & avoid
/// processing if have already timed out before starting.
pub(crate) struct Dispatcher {
    sender: mpsc::Sender<(DispatchRequest, InitActionContext, JobToken)>,
}

impl Dispatcher {
    /// Creates a new `Dispatcher` starting a new thread and channel.
    pub(crate) fn new<O: Output>(out: O) -> Self {
        let (sender, receiver) = mpsc::channel::<(DispatchRequest, InitActionContext, JobToken)>();

        thread::Builder::new()
            .name("dispatch-worker".into())
            .spawn(move || {
                while let Ok((request, ctx, token)) = receiver.recv() {
                    request.handle(ctx, &out);
                    drop(token);
                }
            })
            .unwrap();

        Self { sender }
    }

    /// Sends a request to the dispatch-worker thread; does not block.
    pub(crate) fn dispatch<R: Into<DispatchRequest>>(
        &mut self,
        request: R,
        ctx: InitActionContext,
    ) {
        let (job, token) = ConcurrentJob::new();
        ctx.add_job(job);
        if let Err(err) = self.sender.send((request.into(), ctx, token)) {
            debug!("failed to dispatch request: {:?}", err);
        }
    }
}

/// Stdin-non-blocking request logic designed to be packed into a `DispatchRequest`
/// and handled on the `WORK_POOL` via a `Dispatcher`.
pub trait RequestAction: LSPRequest {
    /// Serializable response type.
    type Response: server::Response + Send;

    /// Max duration this request should finish within; also see `fallback_response()`.
    fn timeout() -> Duration {
        DEFAULT_REQUEST_TIMEOUT
    }

    /// Returns a response used in timeout scenarios.
    fn fallback_response() -> Result<Self::Response, ResponseError>;

    /// Request processing logic.
    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError>;
}
