use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

use crate::{
    ContentHash, EntityId, InstalledPlugin, PluginCapability, PluginCommandResult, PluginError,
    PluginLimits, PluginManifest, PluginRuntime, canonical_json_hash, decode_unique_json,
};

const INTERACTION_SCHEMA: &str = "stcli.interaction/v1";
const DECLARATION_DOMAIN: &str = "stcli:interaction-declaration:v1";
const SURFACE_DOMAIN: &str = "stcli:interaction-surface:v1";
const TARGET_DOMAIN: &str = "stcli:interaction-target:v1";
const REVISION_DOMAIN: &str = "stcli:interaction-revision:v1";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum InteractionValue {
    Boolean(bool),
    Number(Number),
    Text(String),
}

impl InteractionValue {
    fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::Bool(value) => Some(Self::Boolean(*value)),
            Value::Number(value) => Some(Self::Number(value.clone())),
            Value::String(value) => Some(Self::Text(value.clone())),
            _ => None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Boolean(value) => Value::Bool(*value),
            Self::Number(value) => Value::Number(value.clone()),
            Self::Text(value) => Value::String(value.clone()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "submission", rename_all = "kebab-case")]
pub enum InteractionSubmission {
    Save {
        target: ContentHash,
        edits: Vec<InteractionEdit>,
    },
    Invoke {
        target: ContentHash,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InteractionEdit {
    pub target: ContentHash,
    pub value: InteractionValue,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum InteractionOutcome {
    Saved { configuration_revision: ContentHash },
    Invoked { result: Box<PluginCommandResult> },
    Rejected { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionResult {
    pub surface: InteractionSurface,
    pub outcome: InteractionOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InteractionControl {
    Boolean,
    Number,
    Text,
    MultilineText,
    Choice,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionChoice {
    pub value: InteractionValue,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionConstraints {
    pub integer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<Number>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionField {
    pub target: ContentHash,
    pub label: String,
    pub help: String,
    pub control: InteractionControl,
    pub value: Option<InteractionValue>,
    pub constraints: InteractionConstraints,
    pub choices: Vec<InteractionChoice>,
    pub visible: bool,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionGroup {
    pub label: String,
    pub help: String,
    pub fields: Vec<InteractionField>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionAction {
    pub target: ContentHash,
    pub label: String,
    pub help: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionSurface {
    pub session_id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<EntityId>,
    pub extension_id: String,
    pub package_version: String,
    pub component_sha256: ContentHash,
    pub declaration_hash: ContentHash,
    pub id: ContentHash,
    pub revision: ContentHash,
    pub label: String,
    pub help: String,
    pub groups: Vec<InteractionGroup>,
    pub save: InteractionAction,
    pub actions: Vec<InteractionAction>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct InteractionDeclaration {
    pub(crate) id: String,
    pub(crate) hash: ContentHash,
    fields: BTreeMap<String, DeclaredField>,
    groups: Vec<DeclaredGroup>,
    actions: Vec<DeclaredAction>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DeclaredField {
    property: String,
    label: String,
    help: String,
    control: InteractionControl,
    kind: FieldKind,
    default: InteractionValue,
    minimum: Option<Number>,
    maximum: Option<Number>,
    choices: Vec<DeclaredChoice>,
    choice_source: Option<ChoiceSource>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum FieldKind {
    Boolean,
    Integer,
    Number,
    String,
}

#[derive(Clone, Debug, Serialize)]
struct DeclaredChoice {
    value: InteractionValue,
    label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ChoiceSource {
    ProviderProfiles,
}

#[derive(Clone, Debug, Serialize)]
struct DeclaredGroup {
    label: String,
    help: String,
    fields: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DeclaredAction {
    id: String,
    label: String,
    help: String,
    command: String,
    requires_branch: bool,
    requires_completed_attempt: bool,
    capabilities: BTreeSet<PluginCapability>,
}

#[derive(Deserialize)]
struct Annotation {
    schema: String,
    id: String,
    component_sha256: ContentHash,
    groups: Vec<GroupSource>,
    actions: Vec<ActionSource>,
}

#[derive(Deserialize)]
struct GroupSource {
    label: String,
    help: String,
    fields: Vec<String>,
}

#[derive(Deserialize)]
struct ActionSource {
    id: String,
    label: String,
    help: String,
    command: String,
    requires_branch: bool,
    requires_completed_attempt: bool,
    capabilities: BTreeSet<PluginCapability>,
}

pub(crate) fn load_interaction_declaration(
    installed: &InstalledPlugin,
) -> Result<Option<InteractionDeclaration>, PluginError> {
    load_declaration(&installed.manifest, &installed.directory)
}

pub(crate) fn load_declaration(
    manifest: &PluginManifest,
    directory: &Path,
) -> Result<Option<InteractionDeclaration>, PluginError> {
    let Some(schema_name) = &manifest.settings_schema else {
        return Ok(None);
    };
    let root = fs::canonicalize(directory).map_err(|source| PluginError::Read {
        path: directory.to_owned(),
        source,
    })?;
    let schema_path = checked_child(&root, schema_name)?;
    let canonical_schema = fs::canonicalize(&schema_path).map_err(|source| PluginError::Read {
        path: schema_path,
        source,
    })?;
    if !canonical_schema.starts_with(&root) {
        return Err(PluginError::UnsafePath(schema_name.clone()));
    }
    let source = fs::read(&canonical_schema).map_err(|source| PluginError::Read {
        path: canonical_schema,
        source,
    })?;
    if source.len() > PluginLimits::default().input_bytes {
        return Err(PluginError::InputLimit);
    }
    let value = decode_unique_json(&source).map_err(PluginError::Artifact)?;
    let Some(annotation_value) = value.get("x-stcli-interaction") else {
        return Ok(None);
    };
    if manifest.runtime != PluginRuntime::StBridge {
        return Err(invalid("declared interactions require st-bridge runtime"));
    }
    if value.get("type").and_then(Value::as_str) != Some("object") {
        return Err(invalid("declared interaction schema must be an object"));
    }
    let properties = value
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("declared interaction schema requires object properties"))?;
    let annotation: Annotation = serde_json::from_value(annotation_value.clone())
        .map_err(|error| invalid(format!("invalid interaction annotation: {error}")))?;
    if annotation.schema != INTERACTION_SCHEMA {
        return Err(invalid(format!(
            "unsupported interaction schema '{}'",
            annotation.schema
        )));
    }
    if annotation.component_sha256 != manifest.component_sha256 {
        return Err(PluginError::DigestMismatch);
    }
    if annotation.id.trim().is_empty() {
        return Err(invalid("interaction id must not be empty"));
    }

    let mut fields = BTreeMap::new();
    let mut grouped = BTreeSet::new();
    let groups = annotation
        .groups
        .into_iter()
        .map(|group| {
            if group.label.trim().is_empty() || group.help.trim().is_empty() {
                return Err(invalid("interaction groups require label and help"));
            }
            for property in &group.fields {
                if !grouped.insert(property.clone()) {
                    return Err(invalid(format!("duplicate interaction field '{property}'")));
                }
                let source = properties.get(property).ok_or_else(|| {
                    invalid(format!("interaction field '{property}' is not a property"))
                })?;
                fields.insert(property.clone(), parse_field(property, source)?);
            }
            Ok(DeclaredGroup {
                label: group.label,
                help: group.help,
                fields: group.fields,
            })
        })
        .collect::<Result<Vec<_>, PluginError>>()?;

    let mut action_ids = BTreeSet::new();
    let actions = annotation
        .actions
        .into_iter()
        .map(|action| {
            if !action_ids.insert(action.id.clone()) {
                return Err(invalid(format!(
                    "duplicate interaction action '{}'",
                    action.id
                )));
            }
            if action.id.trim().is_empty()
                || action.label.trim().is_empty()
                || action.help.trim().is_empty()
            {
                return Err(invalid("interaction actions require id, label, and help"));
            }
            if !manifest.commands.contains(&action.command) {
                return Err(invalid(format!(
                    "interaction action '{}' binds undeclared command '{}'",
                    action.id, action.command
                )));
            }
            if !action
                .capabilities
                .is_subset(&manifest.requested_capabilities)
            {
                return Err(invalid(format!(
                    "interaction action '{}' requires undeclared capabilities",
                    action.id
                )));
            }
            Ok(DeclaredAction {
                id: action.id,
                label: action.label,
                help: action.help,
                command: action.command,
                requires_branch: action.requires_branch,
                requires_completed_attempt: action.requires_completed_attempt,
                capabilities: action.capabilities,
            })
        })
        .collect::<Result<Vec<_>, PluginError>>()?;

    let declaration_value = json!({
        "schema": INTERACTION_SCHEMA,
        "id": annotation.id,
        "component_sha256": manifest.component_sha256,
        "groups": groups,
        "fields": fields,
        "actions": actions,
    });
    let hash = canonical_json_hash(DECLARATION_DOMAIN, &declaration_value)?;
    Ok(Some(InteractionDeclaration {
        id: declaration_value["id"].as_str().unwrap().to_owned(),
        hash,
        fields,
        groups,
        actions,
    }))
}

fn checked_child(root: &Path, relative: &str) -> Result<std::path::PathBuf, PluginError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(PluginError::UnsafePath(relative.to_owned()));
    }
    Ok(root.join(path))
}

fn parse_field(property: &str, source: &Value) -> Result<DeclaredField, PluginError> {
    let object = source
        .as_object()
        .ok_or_else(|| invalid(format!("interaction field '{property}' must be an object")))?;
    let allowed = BTreeSet::from([
        "type",
        "minimum",
        "maximum",
        "enum",
        "default",
        "title",
        "description",
        "x-stcli-control",
        "x-stcli-choice-labels",
        "x-stcli-choice-source",
    ]);
    if let Some(keyword) = object.keys().find(|key| !allowed.contains(key.as_str())) {
        return Err(invalid(format!(
            "interaction field '{property}' uses unsupported keyword '{keyword}'"
        )));
    }
    let kind = match required_string(object, "type", property)? {
        "boolean" => FieldKind::Boolean,
        "integer" => FieldKind::Integer,
        "number" => FieldKind::Number,
        "string" => FieldKind::String,
        other => {
            return Err(invalid(format!(
                "interaction field '{property}' has unsupported type '{other}'"
            )));
        }
    };
    let control = match required_string(object, "x-stcli-control", property)? {
        "boolean" => InteractionControl::Boolean,
        "number" => InteractionControl::Number,
        "text" => InteractionControl::Text,
        "multiline-text" => InteractionControl::MultilineText,
        "choice" => InteractionControl::Choice,
        other => {
            return Err(invalid(format!(
                "interaction field '{property}' has unsupported control '{other}'"
            )));
        }
    };
    let compatible = matches!(
        (kind, control),
        (FieldKind::Boolean, InteractionControl::Boolean)
            | (
                FieldKind::Integer | FieldKind::Number,
                InteractionControl::Number
            )
            | (
                FieldKind::String,
                InteractionControl::Text | InteractionControl::MultilineText
            )
            | (_, InteractionControl::Choice)
    );
    if !compatible {
        return Err(invalid(format!(
            "interaction field '{property}' control does not match its type"
        )));
    }
    let default_json = object
        .get("default")
        .ok_or_else(|| invalid(format!("interaction field '{property}' requires default")))?;
    validate_kind(property, kind, default_json)?;
    let default = InteractionValue::from_json(default_json).ok_or_else(|| {
        invalid(format!(
            "interaction field '{property}' default must be scalar"
        ))
    })?;
    let minimum = optional_number(object, "minimum", property)?;
    let maximum = optional_number(object, "maximum", property)?;
    if matches!(kind, FieldKind::Boolean | FieldKind::String)
        && (minimum.is_some() || maximum.is_some())
    {
        return Err(invalid(format!(
            "interaction field '{property}' has numeric constraints on a non-number"
        )));
    }
    validate_bounds(
        property,
        kind,
        default_json,
        minimum.as_ref(),
        maximum.as_ref(),
    )?;

    let enum_values = object
        .get("enum")
        .map(|value| {
            value.as_array().ok_or_else(|| {
                invalid(format!(
                    "interaction field '{property}' enum must be an array"
                ))
            })
        })
        .transpose()?;
    let labels = object
        .get("x-stcli-choice-labels")
        .map(|value| {
            value.as_array().ok_or_else(|| {
                invalid(format!(
                    "interaction field '{property}' choice labels must be an array"
                ))
            })
        })
        .transpose()?;
    let source_name = object.get("x-stcli-choice-source").and_then(Value::as_str);
    let choice_source = match source_name {
        None => None,
        Some("provider-profiles") if kind == FieldKind::String => {
            Some(ChoiceSource::ProviderProfiles)
        }
        Some(other) => {
            return Err(invalid(format!(
                "interaction field '{property}' has unsupported choice source '{other}'"
            )));
        }
    };
    if control != InteractionControl::Choice
        && (enum_values.is_some() || labels.is_some() || choice_source.is_some())
    {
        return Err(invalid(format!(
            "interaction field '{property}' has choice metadata for a non-choice control"
        )));
    }
    if enum_values.is_some() && choice_source.is_some() {
        return Err(invalid(format!(
            "interaction field '{property}' mixes static and dynamic choices"
        )));
    }
    let choices = match (enum_values, labels) {
        (Some(values), Some(labels)) if values.len() == labels.len() => values
            .iter()
            .zip(labels)
            .map(|(value, label)| {
                validate_kind(property, kind, value)?;
                let value = InteractionValue::from_json(value).ok_or_else(|| {
                    invalid(format!(
                        "interaction field '{property}' enum values must be scalar"
                    ))
                })?;
                let label = label
                    .as_str()
                    .filter(|label| !label.trim().is_empty())
                    .ok_or_else(|| {
                        invalid(format!(
                            "interaction field '{property}' choice labels must be strings"
                        ))
                    })?;
                Ok(DeclaredChoice {
                    value,
                    label: label.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, PluginError>>()?,
        (None, None) => Vec::new(),
        _ => {
            return Err(invalid(format!(
                "interaction field '{property}' requires matching enum values and choice labels"
            )));
        }
    };
    if control == InteractionControl::Choice && choices.is_empty() && choice_source.is_none() {
        return Err(invalid(format!(
            "interaction field '{property}' choice has no source"
        )));
    }
    if !choices.is_empty() && !choices.iter().any(|choice| choice.value == default) {
        return Err(invalid(format!(
            "interaction field '{property}' default is not a declared choice"
        )));
    }

    Ok(DeclaredField {
        property: property.to_owned(),
        label: required_string(object, "title", property)?.to_owned(),
        help: required_string(object, "description", property)?.to_owned(),
        control,
        kind,
        default,
        minimum,
        maximum,
        choices,
        choice_source,
    })
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    property: &str,
) -> Result<&'a str, PluginError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            invalid(format!(
                "interaction field '{property}' requires string '{key}'"
            ))
        })
}

fn optional_number(
    object: &Map<String, Value>,
    key: &str,
    property: &str,
) -> Result<Option<Number>, PluginError> {
    object
        .get(key)
        .map(|value| {
            value.as_number().cloned().ok_or_else(|| {
                invalid(format!(
                    "interaction field '{property}' {key} must be a number"
                ))
            })
        })
        .transpose()
}

fn validate_kind(property: &str, kind: FieldKind, value: &Value) -> Result<(), PluginError> {
    let valid = match kind {
        FieldKind::Boolean => value.is_boolean(),
        FieldKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        FieldKind::Number => value.is_number(),
        FieldKind::String => value.is_string(),
    };
    if valid {
        Ok(())
    } else {
        Err(invalid(format!(
            "interaction field '{property}' value does not match its type"
        )))
    }
}

fn validate_bounds(
    property: &str,
    kind: FieldKind,
    value: &Value,
    minimum: Option<&Number>,
    maximum: Option<&Number>,
) -> Result<(), PluginError> {
    if !matches!(kind, FieldKind::Integer | FieldKind::Number) {
        return Ok(());
    }
    let value = value
        .as_f64()
        .ok_or_else(|| invalid(format!("interaction field '{property}' must be numeric")))?;
    if minimum
        .and_then(Number::as_f64)
        .is_some_and(|minimum| value < minimum)
        || maximum
            .and_then(Number::as_f64)
            .is_some_and(|maximum| value > maximum)
    {
        return Err(invalid(format!(
            "interaction field '{property}' default is outside its bounds"
        )));
    }
    Ok(())
}

pub(crate) fn effective_settings(
    declaration: &InteractionDeclaration,
    persisted: &Value,
    pinned: &Value,
) -> Result<Value, PluginError> {
    let mut effective = match persisted {
        Value::Object(settings) => settings.clone(),
        Value::Null => match pinned {
            Value::Object(settings) => settings.clone(),
            Value::Null => Map::new(),
            _ => {
                return Err(invalid(
                    "pinned Extension settings must be an object or null",
                ));
            }
        },
        _ => {
            return Err(invalid(
                "persisted Extension settings must be an object or null",
            ));
        }
    };
    for field in declaration.fields.values() {
        effective
            .entry(field.property.clone())
            .or_insert_with(|| field.default.to_json());
    }
    let pinned = match pinned {
        Value::Object(settings) => settings,
        Value::Null => return Ok(Value::Object(effective)),
        _ => {
            return Err(invalid(
                "pinned Extension settings must be an object or null",
            ));
        }
    };
    for property in declaration.fields.keys() {
        if let Some(value) = pinned.get(property) {
            effective.insert(property.clone(), value.clone());
        }
    }
    Ok(Value::Object(effective))
}

pub(crate) fn surface_id(
    session_id: EntityId,
    branch_id: Option<EntityId>,
    installed: &InstalledPlugin,
    declaration: &InteractionDeclaration,
) -> Result<ContentHash, PluginError> {
    Ok(canonical_json_hash(
        SURFACE_DOMAIN,
        &json!({
            "session_id": session_id,
            "branch_id": branch_id,
            "extension_id": installed.manifest.id,
            "package_version": installed.manifest.version,
            "component_sha256": installed.manifest.component_sha256,
            "declaration_hash": declaration.hash,
            "declaration_id": declaration.id,
        }),
    )?)
}

fn target_id(
    surface: &ContentHash,
    category: &str,
    local_id: &str,
) -> Result<ContentHash, PluginError> {
    Ok(canonical_json_hash(
        TARGET_DOMAIN,
        &json!({
            "surface_id": surface,
            "category": category,
            "id": local_id,
        }),
    )?)
}

pub(crate) struct SurfaceBuild<'a> {
    pub session_id: EntityId,
    pub branch_id: Option<EntityId>,
    pub configuration_revision: &'a ContentHash,
    pub installed: &'a InstalledPlugin,
    pub declaration: &'a InteractionDeclaration,
    pub pinned_settings: &'a Value,
    pub persisted_settings: &'a Value,
    pub enabled: bool,
    pub capabilities: &'a BTreeSet<PluginCapability>,
    pub provider_profiles: &'a [String],
    pub completed_attempt: bool,
}

pub(crate) fn build_surface(input: SurfaceBuild<'_>) -> Result<InteractionSurface, PluginError> {
    let effective = effective_settings(
        input.declaration,
        input.persisted_settings,
        input.pinned_settings,
    )?;
    let effective_object = effective
        .as_object()
        .expect("effective settings are object");
    let id = surface_id(
        input.session_id,
        input.branch_id,
        input.installed,
        input.declaration,
    )?;
    let disabled_reason = (!input.enabled).then(|| "Extension is disabled".to_owned());
    let can_save = input.enabled
        && input
            .capabilities
            .contains(&PluginCapability::WriteOwnState);
    let save_reason = disabled_reason.clone().or_else(|| {
        (!input
            .capabilities
            .contains(&PluginCapability::WriteOwnState))
        .then(|| "Extension does not grant write-own-state".to_owned())
    });
    let groups = input
        .declaration
        .groups
        .iter()
        .map(|group| {
            let fields = group
                .fields
                .iter()
                .map(|property| {
                    let field = &input.declaration.fields[property];
                    let value_json = effective_object.get(property).expect("defaults filled");
                    let mut choices = field
                        .choices
                        .iter()
                        .map(|choice| InteractionChoice {
                            value: choice.value.clone(),
                            label: choice.label.clone(),
                        })
                        .collect::<Vec<_>>();
                    if field.choice_source == Some(ChoiceSource::ProviderProfiles) {
                        choices.push(InteractionChoice {
                            value: InteractionValue::Text(String::new()),
                            label: "Use Session provider".to_owned(),
                        });
                        choices.extend(input.provider_profiles.iter().map(|name| {
                            InteractionChoice {
                                value: InteractionValue::Text(name.clone()),
                                label: name.clone(),
                            }
                        }));
                    }
                    let mut error = validate_field_value(field, value_json, &choices).err();
                    if field.choice_source == Some(ChoiceSource::ProviderProfiles)
                        && let Some(InteractionValue::Text(current)) =
                            InteractionValue::from_json(value_json)
                        && !choices
                            .iter()
                            .any(|choice| choice.value == InteractionValue::Text(current.clone()))
                    {
                        error = Some("Provider profile is not configured".to_owned());
                        choices.push(InteractionChoice {
                            value: InteractionValue::Text(current.clone()),
                            label: format!("{current} (unavailable)"),
                        });
                    }
                    Ok(InteractionField {
                        target: target_id(&id, "field", property)?,
                        label: field.label.clone(),
                        help: field.help.clone(),
                        control: field.control,
                        value: InteractionValue::from_json(value_json),
                        constraints: InteractionConstraints {
                            integer: field.kind == FieldKind::Integer,
                            minimum: field.minimum.clone(),
                            maximum: field.maximum.clone(),
                        },
                        choices,
                        visible: true,
                        enabled: can_save,
                        unavailable_reason: save_reason.clone(),
                        error,
                    })
                })
                .collect::<Result<Vec<_>, PluginError>>()?;
            Ok(InteractionGroup {
                label: group.label.clone(),
                help: group.help.clone(),
                fields,
            })
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    let actions = input
        .declaration
        .actions
        .iter()
        .map(|action| {
            let reason = disabled_reason
                .clone()
                .or_else(|| {
                    (!action.capabilities.is_subset(input.capabilities))
                        .then(|| "Extension action capabilities are not granted".to_owned())
                })
                .or_else(|| {
                    (action.requires_branch && input.branch_id.is_none())
                        .then(|| "Action requires a Branch".to_owned())
                })
                .or_else(|| {
                    (action.requires_completed_attempt && !input.completed_attempt)
                        .then(|| "Action requires a completed Primary Attempt".to_owned())
                })
                .or_else(|| {
                    (!cfg!(feature = "scripting"))
                        .then(|| "Extension execution requires scripting support".to_owned())
                });
            Ok(InteractionAction {
                target: target_id(&id, "action", &action.id)?,
                label: action.label.clone(),
                help: action.help.clone(),
                enabled: reason.is_none(),
                unavailable_reason: reason,
            })
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    let save = InteractionAction {
        target: target_id(&id, "save", &input.declaration.id)?,
        label: "Save".to_owned(),
        help: "Save edited values as a new Session Configuration Revision.".to_owned(),
        enabled: can_save,
        unavailable_reason: save_reason,
    };
    let revision = canonical_json_hash(
        REVISION_DOMAIN,
        &json!({
            "surface_id": id,
            "configuration_revision": input.configuration_revision,
            "values": effective_object,
            "groups": groups,
            "save": save,
            "actions": actions,
        }),
    )?;
    Ok(InteractionSurface {
        session_id: input.session_id,
        branch_id: input.branch_id,
        extension_id: input.installed.manifest.id.clone(),
        package_version: input.installed.manifest.version.to_string(),
        component_sha256: input.installed.manifest.component_sha256.clone(),
        declaration_hash: input.declaration.hash.clone(),
        id,
        revision,
        label: input
            .installed
            .manifest
            .display_name
            .clone()
            .unwrap_or_else(|| input.installed.manifest.id.clone()),
        help: "Edit saved Extension settings and run declared actions.".to_owned(),
        groups,
        save,
        actions,
    })
}

fn validate_field_value(
    field: &DeclaredField,
    value: &Value,
    choices: &[InteractionChoice],
) -> Result<(), String> {
    validate_kind(&field.property, field.kind, value)
        .map_err(|error| bounded(error.to_string()))?;
    validate_bounds(
        &field.property,
        field.kind,
        value,
        field.minimum.as_ref(),
        field.maximum.as_ref(),
    )
    .map_err(|error| bounded(error.to_string()))?;
    if field.control == InteractionControl::Choice {
        let value =
            InteractionValue::from_json(value).ok_or_else(|| "Value must be scalar".to_owned())?;
        if !choices.iter().any(|choice| choice.value == value) {
            return Err("Value is not an available choice".to_owned());
        }
    }
    Ok(())
}

pub(crate) fn find_field<'a>(
    declaration: &'a InteractionDeclaration,
    surface: &ContentHash,
    target: &ContentHash,
) -> Result<Option<&'a DeclaredField>, PluginError> {
    for (property, field) in &declaration.fields {
        if target_id(surface, "field", property)? == *target {
            return Ok(Some(field));
        }
    }
    Ok(None)
}

pub(crate) fn find_action<'a>(
    declaration: &'a InteractionDeclaration,
    surface: &ContentHash,
    target: &ContentHash,
) -> Result<Option<&'a DeclaredAction>, PluginError> {
    for action in &declaration.actions {
        if target_id(surface, "action", &action.id)? == *target {
            return Ok(Some(action));
        }
    }
    Ok(None)
}

pub(crate) fn is_save_target(
    declaration: &InteractionDeclaration,
    surface: &ContentHash,
    target: &ContentHash,
) -> Result<bool, PluginError> {
    Ok(target_id(surface, "save", &declaration.id)? == *target)
}

pub(crate) fn validate_edit(
    field: &DeclaredField,
    value: &InteractionValue,
    choices: &[InteractionChoice],
) -> Result<(), String> {
    validate_field_value(field, &value.to_json(), choices)
}

pub(crate) fn field_property(field: &DeclaredField) -> &str {
    &field.property
}

pub(crate) fn action_command(action: &DeclaredAction) -> &str {
    &action.command
}

fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::InvalidManifest(bounded(message.into()))
}

pub(crate) fn bounded(message: impl Into<String>) -> String {
    message.into().chars().take(256).collect()
}
