//! Model validation against a models config file.
//!
//! The models registry is loaded at runtime from a precedence chain: a user
//! override at `~/.nanosandbox/models.json` (or `.yaml`), then an installed or
//! sibling catalog, and finally a built-in catalog compiled into the binary.
//! The built-in fallback guarantees a registry is always available, so model
//! validation always runs.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
///
/// Loaded at runtime from a precedence chain ending in a built-in catalog
/// compiled into the binary, so [`load`](Self::load) always returns a registry.
pub struct ModelsRegistry {
    agents: HashMap<String, Vec<String>>,
}

impl ModelsRegistry {
    /// Default config path: `~/.nanosandbox/models.json`.
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".nanosandbox")
            .join("models.json")
    }

    /// Load the models registry from a precedence chain:
    ///
    /// 1. User override: `~/.nanosandbox/models.json`, then `~/.nanosandbox/models.yaml`.
    /// 2. Installed/sibling catalog: `~/.nanosandbox/registries/models/models.json`,
    ///    then `<cwd>/../registries/models/models.json` (if present).
    /// 3. Built-in catalog compiled into the binary (always available).
    ///
    /// Always returns `Some`.
    pub fn load() -> Option<Self> {
        let mut candidates = Vec::new();

        // 1. User override: JSON, then YAML in the same directory.
        let json_path = Self::default_path();
        candidates.push(json_path.clone());
        candidates.push(json_path.with_extension("yaml"));

        // 2. Installed/sibling catalog.
        if let Some(home) = dirs::home_dir() {
            candidates.push(
                home.join(".nanosandbox")
                    .join("registries")
                    .join("models")
                    .join("models.json"),
            );
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(
                cwd.join("..")
                    .join("registries")
                    .join("models")
                    .join("models.json"),
            );
        }

        Self::load_with(&candidates)
    }

    /// Try each candidate path in order, falling back to the built-in catalog.
    fn load_with(candidates: &[PathBuf]) -> Option<Self> {
        for path in candidates {
            if path.exists() {
                if let Some(registry) = Self::load_from(path) {
                    return Some(registry);
                }
            }
        }
        Self::from_json(include_str!("../../../../../registries/models/models.json"))
    }

    /// Load from a specific file path. Auto-detects JSON vs YAML by extension.
    pub fn load_from(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "json" => Self::from_json(&content),
            _ => Self::from_yaml(&content),
        }
    }

    /// Parse from a JSON string.
    pub fn from_json(content: &str) -> Option<Self> {
        let file: ModelsFile = serde_json::from_str(content).ok()?;
        Some(Self::from_parsed(file))
    }

    /// Parse from a YAML string.
    pub fn from_yaml(content: &str) -> Option<Self> {
        let file: ModelsFile = serde_yaml::from_str(content).ok()?;
        Some(Self::from_parsed(file))
    }

    fn from_parsed(file: ModelsFile) -> Self {
        Self {
            agents: file
                .agents
                .into_iter()
                .map(|(k, v)| (k, v.models))
                .collect(),
        }
    }

    /// Validate a model string for a given agent type.
    /// Returns `Ok(())` if the model is known, or `Err` with a message listing known models.
    pub fn validate(&self, agent_type: &str, model: &str) -> Result<(), String> {
        let models = match self.agents.get(agent_type) {
            Some(m) => m,
            None => return Ok(()), // Unknown agent type — skip validation
        };
        // An agent entry with an empty model list must not reject every model:
        // an empty catalog means "no constraint", not "no valid models".
        if models.is_empty() {
            return Ok(());
        }
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

    const TEST_JSON: &str = r#"{
        "version": 1,
        "agents": {
            "claude": {
                "models": ["claude-opus-4-6", "claude-sonnet-4-5-20250929"]
            },
            "codex": {
                "models": ["o4-mini", "gpt-4.1"]
            },
            "goose": {
                "models": ["claude-opus-4-6", "gpt-4.1"]
            },
            "cursor": {
                "models": ["auto", "sonnet-4.6"]
            }
        }
    }"#;

    const TEST_YAML: &str = r#"
version: 1
agents:
  claude:
    models:
      - claude-opus-4-6
      - claude-sonnet-4-5-20250929
  codex:
    models:
      - o4-mini
