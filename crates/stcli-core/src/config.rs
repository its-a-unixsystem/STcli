use std::{collections::BTreeMap, fs, path::Path, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ContentHash, EgressAllowance, PluginError, PluginPin, PluginRegistry, PluginRuntime,
    ProviderSettings, provider::ProviderError, st_bridge_capability_tier,
    validate_provider_settings,
};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub providers: BTreeMap<String, ProviderSettings>,
    #[serde(rename = "extensions")]
    pub enabled_extensions: BTreeMap<String, GlobalExtensionPin>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GlobalExtensionPin {
    pub version: String,
    pub digest: ContentHash,
    #[serde(default = "empty_json_object")]
    pub settings: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_allow_list: Vec<EgressAllowance>,
}

fn empty_json_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
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
        for (id, pin) in &self.enabled_extensions {
            validate_enabled_extension(id, pin)?;
        }
        Ok(())
    }

    pub fn resolve_enabled_extensions(
        &self,
        registry: &PluginRegistry,
    ) -> Result<Vec<PluginPin>, ConfigError> {
        self.enabled_extensions
            .iter()
            .map(|(id, configured)| resolve_enabled_extension_pin(registry, id, configured))
            .collect()
    }

    pub fn resolve_enabled_extension(
        &self,
        registry: &PluginRegistry,
        id: &str,
    ) -> Result<PluginPin, ConfigError> {
        let configured = self
            .enabled_extensions
            .get(id)
            .ok_or_else(|| ConfigError::EnabledExtensionNotConfigured(id.to_owned()))?;
        resolve_enabled_extension_pin(registry, id, configured)
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
        Self::save_provider_profile(directory, None, name, settings)
    }

    pub fn duplicate_provider_profile(
        directory: &Path,
        source_name: &str,
        target_name: &str,
    ) -> Result<(), ConfigError> {
        let config = Self::load(directory)?;
        if config.providers.contains_key(target_name) {
            return Err(ConfigError::ProfileAlreadyExists(target_name.to_owned()));
        }
        let settings = config.resolve_provider_profile(source_name)?.clone();
        Self::save_provider_profile(directory, None, target_name, settings)
    }

    pub fn save_provider_profile(
        directory: &Path,
        original_name: Option<&str>,
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
        if let Some(original_name) = original_name.filter(|original| *original != name) {
            if providers.contains_key(name) {
                return Err(ConfigError::ProfileAlreadyExists(name.to_owned()));
            }
            if let Some(existing) = providers.remove(original_name) {
                providers.insert(name, existing);
            }
        }
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

    pub fn save_enabled_extension(
        directory: &Path,
        id: &str,
        pin: GlobalExtensionPin,
    ) -> Result<(), ConfigError> {
        validate_enabled_extension(id, &pin).map_err(|source| ConfigError::InvalidExtension {
            id: id.to_owned(),
            source,
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
        if !document.contains_key("extensions") {
            document["extensions"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let extensions = document["extensions"]
            .as_table_mut()
            .ok_or_else(|| ConfigError::ExtensionsNotTable { path: path.clone() })?;
        let item_doc = toml::to_string(&pin).map_err(ConfigError::Serialize)?;
        let item = item_doc
            .parse::<toml_edit::DocumentMut>()
            .map_err(|source| ConfigError::Edit {
                path: path.clone(),
                source,
            })?;
        update_extension_table(
            extensions,
            id,
            toml_edit::Item::Table(item.as_table().clone()),
        );
        fs::write(&path, document.to_string()).map_err(|source| ConfigError::Write { path, source })
    }

    pub fn remove_enabled_extension(directory: &Path, id: &str) -> Result<bool, ConfigError> {
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
        let removed = if let Some(extensions) = document
            .get_mut("extensions")
            .and_then(|p| p.as_table_mut())
        {
            extensions.remove(id).is_some()
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
    pub credential_key: Option<String>,
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
    "credential_key",
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

const EXTENSION_FIELDS: &[&str] = &["version", "digest", "settings", "egress_allow_list"];

fn update_extension_table(extensions: &mut toml_edit::Table, id: &str, desired: toml_edit::Item) {
    let Some(existing) = extensions.get_mut(id) else {
        extensions.insert(id, desired);
        return;
    };
    let (Some(existing), Some(desired)) = (existing.as_table_mut(), desired.as_table()) else {
        extensions.insert(id, desired);
        return;
    };
    for key in EXTENSION_FIELDS {
        if !desired.contains_key(key) {
            existing.remove(key);
        }
    }
    merge_table(existing, desired);
}

fn validate_enabled_extension(id: &str, pin: &GlobalExtensionPin) -> Result<(), ParseError> {
    if id.trim().is_empty() {
        return Err(ParseError::EmptyExtensionId);
    }
    if pin.version.trim().is_empty() {
        return Err(ParseError::EmptyExtensionVersion(id.to_owned()));
    }
    semver::Version::parse(&pin.version).map_err(|source| ParseError::InvalidExtensionVersion {
        id: id.to_owned(),
        source,
    })?;
    if !pin.settings.is_object() {
        return Err(ParseError::InvalidExtensionSettings(id.to_owned()));
    }
    Ok(())
}

fn resolve_enabled_extension_pin(
    registry: &PluginRegistry,
    id: &str,
    configured: &GlobalExtensionPin,
) -> Result<PluginPin, ConfigError> {
    validate_enabled_extension(id, configured).map_err(|source| ConfigError::InvalidExtension {
        id: id.to_owned(),
        source,
    })?;
    let version = semver::Version::parse(&configured.version).map_err(|source| {
        ConfigError::InvalidExtension {
            id: id.to_owned(),
            source: ParseError::InvalidExtensionVersion {
                id: id.to_owned(),
                source,
            },
        }
    })?;
    let installed = registry
        .find_pinned(id, &version, &configured.digest)?
        .ok_or_else(|| ConfigError::EnabledExtensionNotInstalled {
            id: id.to_owned(),
            version: configured.version.clone(),
            digest: configured.digest.clone(),
        })?;
    if installed.manifest.runtime != PluginRuntime::StBridge {
        return Err(ConfigError::EnabledExtensionNotStBridge(id.to_owned()));
    }
    let capabilities = st_bridge_capability_tier();
    if !capabilities.is_subset(&installed.manifest.requested_capabilities) {
        return Err(ConfigError::EnabledExtensionGrantExceeded(id.to_owned()));
    }
    Ok(PluginPin {
        id: id.to_owned(),
        version: configured.version.clone(),
        component_hash: configured.digest.clone(),
        capabilities,
        settings: configured.settings.clone(),
        egress_allow_list: configured.egress_allow_list.clone(),
        enabled: true,
    })
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
    #[error("provider profile '{0}' already exists")]
    ProfileAlreadyExists(String),
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
    #[error("enabled extension '{id}' is invalid: {source}")]
    InvalidExtension { id: String, source: ParseError },
    #[error("the extensions entry in {path} must be a table")]
    ExtensionsNotTable { path: std::path::PathBuf },
    #[error("failed to write {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("extension '{0}' is not enabled globally")]
    EnabledExtensionNotConfigured(String),
    #[error("enabled extension '{id}' version {version} at {digest} is not installed")]
    EnabledExtensionNotInstalled {
        id: String,
        version: String,
        digest: ContentHash,
    },
    #[error("enabled extension '{0}' is not an st-bridge package")]
    EnabledExtensionNotStBridge(String),
    #[error("enabled extension '{0}' does not request the fixed st-bridge capability tier")]
    EnabledExtensionGrantExceeded(String),
    #[error(transparent)]
    Plugin(#[from] PluginError),
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("{0}")]
    Toml(#[from] toml::de::Error),
    #[error("provider profile names cannot be empty")]
    EmptyProfileName,
    #[error("provider profile '{name}' is invalid: {source}")]
    InvalidProfile { name: String, source: ProviderError },
    #[error("enabled extension IDs cannot be empty")]
    EmptyExtensionId,
    #[error("enabled extension '{0}' version cannot be empty")]
    EmptyExtensionVersion(String),
    #[error("enabled extension '{id}' has an invalid version: {source}")]
    InvalidExtensionVersion { id: String, source: semver::Error },
    #[error("enabled extension '{0}' settings must be a JSON object")]
    InvalidExtensionSettings(String),
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
                    credential_key: None,
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
            enabled_extensions: BTreeMap::new(),
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
                        credential_key: None,
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
                        credential_key: None,
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
            enabled_extensions: BTreeMap::new(),
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
            credential_key: None,
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
            credential_key: None,
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
    fn rename_provider_profile_preserves_comments_and_removes_old_name() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# keep\n[providers.old]\n# endpoint\nid = \"old\"\nbase_url = \"https://old.example.com\"\nchat_completions_path = \"/v1/chat/completions\"\ntimeout_seconds = 30\nmodel = \"old-model\"\nstream = true\n",
        )
        .unwrap();
        let settings = ProviderSettings {
            id: "new".to_owned(),
            base_url: "https://new.example.com".to_owned(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            credential_key: None,
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

        Config::save_provider_profile(directory.path(), Some("old"), "new", settings).unwrap();

        let source = fs::read_to_string(path).unwrap();
        assert!(source.contains("# keep"));
        assert!(source.contains("# endpoint"));
        let config = Config::load(directory.path()).unwrap();
        assert!(!config.providers.contains_key("old"));
        assert_eq!(config.providers["new"].model, "new-model");
    }
    #[test]
    fn duplicate_provider_profile_preserves_source_and_unmanaged_configuration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# keep\n[tui]\ntheme = \"dark\"\n\n[providers.source]\nid = \"openai-compatible\"\nbase_url = \"https://example.com\" # endpoint\nchat_completions_path = \"/v1/chat/completions\"\napi_key_env = \"EXAMPLE_API_KEY\"\ntimeout_seconds = 30\nmodel = \"source-model\"\nstream = true\n",
        )
        .unwrap();

        Config::duplicate_provider_profile(directory.path(), "source", "source-copy").unwrap();

        let source = fs::read_to_string(path).unwrap();
        assert!(source.contains("# keep"));
        assert!(source.contains("[tui]\ntheme = \"dark\""));
        assert!(source.contains("base_url = \"https://example.com\" # endpoint"));
        let config = Config::load(directory.path()).unwrap();
        assert_eq!(config.providers["source-copy"], config.providers["source"]);
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
    #[test]
    fn enabled_extension_defaults_round_trip_without_rewriting_unmanaged_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "# keep this comment\n[tui]\ntheme = \"dark\"\n").unwrap();
        let pin = GlobalExtensionPin {
            version: "1.2.3".to_owned(),
            digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .unwrap(),
            settings: serde_json::json!({"mode": "strict"}),
            egress_allow_list: vec![EgressAllowance {
                domain: "example.com".to_owned(),
                secret: None,
            }],
        };

        Config::save_enabled_extension(directory.path(), "example-extension", pin.clone()).unwrap();

        let source = fs::read_to_string(&path).unwrap();
        assert!(source.contains("# keep this comment"));
        assert!(source.contains("[tui]\ntheme = \"dark\""));
        assert!(source.contains("[extensions.example-extension]"));
        let config = Config::load(directory.path()).unwrap();
        assert_eq!(config.enabled_extensions["example-extension"], pin);

        assert!(Config::remove_enabled_extension(directory.path(), "example-extension").unwrap());
        assert!(
            Config::load(directory.path())
                .unwrap()
                .enabled_extensions
                .is_empty()
        );
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("[tui]\ntheme = \"dark\"")
        );
    }

    #[test]
    fn rejects_invalid_enabled_extension_defaults() {
        let error = Config::parse(
            "[extensions.bad]\nversion = \"\"\ndigest = \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
        )
        .unwrap_err();
        assert!(error.to_string().contains("version cannot be empty"));
    }

    #[test]
    fn rejects_stale_enabled_extension_pin() {
        let directory = tempdir().unwrap();
        let configured = GlobalExtensionPin {
            version: "1.2.3".to_owned(),
            digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .parse()
                .unwrap(),
            settings: empty_json_object(),
            egress_allow_list: Vec::new(),
        };
        let config = Config {
            providers: BTreeMap::new(),
            enabled_extensions: BTreeMap::from([("example-extension".to_owned(), configured)]),
        };

        let error = config
            .resolve_enabled_extensions(&PluginRegistry::new(directory.path()))
            .unwrap_err();

        assert!(error.to_string().contains("is not installed"));
    }
}
