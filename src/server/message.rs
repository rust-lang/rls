// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Traits and structs for message handling


use jsonrpc_core::{self as jsonrpc, Id};
use serde;
use serde::ser::{Serialize, Serializer, SerializeStruct};
use serde::Deserialize;
use serde_json;

use lsp_data::{LSPNotification, LSPRequest};
use actions::ActionContext;
use server::io::Output;

use std::fmt;
use std::marker::PhantomData;
use std::time::Instant;

/// A response that just acknowledges receipt of its request.
#[derive(Debug, Serialize)]
pub struct Ack;

/// The lack of a response to a request.
#[derive(Debug)]
pub struct NoResponse;

/// A response to some request.
pub trait Response {
    /// Send the response along the given output.
    fn send<O: Output>(&self, id: usize, out: &O);
}

impl Response for NoResponse {
    fn send<O: Output>(&self, _id: usize, _out: &O) {}
}

impl<R: ::serde::Serialize + fmt::Debug> Response for R {
    fn send<O: Output>(&self, id: usize, out: &O) {
        out.success(id, &self);
    }
}

/// An action taken in response to some notification from the client.
/// Blocks stdin whilst being handled.
pub trait BlockingNotificationAction: LSPNotification {
    /// Handle this notification.
    fn handle<O: Output>(params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<(), ()>;
}

/// A request that blocks stdin whilst being handled
pub trait BlockingRequestAction: LSPRequest {
    type Response: Response + fmt::Debug;

    /// Handle request and send its response back along the given output.
    fn handle<O: Output>(
        id: usize,
        params: Self::Params,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<Self::Response, ()>;
}

/// A request that gets JSON serialized in the language server protocol.
pub struct Request<A: LSPRequest> {
    /// The unique request id.
    pub id: usize,
    /// The time the request was received / processed by the main stdin reading thread.
    pub received: Instant,
    /// The extra action-specific parameters.
    pub params: A::Params,
    /// This request's handler action.
    pub _action: PhantomData<A>,
}

impl<A: LSPRequest> Request<A> {
    /// Creates a server `Request` structure with given `params`.
    pub fn new(id: usize, params: A::Params) -> Request<A> {
        Request {
            id,
            received: Instant::now(),
            params,
            _action: PhantomData,
        }
    }
}

/// A notification that gets JSON serialized in the language server protocol.
#[derive(Debug, PartialEq)]
pub struct Notification<A: LSPNotification> {
    /// The extra action-specific parameters.
    pub params: A::Params,
    /// The action responsible for this notification.
    pub _action: PhantomData<A>,
}

impl<A: LSPNotification> Notification<A> {
    /// Creates a `Notification` structure with given `params`.
    pub fn new(params: A::Params) -> Notification<A> {
        Notification {
            params,
            _action: PhantomData,
        }
    }
}

impl<'a, A> From<&'a Request<A>> for RawMessage
where
    A: LSPRequest,
    <A as LSPRequest>::Params: serde::Serialize
{
    fn from(request: &Request<A>) -> RawMessage {
        let method = <A as LSPRequest>::METHOD.to_owned();

        let params = match serde_json::to_value(&request.params).unwrap() {
            params @ serde_json::Value::Array(_) |
            params @ serde_json::Value::Object(_) |
            // Internally we represent missing params by Null
            params @ serde_json::Value::Null => params,
            _ => unreachable!("Bad parameter type found for {:?} request", method),
        };

        RawMessage {
            method,
            // FIXME: for now we support only numeric ids
            id: Some(Id::Num(request.id as u64)),
            params
        }
    }
}

impl<'a, A> From<&'a Notification<A>> for RawMessage
where
    A: LSPNotification,
    <A as LSPNotification>::Params: serde::Serialize
{
    fn from(notification: &Notification<A>) -> RawMessage {
        let method = <A as LSPNotification>::METHOD.to_owned();

        let params = match serde_json::to_value(&notification.params).unwrap() {
            params @ serde_json::Value::Array(_) |
            params @ serde_json::Value::Object(_) |
            // Internally we represent missing params by Null
            params @ serde_json::Value::Null => params,
            _ => unreachable!("Bad parameter type found for {:?} request", method),
        };

        RawMessage {
            method,
            id: None,
            params
        }
    }
}

impl<A: BlockingRequestAction> Request<A> {
    pub fn blocking_dispatch<O: Output>(
        self,
        ctx: &mut ActionContext,
        out: &O,
    ) -> Result<A::Response, ()> {
        let result = A::handle(self.id, self.params, ctx, out.clone())?;
        result.send(self.id, out);
        Ok(result)
    }
}

impl<A: BlockingNotificationAction> Notification<A> {
    pub fn dispatch<O: Output>(self, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        A::handle(self.params, ctx, out)?;
        Ok(())
    }
}

impl<'a, A> fmt::Display for Request<A>
where
    A: LSPRequest,
    <A as LSPRequest>::Params: serde::Serialize,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let raw: RawMessage = self.into();
        match serde_json::to_string(&raw) {
            Ok(val) => val.fmt(f),
            Err(_) => Err(fmt::Error)
        }
    }
}

