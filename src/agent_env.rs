//! Agent environment resolution shared by the TUI and the headless/slim deploy path.

use std::collections::HashMap;

/// Map a model id's `provider/model` prefix to a Goose provider name.
///
/// Known first-party prefixes map to their Goose provider; any other prefix
/// (e.g. `aihubmix/…`, `zai/…`) is an OpenRouter-hosted sub-provider, which
/// Goose reaches through its `openrouter` provider.
fn goose_provider_for_model(model: &str) -> Option<&'static str> {
    sandbox::providers::goose_provider(model)
}

/// Goose requires BOTH `GOOSE_PROVIDER` and `GOOSE_MODEL`: a provider alone
/// exits 1 with "No model configured", and neither exits 1 with "No provider
/// configured". Resolve them from (in order) the sandbox's requested model id,
/// then an available provider API key. No-op when the user already set
/// `GOOSE_PROVIDER`.
pub fn resolve_goose_env(env: &mut HashMap<String, String>, model: Option<&str>) {
    if env.contains_key("GOOSE_PROVIDER") {
        return;
    }

    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        let provider = goose_provider_for_model(model).or_else(|| {
            env.contains_key("OPENROUTER_API_KEY")
                .then_some("openrouter")
        });
        if let Some(provider) = provider {
            let goose_model = if provider == "openrouter" {
                model.to_string()
            } else {
                model
                    .split_once('/')
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| model.to_string())
            };
            env.insert("GOOSE_PROVIDER".to_string(), provider.to_string());
            env.entry("GOOSE_MODEL".to_string()).or_insert(goose_model);
            return;
        }
    }

    let fallback = if env.contains_key("ANTHROPIC_API_KEY") {
        ("anthropic", "claude-sonnet-4-5-20250929")
    } else if env.contains_key("OPENAI_API_KEY") {
        ("openai", "gpt-4o")
    } else if env.contains_key("OPENROUTER_API_KEY") {
        ("openrouter", "anthropic/claude-sonnet-4.5")
    } else if env.contains_key("GOOGLE_API_KEY") {
        ("google", "gemini-2.5-pro")
    } else {
        return;
    };
    env.insert("GOOSE_PROVIDER".to_string(), fallback.0.to_string());
    env.entry("GOOSE_MODEL".to_string())
        .or_insert_with(|| fallback.1.to_string());
}
