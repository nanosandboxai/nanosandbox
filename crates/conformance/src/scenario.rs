//! Scenario model: the JSON schema that scripts a conformance exchange.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A full scripted exchange between an SDK driver and the conformance server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// Capabilities the server advertises in the hello handshake.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Scripted request/response steps, consumed in order.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// SDK-level expected outcomes (documented; enforced by the driver).
    #[serde(default)]
    pub assert: Value,
}

/// One scripted request/response exchange.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    /// Optional partial request match. When present, the incoming request must
    /// contain every field here with an equal value (nested objects recurse).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<Value>,
    /// One or more response frames. A list is flushed back-to-back without
    /// waiting for another request line (the streaming contract).
    #[serde(default, deserialize_with = "de_respond")]
    pub respond: Vec<Value>,
    /// Delay before sending the response, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    /// Max time to wait for the matching request, in milliseconds. When it
    /// elapses the server answers with a `timeout` error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Accept either a single frame object or an array of frames for `respond`.
fn de_respond<'de, D>(d: D) -> Result<Vec<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Value::deserialize(d)?;
    match v {
        Value::Array(items) => Ok(items),
        Value::Object(_) => Ok(vec![v]),
        _ => Err(serde::de::Error::custom(
            "respond must be a frame object or an array of frame objects",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_full_scenario() {
        let raw = r#"{
            "id": "exec_stream_basic",
            "description": "streamed exec with stdout and exit",
            "capabilities": ["lifecycle", "exec"],
            "steps": [
                {
                    "expect": {"type": "exec_stream", "command": "echo"},
                    "respond": [
                        {"type": "ok", "version": "1.0.0"},
                        {"type": "guest", "event": "stdout", "data": "hi\n"},
                        {"type": "guest", "event": "exit", "code": 0}
                    ],
                    "delay_ms": 5,
                    "timeout_ms": 2000
                }
            ],
            "assert": {"exit_code": 0, "stdout_contains": ["hi"]}
        }"#;
        let s: Scenario = serde_json::from_str(raw).unwrap();
        assert_eq!(s.id, "exec_stream_basic");
        assert_eq!(s.capabilities, vec!["lifecycle", "exec"]);
        assert_eq!(s.steps.len(), 1);
        assert_eq!(s.steps[0].respond.len(), 3);
        assert_eq!(s.steps[0].delay_ms, Some(5));
        assert_eq!(s.steps[0].timeout_ms, Some(2000));

        let re: Scenario = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(re, s);
    }

    #[test]
    fn single_frame_respond_is_normalized_to_a_list() {
        let raw = r#"{
            "id": "ping",
            "capabilities": ["lifecycle"],
            "steps": [{"expect": {"type": "ping"}, "respond": {"type": "pong", "version": "1"}}]
        }"#;
        let s: Scenario = serde_json::from_str(raw).unwrap();
        assert_eq!(s.steps[0].respond.len(), 1);
        assert_eq!(s.steps[0].respond[0]["type"], "pong");
    }

    #[test]
    fn missing_optional_fields_default() {
        let raw = r#"{"id": "minimal"}"#;
        let s: Scenario = serde_json::from_str(raw).unwrap();
        assert!(s.description.is_empty());
        assert!(s.capabilities.is_empty());
        assert!(s.steps.is_empty());
        assert_eq!(s.assert, Value::Null);
    }

    #[test]
    fn respond_rejects_non_object() {
        let raw = r#"{"id": "bad", "steps": [{"respond": 42}]}"#;
        assert!(serde_json::from_str::<Scenario>(raw).is_err());
    }
}
