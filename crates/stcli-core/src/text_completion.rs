use std::{borrow::Cow, collections::BTreeMap};

use handlebars::Handlebars;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ChatRole, PromptError, PromptPruning, PromptSegment, TokenizerId};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormatMode {
    #[default]
    ChatCompletion,
    TextCompletion,
}

impl FormatMode {
    pub fn is_chat_completion(&self) -> bool {
        *self == Self::ChatCompletion
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NamesBehavior {
    None,
    #[default]
    Force,
    Always,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct InstructTemplate {
    pub input_sequence: String,
    pub output_sequence: String,
    pub system_sequence: String,
    pub input_suffix: String,
    pub output_suffix: String,
    pub system_suffix: String,
    pub first_input_sequence: String,
    pub last_input_sequence: String,
    pub first_output_sequence: String,
    pub last_output_sequence: String,
    pub last_system_sequence: String,
    pub stop_sequence: String,
    pub wrap: bool,
    pub r#macro: bool,
    pub names_behavior: NamesBehavior,
    pub skip_examples: bool,
    pub system_same_as_user: bool,
    pub sequences_as_stop_strings: bool,
    pub story_string_prefix: String,
    pub story_string_suffix: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct ContextFormatting {
    pub story_string: String,
    pub example_separator: String,
    pub chat_start: String,
    pub turn_separator: String,
    pub use_stop_strings: bool,
    pub names_as_stop_strings: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct TextProjection {
    pub prompt: String,
    pub stop_sequences: Vec<String>,
}

pub(crate) fn project_text_completion(
    segments: &[PromptSegment],
    instruct: &InstructTemplate,
    context: &ContextFormatting,
    persona_name: &str,
    character_name: &str,
    assistant_prefill: Option<&str>,
) -> Result<TextProjection, PromptError> {
    let mut story = BTreeMap::<&str, String>::new();
    for key in [
        "system",
        "description",
        "personality",
        "scenario",
        "wiBefore",
        "wiAfter",
        "anchorBefore",
        "anchorAfter",
        "personaDescription",
        "persona_description",
    ] {
        story.insert(key, String::new());
    }
    story.insert("persona", persona_name.to_owned());
    for segment in segments.iter().filter(|segment| !segment.pruned) {
        let Some(key) = story_key(segment) else {
            continue;
        };
        let value = story.get_mut(key).expect("story key is initialized");
        if !value.is_empty() && !segment.content.is_empty() {
            value.push('\n');
        }
        value.push_str(&segment.content);
    }
    story.insert("persona_description", story["personaDescription"].clone());

    let mut registry = Handlebars::new();
    registry.register_escape_fn(handlebars::no_escape);
    let persona_description = story["personaDescription"].as_str();
    let story_text = registry
        .render_template(
            &context.story_string,
            &json!({
                "system": story["system"],
                "description": story["description"],
                "personality": story["personality"],
                "scenario": story["scenario"],
                "wiBefore": story["wiBefore"],
                "wiAfter": story["wiAfter"],
                "persona": story["persona"],
                "personaDescription": story["personaDescription"],
                "persona_description": story["persona_description"],
                "anchorBefore": story["anchorBefore"],
                "anchorAfter": story["anchorAfter"],
                "user": persona_name,
                "char": character_name,
                "trim": "",
            }),
        )
        .map_err(|error| PromptError::ContextTemplate(error.to_string()))?;

    let mut prompt = String::new();
    let story_prefix = render_sequence(
        &instruct.story_string_prefix,
        instruct.r#macro,
        persona_name,
        character_name,
        persona_description,
        "System",
    );
    push_sequence(&mut prompt, &story_prefix, instruct.wrap);
    prompt.push_str(&story_text);
    prompt.push_str(&render_sequence(
        &instruct.story_string_suffix,
        instruct.r#macro,
        persona_name,
        character_name,
        persona_description,
        "System",
    ));

    let conversation = segments
        .iter()
        .filter(|segment| !segment.pruned && story_key(segment).is_none())
        .collect::<Vec<_>>();
    for (index, segment) in conversation.iter().enumerate() {
        if let Some(separator) = separator_before(&conversation, index, context) {
            prompt.push_str(separator);
            if !separator.ends_with('\n') {
                prompt.push('\n');
            }
        }
        let name = match segment.role {
            ChatRole::User => persona_name,
            ChatRole::Assistant => character_name,
            ChatRole::System => "System",
        };
        if instruct.skip_examples && segment.source.starts_with("example:") {
            prompt.push_str(name);
            prompt.push_str(": ");
            prompt.push_str(&segment.content);
            prompt.push('\n');
            continue;
        }
        let sequence = message_sequence(instruct, &conversation, index);
        let sequence = render_sequence(
            sequence,
            instruct.r#macro,
            persona_name,
            character_name,
            persona_description,
            name,
        );
        push_sequence(&mut prompt, &sequence, instruct.wrap);
        if instruct.names_behavior == NamesBehavior::Always {
            prompt.push_str(name);
            prompt.push_str(": ");
        }
        prompt.push_str(&segment.content);
        let suffix = render_sequence(
            message_suffix(instruct, segment.role),
            instruct.r#macro,
            persona_name,
            character_name,
            persona_description,
            name,
        );
        if suffix.is_empty() && instruct.wrap {
            prompt.push('\n');
        } else {
            prompt.push_str(&suffix);
        }
    }

    if instruct.wrap {
        prompt.push('\n');
    }
    let assistant_sequence = if instruct.last_output_sequence.is_empty() {
        &instruct.output_sequence
    } else {
        &instruct.last_output_sequence
    };
    let assistant_sequence = render_sequence(
        assistant_sequence,
        instruct.r#macro,
        persona_name,
        character_name,
        persona_description,
        character_name,
    );
    push_sequence(&mut prompt, &assistant_sequence, instruct.wrap);
    if instruct.names_behavior == NamesBehavior::Always {
        prompt.push_str(character_name);
        prompt.push(':');
        if assistant_prefill.is_some() {
            prompt.push(' ');
        }
    }
    if let Some(prefill) = assistant_prefill {
        prompt.push_str(prefill);
    }

    Ok(TextProjection {
        prompt,
        stop_sequences: resolve_stop_sequences(
            instruct,
            context,
            persona_name,
            character_name,
            persona_description,
        ),
    })
}

fn story_key(segment: &PromptSegment) -> Option<&'static str> {
    match segment.source.as_str() {
        "main-prompt" => Some("system"),
        "character-description" => Some("description"),
        "character-personality" => Some("personality"),
        "character-scenario" => Some("scenario"),
        "persona-description" => Some("personaDescription"),
        "world-info-before" => Some("wiBefore"),
        "world-info-after" => Some("wiAfter"),
        _ if segment.slot == "pluginBeforeCharacterDefinitions" => Some("anchorBefore"),
        _ if segment.slot == "pluginAfterCharacterDefinitions" => Some("anchorAfter"),
        _ => None,
    }
}

fn separator_before<'a>(
    conversation: &[&PromptSegment],
    index: usize,
    context: &'a ContextFormatting,
) -> Option<&'a str> {
    let segment = conversation[index];
    let previous = index.checked_sub(1).map(|index| conversation[index]);
    if segment.source.starts_with("example:") {
        if previous.is_none()
            || example_block(&segment.source)
                != previous.and_then(|segment| example_block(&segment.source))
        {
            return (!context.example_separator.is_empty())
                .then_some(context.example_separator.as_str());
        }
        return None;
    }
    if previous.is_none() || previous.is_some_and(|segment| segment.source.starts_with("example:"))
    {
        return (!context.chat_start.is_empty()).then_some(context.chat_start.as_str());
    }
    if conversation_group(&segment.source)
        != conversation_group(&previous.expect("previous segment").source)
    {
        return (!context.turn_separator.is_empty()).then_some(context.turn_separator.as_str());
    }
    None
}

fn example_block(source: &str) -> Option<&str> {
    source
        .strip_prefix("example:")
        .map(|example| example.split_once(':').map_or(example, |(block, _)| block))
}

fn conversation_group(source: &str) -> &str {
    if let Some(turn) = source.strip_prefix("turn:") {
        return turn.split_once(':').map_or(source, |(id, _)| id);
    }
    source
}

fn message_sequence<'a>(
    instruct: &'a InstructTemplate,
    conversation: &[&PromptSegment],
    index: usize,
) -> &'a str {
    let segment = conversation[index];
    let role = segment.role;
    let is_example = segment.source.starts_with("example:");
    let first_role = !is_example
        && !conversation[..index]
            .iter()
            .any(|segment| !segment.source.starts_with("example:") && segment.role == role);
    let last_role = !is_example
        && !conversation[index + 1..]
            .iter()
            .any(|segment| !segment.source.starts_with("example:") && segment.role == role);
    match role {
        ChatRole::User if last_role && !instruct.last_input_sequence.is_empty() => {
            &instruct.last_input_sequence
        }
        ChatRole::User if first_role && !instruct.first_input_sequence.is_empty() => {
            &instruct.first_input_sequence
        }
        ChatRole::User => &instruct.input_sequence,
        ChatRole::Assistant if first_role && !instruct.first_output_sequence.is_empty() => {
            &instruct.first_output_sequence
        }
        ChatRole::Assistant => &instruct.output_sequence,
        ChatRole::System if instruct.system_same_as_user => &instruct.input_sequence,
        ChatRole::System if last_role && !instruct.last_system_sequence.is_empty() => {
            &instruct.last_system_sequence
        }
        ChatRole::System => &instruct.system_sequence,
    }
}

