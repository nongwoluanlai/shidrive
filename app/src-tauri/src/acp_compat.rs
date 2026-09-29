//! Deliberately small compatibility boundary around official SDK v1 types.
//!
//! No framing, request IDs, pending table or response dispatch lives here. The
//! SDK owns all JSON-RPC. Preserve legacy `models`/`title`/extension fields which
//! the current schema no longer exposes, and retain previously supported aliases.
use agent_client_protocol::{
    schema::v1 as s, Error, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    UntypedMessage,
};
use serde::Serialize;
use serde_json::{json, Value};

/// A typed official request with a lossless response at the UI/business boundary.
#[derive(Debug, Clone)]
pub(super) struct Preserve<R>(pub R);

impl<R: JsonRpcRequest> JsonRpcMessage for Preserve<R> {
    fn matches_method(method: &str) -> bool {
        R::matches_method(method)
    }
    fn method(&self) -> &str {
        self.0.method()
    }
    fn to_untyped_message(&self) -> Result<UntypedMessage, Error> {
        let mut message = self.0.to_untyped_message()?;
        // Existing adapters using the older spelling still receive it. The
        // canonical configId and value are constructed by the SDK's typed request.
        if message.method == "session/set_config_option" {
            if let Some(id) = message.params.get("configId").cloned() {
                message.params["configOptionId"] = id;
            }
        }
        Ok(message)
    }
    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, Error> {
        R::parse_message(method, params).map(Self)
    }
}
impl<R: JsonRpcRequest> JsonRpcRequest for Preserve<R> {
    type Response = Preserved<R::Response>;
}

#[derive(Debug, Clone)]
pub(super) struct Preserved<T> {
    pub typed: T,
    pub raw: Value,
}
impl<T: JsonRpcResponse> JsonRpcResponse for Preserved<T> {
    fn from_value(method: &str, raw: Value) -> Result<Self, Error> {
        let mut validation = raw.clone();
        if method == "session/set_config_option" {
            // Some adapters acknowledge with null/{} instead of the new options.
            // Do NOT add an empty list to `raw`: that would erase the cached menu.
            if validation.is_null() {
                validation = json!({});
            }
            if let Some(obj) = validation.as_object_mut() {
                obj.entry("configOptions").or_insert_with(|| json!([]));
            }
        }
        if method == "session/list" {
            if validation.is_array() {
                validation = json!({ "sessions": validation });
            }
            if let Some(cursor) = validation.get("next_cursor").cloned() {
                validation["nextCursor"] = cursor;
            }
            if let Some(rows) = validation.get_mut("sessions").and_then(Value::as_array_mut) {
                for row in rows {
                    if let Some(obj) = row.as_object_mut() {
                        for (old, new) in [
                            ("session_id", "sessionId"),
                            ("updated_at", "updatedAt"),
                            ("workingDirectory", "cwd"),
                        ] {
                            if !obj.contains_key(new) {
                                if let Some(v) = obj.get(old).cloned() {
                                    obj.insert(new.into(), v);
                                }
                            }
                        }
                        // The old local SessionInfo allowed an unknown cwd. Keep
                        // it absent in raw/UI rather than inventing a directory.
                        obj.entry("cwd").or_insert_with(|| json!(""));
                    }
                }
            }
        }
        Ok(Self {
            typed: T::from_value(method, validation)?,
            raw,
        })
    }
    fn into_json(self, _method: &str) -> Result<Value, Error> {
        Ok(self.raw)
    }
}

/// Typed notification validation without losing extra fields on tool updates.
#[derive(Debug, Clone)]
pub(super) struct SessionUpdate {
    pub typed: s::SessionNotification,
    pub raw: Value,
}
impl JsonRpcMessage for SessionUpdate {
    fn matches_method(method: &str) -> bool {
        s::SessionNotification::matches_method(method)
    }
    fn method(&self) -> &str {
        "session/update"
    }
    fn to_untyped_message(&self) -> Result<UntypedMessage, Error> {
        UntypedMessage::new(self.method(), &self.raw)
    }
    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, Error> {
        let raw = serde_json::to_value(params)?;
        let mut validation = raw.clone();
        // Older adapters use a text-block array for a single text chunk.
        if let Some(update) = validation.get_mut("update") {
            if matches!(
                update.get("sessionUpdate").and_then(Value::as_str),
                Some("agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk")
            ) {
                if let Some(blocks) = update.get("content").and_then(Value::as_array) {
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect();
                    update["content"] = json!({ "type": "text", "text": text });
                }
            }
        }
        Ok(Self {
            typed: s::SessionNotification::parse_message(method, &validation)?,
            raw,
        })
    }
}
impl JsonRpcNotification for SessionUpdate {}

