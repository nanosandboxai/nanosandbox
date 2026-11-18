//! Pre-flight validation for `/add <agent>`.
//!
//! Every supported agent needs credentials or provider/model configuration
//! before it can start. Without them the agent boots and immediately exits
//! with code 1 — for example Goose prints
//! `No provider configured. Run 'goose configure' first.` and exits 1, which
//! the TUI surfaces as `agent exited with code 1`.
//!
//! However, most modern coding agents support **interactive browser sign-in**
//! (OAuth / device flow). For those agents a missing environment variable is
//! *not* fatal: the agent can start and the TUI auto-opens the provider's
//! authorization URL in the host browser (see [`super::terminal`]'s auth-domain
//! whitelist). Blocking those agents would prevent the browser flow from ever
//! running.
//!
//! So validation has two severities:
//!
//! * [`ValidationResult::Advisory`] — the agent supports browser sign-in. The
//!   user gets an actionable hint but the agent is allowed to start and sign
//!   in through the browser.
//! * [`ValidationResult::Blocking`] — the agent has no interactive sign-in
//!   path. Missing credentials would leave a dead panel whose only symptom is
//!   an opaque exit code, so the add is rejected before the sandbox starts.
//!
//! This module validates the merged panel environment **before** the sandbox
//! is created, so the user gets an actionable message instead of a dead panel
//! whose only symptom is an opaque exit code.

use std::collections::HashMap;

/// A single prerequisite an agent needs before it can start.
#[derive(Debug, Clone, Copy)]
pub struct AgentRequirement {
    /// Human-readable name shown in the error (e.g. "Anthropic API key").
    pub label: &'static str,
    /// Environment variables that satisfy this requirement. Any one is enough.
    pub keys: &'static [&'static str],
    /// Whether the requirement is mandatory. A missing mandatory requirement
    /// blocks the agent from starting.
    pub mandatory: bool,
    /// Short, copy-pasteable hint on how to satisfy the requirement.
    pub hint: &'static str,
}

/// Result of validating an agent's prerequisites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationResult {
    /// The agent has every prerequisite it needs; it can start.
    Ok,
    /// The agent is missing prerequisites, but it supports interactive browser
    /// sign-in. It can start and the user signs in through the browser. The
    /// message describes what is missing and how to fix it.
    Advisory(String),
    /// The agent is missing mandatory prerequisites and has no interactive
    /// sign-in path. It cannot start. The message describes what is missing
    /// and how to fix it.
    Blocking(String),
}

impl ValidationResult {
    /// `true` when the agent may start (fully configured or advisory).
    pub fn can_start(&self) -> bool {
        !matches!(self, ValidationResult::Blocking(_))
    }

    /// The user-facing message, if any.
    pub fn message(&self) -> Option<&str> {
        match self {
            ValidationResult::Ok => None,
            ValidationResult::Advisory(msg) | ValidationResult::Blocking(msg) => Some(msg),
        }
    }
}

/// Built-in agents that support interactive browser sign-in.
///
/// These agents authenticate through an OAuth / device-code flow whose
/// authorization URL the TUI opens in the host browser, so a missing
/// credential is advisory rather than blocking.
///
/// Keep this in sync with the auth-domain whitelist in
/// [`super::terminal`]: every agent listed here must have its sign-in host
/// whitelisted there, otherwise the auto-open never fires and the advisory
/// hint cannot be acted on.
const BROWSER_LOGIN_AGENTS: &[&str] = &[
    "claude", "codex", "gemini", "cursor", "goose", "opencode", "crush", "aider",
];

/// Returns whether the agent supports interactive browser sign-in.
pub fn agent_supports_browser_login(agent: &str) -> bool {
    BROWSER_LOGIN_AGENTS
        .iter()
        .any(|a| a.eq_ignore_ascii_case(agent))
}

/// Provider keys Goose / OpenCode / Crush accept via the OpenAI-compatible
/// provider path.
const PROVIDER_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENROUTER_API_KEY",
];