fn message_suffix(instruct: &InstructTemplate, role: ChatRole) -> &str {
    match role {
        ChatRole::User => &instruct.input_suffix,
        ChatRole::Assistant => &instruct.output_suffix,
        ChatRole::System if instruct.system_same_as_user => &instruct.input_suffix,
        ChatRole::System => &instruct.system_suffix,
    }
}

fn push_sequence(prompt: &mut String, sequence: &str, wrap: bool) {
    prompt.push_str(sequence);
    if wrap && !sequence.is_empty() {
        prompt.push('\n');
    }
}

fn render_sequence<'a>(
    sequence: &'a str,
    enabled: bool,
    persona_name: &str,
    character_name: &str,
    persona_description: &str,
    role_name: &str,
) -> Cow<'a, str> {
    if !enabled || !sequence.contains("{{") {
        return Cow::Borrowed(sequence);
    }
    let mut output = String::with_capacity(sequence.len());
    let mut remaining = sequence;
    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        let (pattern, replacement) = if remaining.starts_with("{{user}}") {
            ("{{user}}", persona_name)
        } else if remaining.starts_with("{{char}}") {
            ("{{char}}", character_name)
        } else if remaining.starts_with("{{personaDescription}}") {
            ("{{personaDescription}}", persona_description)
        } else if remaining.starts_with("{{persona_description}}") {
            ("{{persona_description}}", persona_description)
        } else if remaining.starts_with("{{name}}") {
            ("{{name}}", role_name)
        } else {
            output.push_str("{{");
            remaining = &remaining[2..];
            continue;
        };
        output.push_str(replacement);
        remaining = &remaining[pattern.len()..];
    }
    output.push_str(remaining);
    Cow::Owned(output)
}

