//! Provider resolution against the provider catalog.
//!
//! The provider catalog (`registries/models/provider-catalog.json`) is the
//! source of truth for how each coding agent maps a model id to a provider and
//! which environment keys that provider needs. It is compiled into the binary
//! via [`include_str!`] (same pattern as [`crate::config::models`]) and parsed
//! once on first use behind a [`OnceLock`].

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
struct ProviderCatalog {
    #[allow(dead_code)]
    version: u32,
    canonical: HashMap<String, String>,
    credentials: HashMap<String, Vec<String>>,
    agents: HashMap<String, AgentRule>,
}

#[derive(Debug, Deserialize)]
struct AgentRule {
    mode: String,
    case_sensitive: bool,
    default: Option<String>,
    alt: Vec<String>,
    collapse: HashMap<String, String>,
    collapse_default: Option<String>,
    overrides: HashMap<String, Vec<String>>,
    no_env: Vec<String>,
    #[allow(dead_code)]
    extra_keys: Vec<String>,
    providers: Vec<String>,
    accepted_keys: Vec<String>,
}

static CATALOG: OnceLock<ProviderCatalog> = OnceLock::new();

fn catalog() -> &'static ProviderCatalog {
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../../registries/models/provider-catalog.json"
        ))
        .expect("embedded provider catalog must parse")
    })
}

/// The providers an agent can be credentialed against.
pub fn providers_for(agent: &str) -> Vec<String> {
    catalog()
        .agents
        .get(agent)
        .map(|r| r.providers.clone())
        .unwrap_or_default()
}

/// The full set of environment keys an agent accepts.
pub fn accepted_keys(agent: &str) -> Vec<String> {
    catalog()
        .agents
        .get(agent)
        .map(|r| r.accepted_keys.clone())
        .unwrap_or_default()
}

/// The environment keys required to credential a provider.
pub fn credentials(provider: &str) -> Vec<String> {
    catalog()
        .credentials
        .get(provider)
        .cloned()
        .unwrap_or_default()
}

/// Normalize a model segment: lowercase and strip any `:*` suffix.
fn normalize_segment(segment: &str) -> String {
    let lower = segment.to_lowercase();
    match lower.split_once(':') {
        Some((head, _)) => head.to_string(),
        None => lower,
    }
}

/// Map a model segment to its canonical provider name.
///
/// Lowercases the segment, strips a `:*` suffix, looks it up in the canonical
/// map, and falls back to the normalized segment when unknown.
pub fn canonical(segment: &str) -> String {
    let norm = normalize_segment(segment);
    catalog().canonical.get(&norm).cloned().unwrap_or(norm)
}

/// The outcome of resolving a model id for an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The provider the model maps to, if any.
    pub provider: Option<String>,
    /// The environment keys needed to credential that provider.
    pub keys: Vec<String>,
    /// Whether the provider is OAuth-only (no env key required).
    pub oauth: bool,
}

/// Resolve a model id for an agent against the provider catalog.
///
/// Implements the catalog's resolution algorithm exactly (see the catalog
/// schema). Returns `None` when the agent is unknown or the model cannot be
/// resolved to a credentialable provider.
pub fn resolve(agent: &str, model: &str) -> Option<Resolved> {
    let cat = catalog();
    let rule = cat.agents.get(agent)?;
    let has_slash = model.contains('/');
    let seg = model.split('/').next().unwrap_or("");

    // Bare model (no `/`): use the agent's default provider.
    if !has_slash {
        let p = if rule.mode == "collapse" {
            rule.collapse_default.clone()
        } else {
            rule.default.clone()
        };
        return match p {
            Some(p) => {
                let mut keys = cat.credentials.get(&p).cloned().unwrap_or_default();
                keys.extend(rule.alt.iter().cloned());
                let oauth = cat.credentials.contains_key(&p) && cat.credentials[&p].is_empty();
                Some(Resolved {
                    provider: Some(p),
                    keys,
                    oauth,
                })
            }
            None => Some(Resolved {
                provider: None,
                keys: rule.alt.clone(),
                oauth: false,
            }),
        };
    }

    // Collapse mode: map the first segment through the collapse table.
    if rule.mode == "collapse" {
        let key = if rule.case_sensitive {
            seg.to_string()
        } else {
            seg.to_lowercase()
        };
        let p = rule
            .collapse
            .get(&key)
            .cloned()
            .or(rule.collapse_default.clone());
        return p.map(|p| {
            let keys = cat.credentials.get(&p).cloned().unwrap_or_default();
            let oauth = cat.credentials.contains_key(&p) && cat.credentials[&p].is_empty();
            Resolved {
                provider: Some(p),
                keys,
                oauth,
            }
        });
    }

    // Derived / single mode with a prefixed model.
    let canon = canonical(seg);
    let override_key = if rule.case_sensitive {
        seg.to_string()
    } else {
        seg.to_lowercase()
    };
    if let Some(keys) = rule.overrides.get(&override_key) {
        return Some(Resolved {
            provider: Some(canon.clone()),
            keys: keys.clone(),
            oauth: false,
        });
    }
    if let Some(keys) = rule.overrides.get(&canon) {
        return Some(Resolved {
            provider: Some(canon.clone()),
            keys: keys.clone(),
            oauth: false,
        });
    }
    if rule.no_env.contains(&canon) {
        return Some(Resolved {
            provider: Some(canon.clone()),
            keys: vec![],
            oauth: true,
        });
    }
    if rule.mode == "derived" || rule.providers.contains(&canon) {
        if cat.credentials.contains_key(&canon) || rule.no_env.contains(&canon) {
            let keys = cat.credentials.get(&canon).cloned().unwrap_or_default();
            let oauth = cat.credentials.contains_key(&canon) && cat.credentials[&canon].is_empty();
            return Some(Resolved {
                provider: Some(canon.clone()),
                keys,
                oauth,
            });
        }
        return None;
    }
    None
}

