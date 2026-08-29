use std::{collections::BTreeMap, fs, path::Path};

use serde::Deserialize;
use thiserror::Error;

use crate::{ProviderSettings, provider::ProviderError, validate_provider_settings};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub providers: BTreeMap<String, ProviderSettings>,
}

impl Config {
    pub fn load(directory: &Path) -> Result<Self, ConfigError> {
        let path = directory.join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        Self::parse(&source).map_err(|error| ConfigError::Parse {
            path,
            source: error,
        })
    }

    pub fn parse(source: &str) -> Result<Self, ParseError> {
        let config: Self = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ParseError> {
        for (name, provider) in &self.providers {
            if name.trim().is_empty() {
                return Err(ParseError::EmptyProfileName);
            }
            validate_provider_settings(provider).map_err(|source| ParseError::InvalidProfile {
                name: name.clone(),
                source,
            })?;
        }
        Ok(())
    }

    pub fn resolve_provider_profile(&self, name: &str) -> Result<&ProviderSettings, ConfigError> {
        self.providers
            .get(name)
            .ok_or_else(|| ConfigError::ProfileNotFound {
                name: name.to_owned(),
                available: self.providers.keys().cloned().collect(),
            })
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        source: ParseError,
    },
    #[error(
        "provider profile '{name}' was not found; available profiles: {available}",
        available = format_available(available),
    )]
    ProfileNotFound {
        name: String,
        available: Vec<String>,
    },
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("{0}")]
    Toml(#[from] toml::de::Error),
    #[error("provider profile names cannot be empty")]
    EmptyProfileName,
    #[error("provider profile '{name}' is invalid: {source}")]
    InvalidProfile { name: String, source: ProviderError },
}

fn format_available(names: &[String]) -> String {
    if names.is_empty() {
        "(none configured)".to_owned()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_provider_profiles_from_config_toml() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("config.toml"),
            r#"
[providers.openrouter]
id = "openrouter"
base_url = "https://openrouter.ai"
chat_completions_path = "/api/v1/chat/completions"
timeout_seconds = 60
model = "anthropic/claude-3.5-sonnet"
stream = true

[providers.local]
id = "local"
base_url = "https://localhost:5001"
chat_completions_path = "/v1/chat/completions"
timeout_seconds = 30
model = "local-model"
stream = false
"#,
        )
        .unwrap();

        let config = Config::load(directory.path()).unwrap();
        assert_eq!(config.providers.len(), 2);
        assert_eq!(
            config.providers["openrouter"].model,
            "anthropic/claude-3.5-sonnet"
        );
        assert_eq!(config.providers["local"].base_url, "https://localhost:5001");
    }

    #[test]
    fn returns_default_when_config_toml_is_absent() {
        let directory = tempdir().unwrap();
        let config = Config::load(directory.path()).unwrap();
        assert!(config.providers.is_empty());
    }

    #[test]
    fn ignores_unknown_sections_like_tui() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("config.toml"),
            r#"
[tui]
theme = "dark"
toast_timeout = 3

[providers.test]
id = "test"
base_url = "https://example.com"
chat_completions_path = "/v1/chat/completions"
timeout_seconds = 30
model = "test-model"
stream = true
"#,
        )
        .unwrap();

        let config = Config::load(directory.path()).unwrap();
        assert_eq!(config.providers.len(), 1);
    }

    #[test]
    fn rejects_literal_secrets_in_provider_profiles() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("config.toml"),
            r#"
[providers.bad]
id = "bad"
base_url = "https://example.com"
chat_completions_path = "/v1/chat/completions"
timeout_seconds = 30
model = "model"
stream = true

[providers.bad.static_headers.Authorization]
source = "literal"
value = "Bearer sk-secret"
"#,
        )
        .unwrap();

        let error = Config::load(directory.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("provider profile 'bad' is invalid")
        );
    }

    #[test]
    fn resolve_provider_profile_returns_matching_profile() {
        let config = Config {
            providers: BTreeMap::from([(
                "my-provider".to_owned(),
                ProviderSettings {
                    id: "my-provider".to_owned(),
                    base_url: "https://example.com".to_owned(),
                    chat_completions_path: "/v1/chat/completions".to_owned(),
                    api_key_env: None,
                    static_headers: BTreeMap::new(),
                    timeout_seconds: 30,
                    ca_certificate_pem: None,
                    model: "model-x".to_owned(),
                    stream: true,
                },
            )]),
        };

        let profile = config.resolve_provider_profile("my-provider").unwrap();
        assert_eq!(profile.model, "model-x");
    }

    #[test]
    fn resolve_provider_profile_lists_available_on_miss() {
        let config = Config {
            providers: BTreeMap::from([
                (
                    "alpha".to_owned(),
                    ProviderSettings {
                        id: "alpha".to_owned(),
                        base_url: "https://alpha.example.com".to_owned(),
                        chat_completions_path: "/v1/chat/completions".to_owned(),
                        api_key_env: None,
                        static_headers: BTreeMap::new(),
                        timeout_seconds: 30,
                        ca_certificate_pem: None,
                        model: "m".to_owned(),
                        stream: true,
                    },
                ),
                (
                    "beta".to_owned(),
                    ProviderSettings {
                        id: "beta".to_owned(),
                        base_url: "https://beta.example.com".to_owned(),
                        chat_completions_path: "/v1/chat/completions".to_owned(),
                        api_key_env: None,
                        static_headers: BTreeMap::new(),
                        timeout_seconds: 30,
                        ca_certificate_pem: None,
                        model: "m".to_owned(),
                        stream: true,
                    },
                ),
            ]),
        };

        let error = config.resolve_provider_profile("nonexistent").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("nonexistent"));
        assert!(message.contains("alpha"));
        assert!(message.contains("beta"));
    }
}