fn resolve_stop_sequences(
    instruct: &InstructTemplate,
    context: &ContextFormatting,
    persona_name: &str,
    character_name: &str,
    persona_description: &str,
) -> Vec<String> {
    let mut stops = Vec::new();
    push_stop(
        &mut stops,
        render_sequence(
            &instruct.stop_sequence,
            instruct.r#macro,
            persona_name,
            character_name,
            persona_description,
            character_name,
        )
        .into_owned(),
    );
    if instruct.sequences_as_stop_strings {
        for (sequence, role_name) in [
            (instruct.input_sequence.as_str(), persona_name),
            (instruct.first_input_sequence.as_str(), persona_name),
            (instruct.last_input_sequence.as_str(), persona_name),
            (instruct.output_sequence.as_str(), character_name),
            (instruct.first_output_sequence.as_str(), character_name),
            (instruct.last_output_sequence.as_str(), character_name),
            (instruct.system_sequence.as_str(), "System"),
            (instruct.last_system_sequence.as_str(), "System"),
        ] {
            for sequence in sequence.split('\n') {
                if sequence.trim().is_empty() {
                    continue;
                }
                let sequence = render_sequence(
                    sequence,
                    instruct.r#macro,
                    persona_name,
                    character_name,
                    persona_description,
                    role_name,
                );
                let mut stop = String::with_capacity(sequence.len() + usize::from(instruct.wrap));
                if instruct.wrap {
                    stop.push('\n');
                }
                stop.push_str(&sequence);
                push_stop(&mut stops, stop);
            }
        }
    }
    if context.use_stop_strings {
        push_stop(&mut stops, context.chat_start.clone());
        push_stop(&mut stops, context.example_separator.clone());
    }
    if context.names_as_stop_strings {
        push_stop(&mut stops, format!("{persona_name}:"));
        push_stop(&mut stops, format!("{character_name}:"));
    }
    stops
}

