use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::{
    EntityId, InstalledPlugin, PluginEffect, PluginEvent, PluginGrant, PluginHost, PluginInput,
    PluginReceipt, PluginRuntime, StateMutation, StateTransaction, Store, VariableScope,
    state::{StateError, apply_plugin_command_state_mutations, apply_state_mutations},
    storage::{StorageError, append_event},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StscriptProgram {
    pub commands: Vec<StscriptCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StscriptCommand {
    pub name: String,
    pub named: BTreeMap<String, String>,
    pub unnamed: String,
    pub closure: Option<StscriptProgram>,
    pub else_closure: Option<StscriptProgram>,
    pub pipe_input: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StscriptLimits {
    pub max_steps: usize,
    pub max_depth: usize,
    pub timeout: Duration,
}

impl Default for StscriptLimits {
    fn default() -> Self {
        Self {
            max_steps: 10_000,
            max_depth: 32,
            timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StscriptResult {
    Completed { output: String },
    Aborted { output: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StscriptReplayOutcome {
    pub output: String,
    pub delays: Vec<Duration>,
    pub state_mutations: Vec<StateMutation>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExtensionCommandTrace {
    pub command_execution_id: EntityId,
    pub session_id: EntityId,
    pub plugin_id: String,
    pub command: String,
    pub named: BTreeMap<String, String>,
    pub unnamed: String,
    pub output: String,
    pub receipt: Option<PluginReceipt>,
    pub state_mutations: Vec<StateMutation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn parse_stscript(source: &str) -> Result<StscriptProgram, StscriptError> {
    StscriptProgram::parse(source)
}

impl StscriptProgram {
    pub fn parse(source: &str) -> Result<Self, StscriptError> {
        Self::parse_with_depth_limit(source, StscriptLimits::default().max_depth)
    }

    fn parse_with_depth_limit(source: &str, max_depth: usize) -> Result<Self, StscriptError> {
        Parser::new(source, 0, max_depth).parse_program()
    }

    pub fn evaluate_replay(
        &self,
        limits: StscriptLimits,
    ) -> Result<StscriptReplayOutcome, StscriptError> {
        self.evaluate_replay_with_extension_commands(limits, &[])
    }

    pub fn evaluate_replay_with_extension_commands(
        &self,
        limits: StscriptLimits,
        extension_commands: &[ExtensionCommandTrace],
    ) -> Result<StscriptReplayOutcome, StscriptError> {
        let session_id = extension_commands
            .first()
            .map_or_else(EntityId::new, |command| command.session_id);
        let mut state = StateTransaction::empty(session_id);
        let mut evaluator = Evaluator::replay(&mut state, limits, extension_commands);
        let result = evaluator.evaluate(self, 0)?;
        Ok(StscriptReplayOutcome {
            output: result.output().to_owned(),
            delays: evaluator.delays,
            state_mutations: extension_commands
                .iter()
                .flat_map(|command| command.state_mutations.iter().cloned())
                .collect(),
        })
    }
}

struct ExtensionCommandRuntime {
    session_id: EntityId,
    plugins: Vec<InstalledPlugin>,
    grants: BTreeMap<String, PluginGrant>,
    host: PluginHost,
}

impl ExtensionCommandRuntime {
    fn load(store: &Store, session_id: EntityId) -> Result<Self, StscriptError> {
        let (_, configuration) = store.session_configuration(session_id)?;
        let (plugins, grants) = store.configured_runtime_plugins(&configuration)?;
        Ok(Self {
            session_id,
            plugins: plugins
                .into_iter()
                .filter(|plugin| plugin.manifest.runtime == PluginRuntime::StBridge)
                .collect(),
            grants,
            host: PluginHost::new(Default::default()),
        })
    }

    fn resolve(
        &self,
        command: &str,
        named: &BTreeMap<String, String>,
        unnamed: &str,
        state: &mut StateTransaction,
        traces: &mut Vec<ExtensionCommandTrace>,
    ) -> Result<Option<String>, StscriptError> {
        for installed in &self.plugins {
            let grant = &self.grants[&installed.manifest.id];
            let input = PluginInput {
                event: PluginEvent::Command,
                plugin_id: installed.manifest.id.clone(),
                settings: grant.settings.clone(),
                context: json!({
                    "session_id": self.session_id,
                    "command": command,
                    "named": named,
                    "unnamed": unnamed,
                }),
                payload: json!({
                    "command": command,
                    "named": named,
                    "unnamed": unnamed,
                }),
                state: json!(
                    state.local_namespace(&format!("extension.{}", installed.manifest.id))
                ),
                artifact: serde_json::Value::Null,
                session: json!({"session_id": self.session_id}),
            };
            let command_execution_id = EntityId::new();
            let receipt = match self.host.execute(installed, grant, input) {
                Ok(receipt) => receipt,
                Err(crate::PluginError::StBridgeCommandNotRegistered(_)) => continue,
                Err(error) => {
                    let message = error.to_string();
                    traces.push(ExtensionCommandTrace {
                        command_execution_id,
                        session_id: self.session_id,
                        plugin_id: installed.manifest.id.clone(),
                        command: command.to_owned(),
                        named: named.clone(),
                        unnamed: unnamed.to_owned(),
                        output: String::new(),
                        receipt: None,
                        state_mutations: Vec::new(),
                        error: Some(message),
                    });
                    return Err(error.into());
                }
            };
            let output = receipt
                .effects
                .iter()
                .find_map(|effect| match effect {
                    PluginEffect::Observe { value } => {
                        value.get("output").and_then(serde_json::Value::as_str)
                    }
                    _ => None,
                })
                .unwrap_or_default()
                .to_owned();
            let mut state_mutations = Vec::new();
            for effect in &receipt.effects {
                if let PluginEffect::StateWrite { key, value } = effect {
                    let before = state.get(key.scope, &key.name).cloned();
                    let after = state
                        .set(
                            key.scope,
                            &key.name,
                            value.clone(),
                            &installed.manifest.id,
                            "extension-command",
                        )
                        .clone();
                    state_mutations.push(StateMutation {
                        key: key.clone(),
                        before,
                        after: Some(after),
                    });
                }
            }
            traces.push(ExtensionCommandTrace {
                command_execution_id,
                session_id: self.session_id,
                plugin_id: installed.manifest.id.clone(),
                command: command.to_owned(),
                named: named.clone(),
                unnamed: unnamed.to_owned(),
                output: output.clone(),
                receipt: Some(receipt),
                state_mutations,
                error: None,
            });
            return Ok(Some(output));
        }
        Ok(None)
    }
}

impl Store {
    pub fn execute_stscript(
        &mut self,
        session_id: EntityId,
        execution_id: EntityId,
        source: &str,
        limits: StscriptLimits,
    ) -> Result<StscriptResult, StscriptError> {
        let program = StscriptProgram::parse_with_depth_limit(source, limits.max_depth)?;
        let mut state = self.state_transaction(session_id)?;
        let mut evaluator = Evaluator::live(&mut state, limits, self, session_id);
        let result = match evaluator.evaluate(&program, 0) {
            Ok(result) => result,
            Err(error) => {
                let steps = evaluator.steps;
                let message = error.to_string();
                let command_traces = std::mem::take(&mut evaluator.command_traces);
                drop(evaluator);
                drop(state);
                let transaction = self
                    .connection
                    .transaction()
                    .map_err(StorageError::Sqlite)?;
                append_extension_command_traces(&transaction, &command_traces, false)?;
                append_event(
                    &transaction,
                    Some(session_id),
                    "stscript.failed",
                    &json!({
                        "execution_id": execution_id,
                        "source": source,
                        "error": message,
                        "steps": steps,
                    }),
                )?;
                transaction.commit().map_err(StorageError::Sqlite)?;
                return Err(error);
            }
        };
        let steps = evaluator.steps;
        let delays = evaluator.delays.clone();
        let command_traces = std::mem::take(&mut evaluator.command_traces);
        drop(evaluator);
        let mutations = state.mutations();
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_extension_command_traces(&transaction, &command_traces, true)?;
        apply_state_mutations(&transaction, session_id, execution_id, &mutations)?;
        append_event(
            &transaction,
            Some(session_id),
            "stscript.executed",
            &json!({
                "execution_id": execution_id,
                "source": source,
                "result": result,
                "steps": steps,
                "delays_ms": delays.iter().map(Duration::as_millis).collect::<Vec<_>>(),
            }),
        )?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(result)
    }
}

fn append_extension_command_traces(
    transaction: &rusqlite::Transaction<'_>,
    traces: &[ExtensionCommandTrace],
    commit_state: bool,
) -> Result<(), StscriptError> {
    for trace in traces {
        append_event(
            transaction,
            Some(trace.session_id),
            "extension.command",
            &json!(trace),
        )?;
        if commit_state {
            apply_plugin_command_state_mutations(
                transaction,
                trace.session_id,
                trace.command_execution_id,
                &trace.state_mutations,
            )?;
        }
    }
    Ok(())
}

struct Evaluator<'a> {
    state: &'a mut StateTransaction,
    locals: BTreeMap<String, String>,
    pipe: String,
    last_condition: Option<bool>,
    steps: usize,
    limits: StscriptLimits,
    started: Instant,
    replay: bool,
    delays: Vec<Duration>,
    store: Option<&'a Store>,
    session_id: EntityId,
    extension_runtime: Option<ExtensionCommandRuntime>,
    replay_commands: &'a [ExtensionCommandTrace],
    replay_command_index: usize,
    command_traces: Vec<ExtensionCommandTrace>,
}

impl<'a> Evaluator<'a> {
    fn live(
        state: &'a mut StateTransaction,
        limits: StscriptLimits,
        store: &'a Store,
        session_id: EntityId,
    ) -> Self {
        Self::new(state, limits, false, Some(store), session_id, &[])
    }

    fn replay(
        state: &'a mut StateTransaction,
        limits: StscriptLimits,
        commands: &'a [ExtensionCommandTrace],
    ) -> Self {
        let session_id = state.session_id();
        Self::new(state, limits, true, None, session_id, commands)
    }

    fn new(
        state: &'a mut StateTransaction,
        limits: StscriptLimits,
        replay: bool,
        store: Option<&'a Store>,
        session_id: EntityId,
        replay_commands: &'a [ExtensionCommandTrace],
    ) -> Self {
        Self {
            state,
            locals: BTreeMap::new(),
            pipe: String::new(),
            last_condition: None,
            steps: 0,
            limits,
            started: Instant::now(),
            replay,
            delays: Vec::new(),
            store,
            session_id,
            extension_runtime: None,
            replay_commands,
            replay_command_index: 0,
            command_traces: Vec::new(),
        }
    }

    fn evaluate(
        &mut self,
        program: &StscriptProgram,
        depth: usize,
    ) -> Result<StscriptResult, StscriptError> {
        if depth > self.limits.max_depth {
            return Err(StscriptError::DepthLimit {
                limit: self.limits.max_depth,
            });
        }
        for command in &program.commands {
            self.tick()?;
            if let Some(result) = self.execute(command, depth)? {
                return Ok(result);
            }
        }
        Ok(StscriptResult::Completed {
            output: self.pipe.clone(),
        })
    }

    fn execute(
        &mut self,
        command: &StscriptCommand,
        depth: usize,
    ) -> Result<Option<StscriptResult>, StscriptError> {
        let inherited = if command.pipe_input {
            self.pipe.clone()
        } else {
            String::new()
        };
        let unnamed = if command.unnamed.is_empty() {
            inherited
        } else {
            self.expand(&command.unnamed)?
        };
        let named = command
            .named
            .iter()
            .map(|(key, value)| Ok((key.clone(), self.expand(value)?)))
            .collect::<Result<BTreeMap<_, _>, StscriptError>>()?;
        if !matches!(command.name.as_str(), "if" | "else") {
            self.last_condition = None;
        }
        match command.name.as_str() {
            "pass" | "echo" => self.pipe = unnamed,
            "setvar" | "setglobalvar" | "let" => {
                let named_key = named.get("key").or_else(|| named.get("name")).cloned();
                let key = named_key
                    .clone()
                    .or_else(|| first_word(&unnamed).map(str::to_owned))
                    .ok_or_else(|| StscriptError::MissingArgument {
                        command: command.name.clone(),
                        argument: "key",
                    })?;
                let value = named.get("value").cloned().unwrap_or_else(|| {
                    if named_key.is_some() && !unnamed.is_empty() {
                        unnamed.clone()
                    } else {
                        value_after_key(&unnamed, &key).unwrap_or_else(|| self.pipe.clone())
                    }
                });
                if command.name == "let" {
                    self.locals.insert(key, value.clone());
                } else {
                    let scope = if command.name == "setglobalvar" {
                        VariableScope::Global
                    } else {
                        VariableScope::Local
                    };
                    self.state
                        .set_raw(scope, key, &value, "stscript", &command.name);
                }
                self.pipe = value;
            }
            "getvar" | "getglobalvar" => {
                let key = named
                    .get("key")
                    .or_else(|| named.get("name"))
                    .map(String::as_str)
                    .or_else(|| first_word(&unnamed))
                    .unwrap_or_default();
                let scope = if command.name == "getglobalvar" {
                    VariableScope::Global
                } else {
                    VariableScope::Local
                };
                self.pipe = self
                    .state
                    .get(scope, key)
                    .map(|cell| cell.raw_value.clone())
                    .unwrap_or_default();
            }
            "incvar" | "decvar" => {
                let key = first_word(&unnamed).unwrap_or_default();
                let delta = if command.name == "incvar" { 1 } else { -1 };
                self.pipe = self
                    .state
                    .increment(VariableScope::Local, key, delta, "stscript", &command.name)
                    .raw_value
                    .clone();
            }
            "eval" => self.pipe = format_number(eval_arithmetic(&unnamed)?),
            "if" => {
                let condition = self.command_condition(command)?;
                let branch = if condition {
                    command.closure.as_ref()
                } else {
                    command.else_closure.as_ref()
                };
                if let Some(branch) = branch {
                    let result = self.evaluate(branch, depth + 1)?;
                    if matches!(result, StscriptResult::Aborted { .. }) {
                        return Ok(Some(result));
                    }
                    self.pipe = result.output().to_owned();
                } else {
                    self.pipe.clear();
                }
                self.last_condition = Some(condition);
            }
            "else" => {
                let execute = self.last_condition.take() == Some(false);
                if execute && let Some(branch) = &command.closure {
                    let result = self.evaluate(branch, depth + 1)?;
                    if matches!(result, StscriptResult::Aborted { .. }) {
                        return Ok(Some(result));
                    }
                    self.pipe = result.output().to_owned();
                }
            }
            "while" => {
                let Some(body) = &command.closure else {
                    return Err(StscriptError::MissingClosure(command.name.clone()));
                };
                while self.command_condition(command)? {
                    self.tick()?;
                    let result = self.evaluate(body, depth + 1)?;
                    if matches!(result, StscriptResult::Aborted { .. }) {
                        return Ok(Some(result));
                    }
                    self.pipe = result.output().to_owned();
                }
            }
            "delay" => {
                let milliseconds = unnamed
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| StscriptError::InvalidDelay(unnamed.clone()))?;
                let duration = Duration::from_millis(milliseconds);
                self.delays.push(duration);
                if !self.replay {
                    let remaining = self.limits.timeout.saturating_sub(self.started.elapsed());
                    if duration > remaining {
                        return Err(StscriptError::Timeout {
                            timeout: self.limits.timeout,
                        });
                    }
                    std::thread::sleep(duration);
                }
            }
            "abort" => {
                return Ok(Some(StscriptResult::Aborted {
                    output: self.pipe.clone(),
                }));
            }
            name => {
                let Some(output) = self.resolve_extension_command(name, &named, &unnamed)? else {
                    return Err(StscriptError::UnknownCommand(name.to_owned()));
                };
                self.pipe = output;
            }
        }
        Ok(None)
    }

    fn resolve_extension_command(
        &mut self,
        command: &str,
        named: &BTreeMap<String, String>,
        unnamed: &str,
    ) -> Result<Option<String>, StscriptError> {
        if self.replay {
            let Some(recorded) = self.replay_commands.get(self.replay_command_index) else {
                return Ok(None);
            };
            if recorded.command != command
                || &recorded.named != named
                || recorded.unnamed != unnamed
            {
                return Err(StscriptError::ReplayCommandMismatch {
                    expected: recorded.command.clone(),
                    actual: command.to_owned(),
                });
            }
            self.replay_command_index += 1;
            if let Some(error) = &recorded.error {
                return Err(StscriptError::RecordedExtensionCommandFailed(error.clone()));
            }
            self.state
                .apply_recorded_mutations(&recorded.state_mutations);
            return Ok(Some(recorded.output.clone()));
        }

        if self.extension_runtime.is_none() {
            let store = self.store.expect("live evaluator has a Store");
            self.extension_runtime = Some(ExtensionCommandRuntime::load(store, self.session_id)?);
        }
        self.extension_runtime.as_ref().unwrap().resolve(
            command,
            named,
            unnamed,
            self.state,
            &mut self.command_traces,
        )
    }

    fn command_condition(&self, command: &StscriptCommand) -> Result<bool, StscriptError> {
        let named = command
            .named
            .iter()
            .map(|(key, value)| Ok((key.clone(), self.expand(value)?)))
            .collect::<Result<BTreeMap<_, _>, StscriptError>>()?;
        self.condition(&named, &self.expand(&command.unnamed)?)
    }

    fn condition(
        &self,
        named: &BTreeMap<String, String>,
        unnamed: &str,
    ) -> Result<bool, StscriptError> {
        let (left, rule, right) =
            if let (Some(left), Some(right)) = (named.get("left"), named.get("right")) {
                (
                    left.clone(),
                    named
                        .get("rule")
                        .cloned()
                        .unwrap_or_else(|| "eq".to_owned()),
                    right.clone(),
                )
            } else {
                let tokens = unnamed.split_whitespace().collect::<Vec<_>>();
                if tokens.len() < 3 {
                    return Err(StscriptError::InvalidCondition(unnamed.to_owned()));
                }
                (
                    tokens[0].to_owned(),
                    comparison_rule(tokens[1]).to_owned(),
                    tokens[2..].join(" "),
                )
            };
        compare(
            &self.resolve_operand(&left),
            &self.resolve_operand(&right),
            &rule,
        )
    }

    fn resolve_operand(&self, value: &str) -> String {
        if value.parse::<f64>().is_ok() {
            return value.to_owned();
        }
        self.locals
            .get(value)
            .cloned()
            .or_else(|| {
                self.state
                    .get_local_then_global(value)
                    .map(|cell| cell.raw_value.clone())
            })
            .unwrap_or_else(|| value.to_owned())
    }

    fn expand(&self, source: &str) -> Result<String, StscriptError> {
        let mut output = String::new();
        let mut rest = source;
        while let Some(start) = rest.find("{{") {
            output.push_str(&rest[..start]);
            let body_start = start + 2;
            let end = matching_macro_end(rest, body_start)?;
            let body = &rest[body_start..end];
            let replacement = if body == "pipe" {
                self.pipe.clone()
            } else if let Some(name) = body.strip_prefix("getvar::") {
                self.state
                    .get(VariableScope::Local, name)
                    .map(|cell| cell.raw_value.clone())
                    .unwrap_or_default()
            } else if let Some(name) = body.strip_prefix("getglobalvar::") {
                self.state
                    .get(VariableScope::Global, name)
                    .map(|cell| cell.raw_value.clone())
                    .unwrap_or_default()
            } else if let Some(name) = body.strip_prefix("getlocal::") {
                self.locals.get(name).cloned().unwrap_or_default()
            } else if let Some(expression) = body.strip_prefix("eval::") {
                format_number(eval_arithmetic(&self.expand(expression)?)?)
            } else {
                format!("{{{{{body}}}}}")
            };
            output.push_str(&replacement);
            rest = &rest[end + 2..];
        }
        output.push_str(rest);
        Ok(output)
    }

    fn tick(&mut self) -> Result<(), StscriptError> {
        if self.steps >= self.limits.max_steps {
            return Err(StscriptError::StepLimit {
                limit: self.limits.max_steps,
            });
        }
        if self.started.elapsed() > self.limits.timeout {
            return Err(StscriptError::Timeout {
                timeout: self.limits.timeout,
            });
        }
        self.steps += 1;
        Ok(())
    }
}
fn matching_macro_end(source: &str, body_start: usize) -> Result<usize, StscriptError> {
    let bytes = source.as_bytes();
    let mut offset = body_start;
    let mut depth = 1usize;
    while offset + 1 < bytes.len() {
        match &bytes[offset..offset + 2] {
            b"{{" => {
                depth += 1;
                offset += 2;
            }
            b"}}" => {
                depth -= 1;
                if depth == 0 {
                    return Ok(offset);
                }
                offset += 2;
            }
            _ => offset += 1,
        }
    }
    Err(StscriptError::UnclosedMacro)
}

impl StscriptResult {
    fn output(&self) -> &str {
        match self {
            Self::Completed { output } | Self::Aborted { output } => output,
        }
    }
}

struct Parser<'a> {
    source: &'a str,
    depth: usize,
    max_depth: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, depth: usize, max_depth: usize) -> Self {
        Self {
            source,
            depth,
            max_depth,
        }
    }

    fn parse_program(&self) -> Result<StscriptProgram, StscriptError> {
        let segments = split_top_level(self.source, '|')?;
        let mut commands = Vec::new();
        let mut pipe_input = false;
        for segment in segments {
            let source = segment.trim();
            if source.is_empty() {
                pipe_input = false;
                continue;
            }
            let mut command = parse_command(source, self.depth, self.max_depth)?;
            command.pipe_input = pipe_input;
            commands.push(command);
            pipe_input = true;
        }
        Ok(StscriptProgram { commands })
    }
}

fn parse_command(
    source: &str,
    depth: usize,
    max_depth: usize,
) -> Result<StscriptCommand, StscriptError> {
    let Some(source) = source.strip_prefix('/') else {
        return Err(StscriptError::ExpectedCommand(source.to_owned()));
    };
    let tokens = tokenize(source)?;
    let Some(name) = tokens.first().map(|name| name.to_ascii_lowercase()) else {
        return Err(StscriptError::ExpectedCommand(source.to_owned()));
    };
    let mut named = BTreeMap::new();
    let mut unnamed = Vec::new();
    let mut closure = None;
    let mut else_closure = None;
    for token in tokens.into_iter().skip(1) {
        if let Some((key, value)) = split_named(&token) {
            if key == "else" && is_closure(value) {
                else_closure = Some(parse_closure(value, depth, max_depth)?);
            } else {
                named.insert(key.to_owned(), unquote(value)?);
            }
        } else if is_closure(&token) {
            closure = Some(parse_closure(&token, depth, max_depth)?);
        } else {
            unnamed.push(unquote(&token)?);
        }
    }
    Ok(StscriptCommand {
        name,
        named,
        unnamed: unnamed.join(" "),
        closure,
        else_closure,
        pipe_input: false,
    })
}

fn parse_closure(
    source: &str,
    depth: usize,
    max_depth: usize,
) -> Result<StscriptProgram, StscriptError> {
    if depth >= max_depth {
        return Err(StscriptError::DepthLimit { limit: max_depth });
    }
    Parser::new(closure_body(source)?, depth + 1, max_depth).parse_program()
}

fn split_top_level(source: &str, separator: char) -> Result<Vec<String>, StscriptError> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = source.chars().peekable();
    let mut quote = None;
    let mut depth = 0usize;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            current.push(character);
            continue;
        }
        if let Some(expected) = quote {
            current.push(character);
            if character == expected {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            current.push(character);
            continue;
        }
        if character == '{' && chars.peek() == Some(&':') {
            depth += 1;
            current.push(character);
            current.push(chars.next().unwrap());
            continue;
        }
        if character == ':' && chars.peek() == Some(&'}') {
            if depth == 0 {
                return Err(StscriptError::UnexpectedClosureEnd);
            }
            depth -= 1;
            current.push(character);
            current.push(chars.next().unwrap());
            continue;
        }
        if character == separator && depth == 0 {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    if quote.is_some() {
        return Err(StscriptError::UnclosedQuote);
    }
    if depth != 0 {
        return Err(StscriptError::UnclosedClosure);
    }
    parts.push(current);
    Ok(parts)
}

fn tokenize(source: &str) -> Result<Vec<String>, StscriptError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = source.chars().peekable();
    let mut quote = None;
    let mut depth = 0usize;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(expected) = quote {
            current.push(character);
            if character == expected {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            current.push(character);
        } else if character == '{' && chars.peek() == Some(&':') {
            depth += 1;
            current.push(character);
            current.push(chars.next().unwrap());
        } else if character == ':' && chars.peek() == Some(&'}') {
            if depth == 0 {
                return Err(StscriptError::UnexpectedClosureEnd);
            }
            depth -= 1;
            current.push(character);
            current.push(chars.next().unwrap());
        } else if character.is_whitespace() && depth == 0 {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if quote.is_some() {
        return Err(StscriptError::UnclosedQuote);
    }
    if depth != 0 {
        return Err(StscriptError::UnclosedClosure);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn split_named(token: &str) -> Option<(&str, &str)> {
    let index = token.find('=')?;
    let key = &token[..index];
    (!key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some((key, &token[index + 1..]))
}

fn is_closure(value: &str) -> bool {
    value.starts_with("{:") && value.ends_with(":}")
}

fn closure_body(value: &str) -> Result<&str, StscriptError> {
    value
        .strip_prefix("{:")
        .and_then(|value| value.strip_suffix(":}"))
        .ok_or(StscriptError::UnclosedClosure)
}

fn unquote(value: &str) -> Result<String, StscriptError> {
    if let Some(first) = value.chars().next()
        && matches!(first, '\'' | '"')
    {
        if !value.ends_with(first) || value.len() < 2 {
            return Err(StscriptError::UnclosedQuote);
        }
        return Ok(value[1..value.len() - 1].to_owned());
    }
    Ok(value.to_owned())
}

fn first_word(value: &str) -> Option<&str> {
    value
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
}

fn value_after_key(value: &str, key: &str) -> Option<String> {
    let rest = value.strip_prefix(key)?.trim_start();
    (!rest.is_empty()).then(|| rest.to_owned())
}

fn comparison_rule(operator: &str) -> &str {
    match operator {
        "==" | "=" => "eq",
        "!=" => "neq",
        "<" => "lt",
        ">" => "gt",
        "<=" => "lte",
        ">=" => "gte",
        rule => rule,
    }
}

fn compare(left: &str, right: &str, rule: &str) -> Result<bool, StscriptError> {
    let numeric = || Some((left.parse::<f64>().ok()?, right.parse::<f64>().ok()?));
    match rule {
        "eq" => Ok(numeric().map_or_else(|| left == right, |(left, right)| left == right)),
        "neq" => Ok(numeric().map_or_else(|| left != right, |(left, right)| left != right)),
        "lt" => Ok(numeric().map_or_else(|| left < right, |(left, right)| left < right)),
        "gt" => Ok(numeric().map_or_else(|| left > right, |(left, right)| left > right)),
        "lte" => Ok(numeric().map_or_else(|| left <= right, |(left, right)| left <= right)),
        "gte" => Ok(numeric().map_or_else(|| left >= right, |(left, right)| left >= right)),
        "not" => Ok(!truthy(left)),
        "in" => Ok(left
            .to_ascii_lowercase()
            .contains(&right.to_ascii_lowercase())),
        "nin" => Ok(!left
            .to_ascii_lowercase()
            .contains(&right.to_ascii_lowercase())),
        _ => Err(StscriptError::UnknownRule(rule.to_owned())),
    }
}

fn truthy(value: &str) -> bool {
    !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
}

fn eval_arithmetic(source: &str) -> Result<f64, StscriptError> {
    ArithmeticParser::new(source).parse()
}

struct ArithmeticParser<'a> {
    source: &'a [u8],
    offset: usize,
}

impl<'a> ArithmeticParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            offset: 0,
        }
    }

    fn parse(mut self) -> Result<f64, StscriptError> {
        let value = self.expression()?;
        self.whitespace();
        if self.offset != self.source.len() {
            return Err(StscriptError::InvalidExpression(
                String::from_utf8_lossy(self.source).into_owned(),
            ));
        }
        Ok(value)
    }

    fn expression(&mut self) -> Result<f64, StscriptError> {
        let mut value = self.term()?;
        loop {
            self.whitespace();
            match self.peek() {
                Some(b'+') => {
                    self.offset += 1;
                    value += self.term()?;
                }
                Some(b'-') => {
                    self.offset += 1;
                    value -= self.term()?;
                }
                _ => return Ok(value),
            }
        }
    }

    fn term(&mut self) -> Result<f64, StscriptError> {
        let mut value = self.factor()?;
        loop {
            self.whitespace();
            match self.peek() {
                Some(b'*') => {
                    self.offset += 1;
                    value *= self.factor()?;
                }
                Some(b'/') => {
                    self.offset += 1;
                    value /= self.factor()?;
                }
                _ => return Ok(value),
            }
        }
    }

    fn factor(&mut self) -> Result<f64, StscriptError> {
        self.whitespace();
        if self.peek() == Some(b'(') {
            self.offset += 1;
            let value = self.expression()?;
            self.whitespace();
            if self.peek() != Some(b')') {
                return Err(StscriptError::InvalidExpression(
                    String::from_utf8_lossy(self.source).into_owned(),
                ));
            }
            self.offset += 1;
            return Ok(value);
        }
        let start = self.offset;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.offset += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.')) {
            self.offset += 1;
        }
        if start == self.offset {
            return Err(StscriptError::InvalidExpression(
                String::from_utf8_lossy(self.source).into_owned(),
            ));
        }
        std::str::from_utf8(&self.source[start..self.offset])
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| {
                StscriptError::InvalidExpression(String::from_utf8_lossy(self.source).into_owned())
            })
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(byte) if byte.is_ascii_whitespace()) {
            self.offset += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.offset).copied()
    }
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[derive(Debug, Error)]
pub enum StscriptError {
    #[error("expected slash command, found '{0}'")]
    ExpectedCommand(String),
    #[error("quoted argument is not closed")]
    UnclosedQuote,
    #[error("closure is not closed")]
    UnclosedClosure,
    #[error("unexpected closure end")]
    UnexpectedClosureEnd,
    #[error("macro is not closed")]
    UnclosedMacro,
    #[error("unknown STscript command '/{0}'")]
    UnknownCommand(String),
    #[error("/{command} requires argument '{argument}'")]
    MissingArgument {
        command: String,
        argument: &'static str,
    },
    #[error("/{0} requires a closure")]
    MissingClosure(String),
    #[error("invalid condition '{0}'")]
    InvalidCondition(String),
    #[error("unknown comparison rule '{0}'")]
    UnknownRule(String),
    #[error("invalid arithmetic expression '{0}'")]
    InvalidExpression(String),
    #[error("invalid delay '{0}'")]
    InvalidDelay(String),
    #[error("STscript exceeded the {limit} instruction step limit")]
    StepLimit { limit: usize },
    #[error("STscript exceeded the {limit} closure depth limit")]
    DepthLimit { limit: usize },
    #[error("STscript exceeded its {timeout:?} timeout")]
    Timeout { timeout: Duration },
    #[error("recorded Extension command '/{expected}' does not match replay command '/{actual}'")]
    ReplayCommandMismatch { expected: String, actual: String },
    #[error("recorded Extension command failed: {0}")]
    RecordedExtensionCommandFailed(String),
    #[error(transparent)]
    Plugin(#[from] crate::PluginError),
    #[error(transparent)]
    Turn(#[from] crate::TurnError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}
