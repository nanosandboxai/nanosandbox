//! Model validation against models.yaml.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct ModelsFile {
    #[allow(dead_code)]
    version: u32,
    agents: HashMap<String, AgentModels>,
}

#[derive(Debug, Deserialize)]
struct AgentModels {
    models: Vec<String>,
}

/// Registry of known model identifiers per agent type.
/// Loaded from models.yaml (embedded at compile time).
pub struct ModelsRegistry {
    agents: HashMap<String, Vec<String>>,
}

impl ModelsRegistry {
    /// Load the models registry from the embedded models.yaml.
    pub fn load() -> Option<Self> {
        let content = include_str!("../../models.yaml");
        let file: ModelsFile = serde_yaml::from_str(content).ok()?;
        Some(Self {
            agents: file
                .agents
                .into_iter()
                .map(|(k, v)| (k, v.models))
                .collect(),
        })
    }

    /// Validate a model string for a given agent type.
    /// Returns `Ok(())` if the model is known, or `Err` with a message listing known models.
    pub fn validate(&self, agent_type: &str, model: &str) -> Result<(), String> {
        let models = match self.agents.get(agent_type) {
            Some(m) => m,
            None => return Ok(()), // Unknown agent type — skip validation
        };
        if models.iter().any(|m| m == model) {
            Ok(())
        } else {
            Err(format!(
                "unknown model '{}' for agent type '{}'. Known models: {}",
                model,
                agent_type,
                models.join(", ")
            ))
        }
    }

    /// Get all known models for an agent type (for autocomplete).
    pub fn models_for(&self, agent_type: &str) -> &[String] {
        self.agents
            .get(agent_type)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_models_registry() {
        let registry = ModelsRegistry::load().expect("should load models.yaml");
        assert!(!registry.agents.is_empty());
        assert!(registry.agents.contains_key("claude"));
        assert!(registry.agents.contains_key("codex"));
        assert!(registry.agents.contains_key("goose"));
        assert!(registry.agents.contains_key("cursor"));
    }

    #[test]
    fn test_validate_known_model() {
        let registry = ModelsRegistry::load().unwrap();
        assert!(registry.validate("claude", "claude-sonnet-4-5-20250929").is_ok());
        assert!(registry.validate("codex", "o4-mini").is_ok());
    }

    #[test]
    fn test_validate_unknown_model() {
        let registry = ModelsRegistry::load().unwrap();
        let result = registry.validate("claude", "nonexistent-model");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("nonexistent-model"));
        assert!(err.contains("claude-sonnet-4-5-20250929"));
    }

    #[test]
    fn test_validate_unknown_agent_type_skips() {
        let registry = ModelsRegistry::load().unwrap();
        assert!(registry.validate("unknown-agent", "any-model").is_ok());
    }

    #[test]
    fn test_models_for() {
        let registry = ModelsRegistry::load().unwrap();
        let claude_models = registry.models_for("claude");
        assert!(!claude_models.is_empty());
        assert!(claude_models.contains(&"claude-sonnet-4-5-20250929".to_string()));
    }

    #[test]
    fn test_models_for_unknown_agent() {
        let registry = ModelsRegistry::load().unwrap();
        let models = registry.models_for("unknown");
        assert!(models.is_empty());
    }
}