fn push_stop(stops: &mut Vec<String>, value: String) {
    if !value.is_empty() && !stops.contains(&value) {
        stops.push(value);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prune_text_completion(
    tokenizer: TokenizerId,
    segments: &mut [PromptSegment],
    instruct: &InstructTemplate,
    context: &ContextFormatting,
    persona_name: &str,
    character_name: &str,
    assistant_prefill: Option<&str>,
    context_limit: usize,
    response_reserve: usize,
) -> Result<(TextProjection, PromptPruning), PromptError> {
    let prompt_limit = context_limit.saturating_sub(response_reserve);
    let mut projection = project_text_completion(
        segments,
        instruct,
        context,
        persona_name,
        character_name,
        assistant_prefill,
    )?;
    let initial_tokens = tokenizer.count(&projection.prompt);
    let mut kept_tokens = initial_tokens;
    let mut groups = BTreeMap::<String, (u32, usize, Vec<usize>)>::new();
    for (index, segment) in segments.iter().enumerate() {
        if segment.truncation_priority == u32::MAX {
            continue;
        }
        let group = pruning_group(segment);
        let entry = groups
            .entry(group)
            .or_insert((segment.truncation_priority, index, Vec::new()));
        entry.0 = entry.0.min(segment.truncation_priority);
        entry.1 = entry.1.min(index);
        entry.2.push(index);
    }
    let mut candidates = groups.into_values().collect::<Vec<_>>();
    candidates.sort_by_key(|(priority, first_index, _)| (*priority, *first_index));
    for (_, _, indices) in candidates {
        if kept_tokens <= prompt_limit {
            break;
        }
        for index in indices {
            segments[index].pruned = true;
        }
        projection = project_text_completion(
            segments,
            instruct,
            context,
            persona_name,
            character_name,
            assistant_prefill,
        )?;
        kept_tokens = tokenizer.count(&projection.prompt);
    }
    if kept_tokens > prompt_limit {
        return Err(PromptError::ContextOverflow {
            required: kept_tokens,
            available: prompt_limit,
        });
    }
    Ok((
        projection,
        PromptPruning {
            context_limit,
            response_reserve,
            prompt_limit,
            kept_tokens,
            pruned_tokens: initial_tokens.saturating_sub(kept_tokens),
        },
    ))
}

fn pruning_group(segment: &PromptSegment) -> String {
    if let Some(turn) = segment.source.strip_prefix("turn:") {
        return format!("turn:{}", turn.split_once(':').map_or(turn, |(id, _)| id));
    }
    if let Some(example) = segment.source.strip_prefix("example:") {
        return format!(
            "example:{}",
            example.split_once(':').map_or(example, |(block, _)| block)
        );
    }
    segment.source.clone()
}
