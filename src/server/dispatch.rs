// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use super::requests::*;
use actions::work_pool;
use actions::work_pool::WorkDescription;
use jsonrpc_core as jsonrpc;
use jsonrpc_core::types::ErrorCode;
use server;
use server::{Request, Response};
use server::io::Output;
use actions::InitActionContext;
use lsp_data::LSPRequest;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

lazy_static! {
    pub static ref DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(::COMPILER_TIMEOUT);
}

/// Macro enum `DispatchRequest` packing in various similar `Request` types
macro_rules! define_dispatch_request_enum {
    ($($request_type:ident),*) => {
        #[allow(large_enum_variant)] // seems ok for a short lived macro-enum
        pub enum DispatchRequest {
            $(
                $request_type(Request<$request_type>, InitActionContext),
            )*
        }

        $(
            impl From<(Request<$request_type>, InitActionContext)> for DispatchRequest {
                fn from((req, ctx): (Request<$request_type>, InitActionContext)) -> Self {
                    DispatchRequest::$request_type(req, ctx)
                }
            }
        )*

        impl DispatchRequest {
            fn handle<O: Output>(self, out: &O) {
                match self {
                $(
                    DispatchRequest::$request_type(req, ctx) => {
                        let Request { id, params, received, .. } = req;
                        let timeout = $request_type::timeout();

                        let receiver = work_pool::receive_from_thread(move || {
                            // checking timeout here can prevent starting expensive work that has
                            // already timed out due to previous long running requests
                            // Note: done here on the threadpool as pool scheduling may incur
                            // a further delay
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
    FindImpls,
    DocumentHighlight,
    Rename,
    CodeAction,
    ResolveCompletion,
    Formatting,
    RangeFormatting,
    ExecuteCommand
);

/// Provides ability to dispatch requests to a worker thread that will
/// handle the requests sequentially, without blocking stdin.
/// Requests dispatched this way are automatically timed out & avoid
/// processing if have already timed out before starting.
pub struct Dispatcher {
    sender: mpsc::Sender<DispatchRequest>,

    request_handled_receiver: mpsc::Receiver<()>,
    /// Number of as-yet-unhandled requests dispatched to the worker thread
    in_flight_requests: usize,
}

impl Dispatcher {
    /// Creates a new `Dispatcher` starting a new thread and channel
    pub fn new<O: Output>(out: O) -> Self {
        let (sender, receiver) = mpsc::channel::<DispatchRequest>();
        let (request_handled_sender, request_handled_receiver) = mpsc::channel::<()>();

        thread::Builder::new()
            .name("dispatch-worker".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    request.handle(&out);
                    let _ = request_handled_sender.send(());
                }
            })
            .unwrap();

        Self {
            sender,
            request_handled_receiver,
            in_flight_requests: 0,
        }
    }

    /// Blocks until all dispatched requests have been handled
    pub fn await_all_dispatched(&mut self) {
        while self.in_flight_requests != 0 {
            self.request_handled_receiver.recv().unwrap();
            self.in_flight_requests -= 1;
        }
    }

    /// Sends a request to the dispatch-worker thread, does not block
    pub fn dispatch<R: Into<DispatchRequest>>(&mut self, request: R) {
        if let Err(err) = self.sender.send(request.into()) {
            debug!("Failed to dispatch request: {:?}", err);
        } else {
            self.in_flight_requests += 1;
        }

        // Clear the handled queue if possible in a non-blocking way
        while self.request_handled_receiver.try_recv().is_ok() {
            self.in_flight_requests -= 1;
        }
    }
}

/// Stdin-nonblocking request logic designed to be packed into a `DispatchRequest`
/// and handled on the `WORK_POOL` via a `Dispatcher`.
pub trait RequestAction: LSPRequest {
    /// Serializable response type
    type Response: server::Response + Send;

    /// Max duration this request should finish within, also see `fallback_response()`
    fn timeout() -> Duration {
        *DEFAULT_REQUEST_TIMEOUT
    }

    /// Returns a response used in timeout scenarios
    fn fallback_response() -> Result<Self::Response, ResponseError>;

    /// Request processing logic
    fn handle(
        ctx: InitActionContext,
        params: Self::Params,
    ) -> Result<Self::Response, ResponseError>;
}

/// Wrapper for a response error
#[derive(Debug)]
pub enum ResponseError {
    /// Error with no special response to the client
    Empty,
    /// Error with a response to the client
    Message(jsonrpc::ErrorCode, String),
}

impl From<()> for ResponseError {
    fn from(_: ()) -> Self {
        ResponseError::Empty
    }
}