/// Returns the requirements for an agent.
///
/// Agents not listed here (custom images, unknown names) have no mandatory
/// requirements, so validation is a no-op for them.
pub fn agent_requirements(agent: &str) -> &'static [AgentRequirement] {
    match agent {
        "claude" => &[AgentRequirement {
            label: "Anthropic API key",
            keys: &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
            mandatory: true,
            hint: "export ANTHROPIC_API_KEY=sk-ant-... (or CLAUDE_CODE_OAUTH_TOKEN=...) before launching the TUI, or sign in through the browser",
        }],
        "codex" => &[AgentRequirement {
            label: "OpenAI API key",
            keys: &["OPENAI_API_KEY", "CODEX_API_KEY"],
            mandatory: true,
            hint: "export OPENAI_API_KEY=sk-... (or CODEX_API_KEY=...) before launching the TUI, or sign in through the browser",
        }],
        "goose" => &[
            AgentRequirement {
                label: "Goose provider",
                keys: &["GOOSE_PROVIDER"],
                mandatory: true,
                hint: "export GOOSE_PROVIDER=anthropic (or openai, google, openrouter), or run 'goose configure'",
            },
            AgentRequirement {
                label: "Goose model",
                keys: &["GOOSE_MODEL"],
                mandatory: true,
                hint: "export GOOSE_MODEL=claude-sonnet-4-5-20250929 (or another model id), or run 'goose configure'",
            },
            AgentRequirement {
                label: "Provider API key",
                keys: PROVIDER_KEYS,
                mandatory: true,
                hint: "export ANTHROPIC_API_KEY=... or OPENAI_API_KEY=... matching GOOSE_PROVIDER, or sign in through the browser",
            },
        ],
        "gemini" => &[AgentRequirement {
            label: "Gemini API key",
            keys: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            mandatory: true,
            hint: "export GEMINI_API_KEY=... (or GOOGLE_API_KEY=...) before launching the TUI, or sign in through the browser",
        }],
        "cursor" => &[AgentRequirement {
            label: "Cursor API key",
            keys: &["CURSOR_API_KEY"],
            mandatory: true,
            hint: "export CURSOR_API_KEY=... before launching the TUI, or sign in through the browser",
        }],
        "opencode" => &[AgentRequirement {
            label: "Provider API key",
            keys: PROVIDER_KEYS,
            mandatory: true,
            hint: "export ANTHROPIC_API_KEY=... or OPENAI_API_KEY=... before launching the TUI, or sign in through the browser",
        }],
        "aider" => &[AgentRequirement {
            label: "Provider API key",
            keys: &[
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "GEMINI_API_KEY",
                "DEEPSEEK_API_KEY",
            ],
            mandatory: true,
            hint: "export ANTHROPIC_API_KEY=... or OPENAI_API_KEY=... before launching the TUI, or sign in through the browser",
        }],
        "crush" => &[AgentRequirement {
            label: "Provider API key",
            keys: PROVIDER_KEYS,
            mandatory: true,
            hint: "export ANTHROPIC_API_KEY=... or OPENAI_API_KEY=... before launching the TUI, or sign in through the browser",
        }],
        _ => &[],
    }
}

/// Validate an agent's merged environment against its requirements.
///
/// Returns [`ValidationResult::Ok`] when the agent can start, an
/// [`ValidationResult::Advisory`] when credentials are missing but the agent
/// supports browser sign-in (the agent may still start), or a
/// [`ValidationResult::Blocking`] when credentials are missing and the agent
/// cannot sign in interactively.
pub fn validate_agent(agent: &str, env: &HashMap<String, String>) -> ValidationResult {
    let reqs = agent_requirements(agent);
    let missing: Vec<&AgentRequirement> = reqs
        .iter()
        .filter(|r| r.mandatory && !r.keys.iter().any(|k| env.contains_key(*k)))
        .collect();
    if missing.is_empty() {
        return ValidationResult::Ok;
    }

    let msg = build_message(agent, &missing);
    if agent_supports_browser_login(agent) {
        ValidationResult::Advisory(msg)
    } else {
        ValidationResult::Blocking(msg)
    }
}

