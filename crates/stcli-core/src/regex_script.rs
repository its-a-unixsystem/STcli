//! SillyTavern regex-script parsing and application.
//!
//! SillyTavern stores chat content raw and applies regex scripts at each
//! consumption point (display vs. prompt) filtered by the script's flags. STcli
//! follows the same model: stored artifacts and candidates stay raw, and the
//! prompt compiler applies scripts transiently while assembling the request.
//! This module owns the pure parsing and substitution; the isolated matching
//! runs through [`crate::EcmaRegexWorker`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::EcmaRegexError;
use crate::ecma_regex::RegexMatch;

/// Consumption channel a script is applied at. Maps to SillyTavern's numeric
/// `placement` codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum RegexPlacement {
    UserInput,
    AiOutput,
    SlashCommand,
    WorldInfo,
    Reasoning,
}

impl RegexPlacement {
    pub const CODE_USER_INPUT: u64 = 1;
    pub const CODE_AI_OUTPUT: u64 = 2;

    pub fn code(self) -> u64 {
        match self {
            Self::UserInput => 1,
            Self::AiOutput => 2,
            Self::SlashCommand => 3,
            Self::WorldInfo => 5,
            Self::Reasoning => 6,
        }
    }
}

/// Whether macros in the find pattern are substituted before compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum SubstituteMode {
    None,
    Raw,
    Escaped,
}

impl SubstituteMode {
    fn from_code(code: u64) -> Self {
        match code {
            1 => Self::Raw,
            2 => Self::Escaped,
            _ => Self::None,
        }
    }
}

/// A parsed SillyTavern regex script.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RegexScript {
    /// Caller-assigned stable identifier (STcli uses the script digest) used in
    /// application receipts.
    pub id: String,
    pub name: String,
    pub find_pattern: String,
    pub find_flags: String,
    pub replace_string: String,
    pub trim_strings: Vec<String>,
    pub placements: Vec<u64>,
    pub disabled: bool,
    pub markdown_only: bool,
    pub prompt_only: bool,
    pub min_depth: Option<i64>,
    pub max_depth: Option<i64>,
    pub substitute_mode: SubstituteMode,
}

