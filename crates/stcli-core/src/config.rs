use std::{collections::BTreeMap, fs, path::Path, str::FromStr};

use serde::{Deserialize, Serialize};
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

    pub fn add_provider_profile(
        directory: &Path,
        name: &str,
        settings: ProviderSettings,
    ) -> Result<(), ConfigError> {
        if name.trim().is_empty() {
            return Err(ConfigError::InvalidProfile {
                name: name.to_owned(),
                source: ParseError::EmptyProfileName,
            });
        }
        validate_provider_settings(&settings).map_err(|source| ConfigError::InvalidProfile {
            name: name.to_owned(),
            source: ParseError::InvalidProfile {
                name: name.to_owned(),
                source,
            },
        })?;
        fs::create_dir_all(directory).map_err(|source| ConfigError::Write {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = directory.join("config.toml");
        let source = if path.exists() {
            fs::read_to_string(&path).map_err(|source| ConfigError::Read {
                path: path.clone(),
                source,
            })?
        } else {
            String::new()
        };
        let mut document =
            toml_edit::DocumentMut::from_str(&source).map_err(|source| ConfigError::Edit {
                path: path.clone(),
                source,
            })?;
        if !document.contains_key("providers") {
            document["providers"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let providers = document["providers"]
            .as_table_mut()
            .ok_or_else(|| ConfigError::ProvidersNotTable { path: path.clone() })?;
        let toml_str = toml::to_string(&settings).map_err(ConfigError::Serialize)?;
        let item_doc = toml_str
            .parse::<toml_edit::DocumentMut>()
            .map_err(|source| ConfigError::Edit {
                path: path.clone(),
                source,
            })?;
        update_provider_table(
            providers,
            name,
            toml_edit::Item::Table(item_doc.as_table().clone()),
        );
        fs::write(&path, document.to_string()).map_err(|source| ConfigError::Write { path, source })
    }

    pub fn remove_provider_profile(directory: &Path, name: &str) -> Result<bool, ConfigError> {
        let path = directory.join("config.toml");
        if !path.exists() {
            return Ok(false);
        }
        let source = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let mut document =
            toml_edit::DocumentMut::from_str(&source).map_err(|source| ConfigError::Edit {
                path: path.clone(),
                source,
            })?;
        let removed =
            if let Some(providers) = document.get_mut("providers").and_then(|p| p.as_table_mut()) {
                providers.remove(name).is_some()
            } else {
                false
            };
        if removed {
            fs::write(&path, document.to_string())
                .map_err(|source| ConfigError::Write { path, source })?;
        }
        Ok(removed)
    }

    pub fn load_provider_templates(
        directory: &Path,
    ) -> Result<BTreeMap<String, ProviderTemplate>, ConfigError> {
        let path = directory.join("provider-templates.toml");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let source = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        toml::from_str(&source).map_err(|source| ConfigError::Parse {
            path,
            source: ParseError::Toml(source),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderTemplate {
    pub name: String,
    pub id: String,
    pub base_url: String,
    pub chat_completions_path: String,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub default_model: String,
    #[serde(default = "default_stream")]
    pub stream: bool,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_stream() -> bool {
    true
}

fn default_timeout() -> u64 {
    120
}

const PROVIDER_FIELDS: &[&str] = &[
    "id",
    "base_url",
    "chat_completions_path",
    "api_key_env",
    "static_headers",
    "timeout_seconds",
    "ca_certificate_pem",
    "model",
    "stream",
];

fn update_provider_table(providers: &mut toml_edit::Table, name: &str, desired: toml_edit::Item) {
    let Some(existing) = providers.get_mut(name) else {
        providers.insert(name, desired);
        return;
    };
    let (Some(existing), Some(desired)) = (existing.as_table_mut(), desired.as_table()) else {
        providers.insert(name, desired);
        return;
    };
    for key in PROVIDER_FIELDS {
        if !desired.contains_key(key) {
            existing.remove(key);
        }
    }
    merge_table(existing, desired);
}

fn merge_table(existing: &mut toml_edit::Table, desired: &toml_edit::Table) {
    for (key, desired_item) in desired {
        if let Some(existing_item) = existing.get_mut(key) {
            merge_item(existing_item, desired_item.clone());
        } else {
            existing.insert(key, desired_item.clone());
        }
    }
}

fn merge_item(existing: &mut toml_edit::Item, desired: toml_edit::Item) {
    match (existing, desired) {
        (toml_edit::Item::Value(existing), toml_edit::Item::Value(mut desired)) => {
            *desired.decor_mut() = existing.decor().clone();
            *existing = desired;
        }
        (toml_edit::Item::Table(existing), toml_edit::Item::Table(desired)) => {
            let removed = existing
                .iter()
                .filter(|(key, _)| !desired.contains_key(key))
                .map(|(key, _)| key.to_owned())
                .collect::<Vec<_>>();
            for key in removed {
                existing.remove(&key);
            }
            merge_table(existing, &desired);
        }
        (existing, desired) => *existing = desired,
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
    #[error("provider profile '{name}' is invalid: {source}")]
    InvalidProfile { name: String, source: ParseError },
    #[error("failed to edit {path}: {source}")]
    Edit {
        path: std::path::PathBuf,
        source: toml_edit::TomlError,
    },
    #[error("failed to serialize provider profile: {0}")]
    Serialize(toml::ser::Error),
    #[error("the providers entry in {path} must be a table")]
    ProvidersNotTable { path: std::path::PathBuf },
    #[error("failed to write {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        source: std::io::Error,
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
                    format_mode: Default::default(),
                    completions_path: None,
                    instruct_template: None,
                    context_formatting: None,
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
                        format_mode: Default::default(),
                        completions_path: None,
                        instruct_template: None,
                        context_formatting: None,
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
                        format_mode: Default::default(),
                        completions_path: None,
                        instruct_template: None,
                        context_formatting: None,
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
    #[test]
    fn add_provider_profile_preserves_unmanaged_configuration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# keep this comment\n[tui]\ntheme = \"dark\"\n\n[providers.existing]\nid = \"existing\"\nbase_url = \"https://existing.example.com\"\nchat_completions_path = \"/v1/chat/completions\"\ntimeout_seconds = 30\nmodel = \"existing-model\"\nstream = true\n",
        )
        .unwrap();
        let settings = ProviderSettings {
            id: "new-profile".to_owned(),
            base_url: "https://api.example.com".to_owned(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: Some("EXAMPLE_API_KEY".to_owned()),
            static_headers: BTreeMap::new(),
            timeout_seconds: 120,
            ca_certificate_pem: None,
            model: "example-model".to_owned(),
            stream: true,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        };

        Config::add_provider_profile(directory.path(), "new-profile", settings.clone()).unwrap();

        let source = fs::read_to_string(path).unwrap();
        assert!(source.contains("# keep this comment"));
        assert!(source.contains("[tui]\ntheme = \"dark\""));
        let config = Config::load(directory.path()).unwrap();
        assert_eq!(config.providers["existing"].model, "existing-model");
        assert_eq!(config.providers["new-profile"], settings);
    }

    #[test]
    fn updating_provider_profile_preserves_profile_comments() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "[providers.example]\nid = \"example\"\nbase_url = \"https://old.example.com\" # keep endpoint note\nchat_completions_path = \"/v1/chat/completions\"\ntimeout_seconds = 30\nmodel = \"old-model\"\nstream = true\n",
        )
        .unwrap();
        let settings = ProviderSettings {
            id: "example".to_owned(),
            base_url: "https://new.example.com".to_owned(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 60,
            ca_certificate_pem: None,
            model: "new-model".to_owned(),
            stream: false,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        };

        Config::add_provider_profile(directory.path(), "example", settings).unwrap();

        let source = fs::read_to_string(path).unwrap();
        assert!(source.contains("base_url = \"https://new.example.com\" # keep endpoint note"));
    }

    #[test]
    fn remove_provider_profile_removes_entry_and_preserves_others() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# top-level comment\n[tui]\ntheme = \"dark\"\n\n[providers.to_remove]\nid = \"to_remove\"\nbase_url = \"https://remove.example.com\"\nchat_completions_path = \"/v1/chat/completions\"\ntimeout_seconds = 30\nmodel = \"remove-model\"\nstream = true\n\n# keep this provider\n[providers.keep]\nid = \"keep\"\nbase_url = \"https://keep.example.com\"\nchat_completions_path = \"/v1/chat/completions\"\ntimeout_seconds = 30\nmodel = \"keep-model\"\nstream = true\n",
        )
        .unwrap();

        let removed = Config::remove_provider_profile(directory.path(), "to_remove").unwrap();
        assert!(removed);

        let source = fs::read_to_string(&path).unwrap();
        assert!(source.contains("# top-level comment"));
        assert!(source.contains("[tui]\ntheme = \"dark\""));
        assert!(source.contains("# keep this provider"));
        assert!(!source.contains("to_remove"));
        assert!(source.contains("keep"));

        let config = Config::load(directory.path()).unwrap();
        assert_eq!(config.providers.len(), 1);
        assert!(config.providers.contains_key("keep"));

        let removed_again = Config::remove_provider_profile(directory.path(), "to_remove").unwrap();
        assert!(!removed_again);
    }

    #[test]
    fn load_provider_templates_parses_file_and_handles_absent() {
        let directory = tempdir().unwrap();
        let empty = Config::load_provider_templates(directory.path()).unwrap();
        assert!(empty.is_empty());

        let templates_path = directory.path().join("provider-templates.toml");
        fs::write(
            &templates_path,
            r#"
[openrouter]
name = "OpenRouter"
id = "openrouter"
base_url = "https://openrouter.ai"
chat_completions_path = "/api/v1/chat/completions"
api_key_env = "OPENROUTER_API_KEY"
default_model = "anthropic/claude-3.5-sonnet"
stream = true
timeout_seconds = 120
"#,
        )
        .unwrap();

        let templates = Config::load_provider_templates(directory.path()).unwrap();
        assert_eq!(templates.len(), 1);
        let openrouter = &templates["openrouter"];
        assert_eq!(openrouter.name, "OpenRouter");
        assert_eq!(openrouter.base_url, "https://openrouter.ai");
        assert_eq!(openrouter.default_model, "anthropic/claude-3.5-sonnet");
        assert!(openrouter.stream);
    }
}
