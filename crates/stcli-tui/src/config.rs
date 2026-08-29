use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;
use stcli_core::Config as CoreConfig;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct TuiSettings {
    pub theme: ThemeChoice,
    pub toast_timeout: u64,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Auto,
            toast_timeout: 5,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawTuiConfig {
    tui: TuiSettings,
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub tui: TuiSettings,
    pub core: CoreConfig,
}

impl Config {
    pub fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let raw: RawTuiConfig = toml::from_str(&source)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let core = CoreConfig::parse(&source)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(Self { tui: raw.tui, core })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    #[test]
    fn rejects_literal_secrets_before_the_tui_starts() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("config.toml"),
            r#"
[providers.local]
id = "local"
base_url = "https://localhost"
chat_completions_path = "/v1/chat/completions"
timeout_seconds = 30
model = "model"
stream = true

[providers.local.static_headers.X-Auth-Token]
source = "literal"
value = "secret"
"#,
        )
        .unwrap();

        let error = Config::load(directory.path()).unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("provider profile 'local' is invalid"),
            "expected profile validation error, got: {message}"
        );
    }

    #[test]
    fn defaults_are_minimal() {
        let config = Config::default();
        assert_eq!(config.tui.theme, ThemeChoice::Auto);
        assert_eq!(config.tui.toast_timeout, 5);
        assert_eq!(config.core.providers, BTreeMap::new());
    }
}