"#;

    fn test_registry() -> ModelsRegistry {
        ModelsRegistry::from_json(TEST_JSON).expect("test JSON should parse")
    }

    #[test]
    fn test_from_json() {
        let registry = ModelsRegistry::from_json(TEST_JSON).unwrap();
        assert!(registry.agents.contains_key("claude"));
        assert!(registry.agents.contains_key("codex"));
        assert!(registry.agents.contains_key("goose"));
        assert!(registry.agents.contains_key("cursor"));
    }

    #[test]
    fn test_from_yaml() {
        let registry = ModelsRegistry::from_yaml(TEST_YAML).unwrap();
        assert!(registry.agents.contains_key("claude"));
        assert!(registry.agents.contains_key("codex"));
    }

    #[test]
    fn test_validate_known_model() {
        let registry = test_registry();
        assert!(registry
            .validate("claude", "claude-sonnet-4-5-20250929")
            .is_ok());
        assert!(registry.validate("codex", "o4-mini").is_ok());
    }

    #[test]
    fn test_validate_unknown_model() {
        let registry = test_registry();
        let result = registry.validate("claude", "nonexistent-model");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("nonexistent-model"));
        assert!(err.contains("claude-sonnet-4-5-20250929"));
    }

    #[test]
    fn test_validate_unknown_agent_type_skips() {
        let registry = test_registry();
        assert!(registry.validate("unknown-agent", "any-model").is_ok());
    }

    #[test]
    fn test_validate_empty_model_list_skips() {
        // An agent entry with an empty model list must not reject every model.
        let registry =
            ModelsRegistry::from_json(r#"{"version":1,"agents":{"oh-my-codex":{"models":[]}}}"#)
                .expect("should parse");
        assert!(registry.validate("oh-my-codex", "any-model").is_ok());
    }

    #[test]
    fn test_models_for() {
        let registry = test_registry();
        let claude_models = registry.models_for("claude");
        assert!(!claude_models.is_empty());
        assert!(claude_models.contains(&"claude-sonnet-4-5-20250929".to_string()));
    }

    #[test]
    fn test_models_for_unknown_agent() {
        let registry = test_registry();
        let models = registry.models_for("unknown");
        assert!(models.is_empty());
    }

    #[test]
    fn test_load_from_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.json");
        std::fs::write(&path, TEST_JSON).unwrap();

        let registry = ModelsRegistry::load_from(&path).unwrap();
        assert!(registry.agents.contains_key("claude"));
    }

    #[test]
    fn test_load_from_yaml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models.yaml");
        std::fs::write(&path, TEST_YAML).unwrap();

        let registry = ModelsRegistry::load_from(&path).unwrap();
        assert!(registry.agents.contains_key("claude"));
    }

    #[test]
    fn test_load_from_missing_file() {
        let result = ModelsRegistry::load_from(Path::new("/tmp/nonexistent_nanosb_models.json"));
        assert!(result.is_none());
    }

    #[test]
    fn test_default_path() {
        let path = ModelsRegistry::default_path();
        let s = path.to_string_lossy();
        assert!(s.ends_with("models.json"));
        assert!(s.contains(".nanosandbox"));
    }

    #[test]
    fn test_from_invalid_json() {
        assert!(ModelsRegistry::from_json("not json").is_none());
    }

    #[test]
    fn test_from_invalid_yaml() {
        assert!(ModelsRegistry::from_yaml(":::invalid").is_none());
    }

    #[test]
    fn test_load_always_returns_builtin() {
        let registry = ModelsRegistry::load().expect("built-in catalog should always load");
        assert!(registry.agents.contains_key("claude"));
    }

    #[test]
    fn test_builtin_catalog_validates_known_model() {
        let registry = ModelsRegistry::load().expect("built-in catalog should always load");
        assert!(registry
            .validate("claude", "claude-sonnet-4-5-20250929")
            .is_ok());
    }

    #[test]
    fn test_builtin_catalog_rejects_unknown_model() {
        let registry = ModelsRegistry::load().expect("built-in catalog should always load");
        assert!(registry.validate("claude", "gpt-nonexistent").is_err());
    }

    #[test]
    fn test_unknown_agent_type_still_skips() {
        let registry = ModelsRegistry::load().expect("built-in catalog should always load");
        assert!(registry.validate("not-an-agent", "whatever").is_ok());
    }

    #[test]
    fn test_load_with_prefers_first_existing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        std::fs::write(
            &first,
            r#"{"version":1,"agents":{"claude":{"models":["first-model"]}}}"#,
        )
        .unwrap();
        std::fs::write(
            &second,
            r#"{"version":1,"agents":{"claude":{"models":["second-model"]}}}"#,
        )
        .unwrap();

        let registry = ModelsRegistry::load_with(&[first, second]).unwrap();
        assert!(registry.validate("claude", "first-model").is_ok());
        assert!(registry.validate("claude", "second-model").is_err());
    }
}