/// Map a model id's first segment to a Goose provider name.
///
/// Case-sensitive, reads the goose rule's collapse table from the catalog
/// (NOT hardcoded). Returns `None` for unknown prefixes, mirroring the legacy
/// `goose_provider_for_model` behavior.
pub fn goose_provider(model: &str) -> Option<&'static str> {
    let cat = catalog();
    let rule = cat.agents.get("goose")?;
    let seg = model.split('/').next().unwrap_or("");
    let key = if rule.case_sensitive {
        seg.to_string()
    } else {
        seg.to_lowercase()
    };
    let p = rule.collapse.get(&key)?;
    Some(p.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> Vec<&'static str> {
        crate::config::AgentType::ALL
            .iter()
            .map(|a| a.as_str())
            .collect()
    }

    /// Union of keys from `nanosb_cli::validation::agent_requirements(agent)`,
    /// mirrored here because the `sandbox` crate cannot depend on `nanosb-cli`.
    fn req_keys(agent: &str) -> Vec<&'static str> {
        match agent {
            "claude" => vec!["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
            "codex" => vec!["OPENAI_API_KEY", "CODEX_API_KEY"],
            "goose" => vec![
                "GOOSE_PROVIDER",
                "GOOSE_MODEL",
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "GOOGLE_API_KEY",
                "OPENROUTER_API_KEY",
            ],
            "gemini" => vec!["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            "cursor" => vec!["CURSOR_API_KEY"],
            "opencode" => vec![
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "OPENROUTER_API_KEY",
                "GOOGLE_API_KEY",
                "HF_TOKEN",
                "OPENCODE_API_KEY",
                "BAKERYHUB_API_KEY",
            ],
            "aider" => vec![
                "ANTHROPIC_API_KEY",
                "OPENAI_API_KEY",
                "GEMINI_API_KEY",
                "DEEPSEEK_API_KEY",
                "OPENROUTER_API_KEY",
                "GROQ_API_KEY",
                "FIREWORKS_API_KEY",
                "AZURE_API_KEY",
                "XAI_API_KEY",
                "COHERE_API_KEY",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "GOOGLE_APPLICATION_CREDENTIALS",
            ],
            "crush" => vec![
                "AIHUBMIX_API_KEY",
                "VERCEL_API_KEY",
                "OPENROUTER_API_KEY",
                "GOOGLE_API_KEY",
                "VENICE_API_KEY",
                "CORTECS_API_KEY",
                "OPENCODE_API_KEY",
                "IONET_API_KEY",
                "OPENAI_API_KEY",
                "ATLASCLOUD_API_KEY",
                "FIREWORKS_API_KEY",
                "NEBIUS_API_KEY",
                "ALIBABA_SINGAPORE_API_KEY",
                "NEURALWATT_API_KEY",
                "AVIAN_API_KEY",
                "CHUTES_API_KEY",
                "BASETEN_API_KEY",
                "ANTHROPIC_API_KEY",
                "QINIUCLOUD_API_KEY",
                "AZURE_OPENAI_API_KEY",
                "SYNTHETIC_API_KEY",
                "ZAI_API_KEY",
                "ZHIPU_API_KEY",
                "GEMINI_API_KEY",
                "SCW_SECRET_KEY",
                "ALIBABA_US_API_KEY",
                "MOONSHOT_API_KEY",
                "MINIMAX_API_KEY",
                "XAI_API_KEY",
                "HF_TOKEN",
                "KIMI_CODING_API_KEY",
                "PIONEER_API_KEY",
                "CEREBRAS_API_KEY",
                "DEEPSEEK_API_KEY",
                "GROQ_API_KEY",
                "PARETO_API_KEY",
                "CORALBRICKS_API_KEY",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "GOOGLE_APPLICATION_CREDENTIALS",
            ],
            "oh-my-openagent" => vec!["OPENAI_API_KEY", "ANTHROPIC_API_KEY"],
            "oh-my-codex" => vec!["OPENAI_API_KEY", "CODEX_API_KEY"],
            "oh-my-claude" => vec!["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
            _ => vec![],
        }
    }

    /// Keys from `nanosb_cli`'s `required_api_keys(agent)` (run.rs), mirrored
    /// here for the same dependency-direction reason.
    fn required_api_keys(agent: &str) -> Vec<&'static str> {
        match agent {
            "claude" => vec!["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
            "codex" => vec!["OPENAI_API_KEY", "CODEX_API_KEY"],
            "goose" => vec![
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "GOOGLE_API_KEY",
                "OPENROUTER_API_KEY",
            ],
            "opencode" => vec![
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "GOOGLE_API_KEY",
                "OPENROUTER_API_KEY",
            ],
            "aider" => vec![
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "GEMINI_API_KEY",
                "DEEPSEEK_API_KEY",
            ],
            "gemini" => vec!["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            "crush" => vec![
                "OPENAI_API_KEY",
                "ANTHROPIC_API_KEY",
                "GOOGLE_API_KEY",
                "OPENROUTER_API_KEY",
            ],
            "cursor" => vec!["CURSOR_API_KEY"],
            "oh-my-openagent" => vec!["OPENAI_API_KEY", "ANTHROPIC_API_KEY"],
            "oh-my-codex" => vec!["OPENAI_API_KEY", "CODEX_API_KEY"],
            "oh-my-claude" => vec!["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
            _ => vec![],
        }
    }

    #[test]
    fn accepted_keys_agree_with_agent_requirements() {
        for agent in agents() {
            let accepted = accepted_keys(agent);
            let req = req_keys(agent);
            // req - accepted ⊆ {GOOSE_PROVIDER, GOOSE_MODEL}
            for k in &req {
                if !accepted.contains(&k.to_string()) {
                    assert!(
                        *k == "GOOSE_PROVIDER" || *k == "GOOSE_MODEL",
                        "agent {agent}: req key {k} not in accepted"
                    );
                }
            }
            // accepted ⊆ req
            for k in &accepted {
                assert!(
                    req.contains(&k.as_str()),
                    "agent {agent}: accepted key {k} not in req"
                );
            }
        }
    }

    #[test]
    fn every_provider_resolves() {
        for agent in agents() {
            let no_env = catalog().agents[agent].no_env.clone();
            for p in providers_for(agent) {
                let creds = credentials(&p);
                assert!(
                    !creds.is_empty() || no_env.contains(&p),
                    "agent {agent}: provider {p} has no credentials and is not in no_env"
                );
            }
        }
    }

    #[test]
    fn every_prefixed_model_resolves() {
        let registry = crate::config::models::ModelsRegistry::load()
            .expect("built-in models catalog should load");
        for agent in agents() {
            for m in registry.models_for(agent) {
                if !m.contains('/') {
                    continue;
                }
                let r = resolve(agent, m)
                    .unwrap_or_else(|| panic!("agent {agent}: prefixed model {m} did not resolve"));
                assert!(
                    !r.keys.is_empty() || r.oauth,
                    "agent {agent}: prefixed model {m} resolved to empty non-oauth credential"
                );
            }
        }
    }

    #[test]
    fn goose_case_sensitivity() {
        assert_eq!(
            resolve("goose", "anthropic/x").unwrap().provider.as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            resolve("goose", "Anthropic/x").unwrap().provider.as_deref(),
            Some("openrouter")
        );
    }

    #[test]
    fn required_api_keys_subset_of_accepted() {
        for agent in agents() {
            let accepted = accepted_keys(agent);
            for k in required_api_keys(agent) {
                assert!(
                    accepted.contains(&k.to_string()),
                    "agent {agent}: required key {k} not in accepted"
                );
            }
        }
    }

    #[test]
    fn goose_provider_matches_legacy_semantics() {
        assert_eq!(goose_provider("anthropic/x"), Some("anthropic"));
        assert_eq!(goose_provider("openai/x"), Some("openai"));
        assert_eq!(goose_provider("google/x"), Some("google"));
        assert_eq!(goose_provider("gemini/x"), Some("google"));
        assert_eq!(goose_provider("openrouter/x"), Some("openrouter"));
        assert_eq!(goose_provider("unknown/x"), None);
        // Case-sensitive: uppercase prefix is not in the collapse table.
        assert_eq!(goose_provider("Anthropic/x"), None);
    }
}
