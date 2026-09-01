use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use rquickjs::{
    CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Object, Runtime, Value,
    context::{EvalOptions, intrinsic},
};
use serde_json::Value as JsonValue;

use crate::{
    PluginEffect, PluginError, PluginEvent, PluginInput, PromptContribution, PromptSlot,
    ScriptLimits, ScriptLog, StateKey, VariableScope,
};

pub struct ScriptOutcome {
    pub effects: Vec<PluginEffect>,
    pub logs: Vec<ScriptLog>,
}

struct Sink {
    plugin_id: String,
    limits: ScriptLimits,
    state: BTreeMap<String, JsonValue>,
    effects: Vec<PluginEffect>,
    logs: Vec<ScriptLog>,
    injections: usize,
}

pub fn execute(
    plugin_id: &str,
    source: &str,
    event: PluginEvent,
    input_json: &str,
    limits: ScriptLimits,
) -> Result<ScriptOutcome, PluginError> {
    let input = serde_json::from_str::<PluginInput>(input_json)?;
    let state = serde_json::from_value::<BTreeMap<String, JsonValue>>(input.state)?;
    let sink = Rc::new(RefCell::new(Sink {
        plugin_id: plugin_id.to_owned(),
        limits,
        state,
        effects: Vec::new(),
        logs: Vec::new(),
        injections: 0,
    }));

    let runtime = Runtime::new().map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
    runtime.set_memory_limit(limits.memory_bytes);
    runtime.set_max_stack_size(limits.stack_bytes);
    let ticks = Rc::new(Cell::new(limits.interrupt_ticks));
    let budget = Rc::clone(&ticks);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let left = budget.get();
        if left == 0 {
            return true;
        }
        budget.set(left - 1);
        false
    })));
    let context = Context::custom::<(
        intrinsic::Eval,
        intrinsic::Json,
        intrinsic::RegExp,
        intrinsic::RegExpCompiler,
        intrinsic::MapSet,
    )>(&runtime)
    .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;

    context.with(|ctx| -> Result<(), PluginError> {
        install(&ctx, Rc::clone(&sink))
            .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
        let globals = ctx.globals();
        globals
            .remove("eval")
            .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
        globals
            .get::<_, Object>("Math")
            .and_then(|math| math.remove("random"))
            .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;

        let mut options = EvalOptions::default();
        options.strict = false;
        ctx.eval_with_options::<(), _>(source, options)
            .catch(&ctx)
            .map_err(|error| map_caught(&ctx, plugin_id, &ticks, error))?;

        let hook_name = hook_name(event);
        let hook: Value = globals
            .get(hook_name)
            .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
        if !hook.is_function() {
            return Err(PluginError::ScriptHookMissing {
                plugin: plugin_id.to_owned(),
                hook: hook_name.to_owned(),
            });
        }
        let argument = ctx
            .json_parse(input_json)
            .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
        hook.into_function()
            .expect("checked function")
            .call::<_, Value>((argument,))
            .catch(&ctx)
            .map_err(|error| map_caught(&ctx, plugin_id, &ticks, error))?;
        Ok(())
    })?;

    let sink = sink.borrow();
    Ok(ScriptOutcome {
        effects: sink.effects.clone(),
        logs: sink.logs.clone(),
    })
}

