use log::{debug, trace};

use super::{Notification, Request, RequestId};
use crate::lsp_data::{LSPNotification, LSPRequest};

use std::fmt;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use jsonrpc_core::{self as jsonrpc, response, version, Id};

/// Anything that can read language server input messages.
pub trait MessageReader {
    /// Read the next input message.
    fn read_message(&self) -> Option<String>;
}

/// A message reader that gets messages from `stdin`.
pub(super) struct StdioMsgReader;

impl MessageReader for StdioMsgReader {
    fn read_message(&self) -> Option<String> {
        let stdin = io::stdin();
        let mut locked = stdin.lock();
        match read_message(&mut locked) {
            Ok(message) => Some(message),
            Err(err) => {
                debug!("{:?}", err);
                None
            }
        }
    }
}

// Reads the content of the next message from given input.
//
// The input is expected to provide a message as described by "Base Protocol" of Language Server
// Protocol.
fn read_message<R: BufRead>(input: &mut R) -> Result<String, io::Error> {
    // Read in the "Content-Length: xx" part.
    let mut size: Option<usize> = None;
    loop {
        let mut buffer = String::new();
        input.read_line(&mut buffer)?;

        // End of input.
        if buffer.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "EOF encountered in the middle of reading LSP headers",
            ));
        }

        // Header section is finished, break from the loop.
        if buffer == "\r\n" {
            break;
        }

        let res: Vec<&str> = buffer.split(' ').collect();

        // Make sure header is valid.
        if res.len() != 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Header '{}' is malformed", buffer),
            ));
        }
        let header_name = res[0].to_lowercase();
        let header_value = res[1].trim();

        match header_name.as_ref() {
            "content-length:" => {
                size = Some(usize::from_str_radix(header_value, 10).map_err(|_e| {
                    io::Error::new(io::ErrorKind::InvalidData, "Couldn't read size")
                })?);
            }
            "content-type:" => {
                if header_value != "utf8" && header_value != "utf-8" {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Content type '{}' is invalid", header_value),
                    ));
                }
            }
            // Ignore unknown headers (specification doesn't say what to do in this case).
            _ => (),
        }
    }
    let size = match size {
        Some(size) => size,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Message is missing 'content-length' header",
            ));
        }
    };
    trace!("reading: {:?} bytes", size);

    let mut content = vec![0; size];
    input.read_exact(&mut content)?;

    String::from_utf8(content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Anything that can send notifications and responses to a language server client.
pub trait Output: Sync + Send + Clone + 'static {
    /// Sends a response string along the output.
    fn response(&self, output: String);

    /// Gets a new unique ID.
    fn provide_id(&self) -> RequestId;

    /// Notifies the client of a failure.
    fn failure(&self, id: jsonrpc::Id, error: jsonrpc::Error) {
        let response = response::Failure { jsonrpc: Some(version::Version::V2), id, error };

        self.response(serde_json::to_string(&response).unwrap());
    }

    /// Notifies the client of a failure with the given diagnostic message.
    fn failure_message<M: Into<String>>(&self, id: RequestId, code: jsonrpc::ErrorCode, msg: M) {
        let error = jsonrpc::Error { code, message: msg.into(), data: None };
        self.failure(Id::from(&id), error);
    }

    /// Sends a successful response or notification along the output.
    fn success<D: ::serde::Serialize + fmt::Debug>(&self, id: RequestId, data: &D) {
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

    /// Sends a notification along the output.
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
    next_id: Arc<AtomicU64>,
}

impl StdioOutput {
    /// Constructs a new `stdout` output.
    pub(crate) fn new() -> StdioOutput {
        StdioOutput { next_id: Arc::new(AtomicU64::new(1)) }
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

    fn provide_id(&self) -> RequestId {
        RequestId::Num(self.next_id.fetch_add(1, Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_message_returns_message_from_valid_lsr_input() {
        let mut input = io::Cursor::new("Content-Length: 7\r\n\r\nMessage");

        let message =
            read_message(&mut input).expect("Reading a message from valid input should succeed");

        assert_eq!(message, "Message");
    }

    #[test]
    fn read_message_fails_on_empty_input() {
        let mut input = io::Cursor::new("");

        read_message(&mut input).expect_err("Empty input should cause failure");
    }

    #[test]
    fn read_message_returns_message_from_input_with_multiple_headers() {
        let mut input =
            io::Cursor::new("Content-Type: utf-8\r\nContent-Length: 12\r\n\r\nSome Message");

        let message =
            read_message(&mut input).expect("Reading a message from valid input should succeed");

        assert_eq!(message, "Some Message");
    }

    #[test]
    fn read_message_returns_message_from_input_with_unknown_headers() {
        let mut input =
            io::Cursor::new("Unknown-Header: value\r\nContent-Length: 12\r\n\r\nSome Message");

        let message =
            read_message(&mut input).expect("Reading a message from valid input should succeed");

        assert_eq!(message, "Some Message");
    }

    #[test]
    fn read_message_fails_when_length_header_is_missing() {
        let mut input = io::Cursor::new("Content-Type: utf8\r\n\r\nSome Message");

        read_message(&mut input).expect_err("Reading a message with no length header should fail.");
    }

    #[test]
    fn read_message_fails_when_content_type_is_invalid() {
        let mut input =
            io::Cursor::new("Content-Length: 12\r\nContent-Type: invalid\r\n\r\nSome Message");

        read_message(&mut input)
            .expect_err("Reading a message with invalid content type should fail.");
    }

    #[test]
    fn read_message_fails_when_header_line_is_invalid() {
        let mut input = io::Cursor::new("Invalid-Header\r\nContent-Length: 12\r\n\r\nSome Message");

        read_message(&mut input).expect_err("Reading a message with invalid header should fail.");
    }

    #[test]
    fn read_message_fails_when_length_is_not_numeric() {
        let mut input = io::Cursor::new("Content-Length: abcd\r\n\r\nMessage");

        read_message(&mut input).expect_err("Reading a message with no length header should fail.");
    }

    #[test]
    fn read_message_fails_when_length_is_too_large_integer() {
        let mut input = io::Cursor::new("Content-Length: 1000000000000000000000\r\n\r\nMessage");

        read_message(&mut input).expect_err(
            "Reading a message with length too large to fit into 64bit integer should fail.",
        );
    }

    #[test]
    fn read_message_fails_when_content_is_not_valid_utf8() {
        let mut input = io::Cursor::new(b"Content-Length: 7\r\n\r\n\x82\xe6\x82\xa8\x82\xb1\x82");

        read_message(&mut input).expect_err(
            "Reading a message with content containing invalid utf8 sequences should fail.",
        );
    }

    #[test]
    fn read_message_fails_when_input_contains_only_header() {
        let mut input = io::Cursor::new(b"Content-Length: 7\r\n");

        read_message(&mut input).expect_err("Reading should fail when input ends after header.");
    }
}
