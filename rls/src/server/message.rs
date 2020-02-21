//! Traits and structs for message handling.

use std::fmt;
use std::marker::PhantomData;
use std::time::Instant;

use jsonrpc_core::{self as jsonrpc, Id};
use log::debug;
use lsp_types::notification::ShowMessage;
use serde::ser::{Serialize, SerializeStruct, Serializer};
use serde::Deserialize;
use serde_derive::Serialize;

use crate::actions::{ActionContext, InitActionContext};
use crate::lsp_data::{LSPNotification, LSPRequest, MessageType, ShowMessageParams, WorkspaceEdit};
use crate::server::io::Output;

/// A response that just acknowledges receipt of its request.
#[derive(Debug, Serialize)]
pub struct Ack;

/// The lack of a response to a request.
#[derive(Debug)]
pub struct NoResponse;

/// A response to some request.
pub trait Response {
    /// Sends the response along the given output.
    fn send<O: Output>(self, id: RequestId, out: &O);
}

impl Response for NoResponse {
    fn send<O: Output>(self, _id: RequestId, _out: &O) {}
}

impl<R: ::serde::Serialize + fmt::Debug> Response for R {
    fn send<O: Output>(self, id: RequestId, out: &O) {
        out.success(id, &self);
    }
}

/// Wrapper for a response error.
#[derive(Debug)]
pub enum ResponseError {
    /// Error with no special response to the client.
    Empty,
    /// Error with a response to the client.
    Message(jsonrpc::ErrorCode, String),
}

impl From<()> for ResponseError {
    fn from(_: ()) -> Self {
        ResponseError::Empty
    }
}

/// Some actions can succeed in LSP terms, but can't succeed in user terms.
/// This response allows an action to send a message to the user (currently
/// only a warning) or a proper response.
#[derive(Debug)]
pub enum ResponseWithMessage<R: DefaultResponse> {
    Response(R),
    Warn(String),
}

/// A response that has a default value.
pub trait DefaultResponse: Response + ::serde::Serialize + fmt::Debug {
    fn default() -> Self;
}

impl<R: DefaultResponse> Response for ResponseWithMessage<R> {
    fn send<O: Output>(self, id: RequestId, out: &O) {
        match self {
            ResponseWithMessage::Response(r) => out.success(id, &r),
            ResponseWithMessage::Warn(s) => {
                out.notify(Notification::<ShowMessage>::new(ShowMessageParams {
                    typ: MessageType::Warning,
                    message: s,
                }));

                let default = R::default();
                default.send(id, out);
            }
        }
    }
}

impl DefaultResponse for WorkspaceEdit {
    fn default() -> WorkspaceEdit {
        WorkspaceEdit { changes: None, document_changes: None }
    }
}

/// An action taken in response to some notification from the client.
/// Blocks stdin whilst being handled.
pub trait BlockingNotificationAction: LSPNotification {
    /// Handles this notification.
    fn handle<O: Output>(_: Self::Params, _: &mut InitActionContext, _: O) -> Result<(), ()>;
}

/// A request that blocks stdin whilst being handled.
pub trait BlockingRequestAction: LSPRequest {
    type Response: Response + fmt::Debug;

    /// Handles request and returns its response. Output is also provided for additional messaging.
    fn handle<O: Output>(
        id: RequestId,
        params: Self::Params,
        ctx: &mut ActionContext,
        out: O,
    ) -> Result<Self::Response, ResponseError>;
}

/// A request ID as defined by language server protocol.
///
/// It only describes valid request IDs -- a case for notification (where `id` is not specified) is
/// not included here.
#[derive(Debug, PartialEq, Clone, Hash, Eq)]
pub enum RequestId {
    Str(String),
    Num(u64),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestId::Str(ref s) => write!(f, "\"{}\"", s),
            RequestId::Num(n) => write!(f, "{}", n),
        }
    }
}

impl<'a> From<&'a RequestId> for Id {
    fn from(request_id: &RequestId) -> Self {
        match request_id {
            RequestId::Str(ref s) => Id::Str(s.to_string()),
            RequestId::Num(n) => Id::Num(*n),
        }
    }
}

/// A request that gets JSON serialized in the language server protocol.
pub struct Request<A: LSPRequest> {
    /// The unique request ID.
    pub id: RequestId,
    /// The time the request was received / processed by the main stdin reading thread.
    pub received: Instant,
    /// The extra action-specific parameters.
    pub params: A::Params,
    /// This request's handler action.
    pub _action: PhantomData<A>,
}

impl<A: LSPRequest> Request<A> {
    /// Creates a server `Request` structure with given `params`.
    pub fn new(id: RequestId, params: A::Params) -> Request<A> {
        Request { id, received: Instant::now(), params, _action: PhantomData }
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
        Notification { params, _action: PhantomData }
    }
}

