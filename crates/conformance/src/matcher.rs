//! Request matching, capability gating, and terminal-frame detection.

use nanosb_protocol::Capability;
use serde_json::Value;

/// Whether `needle` is a partial match of `haystack`: every field in `needle`
/// must be present in `haystack` with an equal (or recursively matching) value.
pub fn partial_match(needle: &Value, haystack: &Value) -> bool {
    match needle {
        Value::Object(n) => {
            let Value::Object(h) = haystack else {
                return false;
            };
            n.iter().all(|(k, v)| match h.get(k) {
                Some(hv) => partial_match(v, hv),
                None => false,
            })
        }
        Value::Array(n) => {
            let Value::Array(h) = haystack else {
                return false;
            };
            n.len() == h.len() && n.iter().zip(h.iter()).all(|(a, b)| partial_match(a, b))
        }
        other => other == haystack,
    }
}

/// The capability a request verb requires, if any. Unknown verbs are not gated.
pub fn required_capability(ty: &str) -> Option<Capability> {
    Some(match ty {
        "hello" | "ping" | "status" | "create" | "start" | "stop" | "kill" | "remove" | "get"
        | "list" | "modify" | "drain" | "touch" => Capability::Lifecycle,
        "exec" | "exec_stream" | "shell" | "attach" | "attach_input" | "attach_resize"
        | "exec_input" | "exec_resize" => Capability::Exec,
        "wait" => Capability::Wait,
        "fs_read" | "fs_read_to_string" | "fs_write" | "fs_write_stream" | "fs_data"
        | "fs_list" | "fs_stat" | "fs_exists" | "fs_mkdir" | "fs_remove" | "fs_remove_dir"
        | "fs_copy" | "fs_rename" | "fs_copy_from_host" | "fs_copy_to_host" => Capability::Fs,
        "logs" => Capability::Logs,
        "metrics" | "metrics_stream" => Capability::Metrics,
        "volume_create" | "volume_list" | "volume_get" | "volume_remove" | "mount_add" => {
            Capability::Volumes
        }
        "secret_set" | "secret_list" | "secret_remove" => Capability::Secrets,
        "snapshot_create" | "snapshot_list" | "snapshot_restore" | "snapshot_fork"
        | "snapshot_remove" | "pause" | "resume" => Capability::Snapshots,
        _ => return None,
    })
}

/// Whether a response frame terminates a streamed exchange.
pub fn is_terminal_frame(frame: &Value) -> bool {
    let ty = frame.get("type").and_then(Value::as_str).unwrap_or("");
    if ty == "guest" {
        matches!(
            frame.get("event").and_then(Value::as_str),
            Some("exit") | Some("error")
        )
    } else if ty == "event" {
        frame.get("event").and_then(Value::as_str) == Some("done")
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_match_scalars_and_nested() {
        let req = json!({"type": "exec", "command": "echo", "args": ["hi"], "env": {"A": "1"}});
        assert!(partial_match(&json!({"type": "exec"}), &req));
        assert!(partial_match(
            &json!({"type": "exec", "command": "echo"}),
            &req
        ));
        assert!(partial_match(&json!({"env": {"A": "1"}}), &req));
        assert!(!partial_match(&json!({"command": "ls"}), &req));
        assert!(!partial_match(&json!({"env": {"B": "1"}}), &req));
        assert!(!partial_match(
            &json!({"type": "exec", "args": ["hi", "x"]}),
            &req
        ));
    }

    #[test]
    fn capability_mapping_covers_verb_groups() {
        assert_eq!(required_capability("create"), Some(Capability::Lifecycle));
        assert_eq!(required_capability("exec_stream"), Some(Capability::Exec));
        assert_eq!(required_capability("exec_input"), Some(Capability::Exec));
        assert_eq!(required_capability("exec_resize"), Some(Capability::Exec));
        assert_eq!(required_capability("wait"), Some(Capability::Wait));
        assert_eq!(required_capability("fs_write"), Some(Capability::Fs));
        assert_eq!(required_capability("logs"), Some(Capability::Logs));
        assert_eq!(
            required_capability("metrics_stream"),
            Some(Capability::Metrics)
        );
        assert_eq!(
            required_capability("volume_create"),
            Some(Capability::Volumes)
        );
        assert_eq!(required_capability("secret_set"), Some(Capability::Secrets));
        assert_eq!(
            required_capability("snapshot_fork"),
            Some(Capability::Snapshots)
        );
        assert_eq!(required_capability("pause"), Some(Capability::Snapshots));
        assert_eq!(required_capability("bogus_verb"), None);
    }

    #[test]
    fn terminal_frame_detection() {
        assert!(is_terminal_frame(
            &json!({"type": "guest", "event": "exit", "code": 0})
        ));
        assert!(is_terminal_frame(
            &json!({"type": "guest", "event": "error", "message": "x"})
        ));
        assert!(is_terminal_frame(
            &json!({"type": "event", "event": "done"})
        ));
        assert!(!is_terminal_frame(
            &json!({"type": "guest", "event": "stdout", "data": "a"})
        ));
        assert!(!is_terminal_frame(&json!({"type": "ok", "version": "1"})));
    }
}
