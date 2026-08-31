use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use ulid::Ulid;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct PersonaStore {
    personas: BTreeMap<String, String>,
    persona_descriptions: BTreeMap<String, PersonaDescription>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_persona: Option<String>,
    #[serde(flatten)]
    metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
struct PersonaDescription {
    description: String,
    position: i64,
    #[serde(flatten)]
    metadata: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Persona {
    pub key: String,
    pub name: String,
    pub description: String,
    pub position: i64,
}

impl PersonaStore {
    pub fn load(directory: &Path) -> Result<Self, PersonaStoreError> {
        let path = directory.join("personas.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::load_path(&path)
    }

    pub fn save(&self, directory: &Path) -> Result<(), PersonaStoreError> {
        fs::create_dir_all(directory).map_err(|source| PersonaStoreError::Write {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = directory.join("personas.json");
        let temporary = directory.join(format!("personas.json.{}.tmp", Ulid::new()));
        let mut bytes = serde_json::to_vec_pretty(self).map_err(PersonaStoreError::Serialize)?;
        bytes.push(b'\n');
        fs::write(&temporary, bytes).map_err(|source| PersonaStoreError::Write {
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, &path).map_err(|source| PersonaStoreError::Write { path, source })
    }

    pub fn personas(&self) -> Vec<Persona> {
        let mut personas = self
            .personas
            .iter()
            .map(|(key, name)| {
                let description = self.persona_descriptions.get(key);
                Persona {
                    key: key.clone(),
                    name: name.clone(),
                    description: description
                        .map(|entry| entry.description.clone())
                        .unwrap_or_default(),
                    position: description.map_or(0, |entry| entry.position),
                }
            })
            .collect::<Vec<_>>();
        personas.sort_by(|left, right| {
            left.position
                .cmp(&right.position)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.key.cmp(&right.key))
        });
        personas
    }

    pub fn get(&self, key: &str) -> Option<Persona> {
        let name = self.personas.get(key)?;
        let description = self.persona_descriptions.get(key);
        Some(Persona {
            key: key.to_owned(),
            name: name.clone(),
            description: description
                .map(|entry| entry.description.clone())
                .unwrap_or_default(),
            position: description.map_or(0, |entry| entry.position),
        })
    }

    pub fn default_persona(&self) -> Option<&str> {
        self.default_persona.as_deref()
    }

    pub fn insert(&mut self, name: impl Into<String>, description: impl Into<String>) -> String {
        let key = format!("stcli-{}", Ulid::new());
        self.personas.insert(key.clone(), name.into());
        self.persona_descriptions.insert(
            key.clone(),
            PersonaDescription {
                description: description.into(),
                ..PersonaDescription::default()
            },
        );
        key
    }

    pub fn duplicate(&mut self, key: &str) -> Result<String, PersonaStoreError> {
        let source_description = self
            .persona_descriptions
            .get(key)
            .ok_or_else(|| PersonaStoreError::PersonaNotFound(key.to_owned()))?;
        let source_name = self
            .personas
            .get(key)
            .ok_or_else(|| PersonaStoreError::PersonaNotFound(key.to_owned()))?;
        let name = self.available_copy_name(source_name);
        let new_key = format!("stcli-{}", Ulid::new());
        self.personas.insert(new_key.clone(), name);
        self.persona_descriptions
            .insert(new_key.clone(), source_description.clone());
        Ok(new_key)
    }

    pub fn update(
        &mut self,
        key: &str,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<(), PersonaStoreError> {
        let Some(existing_name) = self.personas.get_mut(key) else {
            return Err(PersonaStoreError::PersonaNotFound(key.to_owned()));
        };
        *existing_name = name.into();
        self.persona_descriptions
            .entry(key.to_owned())
            .or_default()
            .description = description.into();
        Ok(())
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let removed = self.personas.remove(key).is_some();
        self.persona_descriptions.remove(key);
        if self.default_persona.as_deref() == Some(key) {
            self.default_persona = None;
        }
        removed
    }

    pub fn import_backup(&mut self, path: &Path) -> Result<usize, PersonaStoreError> {
        let imported = Self::load_path(path)?;
        let count = imported.personas.len();
        self.personas.extend(imported.personas);
        self.persona_descriptions
            .extend(imported.persona_descriptions);
        if imported.default_persona.is_some() {
            self.default_persona = imported.default_persona;
        }
        self.metadata.extend(imported.metadata);
        Ok(count)
    }

    fn load_path(path: &Path) -> Result<Self, PersonaStoreError> {
        let source = fs::read(path).map_err(|source| PersonaStoreError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&source).map_err(|source| PersonaStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    fn available_copy_name(&self, source_name: &str) -> String {
        let base = format!("{source_name}-copy");
        if !self.personas.values().any(|name| name == &base) {
            return base;
        }
        (2..)
            .map(|suffix| format!("{base}-{suffix}"))
            .find(|candidate| !self.personas.values().any(|name| name == candidate))
            .expect("unbounded suffix sequence always contains an available name")
    }
}

#[derive(Debug, Error)]
pub enum PersonaStoreError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to serialize personas: {0}")]
    Serialize(serde_json::Error),
    #[error("failed to write {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("persona '{0}' was not found")]
    PersonaNotFound(String),
}
