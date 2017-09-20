// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use serde_json;

use lsp_data::*;

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{Ordering, AtomicU32};

use jsonrpc_core::{self as jsonrpc, Id, response, version};


pub trait MessageReader {
    fn read_message(&self) -> Option<String> {
        None
    }
}

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
        handle_err!(io::stdin().read_line(&mut buffer), "Could not read from stdin");

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

        let size = handle_err!(usize::from_str_radix(&res[1].trim(), 10), "Couldn't read size");
        trace!("reading: {} bytes", size);

        // Skip the new lines
        let mut tmp = String::new();
        handle_err!(io::stdin().read_line(&mut tmp), "Could not read from stdin");

        let mut content = vec![0; size];
        handle_err!(io::stdin().read_exact(&mut content), "Could not read from stdin");

        let content = handle_err!(String::from_utf8(content), "Non-utf8 input");

        Some(content)
    }
}

pub trait Output: Sync + Send + Clone + 'static {
    fn response(&self, output: String);
    fn provide_id(&self) -> u32;

    fn failure(&self, id: jsonrpc::Id, error: jsonrpc::Error) {
        let response = response::Failure {
            jsonrpc: Some(version::Version::V2),
            id,
            error,
        };

        self.response(serde_json::to_string(&response).unwrap());
    }

    fn failure_message<M: Into<String>>(&self, id: usize, code: jsonrpc::ErrorCode, msg: M) {
        let error = jsonrpc::Error {
            code: code,
            message: msg.into(),
            data: None
        };
        self.failure(Id::Num(id as u64), error);
    }

    fn success<D: ::serde::Serialize + fmt::Debug>(&self, id: usize, data: &D) {
        let data = match serde_json::to_string(data) {
            Ok(data) => data,
            Err(e) => {
                debug!("Could not serialise data for success message. ");
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

    fn notify(&self, notification: NotificationMessage) {
        self.response(serde_json::to_string(&notification).unwrap());
    }
}

#[derive(Clone)]
pub(super) struct StdioOutput {
    next_id: Arc<AtomicU32>,
}

impl StdioOutput {
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

        print!("{}", o);
        io::stdout().flush().unwrap();
    }

    fn provide_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}
