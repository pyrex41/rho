use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Anthropic,
    #[serde(alias = "openai-compatible")]
    OpenAi,
    #[serde(alias = "xai-responses")]
    XaiResponses,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// User-facing ID (e.g. "gpt-4o", "claude-sonnet")
    pub id: String,
    pub provider: ProviderType,
    /// Wire model ID sent to the API
    pub model_id: String,
    /// Empty = use provider default
    #[serde(default)]
    pub base_url: String,
    /// Env var name holding the API key (e.g. "OPENAI_API_KEY")
    pub api_key_env: Option<String>,
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    /// Whether the model supports extended thinking
    #[serde(default)]
    pub thinking: bool,
    /// Provider-managed tools to inject into the request (e.g. xAI's "web_search", "x_search").
    /// These are not executed locally — they run on the provider's servers.
    #[serde(default)]
    pub server_tools: Option<Vec<String>>,
}

fn default_context_window() -> usize {
    200_000
}
fn default_max_tokens() -> usize {
    8_192
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsFile {
    #[serde(default, rename = "model")]
    pub models: Vec<ModelConfig>,
}

pub struct ModelRegistry {
    models: Vec<ModelConfig>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRegistry {
    /// Create registry with built-in Anthropic defaults.
    pub fn new() -> Self {
        Self {
            models: built_in_models(),
        }
    }

    /// Load from `~/.rho/models.toml`, merging with built-ins.
    /// User configs override built-ins by id.
    pub fn load() -> Self {
        let mut registry = Self::new();

        if let Some(home) = dirs::home_dir() {
            let path = home.join(".rho").join("models.toml");
            if path.is_file() {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match toml::from_str::<ModelsFile>(&content) {
                        Ok(file) => {
                            for user_model in file.models {
                                if let Some(existing) =
                                    registry.models.iter_mut().find(|m| m.id == user_model.id)
                                {
                                    *existing = user_model;
                                } else {
                                    registry.models.push(user_model);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse ~/.rho/models.toml: {}", e);
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Failed to read ~/.rho/models.toml: {}", e);
                    }
                }
            }
        }

        load_zen_models(&mut registry.models);

        registry
    }

    pub fn get(&self, id: &str) -> Option<&ModelConfig> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn list(&self) -> &[ModelConfig] {
        &self.models
    }

    /// Convert a `ModelConfig` to the runtime `Model` type.
    pub fn to_model(config: &ModelConfig) -> crate::types::Model {
        crate::types::Model {
            id: config.model_id.clone(),
            name: config.id.clone(),
            provider: match config.provider {
                ProviderType::Anthropic => "anthropic".into(),
                ProviderType::OpenAi => "openai".into(),
                ProviderType::XaiResponses => "xai-responses".into(),
            },
            base_url: config.base_url.clone(),
            reasoning: config.thinking,
            context_window: config.context_window,
            max_tokens: config.max_tokens,
        }
    }

    /// Resolve the API key for a model config.
    ///
    /// Resolution order:
    /// 1. `api_key_env` env var (if set and non-empty)
    /// 2. `anthropic-auth` keychain / OAuth (for Anthropic provider only)
    /// 3. `"local"` (for localhost/127.0.0.1 base URLs — e.g. Ollama)
    /// 4. Error
    pub fn resolve_api_key(config: &ModelConfig) -> Result<String, String> {
        // 1. Try designated env var
        if let Some(ref env_var) = config.api_key_env {
            if let Ok(val) = std::env::var(env_var) {
                if !val.is_empty() {
                    return Ok(val);
                }
            }
        }

        // 2. For Anthropic, try keychain / OAuth credentials
        if config.provider == ProviderType::Anthropic {
            if let Ok(token) = crate::auth::get_token() {
                return Ok(token);
            }
        }

        // 3. For localhost endpoints (Ollama, etc.), no auth needed
        if config.base_url.contains("localhost") || config.base_url.contains("127.0.0.1") {
            return Ok("local".into());
        }

        Err(format!(
            "No API key found for model '{}'. Set the {} environment variable.",
            config.id,
            config
                .api_key_env
                .as_deref()
                .unwrap_or("appropriate API key env var")
        ))
    }
}

/// Load Zen models into the registry (only if OPENCODE_ZEN_API_KEY is set).
fn load_zen_models(models: &mut Vec<ModelConfig>) {
    if std::env::var("OPENCODE_ZEN_API_KEY")
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
    {
        return;
    }

    let zen_ids = crate::zen::fetch_zen_models();
    for model_id in zen_ids {
        let registry_id = format!("zen-{}", model_id);
        // Skip if user already defined this ID
        if models.iter().any(|m| m.id == registry_id) {
            continue;
        }

        let (provider, base_url) = if model_id.contains("claude") {
            (ProviderType::Anthropic, "https://opencode.ai/zen".to_string())
        } else {
            (ProviderType::OpenAi, "https://opencode.ai/zen/v1".to_string())
        };

        models.push(ModelConfig {
            id: registry_id,
            provider,
            model_id: model_id.clone(),
            base_url,
            api_key_env: Some("OPENCODE_ZEN_API_KEY".into()),
            context_window: 200_000,
            max_tokens: 16_384,
            thinking: model_id.contains("opus"),
            server_tools: None,
        });
    }
}

fn built_in_models() -> Vec<ModelConfig> {
    vec![
        // Anthropic
        ModelConfig {
            id: "claude-sonnet".into(),
            provider: ProviderType::Anthropic,
            model_id: "claude-sonnet-4-6".into(),
            base_url: String::new(),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            context_window: 200_000,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
        },
        ModelConfig {
            id: "claude-opus".into(),
            provider: ProviderType::Anthropic,
            model_id: "claude-opus-4-6".into(),
            base_url: String::new(),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            context_window: 200_000,
            max_tokens: 8_192,
            thinking: true,
            server_tools: None,
        },
        ModelConfig {
            id: "claude-haiku".into(),
            provider: ProviderType::Anthropic,
            model_id: "claude-haiku-4-5-20251001".into(),
            base_url: String::new(),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            context_window: 200_000,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
        },
        // xAI (Grok) — OpenAI-compatible endpoint
        ModelConfig {
            id: "grok-3".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-3".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: false,
            server_tools: None,
        },
        ModelConfig {
            id: "grok-3-mini".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-3-mini".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
        },
        ModelConfig {
            id: "grok-2".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-2-1212".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 32_768,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
        },
        // xAI Grok 4.20 experimental beta
        ModelConfig {
            id: "grok-4.20-reasoning".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-4.20-experimental-beta-0304-reasoning".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: true,
            server_tools: None,
        },
        ModelConfig {
            id: "grok-4.20-non-reasoning".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-4.20-experimental-beta-0304-non-reasoning".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: false,
            server_tools: None,
        },
        ModelConfig {
            id: "grok-4.20-multi-agent".into(),
            provider: ProviderType::XaiResponses,
            model_id: "grok-4.20-multi-agent-experimental-beta-0304".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: false,
            server_tools: None,
        },
        // Additional xAI models
        ModelConfig {
            id: "grok-code-fast-1".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-code-fast-1".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: false,
            server_tools: None,
        },
        ModelConfig {
            id: "grok-4-1-reasoning".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-4-1-reasoning".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: true,
            server_tools: None,
        },
        ModelConfig {
            id: "grok-4.20-beta-0309-reasoning".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-4.20-beta-0309-reasoning".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: true,
            server_tools: None,
        },
        ModelConfig {
            id: "grok-4.20-multi-agent-beta-0309".into(),
            provider: ProviderType::XaiResponses,
            model_id: "grok-4.20-multi-agent-beta-0309".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: false,
            server_tools: None,
        },
        // OpenAI GPT models (latest as of 2026)
        ModelConfig {
            id: "gpt-5.4".into(),
            provider: ProviderType::OpenAi,
            model_id: "gpt-5.4".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            context_window: 1_000_000,
            max_tokens: 128_000,
            thinking: false,
            server_tools: None,
        },
        ModelConfig {
            id: "gpt-5.4-mini".into(),
            provider: ProviderType::OpenAi,
            model_id: "gpt-5.4-mini".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            context_window: 400_000,
            max_tokens: 128_000,
            thinking: false,
            server_tools: None,
        },
        ModelConfig {
            id: "gpt-5.4-nano".into(),
            provider: ProviderType::OpenAi,
            model_id: "gpt-5.4-nano".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            context_window: 400_000,
            max_tokens: 128_000,
            thinking: false,
            server_tools: None,
        },

    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_builtin_models() {
        let registry = ModelRegistry::new();
        // 3 Anthropic + 3 OpenAI + 10 xAI/Grok
        assert_eq!(registry.list().len(), 16);
    }

    #[test]
    fn builtin_grok_models_use_openai_provider() {
        let registry = ModelRegistry::new();
        let grok = registry.get("grok-3").unwrap();
        assert_eq!(grok.provider, ProviderType::OpenAi);
        assert_eq!(grok.base_url, "https://api.x.ai/v1");
        assert_eq!(grok.api_key_env.as_deref(), Some("XAI_API_KEY"));
        assert_eq!(grok.model_id, "grok-3");
    }

    #[test]
    fn get_builtin_model() {
        let registry = ModelRegistry::new();
        let m = registry.get("claude-sonnet").unwrap();
        assert_eq!(m.model_id, "claude-sonnet-4-6");
        assert_eq!(m.provider, ProviderType::Anthropic);
        assert!(!m.thinking);
    }

    #[test]
    fn get_claude_opus_has_thinking() {
        let registry = ModelRegistry::new();
        let m = registry.get("claude-opus").unwrap();
        assert!(m.thinking);
    }

    #[test]
    fn get_missing_returns_none() {
        let registry = ModelRegistry::new();
        assert!(registry.get("non-existent-model").is_none());
    }

    #[test]
    fn to_model_maps_fields() {
        let config = ModelConfig {
            id: "test-model".into(),
            provider: ProviderType::OpenAi,
            model_id: "gpt-4o".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            context_window: 128_000,
            max_tokens: 16_384,
            thinking: false,
            server_tools: None,
        };
        let model = ModelRegistry::to_model(&config);
        assert_eq!(model.id, "gpt-4o");
        assert_eq!(model.name, "test-model");
        assert_eq!(model.provider, "openai");
        assert_eq!(model.base_url, "https://api.openai.com/v1");
        assert_eq!(model.context_window, 128_000);
        assert_eq!(model.max_tokens, 16_384);
    }

    #[test]
    fn resolve_api_key_from_env() {
        let config = ModelConfig {
            id: "test".into(),
            provider: ProviderType::OpenAi,
            model_id: "gpt-4o".into(),
            base_url: String::new(),
            api_key_env: Some("__RHO_TEST_KEY__".into()),
            context_window: 128_000,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
        };
        std::env::set_var("__RHO_TEST_KEY__", "test-api-key-123");
        let key = ModelRegistry::resolve_api_key(&config).unwrap();
        assert_eq!(key, "test-api-key-123");
        std::env::remove_var("__RHO_TEST_KEY__");
    }

    #[test]
    fn resolve_api_key_localhost_returns_local() {
        let config = ModelConfig {
            id: "ollama".into(),
            provider: ProviderType::OpenAi,
            model_id: "llama3".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key_env: None,
            context_window: 128_000,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
        };
        let key = ModelRegistry::resolve_api_key(&config).unwrap();
        assert_eq!(key, "local");
    }

    #[test]
    fn load_merges_user_toml_override() {
        // This test only exercises the parsing logic, not file I/O
        let toml_str = r#"
[[model]]
id = "claude-sonnet"
provider = "anthropic"
model_id = "claude-sonnet-4-6-custom"
api_key_env = "ANTHROPIC_API_KEY"
context_window = 200000
max_tokens = 8192
"#;
        let file: ModelsFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.models.len(), 1);
        assert_eq!(file.models[0].model_id, "claude-sonnet-4-6-custom");
    }

    #[test]
    fn load_parses_openai_provider() {
        let toml_str = r#"
[[model]]
id = "gpt-4o"
provider = "openai"
model_id = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
context_window = 128000
max_tokens = 16384
"#;
        let file: ModelsFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.models[0].provider, ProviderType::OpenAi);
    }
}
