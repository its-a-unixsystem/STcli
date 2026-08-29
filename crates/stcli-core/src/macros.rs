use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{StateTransaction, VariableScope};

#[derive(Clone, Debug, Default)]
pub struct MacroContext {
    pub values: BTreeMap<String, String>,
    pub outlets: BTreeMap<String, String>,
    pub registered_macros: BTreeMap<String, String>,
    pub plugins: BTreeSet<String>,
    pub random_seed: u64,
}

impl MacroContext {
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.values.insert(name.into().to_lowercase(), value.into());
    }
    pub fn register(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.registered_macros
            .insert(name.into().to_lowercase(), value.into());
    }

    fn value(&self, name: &str) -> String {
        self.values
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MacroRender {
    pub text: String,
    pub evaluations: Vec<MacroEvaluation>,
    pub warnings: Vec<MacroWarning>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MacroEvaluation {
    pub name: String,
    pub arguments: Vec<String>,
    pub output: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MacroWarning {
    pub code: String,
    pub message: String,
}

pub struct MacroEngine {
    rng: DeterministicRng,
}

impl MacroEngine {
    pub fn new(seed: u64) -> Self {
        Self {
            rng: DeterministicRng(seed),
        }
    }

    pub fn render(
        &mut self,
        input: &str,
        context: &MacroContext,
        state: &mut StateTransaction,
    ) -> Result<MacroRender, MacroError> {
        self.render_with_transform(input, context, state, None::<fn(&str) -> String>)
    }

    pub fn render_with_transform(
        &mut self,
        input: &str,
        context: &MacroContext,
        state: &mut StateTransaction,
        transform: Option<impl Fn(&str) -> String>,
    ) -> Result<MacroRender, MacroError> {
        let mut evaluations = Vec::new();
        let mut warnings = Vec::new();
        let mut text = self.render_text(
            input,
            context,
            state,
            &mut evaluations,
            &mut warnings,
            &transform,
        )?;
        if input.trim_end().ends_with("{{trim}}") {
            text = text.trim().to_owned();
        }
        Ok(MacroRender {
            text,
            evaluations,
            warnings,
        })
    }

    fn render_text<F: Fn(&str) -> String>(
        &mut self,
        input: &str,
        context: &MacroContext,
        state: &mut StateTransaction,
        evaluations: &mut Vec<MacroEvaluation>,
        warnings: &mut Vec<MacroWarning>,
        transform: &Option<F>,
    ) -> Result<String, MacroError> {
        let mut output = String::new();
        let mut cursor = 0;
        while let Some(relative) = input[cursor..].find("{{") {
            let start = cursor + relative;
            output.push_str(&input[cursor..start]);
            let Some((tag, after_tag)) = balanced_tag(input, start) else {
                output.push_str(&input[start..]);
                return Ok(output);
            };
            let parsed = ParsedTag::parse(&tag)?;
            if parsed.closing {
                output.push_str(&input[start..after_tag]);
                cursor = after_tag;
                continue;
            }
            if parsed.name.eq_ignore_ascii_case("if")
                && let Some((close_start, close_end, else_range)) =
                    find_matching_close(input, after_tag, &parsed.name)?
            {
                let (truthy_raw, falsy_raw) = if let Some((else_start, else_end)) = else_range {
                    (&input[after_tag..else_start], &input[else_end..close_start])
                } else {
                    (&input[after_tag..close_start], "")
                };
                let condition = self.render_text(
                    parsed
                        .arguments
                        .first()
                        .map(String::as_str)
                        .unwrap_or_default(),
                    context,
                    state,
                    evaluations,
                    warnings,
                    transform,
                )?;
                let condition_matches = resolve_condition(&condition, context, state);
                let chosen = if condition_matches {
                    truthy_raw
                } else {
                    falsy_raw
                };
                let rendered =
                    self.render_text(chosen, context, state, evaluations, warnings, transform)?;
                evaluations.push(MacroEvaluation {
                    name: parsed.name,
                    arguments: vec![condition],
                    output: rendered.clone(),
                });
                output.push_str(&rendered);
                cursor = close_end;
                continue;
            }

            let (body, next_cursor) = if let Some((close_start, close_end, _)) =
                find_matching_close(input, after_tag, &parsed.name)?
            {
                (Some(&input[after_tag..close_start]), close_end)
            } else {
                (None, after_tag)
            };
            let mut arguments =
                Vec::with_capacity(parsed.arguments.len() + usize::from(body.is_some()));
            for argument in parsed.arguments {
                arguments.push(self.render_text(
                    &argument,
                    context,
                    state,
                    evaluations,
                    warnings,
                    transform,
                )?);
            }
            if let Some(body) = body {
                let rendered =
                    self.render_text(body, context, state, evaluations, warnings, transform)?;
                arguments.push(if parsed.preserve_whitespace {
                    rendered
                } else {
                    trim_scoped(&rendered)
                });
            }
            let source = &input[start..next_cursor];
            let mut rendered =
                self.evaluate(&parsed.name, &arguments, source, context, state, warnings)?;
            if let Some(transform) = transform {
                rendered = transform(&rendered);
            }
            evaluations.push(MacroEvaluation {
                name: parsed.name,
                arguments,
                output: rendered.clone(),
            });
            output.push_str(&rendered);
            cursor = next_cursor;
        }
        output.push_str(&input[cursor..]);
        Ok(output)
    }

    fn evaluate(
        &mut self,
        name: &str,
        arguments: &[String],
        source: &str,
        context: &MacroContext,
        state: &mut StateTransaction,
        warnings: &mut Vec<MacroWarning>,
    ) -> Result<String, MacroError> {
        let lower = name.to_lowercase();
        if lower.starts_with("//") {
            return Ok(String::new());
        }
        if let Some(variable) = lower.strip_prefix('.') {
            return variable_shorthand(VariableScope::Local, variable, arguments, state);
        }
        if let Some(variable) = lower.strip_prefix('$') {
            return variable_shorthand(VariableScope::Global, variable, arguments, state);
        }
        let argument = |index: usize| arguments.get(index).map(String::as_str).unwrap_or_default();
        let output = match lower.as_str() {
            "space" => repeat_text(' ', argument(0))?,
            "newline" => repeat_text('\n', argument(0))?,
            "noop" | "else" | "//" => String::new(),
            "trim" => argument(0).trim().to_owned(),
            "reverse" => argument(0).chars().rev().collect(),
            "user"
            | "char"
            | "charprompt"
            | "charinstruction"
            | "chardescription"
            | "charpersonality"
            | "charscenario"
            | "description"
            | "personality"
            | "scenario"
            | "persona"
            | "group"
            | "groupnotmuted"
            | "summary"
            | "short_term_memory"
            | "long_term_memory"
            | "mesexamplesraw"
            | "mesexamples"
            | "chardepthprompt"
            | "charcreatornotes"
            | "charfirstmessage"
            | "charversion"
            | "model"
            | "original"
            | "lastmessage"
            | "lastchatmessage"
            | "lastmessageid"
            | "lastusermessage"
            | "lastcharmessage"
            | "firstincludedmessageid"
            | "firstdisplayedmessageid"
            | "lastswipeid"
            | "currentswipeid"
            | "allchatrange"
            | "time"
            | "date"
            | "weekday"
            | "isotime"
            | "isodate"
            | "datetimeformat"
            | "idleduration"
            | "timediff"
            | "lastgenerationtype"
            | "maxprompt"
            | "maxcontext"
            | "maxresponse" => context.value(&lower),
            "hasextension" => context.plugins.contains(argument(0)).to_string(),
            "outlet" => context
                .outlets
                .get(argument(0))
                .cloned()
                .unwrap_or_default(),
            "setvar" => {
                state.set_raw(
                    VariableScope::Local,
                    argument(0),
                    argument(1),
                    "macro",
                    "setvar",
                );
                String::new()
            }
            "setglobalvar" => {
                state.set_raw(
                    VariableScope::Global,
                    argument(0),
                    argument(1),
                    "macro",
                    "setglobalvar",
                );
                String::new()
            }
            "addvar" => {
                if argument(1).parse::<f64>().is_err()
                    || state.get(VariableScope::Local, argument(0)).is_none()
                {
                    warnings.push(MacroWarning {
                        code: "unknown-macro-preserved".to_owned(),
                        message: format!(
                            "addition target '{}' is unresolved and preserved literally",
                            argument(0)
                        ),
                    });
                    source.to_owned()
                } else {
                    state.add_raw(
                        VariableScope::Local,
                        argument(0),
                        argument(1),
                        "macro",
                        "addvar",
                    );
                    String::new()
                }
            }
            "addglobalvar" => {
                state.add_raw(
                    VariableScope::Global,
                    argument(0),
                    argument(1),
                    "macro",
                    "addglobalvar",
                );
                String::new()
            }
            "incvar" => state
                .increment(VariableScope::Local, argument(0), 1, "macro", "incvar")
                .raw_value
                .clone(),
            "incglobalvar" => state
                .increment(
                    VariableScope::Global,
                    argument(0),
                    1,
                    "macro",
                    "incglobalvar",
                )
                .raw_value
                .clone(),
            "decvar" => state
                .increment(VariableScope::Local, argument(0), -1, "macro", "decvar")
                .raw_value
                .clone(),
            "decglobalvar" => state
                .increment(
                    VariableScope::Global,
                    argument(0),
                    -1,
                    "macro",
                    "decglobalvar",
                )
                .raw_value
                .clone(),
            "getvar" => state
                .get(VariableScope::Local, argument(0))
                .map(|cell| cell.raw_value.clone())
                .unwrap_or_default(),
            "getglobalvar" => {
                if let Some(cell) = state.get(VariableScope::Global, argument(0)) {
                    cell.raw_value.clone()
                } else {
                    warnings.push(MacroWarning {
                        code: "unknown-macro-preserved".to_owned(),
                        message: format!(
                            "extension-owned global variable '{}' is unresolved and preserved literally",
                            argument(0)
                        ),
                    });
                    source.to_owned()
                }
            }
            "hasvar" => state
                .get(VariableScope::Local, argument(0))
                .is_some()
                .to_string(),
            "hasglobalvar" => state
                .get(VariableScope::Global, argument(0))
                .is_some()
                .to_string(),
            "deletevar" => {
                state.delete(VariableScope::Local, argument(0));
                String::new()
            }
            "deleteglobalvar" => {
                state.delete(VariableScope::Global, argument(0));
                String::new()
            }
            "random" => choose(&choice_arguments(arguments), self.rng.next())
                .unwrap_or_default()
                .to_owned(),
            "pick" => {
                let choices = choice_arguments(arguments);
                let digest = Sha256::digest(choices.join("\0").as_bytes());
                let seed = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"));
                choose(&choices, seed).unwrap_or_default().to_owned()
            }
            "roll" => roll(argument(0), &mut self.rng)?,
            "if" => {
                if resolve_condition(argument(0), context, state) {
                    argument(1).to_owned()
                } else {
                    String::new()
                }
            }
            "input" | "banned" | "notchar" | "ismobile" | "systemprompt" => {
                return Err(MacroError::HardUnsupported(name.to_owned()));
            }
            _ => {
                if let Some(value) = context.registered_macros.get(&lower) {
                    value.clone()
                } else {
                    warnings.push(MacroWarning {
                        code: "unknown-macro-preserved".to_owned(),
                        message: format!("macro '{name}' is unresolved and preserved literally"),
                    });
                    source.to_owned()
                }
            }
        };
        Ok(output)
    }
}
fn resolve_condition(condition: &str, context: &MacroContext, state: &StateTransaction) -> bool {
    let condition = condition.trim();
    let (inverted, condition) = condition
        .strip_prefix('!')
        .map(|value| (true, value.trim_start()))
        .unwrap_or((false, condition));
    let value = if let Some(name) = condition.strip_prefix('.') {
        state
            .get(VariableScope::Local, name)
            .map(|cell| cell.raw_value.as_str())
            .unwrap_or_default()
    } else if let Some(name) = condition.strip_prefix('$') {
        state
            .get(VariableScope::Global, name)
            .map(|cell| cell.raw_value.as_str())
            .unwrap_or_default()
    } else {
        context
            .values
            .get(&condition.to_lowercase())
            .map(String::as_str)
            .unwrap_or(condition)
    };
    inverted ^ truthy(value)
}

fn repeat_text(character: char, count: &str) -> Result<String, MacroError> {
    let count = if count.is_empty() {
        1
    } else {
        count
            .parse::<usize>()
            .map_err(|_| MacroError::InvalidCount(count.to_owned()))?
    };
    if count > 10_000 {
        return Err(MacroError::InvalidCount(count.to_string()));
    }
    Ok(std::iter::repeat_n(character, count).collect())
}

fn choice_arguments(arguments: &[String]) -> Vec<String> {
    if arguments.len() != 1 {
        return arguments.to_vec();
    }
    let value = &arguments[0];
    if value.contains("::") {
        value
            .split("::")
            .map(|item| item.trim().to_owned())
            .collect()
    } else {
        value
            .replace("\\,", "\u{0}")
            .split(',')
            .map(|item| item.trim().replace('\u{0}', ","))
            .collect()
    }
}

fn variable_shorthand(
    scope: VariableScope,
    expression: &str,
    arguments: &[String],
    state: &mut StateTransaction,
) -> Result<String, MacroError> {
    let expression = if arguments.is_empty() {
        expression.to_owned()
    } else {
        format!("{expression} {}", arguments.join("::"))
    };
    let operators = [
        "??=", "||=", "+=", "-=", "==", "!=", ">=", "<=", "++", "--", ">", "<", "=", "??", "||",
    ];
    let (name, operator, value) = operators
        .iter()
        .find_map(|operator| {
            expression.find(operator).map(|index| {
                (
                    expression[..index].trim(),
                    *operator,
                    expression[index + operator.len()..].trim(),
                )
            })
        })
        .unwrap_or((expression.trim(), "", ""));
    let current = state.get(scope, name).map(|cell| cell.raw_value.clone());
    let output = match operator {
        "" => current.unwrap_or_default(),
        "=" => {
            state.set_raw(scope, name, value, "macro", "shorthand-set");
            String::new()
        }
        "+=" => {
            state.add_raw(scope, name, value, "macro", "shorthand-add");
            String::new()
        }
        "-=" => {
            let value = value
                .parse::<f64>()
                .map_err(|_| MacroError::InvalidNumber(value.to_owned()))?;
            state.add_raw(
                scope,
                name,
                &(-value).to_string(),
                "macro",
                "shorthand-subtract",
            );
            String::new()
        }
        "++" => state
            .increment(scope, name, 1, "macro", "shorthand-increment")
            .raw_value
            .clone(),
        "--" => state
            .increment(scope, name, -1, "macro", "shorthand-decrement")
            .raw_value
            .clone(),
        "||" => current
            .filter(|current| truthy(current))
            .unwrap_or_else(|| value.to_owned()),
        "??" => current.unwrap_or_else(|| value.to_owned()),
        "||=" => {
            if current.as_deref().is_none_or(|current| !truthy(current)) {
                state.set_raw(scope, name, value, "macro", "shorthand-or-assign");
            }
            state
                .get(scope, name)
                .map(|cell| cell.raw_value.clone())
                .unwrap_or_default()
        }
        "??=" => {
            if current.is_none() {
                state.set_raw(scope, name, value, "macro", "shorthand-nullish-assign");
            }
            state
                .get(scope, name)
                .map(|cell| cell.raw_value.clone())
                .unwrap_or_default()
        }
        "==" | "!=" => {
            let equal = current.unwrap_or_default() == value;
            (if operator == "==" { equal } else { !equal }).to_string()
        }
        ">" | "<" | ">=" | "<=" => {
            let left = current.unwrap_or_default().parse::<f64>().unwrap_or(0.0);
            let right = value.parse::<f64>().unwrap_or(0.0);
            match operator {
                ">" => left > right,
                "<" => left < right,
                ">=" => left >= right,
                "<=" => left <= right,
                _ => unreachable!(),
            }
            .to_string()
        }
        _ => unreachable!(),
    };
    Ok(output)
}

#[derive(Debug)]
struct ParsedTag {
    name: String,
    arguments: Vec<String>,
    closing: bool,
    preserve_whitespace: bool,
}

impl ParsedTag {
    fn parse(tag: &str) -> Result<Self, MacroError> {
        let mut content = tag.trim();
        let closing = content.starts_with('/') && !content.starts_with("//");
        if closing {
            content = content[1..].trim_start();
        }
        let preserve_whitespace = content.starts_with('#');
        if preserve_whitespace {
            content = content[1..].trim_start();
        }
        if content.is_empty() {
            return Err(MacroError::EmptyTag);
        }
        let parts = split_arguments(content);
        let first = parts.first().cloned().unwrap_or_default();
        let (name, mut arguments) = if parts.len() == 1 {
            if first.starts_with('.') || first.starts_with('$') {
                (first, Vec::new())
            } else if let Some((name, argument)) = first.split_once(' ') {
                (name.to_owned(), vec![argument.trim().to_owned()])
            } else if let Some((name, argument)) = first.split_once(':') {
                (name.to_owned(), vec![argument.trim().to_owned()])
            } else {
                (first, Vec::new())
            }
        } else {
            (first, parts[1..].to_vec())
        };
        if (name.starts_with('.') || name.starts_with('$')) && arguments.is_empty() {
            arguments = Vec::new();
        }
        Ok(Self {
            name,
            arguments,
            closing,
            preserve_whitespace,
        })
    }
}

fn balanced_tag(input: &str, start: usize) -> Option<(String, usize)> {
    let bytes = input.as_bytes();
    let mut depth = 0_u32;
    let mut cursor = start;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'{' && bytes[cursor + 1] == b'{' {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor] == b'}' && bytes[cursor + 1] == b'}' {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Some((input[start + 2..cursor - 2].to_owned(), cursor));
            }
        } else {
            cursor += input[cursor..].chars().next()?.len_utf8();
        }
    }
    None
}

type ScopedClose = (usize, usize, Option<(usize, usize)>);

fn find_matching_close(
    input: &str,
    mut cursor: usize,
    name: &str,
) -> Result<Option<ScopedClose>, MacroError> {
    let mut depth = 0_u32;
    let mut else_range = None;
    while let Some(relative) = input[cursor..].find("{{") {
        let start = cursor + relative;
        let Some((tag, end)) = balanced_tag(input, start) else {
            return Ok(None);
        };
        let parsed = ParsedTag::parse(&tag)?;
        if parsed.name.eq_ignore_ascii_case(name) {
            if parsed.closing {
                if depth == 0 {
                    return Ok(Some((start, end, else_range)));
                }
                depth -= 1;
            } else if find_closing_literal(input, end, &parsed.name) {
                depth += 1;
            }
        } else if name.eq_ignore_ascii_case("if")
            && parsed.name.eq_ignore_ascii_case("else")
            && !parsed.closing
            && depth == 0
        {
            else_range = Some((start, end));
        }
        cursor = end;
    }
    Ok(None)
}

fn find_closing_literal(input: &str, cursor: usize, name: &str) -> bool {
    input[cursor..]
        .to_lowercase()
        .contains(&format!("{{{{/{}}}}}", name.to_lowercase()))
}

fn split_arguments(content: &str) -> Vec<String> {
    let bytes = content.as_bytes();
    let mut parts = Vec::new();
    let mut cursor = 0;
    let mut start = 0;
    let mut depth = 0_u32;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'{' && bytes[cursor + 1] == b'{' {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor] == b'}' && bytes[cursor + 1] == b'}' {
            depth = depth.saturating_sub(1);
            cursor += 2;
        } else if bytes[cursor] == b':' && bytes[cursor + 1] == b':' && depth == 0 {
            parts.push(content[start..cursor].trim().to_owned());
            cursor += 2;
            start = cursor;
        } else {
            cursor += content[cursor..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
        }
    }
    parts.push(content[start..].trim().to_owned());
    parts
}

fn trim_scoped(content: &str) -> String {
    let trimmed = content.trim();
    let indentation = trimmed
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .unwrap_or(0);
    trimmed
        .lines()
        .map(|line| line.get(indentation..).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truthy(value: &str) -> bool {
    !value.is_empty()
        && value != "0"
        && !value.eq_ignore_ascii_case("false")
        && !value.eq_ignore_ascii_case("off")
}

fn choose(values: &[String], random: u64) -> Option<&str> {
    if values.is_empty() {
        None
    } else {
        Some(&values[random as usize % values.len()])
    }
}

fn roll(expression: &str, rng: &mut DeterministicRng) -> Result<String, MacroError> {
    let expression = expression.trim();
    let (count, rest) = expression.split_once('d').unwrap_or(("1", expression));
    let count = count
        .parse::<u64>()
        .map_err(|_| MacroError::InvalidRoll(expression.to_owned()))?;
    let (sides, modifier) = rest
        .split_once('+')
        .map(|(sides, modifier)| (sides, modifier.parse::<i64>()))
        .or_else(|| {
            rest.split_once('-')
                .map(|(sides, modifier)| (sides, modifier.parse::<i64>().map(|value| -value)))
        })
        .unwrap_or((rest, Ok(0)));
    let sides = sides
        .parse::<u64>()
        .map_err(|_| MacroError::InvalidRoll(expression.to_owned()))?;
    let modifier = modifier.map_err(|_| MacroError::InvalidRoll(expression.to_owned()))?;
    if count == 0 || sides == 0 || count > 1_000 || sides > 1_000_000 {
        return Err(MacroError::InvalidRoll(expression.to_owned()));
    }
    let total = (0..count)
        .map(|_| (rng.next() % sides + 1) as i64)
        .sum::<i64>()
        + modifier;
    Ok(total.to_string())
}

struct DeterministicRng(u64);

impl DeterministicRng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[derive(Debug, Error)]
pub enum MacroError {
    #[error("macro tag is empty")]
    EmptyTag,
    #[error("macro '{0}' is hard unsupported")]
    HardUnsupported(String),
    #[error("invalid numeric macro argument '{0}'")]
    InvalidNumber(String),
    #[error("invalid dice expression '{0}'")]
    InvalidRoll(String),
    #[error("invalid macro repeat count '{0}'")]
    InvalidCount(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntityId;
    use proptest::prelude::*;

    fn identity_macro_input() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                "[^{}]{0,24}",
                ("[a-z]{1,12}", "[^{}]{0,24}")
                    .prop_map(|(name, body)| format!("{{{{unknown_{name}::{body}}}}}")),
            ],
            0..12,
        )
        .prop_map(|parts| parts.concat())
    }

    fn state() -> StateTransaction {
        StateTransaction::empty(EntityId::new())
    }

    #[test]
    fn nested_macro_arguments_resolve_inside_out() {
        let mut context = MacroContext::default();
        context.insert("char", "Alice");
        let mut state = state();
        state.set_raw(
            VariableScope::Local,
            "Alice_mood",
            "happy",
            "test",
            "fixture",
        );
        let rendered = MacroEngine::new(1)
            .render("{{getvar::{{char}}_mood}}", &context, &mut state)
            .unwrap();
        assert_eq!(rendered.text, "happy");
    }

    #[test]
    fn false_conditional_does_not_execute_discarded_mutation() {
        let context = MacroContext::default();
        let mut state = state();
        let rendered = MacroEngine::new(1)
            .render(
                "{{if::false}}{{setvar::bad::1}}{{else}}{{setvar::good::2}}ok{{/if}}",
                &context,
                &mut state,
            )
            .unwrap();
        assert_eq!(rendered.text, "ok");
        assert!(state.get(VariableScope::Local, "bad").is_none());
        assert_eq!(
            state.get(VariableScope::Local, "good").unwrap().raw_value,
            "2"
        );
    }

    #[test]
    fn cjk_and_emoji_inside_macro_arguments_do_not_split_utf8() {
        let context = MacroContext::default();
        let mut state = state();
        let rendered = MacroEngine::new(1)
            .render(
                "{{setvar::greeting::日本🙂}}{{getvar::greeting}}",
                &context,
                &mut state,
            )
            .unwrap();
        assert_eq!(rendered.text, "日本🙂");
    }

    #[test]
    fn unknown_macro_is_preserved_with_warning() {
        let context = MacroContext::default();
        let mut state = state();
        let rendered = MacroEngine::new(1)
            .render("before {{platformMacro::x}} after", &context, &mut state)
            .unwrap();
        assert_eq!(rendered.text, "before {{platformMacro::x}} after");
        assert_eq!(rendered.warnings.len(), 1);
    }

    #[test]
    fn shorthand_updates_local_variables() {
        let context = MacroContext::default();
        let mut state = state();
        let rendered = MacroEngine::new(1)
            .render(
                "{{.score = 1}}{{.score += 2}}{{.score}}",
                &context,
                &mut state,
            )
            .unwrap();
        assert_eq!(rendered.text, "3");
    }

    #[test]
    fn utility_counts_condition_shorthand_and_hard_unsupported_are_enforced() {
        let mut context = MacroContext::default();
        context.insert("char", "Alice");
        let mut state = state();
        state.set_raw(VariableScope::Local, "enabled", "true", "test", "fixture");
        let rendered = MacroEngine::new(1)
            .render(
                "{{space::2}}{{newline::2}}{{if::.enabled::yes}}{{if::!char::no}}",
                &context,
                &mut state,
            )
            .unwrap();
        assert_eq!(rendered.text, "  \n\nyes");

        let rendered = MacroEngine::new(1)
            .render("{{group}}", &context, &mut state)
            .unwrap();
        assert_eq!(rendered.text, "");
    }

    proptest! {
        #[test]
        fn arbitrary_macro_input_never_panics(input in ".{0,1024}", seed in any::<u64>()) {
            let mut state = state();
            let context = MacroContext::default();
            let _ = MacroEngine::new(seed).render(&input, &context, &mut state);
        }

        #[test]
        fn balanced_unknown_macros_round_trip_under_identity_environment(
            input in identity_macro_input()
        ) {
            let mut state = state();
            let context = MacroContext::default();
            let rendered = MacroEngine::new(0).render(&input, &context, &mut state).unwrap();

            prop_assert_eq!(rendered.text, input);
        }
    }
}