fn install<'js>(ctx: &Ctx<'js>, sink: Rc<RefCell<Sink>>) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    let stcli = Object::new(ctx.clone())?;
    let state = Object::new(ctx.clone())?;
    let prompt = Object::new(ctx.clone())?;

    let get_sink = Rc::clone(&sink);
    state.set(
        "get",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, name: String| {
            if !valid_state_name(&name) {
                return Err(Exception::throw_type(
                    &ctx,
                    &format!("invalid state key '{name}'"),
                ));
            }
            let value = get_sink.borrow().state.get(&name).cloned();
            match value {
                Some(value) => {
                    let json = serde_json::to_string(&value).map_err(|_| {
                        Exception::throw_type(&ctx, "state value could not be serialized")
                    })?;
                    ctx.json_parse(json)
                }
                None => Ok(Value::new_undefined(ctx)),
            }
        })?,
    )?;

    let set_sink = Rc::clone(&sink);
    state.set(
        "set",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, name: String, value: Value<'js>| {
                let text = ctx.json_stringify(value)?.ok_or_else(|| {
                    Exception::throw_type(&ctx, "state value must be JSON-serializable")
                })?;
                let text = text.to_string()?;
                if !valid_state_name(&name) {
                    return Err(Exception::throw_type(
                        &ctx,
                        &format!("invalid state key '{name}'"),
                    ));
                }
                let value = serde_json::from_str::<JsonValue>(&text).map_err(|_| {
                    Exception::throw_type(&ctx, "state value must be JSON-serializable")
                })?;
                let mut sink = set_sink.borrow_mut();
                let key = StateKey {
                    scope: VariableScope::Local,
                    name: format!("{}.{}", sink.plugin_id, name),
                };
                sink.effects.push(PluginEffect::StateWrite {
                    key,
                    value: value.clone(),
                });
                sink.state.insert(name, value);
                Ok(())
            },
        )?,
    )?;

    let prompt_sink = Rc::clone(&sink);
    prompt.set(
        "inject",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, slot: String, content: String| -> rquickjs::Result<()> {
                let slot = serde_json::from_value::<PromptSlot>(JsonValue::String(slot.clone()))
                    .map_err(|_| {
                        Exception::throw_type(&ctx, &format!("unknown prompt slot '{slot}'"))
                    })?;
                let mut sink = prompt_sink.borrow_mut();
                sink.injections += 1;
                let ordinal = sink.injections;
                let name = format!("{}#{ordinal}", sink.plugin_id);
                sink.effects.push(PluginEffect::Prompt {
                    contribution: PromptContribution {
                        slot,
                        name,
                        role: "system".to_owned(),
                        content,
                        depth: None,
                        order: ordinal,
                        outlet: None,
                    },
                });
                Ok(())
            },
        )?,
    )?;

    let log_sink = Rc::clone(&sink);
    stcli.set(
        "log",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, level: String, message: String| {
                if !matches!(level.as_str(), "error" | "warn" | "info" | "debug") {
                    return Err(Exception::throw_type(
                        &ctx,
                        &format!("unknown log level '{level}'"),
                    ));
                }
                let mut sink = log_sink.borrow_mut();
                if sink.logs.len() < sink.limits.log_entries {
                    let message = truncate_utf8(message, sink.limits.log_message_bytes);
                    sink.logs.push(ScriptLog { level, message });
                }
                Ok(())
            },
        )?,
    )?;
    let output_sink = Rc::clone(&sink);
    stcli.set(
        "output",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, value: Value<'js>| -> rquickjs::Result<()> {
                let text = ctx.json_stringify(value)?.ok_or_else(|| {
                    Exception::throw_type(&ctx, "output value must be JSON-serializable")
                })?;
                let value =
                    serde_json::from_str::<JsonValue>(&text.to_string()?).map_err(|_| {
                        Exception::throw_type(&ctx, "output value must be JSON-serializable")
                    })?;
                output_sink
                    .borrow_mut()
                    .effects
                    .push(PluginEffect::Output { value });
                Ok(())
            },
        )?,
    )?;

    stcli.set("state", state)?;
    stcli.set("prompt", prompt)?;
    globals.set("stcli", stcli)?;
    Ok(())
}

fn hook_name(event: PluginEvent) -> &'static str {
    match event {
        PluginEvent::PreLore => "preLore",
        PluginEvent::PrePrompt => "prePrompt",
        PluginEvent::PreRequest => "preRequest",
        PluginEvent::PostCommit => "postCommit",
        PluginEvent::Command => "command",
        PluginEvent::ChatCompletionPromptReady => "chatCompletionPromptReady",
        PluginEvent::InspectArtifact => "inspectArtifact",
        PluginEvent::GenerateInterceptor => "generateInterceptor",
        PluginEvent::StBridgeLifecycle => "stBridgeLifecycle",
    }
}

fn valid_state_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
}

fn truncate_utf8(mut value: String, bytes: usize) -> String {
    if value.len() <= bytes {
        return value;
    }
    let mut boundary = bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn map_caught<'js>(
    ctx: &Ctx<'js>,
    plugin_id: &str,
    ticks: &Cell<u64>,
    error: CaughtError<'js>,
) -> PluginError {
    if ticks.get() == 0 {
        return PluginError::ScriptStepLimit;
    }
    let message = match error {
        CaughtError::Exception(exception) => exception.message().unwrap_or_default(),
        CaughtError::Value(value) => ctx
            .json_stringify(value)
            .ok()
            .flatten()
            .and_then(|value| value.to_string().ok())
            .unwrap_or_else(|| "<non-error throw>".to_owned()),
        CaughtError::Error(error) => error.to_string(),
    };
    PluginError::ScriptTrap {
        plugin: plugin_id.to_owned(),
        message,
    }
}
