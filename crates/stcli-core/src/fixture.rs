use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    EcmaRegexError, EntityId, LoreDecisionOutcome, LoreEngine, LoreError, LoreSettings,
    MacroContext, MacroEngine, MacroError, RegexPlacement, RegexScript, StateTransaction,
    TokenizerId, VariableScope,
    identity::{ContentHash, artifact_revision_hash, canonical_json, canonical_json_hash},
    lore::parse_lore_entries,
    profile::{
        CompatibilityOutcome, CompatibilityProfile, CompatibilitySubject, CompatibilitySubjectKind,
        ProfileError,
    },
};

const FIXTURE_SCHEMA_V1: &str = "stcli.fixture-suite/v1";
const FIXTURE_SCHEMA_V2: &str = "stcli.fixture-suite/v2";
const FIXTURE_SCHEMA_VERSIONS: &[&str] = &[FIXTURE_SCHEMA_V1, FIXTURE_SCHEMA_V2];

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FixtureSuite {
    pub schema: String,
    pub profile: String,
    #[serde(default)]
    pub external_sources: Vec<ExternalFixtureSource>,
    pub cases: Vec<FixtureCase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macro_groups: Vec<MacroGroup>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MacroGroup {
    pub defaults: MacroGroupDefaults,
    pub cases: Vec<MacroGroupCase>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct MacroGroupDefaults {
    #[serde(default)]
    pub context: BTreeMap<String, String>,
    #[serde(default)]
    pub initial_local: BTreeMap<String, String>,
    #[serde(default)]
    pub initial_global: BTreeMap<String, String>,
    #[serde(default)]
    pub plugins: BTreeSet<String>,
    #[serde(default)]
    pub outlets: BTreeMap<String, String>,
    #[serde(default)]
    pub expected_warnings: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MacroGroupCase {
    pub id: String,
    pub input: String,
    pub expected_text: String,
    #[serde(default)]
    pub expected_local: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub expected_global: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub expected_warnings: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExternalFixtureSource {
    pub name: String,
    pub provenance: String,
    pub revision: Option<String>,
    pub sha256: String,
    pub path_environment: String,
    #[serde(default)]
    pub repository_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FixtureHistoryTurn {
    pub user: String,
    pub assistant: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderRequestParityCase {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub preset_source: String,
    pub oracle_source: String,
    pub oracle_key: String,
    pub generation_type: String,
    pub character: String,
    pub persona_name: String,
    pub history: Vec<FixtureHistoryTurn>,
    pub user_content: String,
    pub tokenizer: TokenizerId,
    pub provider_model: String,
    pub provider_stream: bool,
    pub expected_message_count: usize,
    pub expected_settings: Value,
    pub expected_warning_codes: Vec<String>,
    pub expected_effective_settings_hash: ContentHash,
    pub expected_warnings_hash: ContentHash,
    pub expected_pruning: crate::PromptPruning,
    pub expected_setvar_evaluations: usize,
    pub expected_state_mutations: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FixtureCase {
    CanonicalJson {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        domain: String,
        input: Value,
        expected_canonical: String,
        expected_hash: ContentHash,
    },
    ArtifactRevision {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        artifact_kind: String,
        source_format: String,
        source: String,
        expected_hash: ContentHash,
    },
    CompatibilityOutcome {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        subject_kind: CompatibilitySubjectKind,
        subject: String,
        expected_outcome: CompatibilityOutcome,
    },
    MacroRender {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        input: String,
        context: BTreeMap<String, String>,
        initial_local: BTreeMap<String, String>,
        initial_global: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        plugins: BTreeSet<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        outlets: BTreeMap<String, String>,
        expected_text: String,
        expected_local: BTreeMap<String, String>,
        expected_global: BTreeMap<String, String>,
        expected_warnings: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_error: Option<String>,
    },
    LoreEvaluate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        source_revision: ContentHash,
        entries: Value,
        messages: Vec<String>,
        tokenizer: TokenizerId,
        settings: LoreSettings,
        expected_activated: Vec<String>,
        expected_outcomes: BTreeMap<String, LoreDecisionOutcome>,
    },
    RegexScriptApply {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        scripts: Vec<Value>,
        placement: u64,
        depth: i64,
        input: String,
        expected_text: String,
    },
    ProviderRequestParity(ProviderRequestParityCase),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FixtureReport {
    pub profile: String,
    pub files: usize,
    pub total: usize,
    pub passed: usize,
    pub not_run: usize,
    pub cases: Vec<FixtureCaseReport>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FixtureCaseReport {
    pub file: String,
    pub name: String,
    pub passed: bool,
    #[serde(default)]
    pub not_run: bool,
    pub message: String,
}

impl FixtureReport {
    pub fn is_success(&self) -> bool {
        self.total == self.passed && self.not_run == 0
    }
}

pub fn verify_fixture_suite(
    profile_path: impl AsRef<Path>,
    fixtures_path: impl AsRef<Path>,
) -> Result<FixtureReport, FixtureError> {
    let profile = CompatibilityProfile::load(profile_path).map_err(FixtureError::Profile)?;
    let fixture_files = fixture_files(fixtures_path.as_ref())?;
    let mut report = FixtureReport {
        profile: profile.id.clone(),
        files: fixture_files.len(),
        total: 0,
        passed: 0,
        not_run: 0,
        cases: Vec::new(),
    };

    for path in fixture_files {
        let source = fs::read(&path).map_err(|source| FixtureError::Read {
            path: path.clone(),
            source,
        })?;
        let suite = serde_json::from_slice::<FixtureSuite>(&source).map_err(|source| {
            FixtureError::Decode {
                path: path.clone(),
                source,
            }
        })?;
        if !FIXTURE_SCHEMA_VERSIONS.contains(&suite.schema.as_str()) {
            return Err(FixtureError::UnsupportedSchema {
                path,
                schema: suite.schema,
            });
        }
        if suite.profile != profile.id {
            return Err(FixtureError::ProfileMismatch {
                path,
                expected: profile.id.clone(),
                actual: suite.profile,
            });
        }
        for external in &suite.external_sources {
            report.total += 1;
            let case_report = verify_external_source(&path, external)?;
            if case_report.passed {
                report.passed += 1;
            } else if case_report.not_run {
                report.not_run += 1;
            }
            report.cases.push(case_report);
        }

        for case in suite.cases {
            let case_report = verify_case(&profile, &path, case)?;
            report.total += 1;
            if case_report.passed {
                report.passed += 1;
            } else if case_report.not_run {
                report.not_run += 1;
            }
            report.cases.push(case_report);
        }
        for group in &suite.macro_groups {
            for group_case in &group.cases {
                let expanded = expand_macro_group_case(group, group_case);
                let case_report = verify_case(&profile, &path, expanded)?;
                report.total += 1;
                if case_report.passed {
                    report.passed += 1;
                } else if case_report.not_run {
                    report.not_run += 1;
                }
                report.cases.push(case_report);
            }
        }
    }

    Ok(report)
}

/// Collects every case id from all fixture suites in `fixtures_path`.
///
/// Cases without an `id` field are skipped. Macro-group cases contribute
/// their `id` directly. The returned set is the coverage surface the
/// ratchet test checks against the profile and allowlist.
pub fn collect_case_ids(fixtures_path: impl AsRef<Path>) -> Result<BTreeSet<String>, FixtureError> {
    let mut ids = BTreeSet::new();
    for path in fixture_files(fixtures_path.as_ref())? {
        let source = fs::read(&path).map_err(|source| FixtureError::Read {
            path: path.clone(),
            source,
        })?;
        let suite = serde_json::from_slice::<FixtureSuite>(&source).map_err(|source| {
            FixtureError::Decode {
                path: path.clone(),
                source,
            }
        })?;
        for case in &suite.cases {
            if let Some(id) = case_id(case) {
                ids.insert(id);
            }
            if let Some(synthesized) = synthetic_coverage_id(case) {
                ids.insert(synthesized);
            }
        }
        for group in &suite.macro_groups {
            for group_case in &group.cases {
                ids.insert(group_case.id.clone());
            }
        }
    }
    Ok(ids)
}

fn case_id(case: &FixtureCase) -> Option<String> {
    match case {
        FixtureCase::CanonicalJson { id, .. }
        | FixtureCase::ArtifactRevision { id, .. }
        | FixtureCase::CompatibilityOutcome { id, .. }
        | FixtureCase::MacroRender { id, .. }
        | FixtureCase::LoreEvaluate { id, .. }
        | FixtureCase::RegexScriptApply { id, .. } => id.clone(),
        FixtureCase::ProviderRequestParity(c) => c.id.clone(),
    }
}

/// Synthesizes a coverage id from a `compatibility-outcome` case when it
/// lacks an explicit `id`. Only preset-field outcome cases are credited:
/// the spec asks for "at least one case proving its classification" for
/// preset fields, which a classification lookup satisfies. Hard-unsupported
/// macros and scope entries require behavioral rejection/diagnostic
/// fixtures, not mere classification checks, so they are not credited.
fn synthetic_coverage_id(case: &FixtureCase) -> Option<String> {
    match case {
        FixtureCase::CompatibilityOutcome { id: Some(_), .. } => None,
        FixtureCase::CompatibilityOutcome {
            subject_kind: CompatibilitySubjectKind::PresetField,
            subject,
            ..
        } => Some(format!("preset.{subject}.outcome")),
        _ => None,
    }
}

fn verify_external_source(
    manifest_path: &Path,
    source: &ExternalFixtureSource,
) -> Result<FixtureCaseReport, FixtureError> {
    validate_external_source(manifest_path, source)?;
    let file = manifest_path.display().to_string();
    let Some(path) = source.resolve_path(manifest_path) else {
        return Ok(FixtureCaseReport {
            file,
            name: source.name.clone(),
            passed: false,
            not_run: true,
            message: format!(
                "not run: set {} to matching external fixture bytes",
                source.path_environment
            ),
        });
    };
    let bytes = fs::read(&path).map_err(|source_error| FixtureError::ReadExternal {
        name: source.name.clone(),
        path: path.clone(),
        source: source_error,
    })?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != source.sha256 {
        return Err(FixtureError::ExternalDigestMismatch {
            name: source.name.clone(),
            expected: source.sha256.clone(),
            actual,
        });
    }
    Ok(FixtureCaseReport {
        file,
        name: source.name.clone(),
        passed: true,
        not_run: false,
        message: "external fixture digest verified".to_owned(),
    })
}

impl ExternalFixtureSource {
    pub fn resolve_path(&self, manifest_path: &Path) -> Option<PathBuf> {
        if let Some(path) = env::var_os(&self.path_environment) {
            return Some(path.into());
        }
        let path = PathBuf::from(self.repository_path.as_ref()?);
        if path.is_absolute() {
            return Some(path);
        }
        let root = manifest_path
            .ancestors()
            .find(|ancestor| ancestor.join("Cargo.toml").is_file())
            .unwrap_or_else(|| Path::new(""));
        Some(root.join(path))
    }
}

fn validate_external_source(
    manifest_path: &Path,
    source: &ExternalFixtureSource,
) -> Result<(), FixtureError> {
    if source.name.trim().is_empty()
        || source.provenance.trim().is_empty()
        || source.path_environment.trim().is_empty()
        || source.sha256.len() != 64
        || !source.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(FixtureError::InvalidExternalSource {
            path: manifest_path.to_owned(),
            name: source.name.clone(),
        });
    }
    Ok(())
}

fn fixture_files(path: &Path) -> Result<Vec<PathBuf>, FixtureError> {
    if path.is_file() {
        return Ok(vec![path.to_owned()]);
    }

    let mut files = fs::read_dir(path)
        .map_err(|source| FixtureError::ReadDirectory {
            path: path.to_owned(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|plugin| plugin == "json"))
        .collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return Err(FixtureError::NoFixtures(path.to_owned()));
    }
    Ok(files)
}

fn verify_case(
    profile: &CompatibilityProfile,
    path: &Path,
    case: FixtureCase,
) -> Result<FixtureCaseReport, FixtureError> {
    let file = path.display().to_string();
    match case {
        FixtureCase::CanonicalJson {
            name,
            domain,
            input,
            expected_canonical,
            expected_hash,
            ..
        } => {
            let canonical =
                String::from_utf8(canonical_json(&input).map_err(FixtureError::Canonicalize)?)
                    .expect("canonical JSON is valid UTF-8");
            let actual_hash =
                canonical_json_hash(&domain, &input).map_err(FixtureError::Canonicalize)?;
            let passed = canonical == expected_canonical && actual_hash == expected_hash;
            let message = if passed {
                "canonical bytes and hash match".to_owned()
            } else {
                format!(
                    "expected canonical '{expected_canonical}' and hash {expected_hash}, found '{canonical}' and {actual_hash}"
                )
            };
            Ok(FixtureCaseReport {
                file,
                name,
                passed,
                not_run: false,
                message,
            })
        }
        FixtureCase::ArtifactRevision {
            name,
            artifact_kind,
            source_format,
            source,
            expected_hash,
            ..
        } => {
            let actual_hash =
                artifact_revision_hash(&artifact_kind, &source_format, source.as_bytes());
            let passed = actual_hash == expected_hash;
            let message = if passed {
                "artifact revision hash matches".to_owned()
            } else {
                format!("expected {expected_hash}, found {actual_hash}")
            };
            Ok(FixtureCaseReport {
                file,
                name,
                passed,
                not_run: false,
                message,
            })
        }
        FixtureCase::CompatibilityOutcome {
            name,
            subject_kind,
            subject,
            expected_outcome,
            ..
        } => {
            let actual_outcome = profile.classify(&CompatibilitySubject {
                kind: subject_kind,
                name: subject,
            });
            let passed = actual_outcome == expected_outcome;
            let message = if passed {
                format!("compatibility outcome is {actual_outcome:?}")
            } else {
                format!("expected {expected_outcome:?}, found {actual_outcome:?}")
            };
            Ok(FixtureCaseReport {
                file,
                name,
                passed,
                not_run: false,
                message,
            })
        }
        FixtureCase::MacroRender {
            name,
            input,
            context,
            initial_local,
            initial_global,
            plugins,
            outlets,
            expected_text,
            expected_local,
            expected_global,
            expected_warnings,
            expected_error,
            ..
        } => {
            let mut macro_context = MacroContext::default();
            for (key, value) in context {
                macro_context.insert(key, value);
            }
            macro_context.plugins = plugins;
            macro_context.outlets = outlets;
            let mut state = StateTransaction::empty(EntityId::new());
            for (key, value) in initial_local {
                state.set_raw(VariableScope::Local, key, value, "fixture", "initial");
            }
            for (key, value) in initial_global {
                state.set_raw(VariableScope::Global, key, value, "fixture", "initial");
            }
            let render_result = MacroEngine::new(1).render(&input, &macro_context, &mut state);
            if let Some(expected_code) = expected_error {
                let (passed, message) = match render_result {
                    Err(error) => {
                        let display = error.to_string();
                        let passed = display.contains(&expected_code);
                        let message = if passed {
                            format!("expected error matched: {display}")
                        } else {
                            format!(
                                "expected error containing '{expected_code}', found '{display}'"
                            )
                        };
                        (passed, message)
                    }
                    Ok(rendered) => (
                        false,
                        format!(
                            "expected error containing '{expected_code}', but render succeeded with text {:?}",
                            rendered.text
                        ),
                    ),
                };
                return Ok(FixtureCaseReport {
                    file,
                    name,
                    passed,
                    not_run: false,
                    message,
                });
            }
            let rendered = render_result.map_err(FixtureError::Macro)?;
            let (actual_local, actual_global) = state_maps(&state);
            let passed = rendered.text == expected_text
                && actual_local == expected_local
                && actual_global == expected_global
                && rendered.warnings.len() == expected_warnings;
            let message = if passed {
                "macro text, state, and warnings match".to_owned()
            } else {
                format!(
                    "expected text {expected_text:?}, local {expected_local:?}, global {expected_global:?}, warnings {expected_warnings}; found text {:?}, local {:?}, global {:?}, warnings {}",
                    rendered.text,
                    actual_local,
                    actual_global,
                    rendered.warnings.len()
                )
            };
            Ok(FixtureCaseReport {
                file,
                name,
                passed,
                not_run: false,
                message,
            })
        }
        FixtureCase::LoreEvaluate {
            name,
            source_revision,
            entries,
            messages,
            tokenizer,
            settings,
            expected_activated,
            expected_outcomes,
            ..
        } => {
            let entries =
                parse_lore_entries(&source_revision, &entries, 0).map_err(FixtureError::Lore)?;
            let result = LoreEngine::with_worker(
                tokenizer,
                crate::EcmaRegexWorker::new("unused", std::time::Duration::from_secs(1)),
            )
            .evaluate_in_process(&entries, &messages, &settings)
            .map_err(FixtureError::Lore)?;
            let actual_activated = result
                .activated
                .iter()
                .filter_map(|entry| {
                    entry
                        .entry_key
                        .rsplit_once('.')
                        .map(|(_, id)| id.to_owned())
                })
                .collect::<Vec<_>>();
            let mut actual_outcomes = BTreeMap::new();
            for decision in result.decisions {
                if let Some((_, id)) = decision.entry_key.rsplit_once('.') {
                    actual_outcomes.insert(id.to_owned(), decision.outcome);
                }
            }
            let passed =
                actual_activated == expected_activated && actual_outcomes == expected_outcomes;
            let message = if passed {
                "lore activations and final decisions match".to_owned()
            } else {
                format!(
                    "expected activated {expected_activated:?} and outcomes {expected_outcomes:?}; found activated {actual_activated:?} and outcomes {actual_outcomes:?}"
                )
            };
            Ok(FixtureCaseReport {
                file,
                name,
                passed,
                not_run: false,
                message,
            })
        }
        FixtureCase::RegexScriptApply {
            name,
            scripts,
            placement,
            depth,
            input,
            expected_text,
            ..
        } => {
            let parsed: Vec<RegexScript> = scripts
                .iter()
                .map(RegexScript::from_value)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| FixtureError::InvalidRegexScript {
                    case: name.to_owned(),
                })?;
            let channel = match placement {
                1 => RegexPlacement::UserInput,
                2 => RegexPlacement::AiOutput,
                3 => RegexPlacement::SlashCommand,
                5 => RegexPlacement::WorldInfo,
                6 => RegexPlacement::Reasoning,
                code => {
                    return Ok(FixtureCaseReport {
                        file,
                        name,
                        passed: false,
                        not_run: false,
                        message: format!("unknown placement code {code}"),
                    });
                }
            };
            let (actual_text, _) = crate::regex_script::apply_scripts(
                &parsed,
                channel,
                depth,
                &input,
                &mut |pattern, flags, text| match crate::run_replace_worker(
                    crate::RegexReplaceRequest {
                        pattern: pattern.to_owned(),
                        flags: flags.chars().filter(|f| *f != 'g').collect(),
                        global: flags.contains('g'),
                        text: text.to_owned(),
                    },
                ) {
                    crate::RegexReplaceResponse::Matches { matches } => Ok(matches),
                    crate::RegexReplaceResponse::Error { message } => {
                        Err(EcmaRegexError::Pattern(message))
                    }
                },
                &mut None::<fn(&str, bool) -> String>,
            )
            .map_err(FixtureError::Regex)?;
            let passed = actual_text == expected_text;
            let message = if passed {
                "regex script output matches".to_owned()
            } else {
                format!("expected {expected_text:?}, found {actual_text:?}")
            };
            Ok(FixtureCaseReport {
                file,
                name,
                passed,
                not_run: false,
                message,
            })
        }
        FixtureCase::ProviderRequestParity(case) => Ok(FixtureCaseReport {
            file,
            name: case.name,
            passed: false,
            not_run: true,
            message: "not run: provider-request parity requires the Dry Run fixture runner"
                .to_owned(),
        }),
    }
}

fn expand_macro_group_case(group: &MacroGroup, case: &MacroGroupCase) -> FixtureCase {
    FixtureCase::MacroRender {
        id: Some(case.id.clone()),
        name: case.id.clone(),
        input: case.input.clone(),
        context: group.defaults.context.clone(),
        initial_local: group.defaults.initial_local.clone(),
        initial_global: group.defaults.initial_global.clone(),
        plugins: group.defaults.plugins.clone(),
        outlets: group.defaults.outlets.clone(),
        expected_text: case.expected_text.clone(),
        expected_local: case
            .expected_local
            .clone()
            .unwrap_or_else(|| group.defaults.initial_local.clone()),
        expected_global: case
            .expected_global
            .clone()
            .unwrap_or_else(|| group.defaults.initial_global.clone()),
        expected_warnings: case
            .expected_warnings
            .or(group.defaults.expected_warnings)
            .unwrap_or(0),
        expected_error: None,
    }
}

fn state_maps(state: &StateTransaction) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let mut local = BTreeMap::new();
    let mut global = BTreeMap::new();
    for mutation in state.mutations() {
        if let Some(cell) = mutation.after {
            match cell.key.scope {
                VariableScope::Local => {
                    local.insert(cell.key.name, cell.raw_value);
                }
                VariableScope::Global => {
                    global.insert(cell.key.name, cell.raw_value);
                }
            }
        }
    }
    (local, global)
}

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("compatibility profile is invalid: {0}")]
    Profile(ProfileError),
    #[error("failed to read fixture directory '{path}': {source}")]
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("fixture path '{0}' contains no JSON fixtures")]
    NoFixtures(PathBuf),
    #[error("failed to read fixture '{path}': {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to decode fixture '{path}': {source}")]
    Decode {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("fixture '{path}' uses unsupported schema '{schema}'")]
    UnsupportedSchema { path: PathBuf, schema: String },
    #[error("fixture '{path}' targets profile '{actual}', expected '{expected}'")]
    ProfileMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    #[error("fixture '{path}' has invalid external source '{name}'")]
    InvalidExternalSource { path: PathBuf, name: String },
    #[error("failed to read external fixture '{name}' at '{path}': {source}")]
    ReadExternal {
        name: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("external fixture '{name}' digest mismatch: expected {expected}, found {actual}")]
    ExternalDigestMismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("macro fixture failed: {0}")]
    Macro(MacroError),
    #[error("lore fixture failed: {0}")]
    Lore(LoreError),
    #[error("regex fixture failed: {0}")]
    Regex(EcmaRegexError),
    #[error("regex fixture '{case}' contains a script with no findRegex")]
    InvalidRegexScript { case: String },
    #[error("failed to canonicalize fixture JSON: {0}")]
    Canonicalize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    use crate::profile::{MacroManifest, ProfileScope, UpstreamRevision};

    #[test]
    fn checked_in_compatibility_files_match_their_schemas() {
        let schema = serde_json::from_str::<Value>(include_str!(
            "../../../schemas/fixture-suite.schema.json"
        ))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for source in [
            include_str!("../../../compat/fixtures/phase0-contracts.json"),
            include_str!("../../../compat/fixtures/phase3-lore.json"),
            include_str!("../../../compat/fixtures/phase3-macros.json"),
            include_str!("../../../compat/fixtures/phase3-unsupported.json"),
            include_str!("../../../compat/fixtures/phase3-regex.json"),
            include_str!("../../../compat/fixtures/phase4-preset-parity.json"),
        ] {
            let fixture = serde_json::from_str::<Value>(source).unwrap();
            validator.validate(&fixture).unwrap();
        }
        let profile_schema = serde_json::from_str::<Value>(include_str!(
            "../../../schemas/compat-profile.schema.json"
        ))
        .unwrap();
        let profile = serde_json::from_str::<Value>(include_str!(
            "../../../compat/profiles/sillytavern-1.18-core.json"
        ))
        .unwrap();
        jsonschema::validator_for(&profile_schema)
            .unwrap()
            .validate(&profile)
            .unwrap();
    }

    #[test]
    fn fixture_report_includes_failures_without_hiding_them() {
        let directory = tempdir().unwrap();
        let profile_path = directory.path().join("profile.json");
        let fixture_path = directory.path().join("fixture.json");
        let profile = CompatibilityProfile {
            schema: "stcli.compat-profile/v1".to_owned(),
            id: "fixture-profile".to_owned(),
            upstream: UpstreamRevision {
                repository: "https://example.invalid".to_owned(),
                tag: "1.0.0".to_owned(),
                commit: "0000000000000000000000000000000000000000".to_owned(),
                version: "1.0.0".to_owned(),
            },
            scope: ProfileScope {
                formats: vec![],
                prompt_path: "test".to_owned(),
                generation_types: vec![],
                documented_fallback: vec![],
                hard_unsupported: vec![],
                preserved_metadata: vec![],
            },
            macros: MacroManifest {
                exact: vec![],
                documented_fallback: BTreeMap::new(),
                hard_unsupported: vec![],
                source_files: vec![],
            },
            preset_fields: BTreeMap::new(),
        };
        fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();
        let suite = FixtureSuite {
            schema: FIXTURE_SCHEMA_V1.to_owned(),
            profile: profile.id,
            external_sources: vec![ExternalFixtureSource {
                name: "restricted preset".to_owned(),
                provenance: "local acquisition".to_owned(),
                revision: Some("1".to_owned()),
                sha256: "0".repeat(64),
                path_environment: "STCLI_TEST_MISSING_EXTERNAL_FIXTURE".to_owned(),
                repository_path: None,
            }],
            cases: vec![FixtureCase::ArtifactRevision {
                id: None,
                name: "intentional mismatch".to_owned(),
                artifact_kind: "card".to_owned(),
                source_format: "json".to_owned(),
                source: "{}".to_owned(),
                expected_hash: ContentHash::new([0; 32]),
            }],
            macro_groups: Vec::new(),
        };
        fs::write(&fixture_path, serde_json::to_vec(&suite).unwrap()).unwrap();

        let report = verify_fixture_suite(profile_path, fixture_path).unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.passed, 0);
        assert_eq!(report.not_run, 1);
        assert!(!report.is_success());
    }

    #[test]
    fn fixture_report_treats_not_run_as_failure() {
        let report = FixtureReport {
            profile: "fixture-profile".to_owned(),
            files: 1,
            total: 1,
            passed: 0,
            not_run: 1,
            cases: vec![],
        };

        assert!(!report.is_success());
    }

    #[test]
    fn external_fixture_uses_repository_path_without_environment_override() {
        let directory = tempdir().unwrap();
        let source_path = directory.path().join("external.json");
        fs::write(&source_path, b"fixture").unwrap();
        let source = ExternalFixtureSource {
            name: "external".to_owned(),
            provenance: "fixture".to_owned(),
            revision: Some("1".to_owned()),
            sha256: format!("{:x}", Sha256::digest(b"fixture")),
            path_environment: "STCLI_TEST_REPOSITORY_EXTERNAL_FIXTURE".to_owned(),
            repository_path: Some(source_path.display().to_string()),
        };

        let report =
            verify_external_source(&directory.path().join("manifest.json"), &source).unwrap();

        assert!(report.passed);
        assert!(!report.not_run);
    }

    #[test]
    fn external_fixture_digest_mismatch_fails_before_cases() {
        let directory = tempdir().unwrap();
        let source_path = directory.path().join("external.json");
        fs::write(&source_path, b"changed").unwrap();
        let source = ExternalFixtureSource {
            name: "external".to_owned(),
            provenance: "fixture".to_owned(),
            revision: None,
            sha256: "0".repeat(64),
            path_environment: "STCLI_TEST_UNUSED_EXTERNAL_FIXTURE".to_owned(),
            repository_path: Some(source_path.display().to_string()),
        };

        let error =
            verify_external_source(&directory.path().join("manifest.json"), &source).unwrap_err();

        assert!(matches!(error, FixtureError::ExternalDigestMismatch { .. }));
    }
}
