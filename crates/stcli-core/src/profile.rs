use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const PROFILE_SCHEMA: &str = "stcli.compat-profile/v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompatibilityProfile {
    pub schema: String,
    pub id: String,
    pub upstream: UpstreamRevision,
    pub scope: ProfileScope,
    pub macros: MacroManifest,
    #[serde(default)]
    pub preset_fields: BTreeMap<String, CompatibilityOutcome>,
}

impl CompatibilityProfile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ProfileError> {
        let source = fs::read(path).map_err(ProfileError::Read)?;
        let profile = serde_json::from_slice::<Self>(&source).map_err(ProfileError::Decode)?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.schema != PROFILE_SCHEMA {
            return Err(ProfileError::UnsupportedSchema(self.schema.clone()));
        }
        if self.id.trim().is_empty() {
            return Err(ProfileError::EmptyId);
        }
        if self.upstream.commit.len() != 40
            || !self
                .upstream
                .commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ProfileError::InvalidCommit(self.upstream.commit.clone()));
        }

        let mut prior: Option<&str> = None;
        for name in &self.macros.exact {
            if name.trim().is_empty() {
                return Err(ProfileError::EmptyMacro);
            }
            if let Some(previous) = prior {
                let ordering = previous.to_lowercase().cmp(&name.to_lowercase());
                if !ordering.is_lt() {
                    return Err(ProfileError::UnsortedOrDuplicateMacro {
                        previous: previous.to_owned(),
                        current: name.clone(),
                    });
                }
            }
            prior = Some(name);
        }

        Ok(())
    }

    pub fn classify(&self, subject: &CompatibilitySubject) -> CompatibilityOutcome {
        match subject.kind {
            CompatibilitySubjectKind::Format => {
                if contains(&self.scope.formats, &subject.name) {
                    CompatibilityOutcome::Exact
                } else {
                    CompatibilityOutcome::HardUnsupported
                }
            }
            CompatibilitySubjectKind::Generation => {
                if contains(&self.scope.generation_types, &subject.name) {
                    CompatibilityOutcome::Exact
                } else {
                    CompatibilityOutcome::HardUnsupported
                }
            }
            CompatibilitySubjectKind::PromptPath => {
                if self.scope.prompt_path.eq_ignore_ascii_case(&subject.name) {
                    CompatibilityOutcome::Exact
                } else {
                    CompatibilityOutcome::HardUnsupported
                }
            }
            CompatibilitySubjectKind::Macro => {
                if contains_case_insensitive(&self.macros.exact, &subject.name) {
                    CompatibilityOutcome::Exact
                } else if contains_case_insensitive(&self.macros.hard_unsupported, &subject.name) {
                    CompatibilityOutcome::HardUnsupported
                } else {
                    CompatibilityOutcome::DocumentedFallback
                }
            }
            CompatibilitySubjectKind::Metadata => {
                if contains(&self.scope.preserved_metadata, &subject.name) {
                    CompatibilityOutcome::PreservedMetadata
                } else {
                    CompatibilityOutcome::HardUnsupported
                }
            }
            CompatibilitySubjectKind::PresetField => self
                .preset_fields
                .get(&subject.name)
                .copied()
                .unwrap_or(CompatibilityOutcome::HardUnsupported),
            CompatibilitySubjectKind::Feature => {
                if contains(&self.scope.documented_fallback, &subject.name) {
                    CompatibilityOutcome::DocumentedFallback
                } else {
                    CompatibilityOutcome::HardUnsupported
                }
            }
        }
    }
}

fn contains(values: &[String], expected: &str) -> bool {
    values.iter().any(|value| value == expected)
}

