//! The `hello` handshake: version + capability negotiation.
//!
//! Every connection begins with a `hello`. The helper replies with the agreed
//! protocol version and the **capability list** it implements. Clients must
//! check capabilities before calling an optional verb — this is what makes
//! "don't ship a half-implemented verb set" (R1) enforceable.

use serde::{Deserialize, Serialize};

/// A protocol capability. Clients gate optional verbs on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// create/start/stop/kill/remove/get/list/modify/drain/touch/ping.
    Lifecycle,
    /// exec / exec_stream / shell / attach.
    Exec,
    /// fs/* verbs.
    Fs,
    /// logs (+ follow + cursor).
    Logs,
    /// metrics / metrics_stream.
    Metrics,
    /// volumes/* verbs.
    Volumes,
    /// secrets/* verbs.
    Secrets,
    /// snapshot/* + pause/resume.
    Snapshots,
    /// publish TCP/UDP ports + policy passthrough.
    Networking,
    /// labels + handles.
    Handles,
    /// ephemeral sandboxes.
    Ephemeral,
    /// kill with an explicit signal (SIGTERM/SIGKILL).
    Signal,
    /// blocking wait verb.
    Wait,
    /// exec_stream with a PTY + live stdin/resize frames.
    ExecPty,
    /// docker-in-VM configuration surface (Epic 7 D5).
    Docker,
    /// docker-in-VM bridge networking support (Epic 8 Tier 2).
    ///
    /// Advertised when the helper's deps bundle ships a libkrunfw kernel with
    /// the netfilter/bridge/NAT stack. Clients gate `docker.network: bridge`
    /// on this capability; without it they must use `host` or `none`.
    DockerBridge,
}

impl Capability {
    /// Every capability understood by this protocol version.
    pub fn all() -> &'static [Capability] {
        &[
            Capability::Lifecycle,
            Capability::Exec,
            Capability::Fs,
            Capability::Logs,
            Capability::Metrics,
            Capability::Volumes,
            Capability::Secrets,
            Capability::Snapshots,
            Capability::Networking,
            Capability::Handles,
            Capability::Ephemeral,
            Capability::Signal,
            Capability::Wait,
            Capability::ExecPty,
            Capability::Docker,
            Capability::DockerBridge,
        ]
    }

    /// Stable wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Lifecycle => "lifecycle",
            Capability::Exec => "exec",
            Capability::Fs => "fs",
            Capability::Logs => "logs",
            Capability::Metrics => "metrics",
            Capability::Volumes => "volumes",
            Capability::Secrets => "secrets",
            Capability::Snapshots => "snapshots",
            Capability::Networking => "networking",
            Capability::Handles => "handles",
            Capability::Ephemeral => "ephemeral",
            Capability::Signal => "signal",
            Capability::Wait => "wait",
            Capability::ExecPty => "exec_pty",
            Capability::Docker => "docker",
            Capability::DockerBridge => "docker_bridge",
        }
    }
}

/// Client -> helper: the opening frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloRequest {
    /// Client identity, e.g. `"nanosb-client"`, `"nanosb-sdk-python"`.
    pub client: String,
    /// Client version string (informational).
    #[serde(default)]
    pub sdk_version: String,
    /// Inclusive lower bound of protocol versions the client understands.
    pub protocol_min: String,
    /// Inclusive upper bound of protocol versions the client understands.
    pub protocol_max: String,
}

impl HelloRequest {
    pub fn new(client: impl Into<String>, protocol: &str) -> Self {
        Self {
            client: client.into(),
            sdk_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_min: protocol.to_string(),
            protocol_max: protocol.to_string(),
        }
    }

    /// Whether this client's range intersects a single server version.
    pub fn accepts(&self, server: &str) -> bool {
        version_in_range(server, &self.protocol_min, &self.protocol_max)
    }
}

/// Helper -> client: the negotiated result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResponse {
    /// The protocol version both sides will speak.
    pub protocol: String,
    /// Helper build version (informational).
    pub helper_version: String,
    /// Verbs the helper implements.
    pub capabilities: Vec<Capability>,
}

impl HelloResponse {
    pub fn supports(&self, cap: Capability) -> bool {
        self.capabilities.contains(&cap)
    }
}

/// Compare dotted numeric versions. Returns true when `v` is within
/// `[min, max]` inclusive. Non-numeric segments compare lexically.
pub fn version_in_range(v: &str, min: &str, max: &str) -> bool {
    cmp_version(v, min) != std::cmp::Ordering::Less
        && cmp_version(v, max) != std::cmp::Ordering::Greater
}

/// Compare two dotted versions segment by segment.
pub fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<&str> = a.split('.').collect();
    let pb: Vec<&str> = b.split('.').collect();
    for i in 0..pa.len().max(pb.len()) {
        let sa = pa.get(i).copied().unwrap_or("0");
        let sb = pb.get(i).copied().unwrap_or("0");
        let ord = match (sa.parse::<u64>(), sb.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => sa.cmp(sb),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn version_compare_is_numeric() {
        assert_eq!(cmp_version("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(cmp_version("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(cmp_version("1.0.0", "1.0.1"), Ordering::Less);
        assert_eq!(cmp_version("1", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn range_intersects() {
        assert!(version_in_range("1.0.0", "1.0.0", "1.2.0"));
        assert!(!version_in_range("1.3.0", "1.0.0", "1.2.0"));
        assert!(HelloRequest::new("c", "1.0.0").accepts("1.0.0"));
    }

    #[test]
    fn capability_all_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in Capability::all() {
            assert!(seen.insert(c.as_str()), "duplicate {}", c.as_str());
        }
    }

    #[test]
    fn capability_docker_as_str() {
        assert_eq!(Capability::Docker.as_str(), "docker");
        assert!(Capability::all().contains(&Capability::Docker));
    }

    #[test]
    fn capability_docker_bridge_as_str() {
        assert_eq!(Capability::DockerBridge.as_str(), "docker_bridge");
        assert!(Capability::all().contains(&Capability::DockerBridge));
    }

    #[test]
    fn capability_docker_bridge_serde_round_trips() {
        let json = serde_json::to_string(&Capability::DockerBridge).unwrap();
        assert_eq!(json, "\"docker_bridge\"");
        let back: Capability = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Capability::DockerBridge);
    }
}
