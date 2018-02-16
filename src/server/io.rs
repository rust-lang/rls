// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use serde;
use serde_json;

use super::{Notification, Request};
use lsp_data::{LSPNotification, LSPRequest};

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use jsonrpc_core::{self as jsonrpc, response, version, Id};

/// A trait for anything that can read language server input messages.
pub trait MessageReader {
    /// Read the next input message.
    fn read_message(&self) -> Option<String> {
        None
    }
}

/// A message reader that gets messages from `stdin`.
pub(super) struct StdioMsgReader;

impl MessageReader for StdioMsgReader {
    fn read_message(&self) -> Option<String> {
        macro_rules! handle_err {
            ($e: expr, $s: expr) => {
                match $e {
                    Ok(x) => x,
                    Err(_) => {
                        debug!($s);
                        return None;
                    }
                }
            }
        }

        // Read in the "Content-length: xx" part
        let mut buffer = String::new();
        handle_err!(
            io::stdin().read_line(&mut buffer),
            "Could not read from stdin"
        );

        if buffer.is_empty() {
            debug!("Header is empty");
            return None;
        }

        let res: Vec<&str> = buffer.split(' ').collect();

        // Make sure we see the correct header
        if res.len() != 2 {
            debug!("Header is malformed");
            return None;
        }

        if res[0].to_lowercase() != "content-length:" {
            debug!("Header is missing 'content-length'");
            return None;
        }

        let size = handle_err!(
            usize::from_str_radix(res[1].trim(), 10),
            "Couldn't read size"
        );
        trace!("reading: {} bytes", size);

        // Skip the new lines
        let mut tmp = String::new();
        handle_err!(io::stdin().read_line(&mut tmp), "Could not read from stdin");

        let mut content = vec![0; size];
        handle_err!(
            io::stdin().read_exact(&mut content),
            "Could not read from stdin"
        );

        let content = handle_err!(String::from_utf8(content), "Non-utf8 input");

        Some(content)
    }
}

/// Anything that can send notifications and responses to a language server
/// client.
pub trait Output: Sync + Send + Clone + 'static {
    /// Send a response string along the output.
    fn response(&self, output: String);

    /// Get a new unique ID.
    fn provide_id(&self) -> u32;

    /// Notify the client of a failure.
    fn failure(&self, id: jsonrpc::Id, error: jsonrpc::Error) {
        let response = response::Failure {
            jsonrpc: Some(version::Version::V2),
            id,
            error,
        };

        self.response(serde_json::to_string(&response).unwrap());
    }

    /// Notify the client of a failure with the given diagnostic message.
    fn failure_message<M: Into<String>>(&self, id: usize, code: jsonrpc::ErrorCode, msg: M) {
        let error = jsonrpc::Error {
            code: code,
            message: msg.into(),
            data: None,
        };
        self.failure(Id::Num(id as u64), error);
    }

    /// Send a successful response or notification along the output.
    fn success<D: ::serde::Serialize + fmt::Debug>(&self, id: usize, data: &D) {
        let data = match serde_json::to_string(data) {
            Ok(data) => data,
            Err(e) => {
                debug!("Could not serialize data for success message. ");
                debug!("  Data: `{:?}`", data);
                debug!("  Error: {:?}", e);
                return;
            }
        };

        // {
        //     jsonrpc: String,
        //     id: usize,
        //     result: String,
        // }
        let output = format!("{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}", id, data);
        self.response(output);
    }

    /// Send a notification along the output.
    fn notify<A>(&self, notification: Notification<A>)
    where
        A: LSPNotification,
        <A as LSPNotification>::Params: serde::Serialize,
    {
        self.response(format!("{}", notification));
    }

    /// Send a one-shot request along the output.
    /// Ignores any response associated with the request.
    fn request<A>(&self, request: Request<A>)
    where
        A: LSPRequest,
        <A as LSPRequest>::Params: serde::Serialize,
    {
        self.response(format!("{}", request));
    }
}

/// An output that sends notifications and responses on `stdout`.
#[derive(Clone)]
pub(super) struct StdioOutput {
    next_id: Arc<AtomicU32>,
}

impl StdioOutput {
    /// Construct a new `stdout` output.
    pub fn new() -> StdioOutput {
        StdioOutput {
            next_id: Arc::new(AtomicU32::new(1)),
        }
    }
}

impl Output for StdioOutput {
    fn response(&self, output: String) {
        let o = format!("Content-Length: {}\r\n\r\n{}", output.len(), output);

        trace!("response: {:?}", o);

        let stdout = io::stdout();
        let mut stdout_lock = stdout.lock();
        write!(stdout_lock, "{}", o).unwrap();
        stdout_lock.flush().unwrap();
    }

    fn provide_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}