/// Build the multi-line, user-facing message for missing requirements.
fn build_message(agent: &str, missing: &[&AgentRequirement]) -> String {
    let mut lines = vec![format!("Agent '{}' is missing credentials:", agent)];
    for r in missing {
        lines.push(format!(
            "  - {} (set one of: {})",
            r.label,
            r.keys.join(", ")
        ));
    }
    lines.push(String::new());
    lines.push("How to fix:".to_string());
    for r in missing {
        lines.push(format!("  - {}", r.hint));
    }
    lines.push(String::new());
    lines.push("Or add the keys to a per-panel env file: /add <agent> --env-file .env".to_string());
    lines.push("Or set them in the TUI at runtime: /env ANTHROPIC_API_KEY=sk-ant-...".to_string());
    if agent_supports_browser_login(agent) {
        lines.push(
            "Or let the agent sign in through the browser — the TUI opens the auth URL automatically."
                .to_string(),
        );
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn advisory_message(result: ValidationResult) -> String {
        match result {
            ValidationResult::Advisory(msg) => msg,
            other => panic!("expected Advisory, got {other:?}"),
        }
    }

    #[test]
    fn unknown_agent_has_no_requirements() {
        assert!(agent_requirements("myagent").is_empty());
        assert_eq!(validate_agent("myagent", &env(&[])), ValidationResult::Ok);
    }

    #[test]
    fn all_builtin_agents_support_browser_login() {
        for agent in [
            "claude", "codex", "gemini", "cursor", "goose", "opencode", "crush", "aider",
        ] {
            assert!(
                agent_supports_browser_login(agent),
                "{agent} should support browser sign-in"
            );
        }
        assert!(!agent_supports_browser_login("myagent"));
    }

    #[test]
    fn claude_is_advisory_when_key_missing() {
        let msg = advisory_message(validate_agent("claude", &env(&[])));
        assert!(msg.contains("Anthropic API key"), "got: {msg}");
        assert!(msg.contains("ANTHROPIC_API_KEY"), "got: {msg}");
        assert!(msg.contains("browser"), "got: {msg}");

        // Either an API key or an OAuth token satisfies the requirement.
        assert_eq!(
            validate_agent("claude", &env(&[("ANTHROPIC_API_KEY", "sk")])),
            ValidationResult::Ok
        );
        assert_eq!(
            validate_agent("claude", &env(&[("CLAUDE_CODE_OAUTH_TOKEN", "t")])),
            ValidationResult::Ok
        );
    }

    #[test]
    fn codex_is_advisory_when_key_missing() {
        let msg = advisory_message(validate_agent("codex", &env(&[])));
        assert!(msg.contains("OpenAI API key"), "got: {msg}");
        assert_eq!(
            validate_agent("codex", &env(&[("OPENAI_API_KEY", "sk")])),
            ValidationResult::Ok
        );
        assert_eq!(
            validate_agent("codex", &env(&[("CODEX_API_KEY", "sk")])),
            ValidationResult::Ok
        );
    }

    #[test]
    fn goose_is_advisory_when_provider_model_and_key_missing() {
        // Nothing set: provider, model, and key are all reported.
        let msg = advisory_message(validate_agent("goose", &env(&[])));
        assert!(msg.contains("Goose provider"), "got: {msg}");
        assert!(msg.contains("Goose model"), "got: {msg}");
        assert!(msg.contains("Provider API key"), "got: {msg}");
        assert!(msg.contains("browser"), "got: {msg}");

        // Provider + model but no key: only the key is reported.
        let msg = advisory_message(validate_agent(
            "goose",
            &env(&[("GOOSE_PROVIDER", "anthropic"), ("GOOSE_MODEL", "m")]),
        ));
        assert!(!msg.contains("Goose provider"), "got: {msg}");
        assert!(!msg.contains("Goose model"), "got: {msg}");
        assert!(msg.contains("Provider API key"), "got: {msg}");

        // Everything set: passes.
        assert_eq!(
            validate_agent(
                "goose",
                &env(&[
                    ("GOOSE_PROVIDER", "anthropic"),
                    ("GOOSE_MODEL", "m"),
                    ("ANTHROPIC_API_KEY", "sk"),
                ]),
            ),
            ValidationResult::Ok
        );
    }

    #[test]
    fn gemini_is_advisory_when_key_missing() {
        let msg = advisory_message(validate_agent("gemini", &env(&[])));
        assert!(msg.contains("Gemini API key"), "got: {msg}");
        assert_eq!(
            validate_agent("gemini", &env(&[("GEMINI_API_KEY", "k")])),
            ValidationResult::Ok
        );
        assert_eq!(
            validate_agent("gemini", &env(&[("GOOGLE_API_KEY", "k")])),
            ValidationResult::Ok
        );
    }

    #[test]
    fn provider_agents_are_advisory_when_key_missing() {
        for agent in ["opencode", "crush", "aider"] {
            let msg = advisory_message(validate_agent(agent, &env(&[])));
            assert!(msg.contains("Provider API key"), "{agent} got: {msg}");
            assert!(msg.contains("browser"), "{agent} got: {msg}");
            // Each agent's own key set should satisfy it.
            for req in agent_requirements(agent) {
                for key in req.keys {
                    assert_eq!(
                        validate_agent(agent, &env(&[(key, "x")])),
                        ValidationResult::Ok,
                        "{agent} should accept {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn advisory_message_lists_every_missing_requirement_and_hints() {
        let msg = advisory_message(validate_agent("goose", &env(&[])));
        assert!(msg.contains("How to fix:"), "got: {msg}");
        assert!(msg.contains("--env-file"), "got: {msg}");
        assert!(msg.contains("/env ANTHROPIC_API_KEY"), "got: {msg}");
    }

    #[test]
    fn all_requirement_bearing_builtins_are_advisory() {
        // Every built-in agent that has requirements must be advisory (never
        // blocking), because all of them support browser sign-in.
        for agent in BROWSER_LOGIN_AGENTS {
            if agent_requirements(agent).is_empty() {
                continue;
            }
            let result = validate_agent(agent, &env(&[]));
            assert!(
                matches!(result, ValidationResult::Advisory(_)),
                "{agent} should be Advisory, got {result:?}"
            );
        }
    }

    #[test]
    fn can_start_contract() {
        assert!(ValidationResult::Ok.can_start());
        assert!(ValidationResult::Advisory("warn".into()).can_start());
        assert!(!ValidationResult::Blocking("err".into()).can_start());
        assert_eq!(ValidationResult::Ok.message(), None);
        assert_eq!(
            ValidationResult::Advisory("warn".into()).message(),
            Some("warn")
        );
    }
}