impl<'a, A> From<&'a Request<A>> for RawMessage
where
    A: LSPRequest,
    <A as LSPRequest>::Params: serde::Serialize,
{
    fn from(request: &Request<A>) -> RawMessage {
        let method = <A as LSPRequest>::METHOD.to_owned();

        let params = match serde_json::to_value(&request.params).unwrap() {
            params @ serde_json::Value::Array(_) |
            params @ serde_json::Value::Object(_) |
            // We represent missing params internally by Null.
            params @ serde_json::Value::Null => params,
            _ => unreachable!("Bad parameter type found for {:?} request", method),
        };

        RawMessage { method, id: Id::from(&request.id), params }
    }
}

impl<'a, A> From<&'a Notification<A>> for RawMessage
where
    A: LSPNotification,
    <A as LSPNotification>::Params: serde::Serialize,
{
    fn from(notification: &Notification<A>) -> RawMessage {
        let method = <A as LSPNotification>::METHOD.to_owned();

        let params = match serde_json::to_value(&notification.params).unwrap() {
            params @ serde_json::Value::Array(_) |
            params @ serde_json::Value::Object(_) |
            // We represent missing params internally by Null.
            params @ serde_json::Value::Null => params,
            _ => unreachable!("Bad parameter type found for {:?} request", method),
        };

        RawMessage { method, id: Id::Null, params }
    }
}

impl<A: BlockingRequestAction> Request<A> {
    pub fn blocking_dispatch<O: Output>(
        self,
        ctx: &mut ActionContext,
        out: &O,
    ) -> Result<A::Response, ResponseError> {
        A::handle(self.id, self.params, ctx, out.clone())
    }
}

impl<A: BlockingNotificationAction> Notification<A> {
    pub fn dispatch<O: Output>(self, ctx: &mut InitActionContext, out: O) -> Result<(), ()> {
        A::handle(self.params, ctx, out)?;
        Ok(())
    }
}

impl<'a, A> fmt::Display for Request<A>
where
    A: LSPRequest,
    <A as LSPRequest>::Params: serde::Serialize,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw: RawMessage = self.into();
        match serde_json::to_string(&raw) {
            Ok(val) => val.fmt(f),
            Err(_) => Err(fmt::Error),
        }
    }
}

impl<'a, A> fmt::Display for Notification<A>
where
    A: LSPNotification,
    <A as LSPNotification>::Params: serde::Serialize,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw: RawMessage = self.into();
        match serde_json::to_string(&raw) {
            Ok(val) => val.fmt(f),
            Err(_) => Err(fmt::Error),
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct RawMessage {
    pub(crate) method: String,
    pub(crate) id: Id,
    pub(crate) params: serde_json::Value,
}

impl RawMessage {
    pub(crate) fn parse_as_request<'de, R>(&'de self) -> Result<Request<R>, jsonrpc::Error>
    where
        R: LSPRequest,
        <R as LSPRequest>::Params: serde::Deserialize<'de>,
    {
        let parsed_id = match self.id {
            Id::Num(n) => Some(RequestId::Num(n)),
            Id::Str(ref s) => Some(RequestId::Str(s.to_string())),
            Id::Null => None,
        };

        let params = R::Params::deserialize(&self.params)
            .or_else(|e| {
                // Avoid tedious type errors trying to deserialize `()`.
                if std::mem::size_of::<R::Params>() == 0 {
                    R::Params::deserialize(&serde_json::Value::Null).map_err(|_| e)
                } else {
                    Err(e)
                }
            })
            .map_err(|e| {
                debug!("error when parsing as request: {}", e);
                jsonrpc::Error::invalid_params(format!("{}", e))
            })?;

        match parsed_id {
            Some(id) => Ok(Request { id, params, received: Instant::now(), _action: PhantomData }),
            None => Err(jsonrpc::Error::invalid_request()),
        }
    }

    pub(crate) fn parse_as_notification<'de, T>(
        &'de self,
    ) -> Result<Notification<T>, jsonrpc::Error>
    where
        T: LSPNotification,
        <T as LSPNotification>::Params: serde::Deserialize<'de>,
    {
        let params = T::Params::deserialize(&self.params).map_err(|e| {
            debug!("error when parsing as notification: {}", e);
            jsonrpc::Error::invalid_params(format!("{}", e))
        })?;

        Ok(Notification { params, _action: PhantomData })
    }

    pub(crate) fn try_parse(msg: &str) -> Result<Option<RawMessage>, jsonrpc::Error> {
        // Parse the message.
        let ls_command: serde_json::Value =
            serde_json::from_str(msg).map_err(|_| jsonrpc::Error::parse_error())?;

        // Per JSON-RPC/LSP spec, Requests must have ID, whereas Notifications cannot.
        let id = ls_command
            .get("id")
            .map_or(Id::Null, |id| serde_json::from_value(id.to_owned()).unwrap());

        let method = match ls_command.get("method") {
            Some(method) => method,
            // No method means this is a response to one of our requests.
            // FIXME: we should confirm these, but currently just ignore them.
            None => return Ok(None),
        };

        let method = method.as_str().ok_or_else(jsonrpc::Error::invalid_request)?.to_owned();

        // Representing a missing parameter as Null internally instead of `None`,
        // (Null being unused value of param by the JSON-RPC 2.0 spec)
        // in order to unify the type handling –- now the parameter type implements
        // `Deserialize`.
        let params = match ls_command.get("params").map(ToOwned::to_owned) {
            Some(params @ serde_json::Value::Object(..))
            | Some(params @ serde_json::Value::Array(..)) => params,
            // Null as input value is not allowed by JSON-RPC 2.0,
            // but including it for robustness.
            Some(serde_json::Value::Null) | None => serde_json::Value::Null,
            _ => return Err(jsonrpc::Error::invalid_request()),
        };

        Ok(Some(RawMessage { method, id, params }))
    }
}