impl<'a, A> fmt::Display for Notification<A>
where
    A: LSPNotification,
    <A as LSPNotification>::Params: serde::Serialize,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let raw: RawMessage = self.into();
        match serde_json::to_string(&raw) {
            Ok(val) => val.fmt(f),
            Err(_) => Err(fmt::Error)
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct RawMessage {
    pub method: String,
    pub id: Option<Id>,
    pub params: serde_json::Value,
}

impl RawMessage {
    pub fn parse_as_request<'de, R>(&'de self) -> Result<Request<R>, jsonrpc::Error>
    where
        R: LSPRequest,
        <R as LSPRequest>::Params: serde::Deserialize<'de>,
    {
        // FIXME: We only support numeric responses, ideally we should switch from using parsed usize
        // to using jsonrpc_core::Id
        let parsed_numeric_id = match self.id {
            Some(Id::Num(n)) => Some(n as usize),
            Some(Id::Str(ref s)) => usize::from_str_radix(s, 10).ok(),
            _ => None,
        };

        let params = R::Params::deserialize(&self.params).map_err(|e| {
            debug!("error when parsing as request: {}", e);
            jsonrpc::Error::invalid_params(format!("{}", e))
        })?;

        match parsed_numeric_id {
            Some(id) => Ok(Request {
                id,
                params,
                received: Instant::now(),
                _action: PhantomData,
            }),
            None => Err(jsonrpc::Error::invalid_request()),
        }
    }

    pub fn parse_as_notification<'de, T>(&'de self) -> Result<Notification<T>, jsonrpc::Error>
    where
        T: LSPNotification,
        <T as LSPNotification>::Params: serde::Deserialize<'de>,
    {
        let params = T::Params::deserialize(&self.params).map_err(|e| {
            debug!("error when parsing as notification: {}", e);
            jsonrpc::Error::invalid_params(format!("{}", e))
        })?;

        Ok(Notification {
            params,
            _action: PhantomData,
        })
    }

    pub fn try_parse(msg: &str) -> Result<Option<RawMessage>, jsonrpc::Error> {
        // Parse the message.
        let ls_command: serde_json::Value =
            serde_json::from_str(msg).map_err(|_| jsonrpc::Error::parse_error())?;

        // Per JSON-RPC/LSP spec, Requests must have id, whereas Notifications can't
        let id = ls_command
            .get("id")
            .map(|id| serde_json::from_value(id.to_owned()).unwrap());

        let method = match ls_command.get("method") {
            Some(method) => method,
            // No method means this is a response to one of our requests. FIXME: we should
            // confirm these, but currently just ignore them.
            None => return Ok(None),
        };

        let method = method
            .as_str()
            .ok_or_else(jsonrpc::Error::invalid_request)?
            .to_owned();

        // Representing internally a missing parameter as Null instead of None,
        // (Null being unused value of param by the JSON-RPC 2.0 spec)
        // to unify the type handling – now the parameter type implements Deserialize.
        let params = match ls_command.get("params").map(|p| p.to_owned()) {
            Some(params @ serde_json::Value::Object(..)) |
            Some(params @ serde_json::Value::Array(..)) => params,
            // Null as input value is not allowed by JSON-RPC 2.0,
            // but including it for robustness
            Some(serde_json::Value::Null) | None => serde_json::Value::Null,
            _ => return Err(jsonrpc::Error::invalid_request()),
        };

        Ok(Some(RawMessage { method, id, params }))
    }
}