/// Accept the historical `contents` spelling, but write the standard `content`.
#[derive(Debug, Clone)]
pub(super) struct WriteTextFile(pub s::WriteTextFileRequest);
impl JsonRpcMessage for WriteTextFile {
    fn matches_method(method: &str) -> bool {
        s::WriteTextFileRequest::matches_method(method)
    }
    fn method(&self) -> &str {
        self.0.method()
    }
    fn to_untyped_message(&self) -> Result<UntypedMessage, Error> {
        self.0.to_untyped_message()
    }
    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, Error> {
        let mut p = serde_json::to_value(params)?;
        if p.get("content").is_none() {
            if let Some(contents) = p.get("contents").cloned() {
                p["content"] = contents;
            }
        }
        s::WriteTextFileRequest::parse_message(method, &p).map(Self)
    }
}
impl JsonRpcRequest for WriteTextFile {
    type Response = s::WriteTextFileResponse;
}

/// These pre-existing elicitation aliases are NOT advertised as a stable ACP
/// feature. They are explicit extension handlers, transported by the SDK.
#[derive(Debug, Clone)]
pub(super) struct Elicitation {
    pub method: String,
    pub params: Value,
}
impl JsonRpcMessage for Elicitation {
    fn matches_method(method: &str) -> bool {
        matches!(
            method,
            "elicitation/create" | "session/elicitation/create" | "elicitation/request"
        )
    }
    fn method(&self) -> &str {
        &self.method
    }
    fn to_untyped_message(&self) -> Result<UntypedMessage, Error> {
        UntypedMessage::new(&self.method, &self.params)
    }
    fn parse_message(method: &str, params: &impl Serialize) -> Result<Self, Error> {
        if !Self::matches_method(method) {
            return Err(Error::method_not_found());
        }
        let params = serde_json::to_value(params)?;
        if !params.is_object() {
            return Err(Error::invalid_params());
        }
        Ok(Self {
            method: method.into(),
            params,
        })
    }
}
impl JsonRpcRequest for Elicitation {
    type Response = Value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_response_keeps_legacy_models_and_title() {
        let raw = json!({"sessionId":"s", "models":{"currentModelId":"m"}, "title":"Local title"});
        let response =
            Preserved::<s::NewSessionResponse>::from_value("session/new", raw.clone()).unwrap();
        assert_eq!(response.typed.session_id.to_string(), "s");
        assert_eq!(response.raw, raw);
        assert!(Preserved::<s::NewSessionResponse>::from_value("session/new", json!({})).is_err());
    }
    #[test]
    fn empty_config_ack_is_not_an_empty_menu() {
        for raw in [Value::Null, json!({})] {
            let response = Preserved::<s::SetSessionConfigOptionResponse>::from_value(
                "session/set_config_option",
                raw.clone(),
            )
            .unwrap();
            assert_eq!(response.raw, raw);
            assert!(response.raw.get("configOptions").is_none());
        }
    }
    #[test]
    fn config_request_has_canonical_and_legacy_id() {
        let req = Preserve(s::SetSessionConfigOptionRequest::new(
            "s",
            "model",
            s::SessionConfigOptionValue::value_id("m"),
        ));
        let p = req.to_untyped_message().unwrap().params;
        assert_eq!(p["configId"], "model");
        assert_eq!(p["configOptionId"], "model");
        assert_eq!(p["value"], "m");
    }
    #[test]
    fn legacy_list_keeps_unknown_cwd_and_aliases() {
        let raw = json!({"sessions":[{"session_id":"s", "title":"test"}], "next_cursor":"page2"});
        let response =
            Preserved::<s::ListSessionsResponse>::from_value("session/list", raw.clone()).unwrap();
        assert_eq!(response.typed.sessions[0].session_id.to_string(), "s");
        assert_eq!(response.typed.next_cursor.as_deref(), Some("page2"));
        assert_eq!(response.raw, raw);
    }
    #[test]
    fn text_array_update_and_extra_tool_fields_survive() {
        let p = json!({"sessionId":"s", "update":{"sessionUpdate":"agent_message_chunk", "content":[{"type":"text","text":"hello"}]}});
        let notification = SessionUpdate::parse_message("session/update", &p).unwrap();
        assert_eq!(notification.raw, p);
        assert_eq!(notification.typed.session_id.to_string(), "s");
        let p = json!({"sessionId":"s", "update":{"sessionUpdate":"tool_call_update", "toolCallId":"t",
            "content":[{"type":"content", "content":{"type":"text", "text":"only output"}}], "adapterExtension":true}});
        assert_eq!(
            SessionUpdate::parse_message("session/update", &p)
                .unwrap()
                .raw,
            p
        );
    }
    #[test]
    fn standard_write_content_wins_over_legacy_alias() {
        let p =
            json!({"sessionId":"s", "path":"/tmp/test", "content":"standard", "contents":"legacy"});
        assert_eq!(
            WriteTextFile::parse_message("fs/write_text_file", &p)
                .unwrap()
                .0
                .content,
            "standard"
        );
        let p = json!({"sessionId":"s", "path":"/tmp/test", "contents":"legacy"});
        assert_eq!(
            WriteTextFile::parse_message("fs/write_text_file", &p)
                .unwrap()
                .0
                .content,
            "legacy"
        );
    }
}
