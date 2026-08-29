use std::{env, fs, path::PathBuf};

use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, PathError> {
        resolve_with(|name| env::var_os(name).map(PathBuf::from))
    }

    pub fn ensure_exists(&self) -> Result<(), PathError> {
        for path in [&self.config, &self.data, &self.cache] {
            fs::create_dir_all(path).map_err(|source| PathError::Create {
                path: path.clone(),
                source,
            })?;
            set_private_directory_permissions(path)?;
        }
        Ok(())
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("stcli.sqlite3")
    }
    pub fn plugins(&self) -> PathBuf {
        self.data.join("plugins")
    }
}

fn resolve_with(mut get: impl FnMut(&str) -> Option<PathBuf>) -> Result<AppPaths, PathError> {
    if let Some(root) = get("STCLI_HOME") {
        return Ok(AppPaths {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
        });
    }

    #[cfg(windows)]
    {
        let roaming = get("APPDATA").ok_or(PathError::MissingHome)?;
        let local = get("LOCALAPPDATA").unwrap_or_else(|| roaming.clone());
        return Ok(AppPaths {
            config: roaming.join("STcli"),
            data: local.join("STcli").join("data"),
            cache: local.join("STcli").join("cache"),
        });
    }

    #[cfg(not(windows))]
    {
        let home = get("HOME").ok_or(PathError::MissingHome)?;
        let config = get("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let data = get("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"));
        let cache = get("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
        Ok(AppPaths {
            config: config.join("stcli"),
            data: data.join("stcli"),
            cache: cache.join("stcli"),
        })
    }
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &std::path::Path) -> Result<(), PathError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        PathError::Permissions {
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &std::path::Path) -> Result<(), PathError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum PathError {
    #[error("cannot resolve STcli data paths without STCLI_HOME or a platform home directory")]
    MissingHome,
    #[error("failed to create STcli directory '{path}': {source}")]
    Create {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to secure STcli directory '{path}': {source}")]
    Permissions {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn stcli_home_overrides_platform_paths() {
        let environment = BTreeMap::from([("STCLI_HOME", PathBuf::from("/portable"))]);
        let paths = resolve_with(|name| environment.get(name).cloned()).unwrap();
        assert_eq!(paths.config, PathBuf::from("/portable/config"));
        assert_eq!(paths.data, PathBuf::from("/portable/data"));
        assert_eq!(paths.cache, PathBuf::from("/portable/cache"));
    }

    #[cfg(not(windows))]
    #[test]
    fn xdg_paths_fall_back_to_home() {
        let environment = BTreeMap::from([("HOME", PathBuf::from("/home/alice"))]);
        let paths = resolve_with(|name| environment.get(name).cloned()).unwrap();
        assert_eq!(paths.config, PathBuf::from("/home/alice/.config/stcli"));
        assert_eq!(paths.data, PathBuf::from("/home/alice/.local/share/stcli"));
        assert_eq!(paths.cache, PathBuf::from("/home/alice/.cache/stcli"));
    }
}