// Added so we can prepend with extra constant "jsonrpc": "2.0" key.
// Should be resolved once https://github.com/serde-rs/serde/issues/760 is fixed.
impl Serialize for RawMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let serialize_id = self.id.is_some();
        let serialize_params = self.params.is_array() || self.params.is_object();

        let len = 2 + if serialize_id { 1 } else { 0 }
                    + if serialize_params { 1 } else { 0 };
        let mut msg = serializer.serialize_struct("RawMessage", len)?;
        msg.serialize_field("jsonrpc", "2.0")?;
        msg.serialize_field("method", &self.method)?;
        // Notifications don't have Id specified
        if serialize_id {
            msg.serialize_field("id", &self.id)?;
        }
        if serialize_params {
            msg.serialize_field("params", &self.params)?;
        }
        msg.end()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use ls_types::InitializedParams;
    use server::notifications;

    #[test]
    fn test_parse_as_notification() {
        let raw = RawMessage {
            method: "initialize".to_owned(),
            id: None,
            params: serde_json::Value::Object(serde_json::Map::new()),
        };
        let notification: Notification<notifications::Initialized> =
            raw.parse_as_notification().unwrap();

        let expected = Notification::<notifications::Initialized>::new(InitializedParams {});

        assert_eq!(notification.params, expected.params);
        assert_eq!(notification._action, expected._action);
    }

    // http://www.jsonrpc.org/specification#request_object
    #[test]
    fn parse_raw_message() {
        let raw_msg = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "someRpcCall",
        }).to_string();

        let str_msg = RawMessage {
            method: "someRpcCall".to_owned(),
            // FIXME: for now we support only numeric ids
            id: Some(Id::Num(1)),
            // Internally missing parameters are represented as null
            params: serde_json::Value::Null,
        };
        assert_eq!(str_msg, RawMessage::try_parse(&raw_msg).unwrap().unwrap());
    }

    #[test]
    fn serialize_message_no_params() {
        #[derive(Debug)]
        pub enum DummyNotification { }

        impl LSPNotification for DummyNotification {
            type Params = ();
            const METHOD: &'static str = "dummyNotification";
        }

        let notif = Notification::<DummyNotification>::new(());
        let raw = format!("{}", notif);
        let deser: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert!(match deser.get("params") {
            Some(&serde_json::Value::Array(ref arr)) if arr.len() == 0 => true,
            Some(&serde_json::Value::Object(ref map)) if map.len() == 0 => true,
            None => true,
            _ => false,
        });
    }

    #[test]
    fn serialize_message_empty_params() {
        #[derive(Debug)]
        pub enum DummyNotification { }
        #[derive(Serialize)]
        pub struct EmptyParams {}

        impl LSPNotification for DummyNotification {
            type Params = EmptyParams;
            const METHOD: &'static str = "dummyNotification";
        }

        let notif = Notification::<DummyNotification>::new(EmptyParams {});
        let raw = format!("{}", notif);
        let deser: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(*deser.get("params").unwrap(), json!({}));
    }

    #[test]
    fn deserialize_message_empty_params() {
        let msg = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
        let parsed = RawMessage::try_parse(msg).unwrap().unwrap();
        parsed.parse_as_notification::<notifications::Initialized>().unwrap();
    }
}