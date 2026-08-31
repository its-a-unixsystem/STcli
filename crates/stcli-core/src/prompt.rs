use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    ChatRole, ContentHash, MacroEvaluation, RegexScriptApplication, StateMutation, TokenizerId,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptSegment {
    pub source: String,
    pub slot: String,
    pub role: ChatRole,
    pub content: String,
    pub raw_content: String,
    pub token_count: usize,
    pub source_revision: Option<ContentHash>,
    pub source_field: Option<String>,
    pub in_chat_depth: Option<usize>,
    pub in_chat_order: usize,
    pub truncation_priority: u32,
    pub pruned: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macro_evaluations: Vec<MacroEvaluation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regex_applications: Vec<RegexScriptApplication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_mutations: Vec<StateMutation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptPruning {
    pub context_limit: usize,
    pub response_reserve: usize,
    pub prompt_limit: usize,
    pub kept_tokens: usize,
    pub pruned_tokens: usize,
}

impl PromptSegment {
    pub fn new(
        tokenizer: TokenizerId,
        source: impl Into<String>,
        slot: impl Into<String>,
        role: ChatRole,
        content: String,
    ) -> Self {
        let token_count = tokenizer.count(&content);
        Self {
            source: source.into(),
            slot: slot.into(),
            role,
            raw_content: content.clone(),
            content,
            token_count,
            source_revision: None,
            source_field: None,
            in_chat_depth: None,
            in_chat_order: 0,
            truncation_priority: 500,
            pruned: false,
            macro_evaluations: Vec::new(),
            regex_applications: Vec::new(),
            state_mutations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedPromptContent {
    pub content: String,
    pub macro_evaluations: Vec<MacroEvaluation>,
    pub state_mutations: Vec<StateMutation>,
}

impl RenderedPromptContent {
    pub fn plain(content: String) -> Self {
        Self {
            content,
            macro_evaluations: Vec::new(),
            state_mutations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptPreset {
    pub prompts: BTreeMap<String, PresetPrompt>,
    pub order: Vec<PresetOrder>,
}

pub const CHAT_COMPLETION_CHARACTER_ID: u64 = 100001;

impl PromptPreset {
    pub fn parse(value: &Value, character_id: u64) -> Result<Self, PromptError> {
        let prompts = value
            .get("prompts")
            .and_then(Value::as_array)
            .ok_or(PromptError::MissingPrompts)?;
        let mut parsed = BTreeMap::new();
        for prompt in prompts {
            let prompt = prompt.as_object().ok_or(PromptError::InvalidPrompt)?;
            let identifier = prompt
                .get("identifier")
                .and_then(Value::as_str)
                .ok_or(PromptError::MissingIdentifier)?
                .to_owned();
            parsed.insert(
                identifier.clone(),
                PresetPrompt {
                    identifier,
                    role: parse_role(prompt.get("role")),
                    content: prompt
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    marker: prompt
                        .get("marker")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    in_chat_depth: match prompt.get("injection_position").and_then(Value::as_i64) {
                        Some(1) => Some(
                            prompt
                                .get("injection_depth")
                                .and_then(Value::as_u64)
                                .unwrap_or(0) as usize,
                        ),
                        _ => None,
                    },
                    in_chat_order: prompt
                        .get("injection_order")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as usize,
                },
            );
        }
        let order = parse_order(value.get("prompt_order"), character_id);
        Ok(Self {
            prompts: parsed,
            order,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresetPrompt {
    pub identifier: String,
    pub role: ChatRole,
    pub content: String,
    #[serde(default)]
    pub marker: bool,
    pub in_chat_depth: Option<usize>,
    pub in_chat_order: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PresetOrder {
    pub identifier: String,
    pub enabled: bool,
}

pub fn apply_prompt_preset(
    tokenizer: TokenizerId,
    preset: Option<&PromptPreset>,
    segments: Vec<PromptSegment>,
    mut render_custom: impl FnMut(&str, &str) -> Result<RenderedPromptContent, PromptError>,
) -> Result<Vec<PromptSegment>, PromptError> {
    let Some(preset) = preset else {
        return Ok(insert_in_chat_segments(segments));
    };
    let mut slots = BTreeMap::<String, Vec<PromptSegment>>::new();
    let mut slot_order = Vec::new();
    for segment in segments {
        if !slots.contains_key(&segment.slot) {
            slot_order.push(segment.slot.clone());
        }
        slots.entry(segment.slot.clone()).or_default().push(segment);
    }
    let chat_history_enabled = preset
        .order
        .iter()
        .any(|item| item.identifier == "chatHistory" && item.enabled);
    let user_input_enabled = preset
        .order
        .iter()
        .any(|item| item.identifier == "userInput" && item.enabled);
    if chat_history_enabled
        && !user_input_enabled
        && let Some(mut user_input) = slots.remove("userInput")
    {
        slots
            .entry("chatHistory".to_owned())
            .or_default()
            .append(&mut user_input);
    }
    let mut used = BTreeSet::new();
    let mut ordered = Vec::new();
    for item in &preset.order {
        if !item.enabled {
            used.insert(item.identifier.clone());
            continue;
        }
        if let Some(mut native) = slots.remove(&item.identifier) {
            used.insert(item.identifier.clone());
            if let Some(custom) = preset.prompts.get(&item.identifier)
                && !custom.content.is_empty()
                && !is_native_passthrough(&item.identifier)
            {
                let rendered = render_custom(&custom.identifier, &custom.content)?;
                if !rendered.content.trim().is_empty() {
                    let mut replacement = PromptSegment::new(
                        tokenizer,
                        format!("preset:{}", custom.identifier),
                        custom.identifier.clone(),
                        custom.role,
                        rendered.content,
                    );
                    replacement.macro_evaluations = rendered.macro_evaluations;
                    replacement.state_mutations = rendered.state_mutations;
                    replacement.raw_content = custom.content.clone();
                    replacement.in_chat_depth = custom.in_chat_depth;
                    replacement.in_chat_order = custom.in_chat_order;
                    ordered.push(replacement);
                }
            } else {
                ordered.append(&mut native);
            }
            continue;
        }
        let Some(custom) = preset.prompts.get(&item.identifier) else {
            continue;
        };
        if custom.content.is_empty() {
            continue;
        }
        let rendered = render_custom(&custom.identifier, &custom.content)?;
        if rendered.content.trim().is_empty() {
            continue;
        }
        let mut segment = PromptSegment::new(
            tokenizer,
            format!("preset:{}", custom.identifier),
            custom.identifier.clone(),
            custom.role,
            rendered.content,
        );
        segment.raw_content = custom.content.clone();
        segment.macro_evaluations = rendered.macro_evaluations;
        segment.state_mutations = rendered.state_mutations;
        segment.in_chat_depth = custom.in_chat_depth;
        segment.in_chat_order = custom.in_chat_order;
        ordered.push(segment);
        used.insert(item.identifier.clone());
    }
    for slot in slot_order {
        if used.contains(&slot) || (is_native_slot(&slot) && slot != "userInput") {
            continue;
        }
        if let Some(mut remaining) = slots.remove(&slot) {
            ordered.append(&mut remaining);
        }
    }
    Ok(insert_in_chat_segments(ordered))
}

pub fn insert_in_chat_segments(segments: Vec<PromptSegment>) -> Vec<PromptSegment> {
    let (mut top_level, mut in_chat): (Vec<_>, Vec<_>) = segments
        .into_iter()
        .partition(|segment| segment.in_chat_depth.is_none());
    in_chat.sort_by_key(|segment| {
        (
            std::cmp::Reverse(segment.in_chat_depth.unwrap_or(0)),
            segment.in_chat_order,
            segment.source.clone(),
        )
    });
    while !in_chat.is_empty() {
        let depth = in_chat[0].in_chat_depth.unwrap_or(0);
        let insertion_index = top_level.len().saturating_sub(depth);
        let end = in_chat
            .iter()
            .position(|segment| segment.in_chat_depth != Some(depth))
            .unwrap_or(in_chat.len());
        top_level.splice(insertion_index..insertion_index, in_chat.drain(..end));
    }
    top_level
}

pub fn prune_segments(
    segments: &mut [PromptSegment],
    context_limit: usize,
    response_reserve: usize,
) -> Result<PromptPruning, PromptError> {
    let prompt_limit = context_limit.saturating_sub(response_reserve);
    let mut kept_tokens = segments
        .iter()
        .map(|segment| segment.token_count)
        .sum::<usize>();
    let mut groups = BTreeMap::<String, (u32, usize, Vec<usize>)>::new();
    for (index, segment) in segments.iter().enumerate() {
        if segment.truncation_priority == u32::MAX {
            continue;
        }
        let group = if let Some(turn) = segment.source.strip_prefix("turn:") {
            format!(
                "turn:{}",
                turn.rsplit_once(':').map(|(id, _)| id).unwrap_or(turn)
            )
        } else {
            segment.source.clone()
        };
        let entry = groups
            .entry(group)
            .or_insert((segment.truncation_priority, index, Vec::new()));
        entry.0 = entry.0.min(segment.truncation_priority);
        entry.1 = entry.1.min(index);
        entry.2.push(index);
    }
    let mut candidates = groups.into_values().collect::<Vec<_>>();
    candidates.sort_by_key(|(priority, first_index, _)| (*priority, *first_index));
    let mut pruned_tokens = 0;
    for (_, _, indices) in candidates {
        if kept_tokens <= prompt_limit {
            break;
        }
        for index in indices {
            if !segments[index].pruned {
                segments[index].pruned = true;
                kept_tokens = kept_tokens.saturating_sub(segments[index].token_count);
                pruned_tokens += segments[index].token_count;
            }
        }
    }
    if kept_tokens > prompt_limit {
        return Err(PromptError::ContextOverflow {
            required: kept_tokens,
            available: prompt_limit,
        });
    }
    Ok(PromptPruning {
        context_limit,
        response_reserve,
        prompt_limit,
        kept_tokens,
        pruned_tokens,
    })
}

fn is_native_passthrough(identifier: &str) -> bool {
    matches!(
        identifier,
        "worldInfoBefore" | "worldInfoAfter" | "dialogueExamples" | "chatHistory" | "userInput"
    )
}

fn is_native_slot(identifier: &str) -> bool {
    matches!(
        identifier,
        "main"
            | "charDescription"
            | "charPersonality"
            | "scenario"
            | "personaDescription"
            | "dialogueExamples"
            | "nsfw"
            | "jailbreak"
            | "worldInfoBefore"
            | "worldInfoAfter"
            | "chatHistory"
            | "userInput"
            | "enhanceDefinitions"
    )
}

fn parse_order(value: Option<&Value>, character_id: u64) -> Vec<PresetOrder> {
    let Some(value) = value else {
        return Vec::new();
    };
    let order = if let Some(array) = value.as_array() {
        array
            .iter()
            .find(|entry| {
                entry
                    .get("character_id")
                    .and_then(Value::as_u64)
                    .is_some_and(|id| id == character_id)
            })
            .and_then(|entry| entry.get("order").and_then(Value::as_array))
            .or_else(|| {
                array
                    .iter()
                    .find_map(|entry| entry.get("order").and_then(Value::as_array))
            })
            .or(Some(array))
    } else {
        value.get("order").and_then(Value::as_array)
    };
    order
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(PresetOrder {
                identifier: entry.get("identifier")?.as_str()?.to_owned(),
                enabled: entry
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
            })
        })
        .collect()
}

fn parse_role(value: Option<&Value>) -> ChatRole {
    match value.and_then(Value::as_str) {
        Some("user") => ChatRole::User,
        Some("assistant") => ChatRole::Assistant,
        _ => ChatRole::System,
    }
}

#[derive(Debug, Error)]
pub enum PromptError {
    #[error("prompt preset is missing prompts")]
    MissingPrompts,
    #[error("prompt preset prompt must be an object")]
    InvalidPrompt,
    #[error("prompt preset prompt is missing identifier")]
    MissingIdentifier,
    #[error("prompt preset macro evaluation failed: {0}")]
    Render(String),
    #[error("context formatting template failed: {0}")]
    ContextTemplate(String),
    #[error(
        "protected prompt content requires {required} tokens, but only {available} are available"
    )]
    ContextOverflow { required: usize, available: usize },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn preset_orders_native_slots_custom_prompts_and_in_chat_depth() {
        let tokenizer = TokenizerId::Cl100kBase;
        let preset = PromptPreset::parse(&json!({
            "prompts": [
                {"identifier": "custom", "role": "system", "content": "custom"},
                {"identifier": "depth", "role": "user", "content": "depth", "injection_position": 1, "injection_depth": 1}
            ],
            "prompt_order": [{"character_id": 100001, "order": [
                {"identifier": "main", "enabled": true},
                {"identifier": "custom", "enabled": true},
                {"identifier": "chatHistory", "enabled": true},
                {"identifier": "depth", "enabled": true},
                {"identifier": "userInput", "enabled": true}
            ]}]
        }), CHAT_COMPLETION_CHARACTER_ID)
        .unwrap();
        let segments = vec![
            PromptSegment::new(
                tokenizer,
                "history",
                "chatHistory",
                ChatRole::User,
                "old".to_owned(),
            ),
            PromptSegment::new(
                tokenizer,
                "main",
                "main",
                ChatRole::System,
                "main".to_owned(),
            ),
            PromptSegment::new(
                tokenizer,
                "user",
                "userInput",
                ChatRole::User,
                "new".to_owned(),
            ),
        ];
        let ordered = apply_prompt_preset(tokenizer, Some(&preset), segments, |_, value| {
            Ok(RenderedPromptContent::plain(value.to_owned()))
        })
        .unwrap();
        assert_eq!(
            ordered
                .iter()
                .map(|segment| segment.content.as_str())
                .collect::<Vec<_>>(),
            ["main", "custom", "old", "depth", "new"]
        );
    }

    #[test]
    fn parse_order_selects_character_id_100001_over_100000() {
        let preset = PromptPreset::parse(
            &json!({
                "prompts": [
                    {"identifier": "main", "role": "system", "content": "main"}
                ],
                "prompt_order": [
                    {"character_id": 100000, "order": [
                        {"identifier": "main", "enabled": true},
                        {"identifier": "chatHistory", "enabled": true}
                    ]},
                    {"character_id": 100001, "order": [
                        {"identifier": "chatHistory", "enabled": true},
                        {"identifier": "main", "enabled": false}
                    ]}
                ]
            }),
            CHAT_COMPLETION_CHARACTER_ID,
        )
        .unwrap();
        assert_eq!(preset.order.len(), 2);
        assert_eq!(preset.order[0].identifier, "chatHistory");
        assert!(preset.order[0].enabled);
        assert_eq!(preset.order[1].identifier, "main");
        assert!(!preset.order[1].enabled);
    }

    #[test]
    fn disabled_order_entries_suppress_native_slot_fallback() {
        let tokenizer = TokenizerId::Cl100kBase;
        let preset = PromptPreset::parse(
            &json!({
                "prompts": [],
                "prompt_order": [{"character_id": 100001, "order": [
                    {"identifier": "main", "enabled": true},
                    {"identifier": "charDescription", "enabled": false},
                    {"identifier": "chatHistory", "enabled": true}
                ]}]
            }),
            CHAT_COMPLETION_CHARACTER_ID,
        )
        .unwrap();
        let segments = vec![
            PromptSegment::new(tokenizer, "main", "main", ChatRole::System, "main".into()),
            PromptSegment::new(
                tokenizer,
                "char",
                "charDescription",
                ChatRole::System,
                "description".into(),
            ),
            PromptSegment::new(
                tokenizer,
                "history",
                "chatHistory",
                ChatRole::User,
                "chat".into(),
            ),
        ];
        let ordered = apply_prompt_preset(tokenizer, Some(&preset), segments, |_, value| {
            Ok(RenderedPromptContent::plain(value.to_owned()))
        })
        .unwrap();
        let identifiers: Vec<_> = ordered.iter().map(|s| s.content.as_str()).collect();
        assert_eq!(identifiers, ["main", "chat"]);
    }

    #[test]
    fn pruning_discards_examples_then_complete_oldest_turns() {
        let tokenizer = TokenizerId::Cl100kBase;
        let mut main = PromptSegment::new(
            tokenizer,
            "main",
            "main",
            ChatRole::System,
            "main".to_owned(),
        );
        main.truncation_priority = u32::MAX;
        let mut example = PromptSegment::new(
            tokenizer,
            "example:0",
            "dialogueExamples",
            ChatRole::User,
            "an expendable example".to_owned(),
        );
        example.truncation_priority = 50;
        let mut turn_user = PromptSegment::new(
            tokenizer,
            "turn:01:user",
            "chatHistory",
            ChatRole::User,
            "old question".to_owned(),
        );
        turn_user.truncation_priority = 100;
        let mut turn_assistant = PromptSegment::new(
            tokenizer,
            "turn:01:assistant",
            "chatHistory",
            ChatRole::Assistant,
            "old answer".to_owned(),
        );
        turn_assistant.truncation_priority = 100;
        let mut current = PromptSegment::new(
            tokenizer,
            "current-user-action",
            "userInput",
            ChatRole::User,
            "new".to_owned(),
        );
        current.truncation_priority = u32::MAX;
        let protected = main.token_count + current.token_count;
        let mut segments = vec![main, example, turn_user, turn_assistant, current];

        let report = prune_segments(&mut segments, protected, 0).unwrap();

        assert_eq!(report.kept_tokens, protected);
        assert!(segments[1].pruned);
        assert!(segments[2].pruned);
        assert!(segments[3].pruned);
        assert!(!segments[0].pruned);
        assert!(!segments[4].pruned);
    }
}