// Added so we can prepend with extra constant `"jsonrpc": "2.0"` key.
// Should be resolved once <https://github.com/serde-rs/serde/issues/760> is fixed.
impl Serialize for RawMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let serialize_id = match self.id {
            Id::Null => false,
            _ => true,
        };
        let serialize_params = self.params.is_array() || self.params.is_object();

        let len = 2 + if serialize_id { 1 } else { 0 } + if serialize_params { 1 } else { 0 };
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
    use crate::server::notifications;
    use lsp_types::InitializedParams;
    use serde_json::json;

    #[test]
    fn test_parse_as_notification() {
        let raw = RawMessage {
            method: "initialize".to_owned(),
            id: Id::Null,
            params: serde_json::Value::Object(serde_json::Map::new()),
        };
        let notification: Notification<notifications::Initialized> =
            raw.parse_as_notification().unwrap();

        let expected = Notification::<notifications::Initialized>::new(InitializedParams {});

        assert_eq!(notification.params, expected.params);
        assert_eq!(notification._action, expected._action);
    }

    // See <http://www.jsonrpc.org/specification#request_object>.
    #[test]
    fn raw_message_parses_valid_jsonrpc_request_with_string_id() {
        let raw_json = json!({
            "jsonrpc": "2.0",
            "id": "abc",
            "method": "someRpcCall",
        })
        .to_string();

        let expected_msg = RawMessage {
            method: "someRpcCall".to_owned(),
            id: Id::Str("abc".to_owned()),
            // Missing parameters are represented internally as Null.
            params: serde_json::Value::Null,
        };
        assert_eq!(expected_msg, RawMessage::try_parse(&raw_json).unwrap().unwrap());
    }

    #[test]
    fn raw_message_parses_valid_jsonrpc_request_with_numeric_id() {
        let raw_json = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "someRpcCall",
        })
        .to_string();

        let expected_msg = RawMessage {
            method: "someRpcCall".to_owned(),
            id: Id::Num(1),
            // Missing parameters are represented internally as Null.
            params: serde_json::Value::Null,
        };
        assert_eq!(expected_msg, RawMessage::try_parse(&raw_json).unwrap().unwrap());
    }

    #[test]
    fn raw_message_with_string_id_parses_into_request() {
        #[derive(Debug)]
        enum DummyRequest {}
        impl LSPRequest for DummyRequest {
            type Params = ();
            type Result = ();
            const METHOD: &'static str = "dummyRequest";
        }

        let raw_msg = RawMessage {
            method: "dummyRequest".to_owned(),
            id: Id::Str("abc".to_owned()),
            params: serde_json::Value::Null,
        };

        let request: Request<DummyRequest> = raw_msg
            .parse_as_request()
            .expect("RawMessage with string id should parse into request");
        assert_eq!(RequestId::Str("abc".to_owned()), request.id)
    }

    #[test]
    fn serialize_message_no_params() {
        #[derive(Debug)]
        enum DummyNotification {}

        impl LSPNotification for DummyNotification {
            type Params = ();
            const METHOD: &'static str = "dummyNotification";
        }

        let notif = Notification::<DummyNotification>::new(());
        let raw = format!("{}", notif);
        let deser: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert!(match deser.get("params") {
            Some(&serde_json::Value::Array(ref arr)) if arr.is_empty() => true,
            Some(&serde_json::Value::Object(ref map)) if map.is_empty() => true,
            None => true,
            _ => false,
        });
    }

    #[test]
    fn serialize_message_empty_params() {
        #[derive(Debug)]
        enum DummyNotification {}
        #[derive(Serialize)]
        struct EmptyParams {}

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