impl RegexScript {
    /// Parse a script object. Returns `None` when there is no find pattern to
    /// apply.
    pub fn from_value(value: &Value) -> Option<Self> {
        let find = value.get("findRegex").and_then(Value::as_str).unwrap_or("");
        if find.is_empty() {
            return None;
        }
        let (find_pattern, find_flags) = parse_find(find);
        let placements = value
            .get("placement")
            .and_then(Value::as_array)
            .map(|codes| codes.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        Some(Self {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: value
                .get("scriptName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            find_pattern,
            find_flags,
            replace_string: value
                .get("replaceString")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            trim_strings: value
                .get("trimStrings")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            placements,
            disabled: value
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            markdown_only: value
                .get("markdownOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            prompt_only: value
                .get("promptOnly")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            min_depth: value.get("minDepth").and_then(Value::as_i64),
            max_depth: value.get("maxDepth").and_then(Value::as_i64),
            substitute_mode: SubstituteMode::from_code(
                value
                    .get("substituteRegex")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ),
        })
    }

    fn active_at(&self, channel: RegexPlacement, depth: i64) -> bool {
        if self.disabled || self.markdown_only {
            return false;
        }
        if !self.placements.contains(&channel.code()) {
            return false;
        }
        if self.min_depth.is_some_and(|min| depth < min) {
            return false;
        }
        if self.max_depth.is_some_and(|max| depth > max) {
            return false;
        }
        true
    }

    fn active_for_display(&self, channel: RegexPlacement, depth: i64) -> bool {
        if self.disabled || self.prompt_only {
            return false;
        }
        if !self.placements.contains(&channel.code()) {
            return false;
        }
        if self.min_depth.is_some_and(|min| depth < min) {
            return false;
        }
        if self.max_depth.is_some_and(|max| depth > max) {
            return false;
        }
        true
    }
}

/// A record of one script transforming one piece of text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegexScriptApplication {
    pub id: String,
    pub name: String,
    pub placement: u64,
    pub replacements: usize,
}

/// Apply the scripts that target `channel` at `depth` to `text`, in order.
/// Each script consumes the output of the previous one. `finder` performs the
/// isolated matching (typically [`crate::EcmaRegexWorker::find_matches`]).
///
/// When `expand_macros` is provided, macros in `replaceString` are expanded
/// after capture-group substitution, and `substituteRegex` modes are honored
/// for the find pattern (raw or regex-escaped macro expansion).
pub fn apply_scripts(
    scripts: &[RegexScript],
    channel: RegexPlacement,
    depth: i64,
    text: &str,
    finder: &mut impl FnMut(&str, &str, &str) -> Result<Vec<RegexMatch>, EcmaRegexError>,
    expand_macros: &mut Option<impl FnMut(&str, bool) -> String>,
) -> Result<(String, Vec<RegexScriptApplication>), EcmaRegexError> {
    let mut current = text.to_owned();
    let mut applications = Vec::new();
    for script in scripts {
        if !script.active_at(channel, depth) {
            continue;
        }
        let find_pattern = match (script.substitute_mode, expand_macros.as_mut()) {
            (SubstituteMode::Raw, Some(expand)) => expand(&script.find_pattern, false),
            (SubstituteMode::Escaped, Some(expand)) => expand(&script.find_pattern, true),
            _ => script.find_pattern.clone(),
        };
        let matches = finder(&find_pattern, &script.find_flags, &current)?;
        if matches.is_empty() {
            continue;
        }
        let mut output = String::with_capacity(current.len());
        let mut cursor = 0;
        for matched in &matches {
            output.push_str(&current[cursor..matched.start]);
            let replacement =
                expand_replacement(&script.replace_string, matched, &script.trim_strings);
            let replacement = match expand_macros.as_mut() {
                Some(expand) => expand(&replacement, false),
                None => replacement,
            };
            output.push_str(&replacement);
            cursor = matched.end;
        }
        output.push_str(&current[cursor..]);
        applications.push(RegexScriptApplication {
            id: script.id.clone(),
            name: script.name.clone(),
            placement: channel.code(),
            replacements: matches.len(),
        });
        current = output;
    }
    Ok((current, applications))
}

/// Apply display-eligible scripts (including `markdownOnly`, excluding
/// `promptOnly`) to `text`. Used for candidate presentation rendering.
pub fn apply_display_scripts(
    scripts: &[RegexScript],
    text: &str,
    finder: &mut impl FnMut(&str, &str, &str) -> Result<Vec<RegexMatch>, EcmaRegexError>,
) -> Result<String, EcmaRegexError> {
    let mut current = text.to_owned();
    for script in scripts {
        if !script.active_for_display(RegexPlacement::AiOutput, 0) {
            continue;
        }
        let matches = finder(&script.find_pattern, &script.find_flags, &current)?;
        if matches.is_empty() {
            continue;
        }
        let mut output = String::with_capacity(current.len());
        let mut cursor = 0;
        for matched in &matches {
            output.push_str(&current[cursor..matched.start]);
            let replacement =
                expand_replacement(&script.replace_string, matched, &script.trim_strings);
            output.push_str(&replacement);
            cursor = matched.end;
        }
        output.push_str(&current[cursor..]);
        current = output;
    }
    Ok(current)
}

pub fn regex_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn parse_find(find: &str) -> (String, String) {
    if let Some(rest) = find.strip_prefix('/')
        && let Some(last) = rest.rfind('/')
    {
        let flags = &rest[last + 1..];
        if flags
            .chars()
            .all(|character| character.is_ascii_alphabetic())
        {
            return (rest[..last].to_owned(), flags.to_owned());
        }
    }
    (find.to_owned(), String::new())
}

/// Expand a SillyTavern `replaceString`: `{{match}}` resolves to the whole
/// match, and `$0`..`$n` resolve to capture groups (each passed through the
/// script's trim strings). A `$` not followed by digits is left literal.
pub fn expand_replacement(replace: &str, matched: &RegexMatch, trim: &[String]) -> String {
    let source = replace_ci(replace, "{{match}}", "$0");
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '$' {
            out.push(character);
            continue;
        }
        let mut digits = String::new();
        while let Some(next) = chars.peek() {
            if next.is_ascii_digit() {
                digits.push(*next);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            out.push('$');
            continue;
        }
        let value = digits
            .parse::<usize>()
            .ok()
            .and_then(|index| matched.groups.get(index))
            .and_then(Option::as_deref)
            .unwrap_or("");
        out.push_str(&trim_captured(value, trim));
    }
    out
}

fn trim_captured(value: &str, trim: &[String]) -> String {
    let mut result = value.to_owned();
    for needle in trim {
        if !needle.is_empty() {
            result = result.replace(needle.as_str(), "");
        }
    }
    result
}

/// Case-insensitive (ASCII) replacement of every `needle` in `haystack`.
fn replace_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    let hay_lower = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    if needle_lower.is_empty() || !hay_lower.contains(&needle_lower) {
        return haystack.to_owned();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut last = 0;
    let mut search = 0;
    while let Some(offset) = hay_lower[search..].find(&needle_lower) {
        let start = search + offset;
        out.push_str(&haystack[last..start]);
        out.push_str(replacement);
        last = start + needle_lower.len();
        search = last;
    }
    out.push_str(&haystack[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ecma_regex::{RegexReplaceRequest, RegexReplaceResponse, run_replace_worker};

    fn finder(pattern: &str, flags: &str, text: &str) -> Result<Vec<RegexMatch>, EcmaRegexError> {
        match run_replace_worker(RegexReplaceRequest {
            pattern: pattern.to_owned(),
            flags: flags.chars().filter(|f| *f != 'g').collect(),
            global: flags.contains('g'),
            text: text.to_owned(),
        }) {
            RegexReplaceResponse::Matches { matches } => Ok(matches),
            RegexReplaceResponse::Error { message } => Err(EcmaRegexError::Pattern(message)),
        }
    }

    fn no_macros() -> Option<fn(&str, bool) -> String> {
        None
    }

    fn script(find: &str, replace: &str, placements: &[u64]) -> RegexScript {
        RegexScript::from_value(&serde_json::json!({
            "id": "test",
            "scriptName": "Test",
            "findRegex": find,
            "replaceString": replace,
            "placement": placements,
        }))
        .unwrap()
    }

    #[test]
    fn parses_slash_delimited_pattern_and_flags() {
        let parsed = script("/foo/gi", "", &[2]);
        assert_eq!(parsed.find_pattern, "foo");
        assert_eq!(parsed.find_flags, "gi");
    }

    #[test]
    fn treats_bare_string_as_literal_pattern() {
        let parsed = script("a/b", "", &[2]);
        assert_eq!(parsed.find_pattern, "a/b");
        assert_eq!(parsed.find_flags, "");
    }

    #[test]
    fn strips_thinking_block_from_ai_output() {
        let cleanup = script(r"/<think>[\s\S]*?<\/think>\s*/g", "", &[2]);
        let (out, applications) = apply_scripts(
            &[cleanup],
            RegexPlacement::AiOutput,
            0,
            "<think>plan</think>Hello there.",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(out, "Hello there.");
        assert_eq!(applications.len(), 1);
        assert_eq!(applications[0].replacements, 1);
    }

    #[test]
    fn expands_capture_groups_and_match_token() {
        let swap = script(r"/(\w+):(\w+)/g", "{{match}} -> $2/$1", &[1]);
        let (out, _) = apply_scripts(
            &[swap],
            RegexPlacement::UserInput,
            0,
            "a:b c:d",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(out, "a:b -> b/a c:d -> d/c");
    }

    #[test]
    fn applies_trim_strings_to_captured_groups() {
        let mut trimmed = script(r"/\[(.+?)\]/g", "$1", &[2]);
        trimmed.trim_strings = vec!["*".to_owned()];
        let (out, _) = apply_scripts(
            &[trimmed],
            RegexPlacement::AiOutput,
            0,
            "[*bold*] text",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(out, "bold text");
    }

    #[test]
    fn skips_scripts_targeting_other_placements() {
        let user_only = script("secret", "safe", &[1]);
        let (out, applications) = apply_scripts(
            &[user_only],
            RegexPlacement::AiOutput,
            0,
            "a secret value",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(out, "a secret value");
        assert!(applications.is_empty());
    }

    #[test]
    fn markdown_only_scripts_do_not_touch_the_prompt() {
        let mut display = script("plain", "styled", &[2]);
        display.markdown_only = true;
        let (out, applications) = apply_scripts(
            &[display],
            RegexPlacement::AiOutput,
            0,
            "plain",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(out, "plain");
        assert!(applications.is_empty());
    }

    #[test]
    fn respects_depth_bounds() {
        let mut deep = script("x", "y", &[2]);
        deep.min_depth = Some(2);
        let (shallow, applied) = apply_scripts(
            &[deep.clone()],
            RegexPlacement::AiOutput,
            0,
            "x",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(shallow, "x");
        assert!(applied.is_empty());
        let (deep_out, _) = apply_scripts(
            &[deep],
            RegexPlacement::AiOutput,
            3,
            "x",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(deep_out, "y");
    }

    #[test]
    fn chains_scripts_in_order() {
        let first = script("/a/g", "b", &[2]);
        let second = script("/b/g", "c", &[2]);
        let (out, applications) = apply_scripts(
            &[first, second],
            RegexPlacement::AiOutput,
            0,
            "aaa",
            &mut finder,
            &mut no_macros(),
        )
        .unwrap();
        assert_eq!(out, "ccc");
        assert_eq!(applications.len(), 2);
    }

    #[test]
    fn expands_macros_in_replace_string() {
        let s = script(r"/hello/g", "{{char}} says hi", &[2]);
        let mut expander = |input: &str, _escape: bool| input.replace("{{char}}", "Alice");
        let (out, _) = apply_scripts(
            &[s],
            RegexPlacement::AiOutput,
            0,
            "hello world",
            &mut finder,
            &mut Some(&mut expander),
        )
        .unwrap();
        assert_eq!(out, "Alice says hi world");
    }

    #[test]
    fn substitute_regex_raw_expands_macros_in_find_pattern() {
        let mut s = script(r"/{{char}}/g", "NAME", &[2]);
        s.substitute_mode = SubstituteMode::Raw;
        let mut expander = |input: &str, _escape: bool| input.replace("{{char}}", "Alice");
        let (out, _) = apply_scripts(
            &[s],
            RegexPlacement::AiOutput,
            0,
            "Alice walked in",
            &mut finder,
            &mut Some(&mut expander),
        )
        .unwrap();
        assert_eq!(out, "NAME walked in");
    }

    #[test]
    fn substitute_regex_escaped_escapes_metacharacters() {
        let mut s = script(r"/{{char}}/g", "safe", &[1]);
        s.substitute_mode = SubstituteMode::Escaped;
        let mut expander = |input: &str, escape: bool| {
            let expanded = input.replace("{{char}}", "A.B");
            if escape {
                regex_escape(&expanded)
            } else {
                expanded
            }
        };
        let (out_literal, _) = apply_scripts(
            &[s.clone()],
            RegexPlacement::UserInput,
            0,
            "A.B matches but AXB does not",
            &mut finder,
            &mut Some(&mut expander),
        )
        .unwrap();
        assert_eq!(out_literal, "safe matches but AXB does not");
    }

    #[test]
    fn substitute_mode_none_leaves_find_pattern_unchanged() {
        let s = script(r"/{{char}}/g", "NAME", &[2]);
        assert_eq!(s.substitute_mode, SubstituteMode::None);
        let mut expander = |input: &str, _escape: bool| input.replace("{{char}}", "Alice");
        let (out, applied) = apply_scripts(
            &[s],
            RegexPlacement::AiOutput,
            0,
            "{{char}} walked in",
            &mut finder,
            &mut Some(&mut expander),
        )
        .unwrap();
        assert_eq!(out, "NAME walked in");
        assert_eq!(applied.len(), 1);
    }

    #[test]
    fn display_scripts_include_markdown_only() {
        let mut display = script("/plain/g", "**bold**", &[2]);
        display.markdown_only = true;
        let out = apply_display_scripts(&[display], "plain text", &mut finder).unwrap();
        assert_eq!(out, "**bold** text");
    }

    #[test]
    fn display_scripts_exclude_prompt_only() {
        let mut prompt = script("/secret/g", "REDACTED", &[2]);
        prompt.prompt_only = true;
        let out = apply_display_scripts(&[prompt], "secret data", &mut finder).unwrap();
        assert_eq!(out, "secret data");
    }
}
