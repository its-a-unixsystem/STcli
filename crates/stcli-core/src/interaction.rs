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
    OrderedList(Vec<InteractionListItem>),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InteractionListItem {
    pub id: String,
    pub values: BTreeMap<String, InteractionValue>,
}

impl InteractionValue {
    fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::Bool(value) => Some(Self::Boolean(*value)),
            Value::Number(value) => Some(Self::Number(value.clone())),
            Value::String(value) => Some(Self::Text(value.clone())),
            Value::Array(items) => items
                .iter()
                .map(InteractionListItem::from_json)
                .collect::<Option<Vec<_>>>()
                .map(Self::OrderedList),
            _ => None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        match self {
            Self::Boolean(value) => Value::Bool(*value),
            Self::Number(value) => Value::Number(value.clone()),
            Self::Text(value) => Value::String(value.clone()),
            Self::OrderedList(items) => {
                Value::Array(items.iter().map(InteractionListItem::to_json).collect())
            }
        }
    }
}

impl InteractionListItem {
    fn from_json(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let id = object.get("id")?.as_str()?.to_owned();
        let values = object
            .iter()
            .filter(|(key, _)| key.as_str() != "id")
            .map(|(key, value)| Some((key.clone(), InteractionValue::from_json(value)?)))
            .collect::<Option<BTreeMap<_, _>>>()?;
        Some(Self { id, values })
    }

    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("id".to_owned(), Value::String(self.id.clone()));
        object.extend(
            self.values
                .iter()
                .map(|(key, value)| (key.clone(), value.to_json())),
        );
        Value::Object(object)
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InteractionIdentity {
    pub session_id: EntityId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<EntityId>,
    pub extension_id: String,
    pub package_version: String,
    pub component_sha256: ContentHash,
    pub declaration_hash: ContentHash,
    pub surface_id: ContentHash,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InteractionSupport {
    Available,
    PartiallyAvailable,
    Unavailable,
    Unverified,
}

impl InteractionSupport {
    fn is_executable(self) -> bool {
        matches!(self, Self::Available | Self::PartiallyAvailable)
    }
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
    OrderedList,
    ResourceSelector,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionChoice {
    pub value: InteractionValue,
    pub label: String,
    pub available: bool,
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
    pub list: Option<InteractionList>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionList {
    pub fields: Vec<InteractionListField>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionListField {
    pub property: String,
    pub label: String,
    pub control: InteractionControl,
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
    pub support: InteractionSupport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_reason: Option<String>,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InteractionSurface {
    pub identity: InteractionIdentity,
    pub revision: ContentHash,
    pub support: InteractionSupport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_reason: Option<String>,
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
    support: InteractionSupport,
    support_reason: Option<String>,
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
    list: Option<DeclaredList>,
    resource: Option<ResourceKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
enum FieldKind {
    Boolean,
    Integer,
    Number,
    String,
    Array,
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
struct DeclaredList {
    item_id: String,
    fields: Vec<DeclaredListField>,
}

#[derive(Clone, Debug, Serialize)]
struct DeclaredListField {
    property: String,
    label: String,
    control: InteractionControl,
    kind: FieldKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ResourceKind {
    Characters,
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
    command: Option<String>,
    support: InteractionSupport,
    support_reason: Option<String>,
    requires_branch: bool,
    requires_completed_attempt: bool,
    capabilities: BTreeSet<PluginCapability>,
}

#[derive(Deserialize)]
struct Annotation {
    schema: String,
    id: String,
    component_sha256: ContentHash,
    support: InteractionSupport,
    support_reason: Option<String>,
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
    command: Option<String>,
    support: InteractionSupport,
    support_reason: Option<String>,
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
    if annotation.support != InteractionSupport::Available
        && annotation
            .support_reason
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(invalid(
            "non-available interaction support requires a concrete reason",
        ));
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
            if action.support.is_executable() {
                let command = action.command.as_deref().ok_or_else(|| {
                    invalid(format!(
                        "executable interaction action '{}' requires a command",
                        action.id
                    ))
                })?;
                if !manifest.commands.iter().any(|declared| declared == command) {
                    return Err(invalid(format!(
                        "interaction action '{}' binds undeclared command '{command}'",
                        action.id
                    )));
                }
            } else if action.command.is_some() {
                return Err(invalid(format!(
                    "non-executable interaction action '{}' must not bind a command",
                    action.id
                )));
            }
            if action.support != InteractionSupport::Available
                && action.support_reason.as_deref().is_none_or(str::is_empty)
            {
                return Err(invalid(format!(
                    "non-available interaction action '{}' requires a concrete reason",
                    action.id
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
                support: action.support,
                support_reason: action.support_reason,
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
        "support": annotation.support,
        "support_reason": annotation.support_reason,
        "groups": groups,
        "fields": fields,
        "actions": actions,
    });
    let hash = canonical_json_hash(DECLARATION_DOMAIN, &declaration_value)?;
    Ok(Some(InteractionDeclaration {
        id: declaration_value["id"].as_str().unwrap().to_owned(),
        hash,
        support: annotation.support,
        support_reason: annotation.support_reason,
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
        "items",
        "x-stcli-control",
        "x-stcli-choice-labels",
        "x-stcli-choice-source",
        "x-stcli-item-id",
        "x-stcli-resource",
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
        "array" => FieldKind::Array,
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
        "ordered-list" => InteractionControl::OrderedList,
        "resource-selector" => InteractionControl::ResourceSelector,
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
                InteractionControl::Text
                    | InteractionControl::MultilineText
                    | InteractionControl::ResourceSelector
            )
            | (FieldKind::Array, InteractionControl::OrderedList)
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
            "interaction field '{property}' default does not match its control"
        ))
    })?;
    let minimum = optional_number(object, "minimum", property)?;
    let maximum = optional_number(object, "maximum", property)?;
    if matches!(
        kind,
        FieldKind::Boolean | FieldKind::String | FieldKind::Array
    ) && (minimum.is_some() || maximum.is_some())
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
    let list = (control == InteractionControl::OrderedList)
        .then(|| parse_list(property, object))
        .transpose()?;
    let resource = match object.get("x-stcli-resource").and_then(Value::as_str) {
        Some("characters") if control == InteractionControl::ResourceSelector => {
            Some(ResourceKind::Characters)
        }
        Some("provider-profiles") if control == InteractionControl::ResourceSelector => {
            Some(ResourceKind::ProviderProfiles)
        }
        Some(other) => {
            return Err(invalid(format!(
                "interaction field '{property}' has unsupported resource source '{other}'"
            )));
        }
        None if control == InteractionControl::ResourceSelector => {
            return Err(invalid(format!(
                "interaction field '{property}' resource selector requires a source"
            )));
        }
        None => None,
    };
    if control != InteractionControl::OrderedList
        && (object.contains_key("items") || object.contains_key("x-stcli-item-id"))
    {
        return Err(invalid(format!(
            "interaction field '{property}' has list metadata for a non-list control"
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
        list,
        resource,
    })
}

fn parse_list(property: &str, object: &Map<String, Value>) -> Result<DeclaredList, PluginError> {
    let item_id = required_string(object, "x-stcli-item-id", property)?.to_owned();
    let items = object
        .get("items")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            invalid(format!(
                "interaction field '{property}' requires object items"
            ))
        })?;
    let required = items
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid(format!(
                "interaction field '{property}' requires item fields"
            ))
        })?;
    let properties = items
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            invalid(format!(
                "interaction field '{property}' requires item properties"
            ))
        })?;
    if !required
        .iter()
        .any(|value| value.as_str() == Some(&item_id))
    {
        return Err(invalid(format!(
            "interaction field '{property}' item id must be required"
        )));
    }
    let id_field = properties
        .get(&item_id)
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(format!("interaction field '{property}' item id is missing")))?;
    if id_field.get("type").and_then(Value::as_str) != Some("string") {
        return Err(invalid(format!(
            "interaction field '{property}' item id must be a string"
        )));
    }
    let mut fields = Vec::new();
    for name in required
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| *name != item_id)
    {
        let source = properties
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| invalid(format!("interaction list field '{name}' is missing")))?;
        let kind = match required_string(source, "type", name)? {
            "boolean" => FieldKind::Boolean,
            "string" => FieldKind::String,
            other => {
                return Err(invalid(format!(
                    "interaction list field '{name}' has unsupported type '{other}'"
                )));
            }
        };
        let control = match required_string(source, "x-stcli-control", name)? {
            "boolean" if kind == FieldKind::Boolean => InteractionControl::Boolean,
            "text" if kind == FieldKind::String => InteractionControl::Text,
            "multiline-text" if kind == FieldKind::String => InteractionControl::MultilineText,
            _ => {
                return Err(invalid(format!(
                    "interaction list field '{name}' has incompatible control"
                )));
            }
        };
        fields.push(DeclaredListField {
            property: name.to_owned(),
            label: required_string(source, "title", name)?.to_owned(),
            control,
            kind,
        });
    }
    if fields.is_empty() {
        return Err(invalid(format!(
            "interaction field '{property}' ordered list requires editable fields"
        )));
    }
    Ok(DeclaredList { item_id, fields })
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
        FieldKind::Array => value.is_array(),
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
    pub characters: &'a [(ContentHash, String)],
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
    let support_reason = (!input.declaration.support.is_executable())
        .then(|| input.declaration.support_reason.clone())
        .flatten();
    let disabled_reason = (!input.enabled).then(|| "Extension is disabled".to_owned());
    let can_save = input.declaration.support.is_executable()
        && input.enabled
        && input
            .capabilities
            .contains(&PluginCapability::WriteOwnState);
    let save_reason = support_reason
        .clone()
        .or_else(|| disabled_reason.clone())
        .or_else(|| {
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
                            available: true,
                        })
                        .collect::<Vec<_>>();
                    if field.choice_source == Some(ChoiceSource::ProviderProfiles) {
                        choices.push(InteractionChoice {
                            value: InteractionValue::Text(String::new()),
                            label: "Use Session provider".to_owned(),
                            available: true,
                        });
                        choices.extend(input.provider_profiles.iter().map(|name| {
                            InteractionChoice {
                                value: InteractionValue::Text(name.clone()),
                                label: name.clone(),
                                available: true,
                            }
                        }));
                    }
                    if field.resource == Some(ResourceKind::Characters) {
                        choices.extend(input.characters.iter().map(|(reference, label)| {
                            InteractionChoice {
                                value: InteractionValue::Text(reference.to_string()),
                                label: label.clone(),
                                available: true,
                            }
                        }));
                    } else if field.resource == Some(ResourceKind::ProviderProfiles) {
                        choices.extend(input.provider_profiles.iter().map(|name| {
                            InteractionChoice {
                                value: InteractionValue::Text(name.clone()),
                                label: name.clone(),
                                available: true,
                            }
                        }));
                    }
                    let mut error = validate_field_value(field, value_json, &choices).err();
                    if matches!(
                        field.control,
                        InteractionControl::Choice | InteractionControl::ResourceSelector
                    ) && let Some(InteractionValue::Text(current)) =
                        InteractionValue::from_json(value_json)
                        && !current.is_empty()
                        && !choices
                            .iter()
                            .any(|choice| choice.value == InteractionValue::Text(current.clone()))
                    {
                        error = Some("Selected resource is not available".to_owned());
                        choices.push(InteractionChoice {
                            value: InteractionValue::Text(current.clone()),
                            label: format!("{current} (unavailable)"),
                            available: false,
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
                        list: field.list.as_ref().map(|list| InteractionList {
                            fields: list
                                .fields
                                .iter()
                                .map(|field| InteractionListField {
                                    property: field.property.clone(),
                                    label: field.label.clone(),
                                    control: field.control,
                                })
                                .collect(),
                        }),
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
            let reason = (!action.support.is_executable())
                .then(|| action.support_reason.clone())
                .flatten()
                .or_else(|| support_reason.clone())
                .or_else(|| disabled_reason.clone())
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
                support: action.support,
                support_reason: action.support_reason.clone(),
                enabled: reason.is_none(),
                unavailable_reason: reason,
            })
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    let save = InteractionAction {
        target: target_id(&id, "save", &input.declaration.id)?,
        label: "Save".to_owned(),
        help: "Save edited values as a new Session Configuration Revision.".to_owned(),
        support: input.declaration.support,
        support_reason: input.declaration.support_reason.clone(),
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
        identity: InteractionIdentity {
            session_id: input.session_id,
            branch_id: input.branch_id,
            extension_id: input.installed.manifest.id.clone(),
            package_version: input.installed.manifest.version.to_string(),
            component_sha256: input.installed.manifest.component_sha256.clone(),
            declaration_hash: input.declaration.hash.clone(),
            surface_id: id,
        },
        revision,
        support: input.declaration.support,
        support_reason: input.declaration.support_reason.clone(),
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
    if matches!(
        field.control,
        InteractionControl::Choice | InteractionControl::ResourceSelector
    ) {
        let value =
            InteractionValue::from_json(value).ok_or_else(|| "Value must be scalar".to_owned())?;
        if !choices
            .iter()
            .any(|choice| choice.value == value && choice.available)
        {
            return Err("Value is not an available choice".to_owned());
        }
    }
    if field.control == InteractionControl::OrderedList {
        validate_ordered_list(field, value)?;
    }
    Ok(())
}

fn validate_ordered_list(field: &DeclaredField, value: &Value) -> Result<(), String> {
    let declaration = field.list.as_ref().expect("ordered-list declaration");
    let items = value
        .as_array()
        .ok_or_else(|| "Ordered list value must be an array".to_owned())?;
    let mut ids = BTreeSet::new();
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| "Ordered list items must be objects".to_owned())?;
        let id = object
            .get(&declaration.item_id)
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "Ordered list item identity is missing".to_owned())?;
        if !ids.insert(id) {
            return Err("Ordered list item identities must be unique".to_owned());
        }
        if object.len() != declaration.fields.len() + 1
            || object.keys().any(|key| {
                key != &declaration.item_id
                    && !declaration
                        .fields
                        .iter()
                        .any(|field| &field.property == key)
            })
        {
            return Err("Ordered list item fields do not match the declaration".to_owned());
        }
        for declared in &declaration.fields {
            let value = object
                .get(&declared.property)
                .ok_or_else(|| format!("Ordered list item is missing '{}'", declared.label))?;
            validate_kind(&declared.property, declared.kind, value)
                .map_err(|error| bounded(error.to_string()))?;
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

pub(crate) fn action_command(action: &DeclaredAction) -> Option<&str> {
    action.command.as_deref()
}

fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::InvalidManifest(bounded(message.into()))
}

pub(crate) fn bounded(message: impl Into<String>) -> String {
    message.into().chars().take(256).collect()
}