fn contains_case_insensitive(values: &[String], expected: &str) -> bool {
    values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(expected))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilityOutcome {
    ProviderBehavior,
    AssemblyBehavior,
    Exact,
    PreservedMetadata,
    DocumentedFallback,
    HardUnsupported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompatibilitySubject {
    pub kind: CompatibilitySubjectKind,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompatibilitySubjectKind {
    Format,
    Generation,
    PromptPath,
    Macro,
    Metadata,
    PresetField,
    Feature,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpstreamRevision {
    pub repository: String,
    pub tag: String,
    pub commit: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfileScope {
    pub formats: Vec<String>,
    pub prompt_path: String,
    pub generation_types: Vec<String>,
    pub documented_fallback: Vec<String>,
    pub hard_unsupported: Vec<String>,
    pub preserved_metadata: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MacroManifest {
    pub exact: Vec<String>,
    pub documented_fallback: BTreeMap<String, String>,
    pub hard_unsupported: Vec<String>,
    pub source_files: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("failed to read compatibility profile: {0}")]
    Read(std::io::Error),
    #[error("failed to decode compatibility profile: {0}")]
    Decode(serde_json::Error),
    #[error("unsupported compatibility profile schema '{0}'")]
    UnsupportedSchema(String),
    #[error("compatibility profile ID cannot be empty")]
    EmptyId,
    #[error("upstream commit must be a 40-character hexadecimal Git commit, found '{0}'")]
    InvalidCommit(String),
    #[error("exact macro names cannot be empty")]
    EmptyMacro,
    #[error(
        "exact macro list must be case-insensitively sorted and unique: '{previous}' before '{current}'"
    )]
    UnsortedOrDuplicateMacro { previous: String, current: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_profile() -> CompatibilityProfile {
        CompatibilityProfile {
            schema: PROFILE_SCHEMA.to_owned(),
            id: "sillytavern-1.18-core".to_owned(),
            upstream: UpstreamRevision {
                repository: "https://github.com/SillyTavern/SillyTavern".to_owned(),
                tag: "1.18.0".to_owned(),
                commit: "51ad27fb86d39a3daca3adaa970375c9670c12df".to_owned(),
                version: "1.18.0".to_owned(),
            },
            scope: ProfileScope {
                formats: vec!["character-card-v2-json".to_owned()],
                prompt_path: "chat-completion-prompt-manager".to_owned(),
                generation_types: vec!["normal".to_owned()],
                documented_fallback: vec!["approximate-tokenizer".to_owned()],
                hard_unsupported: vec![],
                preserved_metadata: vec!["unknown-json-member".to_owned()],
            },
            macros: MacroManifest {
                exact: vec!["char".to_owned(), "user".to_owned()],
                documented_fallback: BTreeMap::new(),
                hard_unsupported: vec![],
                source_files: vec![],
            },
            preset_fields: BTreeMap::new(),
        }
    }

    #[test]
    fn valid_profile_passes_validation() {
        valid_profile().validate().unwrap();
    }

    #[test]
    fn duplicate_macro_names_fail_case_insensitively() {
        let mut profile = valid_profile();
        profile.macros.exact = vec!["char".to_owned(), "CHAR".to_owned()];
        assert!(matches!(
            profile.validate(),
            Err(ProfileError::UnsortedOrDuplicateMacro { .. })
        ));
    }

    #[test]
    fn classification_distinguishes_all_outcomes() {
        let mut profile = valid_profile();
        profile.scope.hard_unsupported = vec!["group-chat".to_owned()];
        profile.macros.hard_unsupported = vec!["input".to_owned()];
        let classify = |kind, name: &str| {
            profile.classify(&CompatibilitySubject {
                kind,
                name: name.to_owned(),
            })
        };

        assert_eq!(
            classify(CompatibilitySubjectKind::Macro, "char"),
            CompatibilityOutcome::Exact
        );
        assert_eq!(
            classify(CompatibilitySubjectKind::Metadata, "unknown-json-member"),
            CompatibilityOutcome::PreservedMetadata
        );
        assert_eq!(
            classify(CompatibilitySubjectKind::Macro, "platformMacro"),
            CompatibilityOutcome::DocumentedFallback
        );
        assert_eq!(
            classify(CompatibilitySubjectKind::Feature, "group-chat"),
            CompatibilityOutcome::HardUnsupported
        );
    }
}
