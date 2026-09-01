//! SillyTavern Extension compatibility runtime.
//!
//! ## Determinism and async bounds
//!
//! The st-bridge runtime provides bounded nondeterminism to maintain STcli's Replay guarantee:
//! - `Math.random()` uses a seeded Xoshiro128++ PRNG whose seed is recorded in the Turn Trace.
//! - Promises drain through a bounded microtask loop; unsettled work is abandoned with a
//!   Compatibility Warning.
//! - Zero-delay `setTimeout` and `setInterval` calls resolve as microtasks; delayed timers are
//!   rejected with a one-time warning.
//! - Memory, stack, and execution step budgets apply to the persistent context.
//!
//! Replay re-applies recorded effects without re-executing the JavaScript.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    rc::Rc,
    sync::{OnceLock, mpsc},
};

use rquickjs::{
    CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Module, Object, Persistent,
    Runtime, Value, context::intrinsic, function::Args, promise::PromiseState,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::{
    ChatMessage, ChatRole, ContentHash, EntityId, InstalledPlugin, PluginEffect, PluginError,
    PluginEvent, PluginInput, PromptRewriteMessage, ScriptLimits, ScriptOutcome,
};

type Listener = Persistent<Function<'static>>;

#[derive(Clone, Eq, Hash, PartialEq)]
struct ContextKey {
    session_id: EntityId,
    plugin_id: String,
    component_sha256: ContentHash,
}

impl ContextKey {
    fn from_request(installed: &InstalledPlugin, input: &PluginInput) -> Result<Self, PluginError> {
        let session_id = input
            .session
            .get("session_id")
            .and_then(JsonValue::as_str)
            .and_then(|value| value.parse::<EntityId>().ok())
            .ok_or(PluginError::StBridgeSessionIdentity)?;
        Ok(Self {
            session_id,
            plugin_id: installed.manifest.id.clone(),
            component_sha256: installed.manifest.component_sha256.clone(),
        })
    }

    fn prng_seed(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish().max(1)
    }
}

struct BridgeContext {
    context: Context,
    listeners: Rc<RefCell<HashMap<String, Vec<Listener>>>>,
    snapshot: Rc<RefCell<JsonValue>>,
    ticks: Rc<Cell<u64>>,
    last_branch_id: Rc<RefCell<Option<EntityId>>>,
    app_ready_emitted: Rc<Cell<bool>>,
    abandoned: Rc<Cell<bool>>,
    prng_seed: u64,
    logs: Rc<RefCell<Vec<crate::ScriptLog>>>,
}

struct Xoshiro128PlusPlus {
    state: [u32; 4],
}

impl Xoshiro128PlusPlus {
    fn from_seed(seed: u64) -> Self {
        let lower = seed as u32;
        let upper = (seed >> 32) as u32;
        Self {
            state: [
                lower,
                upper,
                lower.wrapping_add(0x9E37_79B9),
                upper.wrapping_add(0x9E37_79B9),
            ],
        }
    }

    fn next(&mut self) -> u32 {
        let result = self.state[0]
            .wrapping_add(self.state[3])
            .rotate_left(7)
            .wrapping_add(self.state[0]);
        let t = self.state[1] << 9;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(11);
        result
    }

    fn next_f64(&mut self) -> f64 {
        let upper = (self.next() >> 5) as u64;
        let lower = (self.next() >> 6) as u64;
        ((upper << 26) | lower) as f64 / (1_u64 << 53) as f64
    }
}

struct Request {
    installed: InstalledPlugin,
    input: PluginInput,
    source: String,
    limits: ScriptLimits,
    response: mpsc::SyncSender<Result<ScriptOutcome, PluginError>>,
}

struct WorkerHandle {
    requests: mpsc::Sender<Request>,
}

#[derive(Default)]
struct Worker {
    contexts: HashMap<ContextKey, BridgeContext>,
}

static WORKER: OnceLock<Result<WorkerHandle, ()>> = OnceLock::new();

pub(crate) fn execute(
    installed: &InstalledPlugin,
    input: &PluginInput,
    source: &str,
    limits: ScriptLimits,
) -> Result<ScriptOutcome, PluginError> {
    let worker = WORKER
        .get_or_init(|| {
            let (requests, receiver) = mpsc::channel();
            std::thread::Builder::new()
                .name("stcli-st-bridge".to_owned())
                .spawn(move || Worker::default().run(receiver))
                .map(|_| WorkerHandle { requests })
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| PluginError::StBridgeWorkerStopped)?;
    let (response, receiver) = mpsc::sync_channel(1);
    worker
        .requests
        .send(Request {
            installed: installed.clone(),
            input: input.clone(),
            source: source.to_owned(),
            limits,
            response,
        })
        .map_err(|_| PluginError::StBridgeWorkerStopped)?;
    receiver
        .recv()
        .map_err(|_| PluginError::StBridgeWorkerStopped)?
}

impl Worker {
    fn run(&mut self, receiver: mpsc::Receiver<Request>) {
        while let Ok(request) = receiver.recv() {
            let timeout_seed = ContextKey::from_request(&request.installed, &request.input)
                .ok()
                .map(|key| key.prng_seed());
            let result = self.execute(
                request.installed,
                request.input,
                &request.source,
                request.limits,
            );
            let result = match result {
                Err(PluginError::StBridgeAsyncTimeout) => Ok(ScriptOutcome {
                    effects: Vec::new(),
                    logs: vec![crate::ScriptLog {
                        level: "warn".to_owned(),
                        message: "st-bridge async callback exceeded 64 microtasks; dispatch effects were discarded".to_owned(),
                    }],
                    prng_seed: timeout_seed,
                }),
                other => other,
            };
            let _ = request.response.send(result);
        }
    }

    fn execute(
        &mut self,
        installed: InstalledPlugin,
        input: PluginInput,
        source: &str,
        limits: ScriptLimits,
    ) -> Result<ScriptOutcome, PluginError> {
        let key = ContextKey::from_request(&installed, &input)?;
        if !self.contexts.contains_key(&key) {
            let context =
                BridgeContext::new(&key, &installed.manifest.id, source, &input.context, limits)?;
            self.contexts.insert(key.clone(), context);
        }

        let context = self
            .contexts
            .get_mut(&key)
            .ok_or(PluginError::StBridgeWorkerStopped)?;
        if context.abandoned.get() {
            return Ok(ScriptOutcome {
                effects: Vec::new(),
                logs: context.take_logs(),
                prng_seed: Some(context.prng_seed),
            });
        }

        // Emit APP_READY once after initialization
        if !context.app_ready_emitted.get() {
            context.dispatch(
                &installed.manifest.id,
                "app_ready",
                &input.context,
                &serde_json::json!({}),
                limits,
            )?;
            context.app_ready_emitted.set(true);
        }

        match input.event {
            PluginEvent::GenerateInterceptor => {
                if let Some(branch_id) = input
                    .session
                    .get("branch_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<EntityId>().ok())
                    && context.last_branch_id.borrow().as_ref() != Some(&branch_id)
                {
                    context.dispatch(
                        &installed.manifest.id,
                        "chat_id_changed",
                        &input.context,
                        &serde_json::json!([branch_id.to_string()]),
                        limits,
                    )?;
                    *context.last_branch_id.borrow_mut() = Some(branch_id);
                }
                context.dispatch(
                    &installed.manifest.id,
                    "generation_started",
                    &input.context,
                    &serde_json::json!([
                        input
                            .session
                            .get("generation_type")
                            .cloned()
                            .unwrap_or(JsonValue::Null),
                        input
                            .session
                            .get("options")
                            .cloned()
                            .unwrap_or(JsonValue::Null),
                        input
                            .session
                            .get("dry_run")
                            .cloned()
                            .unwrap_or(JsonValue::Bool(false)),
                    ]),
                    limits,
                )?;
                if input
                    .payload
                    .get("has_user_message")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    context.dispatch(
                        &installed.manifest.id,
                        "message_sent",
                        &input.context,
                        &serde_json::json!([input
                            .payload
                            .get("message_index")
                            .cloned()
                            .unwrap_or(JsonValue::Null)]),
                        limits,
                    )?;
                }

                // Call named interceptor
                let interceptor_name = installed
                    .manifest
                    .generate_interceptor
                    .as_ref()
                    .ok_or_else(|| PluginError::StBridgeInterceptorMissing {
                        plugin: installed.manifest.id.clone(),
                        name: "<none>".to_owned(),
                    })?;

                let result = context.context.with(|ctx| {
                    *context.snapshot.borrow_mut() = input.context.clone();
                    context.ticks.set(limits.interrupt_ticks);

                    let globals = ctx.globals();
                    let interceptor: Function =
                        globals.get(interceptor_name.as_str()).map_err(|_| {
                            PluginError::StBridgeInterceptorMissing {
                                plugin: installed.manifest.id.clone(),
                                name: interceptor_name.clone(),
                            }
                        })?;

                    let chat_array = input.payload.get("chat").ok_or_else(|| {
                        PluginError::StBridgePayload("payload missing 'chat' array".to_owned())
                    })?;
                    let chat_json = serde_json::to_string(chat_array)?;
                    let chat_arg = ctx
                        .json_parse(chat_json)
                        .map_err(|e| PluginError::StBridgePayload(e.to_string()))?;
                    let undefined = Value::new_undefined(ctx.clone());
                    let result_value = interceptor
                        .call::<_, Value>((
                            chat_arg.clone(),
                            undefined.clone(),
                            undefined.clone(),
                            undefined,
                        ))
                        .catch(&ctx)
                        .map_err(|error| {
                            map_caught(&ctx, &installed.manifest.id, &context.ticks, error, false)
                        })?;

                    let promise = result_value.as_promise();
                    drain_pending_jobs(&ctx, limits.microtask_jobs, &context.abandoned)?;
                    if let Some(promise) = promise {
                        match promise.state() {
                            PromiseState::Resolved => {
                                let result = promise.result::<Value>().ok_or_else(|| {
                                    PluginError::StBridgePayload(
                                        "interceptor promise has no result".to_owned(),
                                    )
                                })?;
                                result.map_err(|error| PluginError::StBridgeHandler {
                                    plugin: installed.manifest.id.clone(),
                                    message: error.to_string(),
                                })?;
                            }
                            PromiseState::Rejected => {
                                return Err(PluginError::StBridgeHandler {
                                    plugin: installed.manifest.id.clone(),
                                    message: "interceptor promise rejected".to_owned(),
                                });
                            }
                            PromiseState::Pending => {
                                context.abandoned.set(true);
                                return Err(PluginError::StBridgeAsyncTimeout);
                            }
                        }
                    }

                    let json_str = ctx
                        .json_stringify(chat_arg)
                        .map_err(|e| PluginError::StBridgePayload(e.to_string()))?
                        .ok_or_else(|| {
                            PluginError::StBridgePayload("interceptor chat is not JSON".to_owned())
                        })?
                        .to_string()
                        .map_err(|e| PluginError::StBridgePayload(e.to_string()))?;
                    Ok::<_, PluginError>(json_str)
                })?;
                let st_messages: Vec<StMessage> = serde_json::from_str(&result)?;
                let messages: Vec<PromptRewriteMessage> = st_messages
                    .into_iter()
                    .map(|msg| {
                        let role = if msg.is_system {
                            ChatRole::System
                        } else if msg.is_user {
                            ChatRole::User
                        } else {
                            ChatRole::Assistant
                        };
                        PromptRewriteMessage {
                            role,
                            content: msg.mes,
                        }
                    })
                    .collect();

                Ok(ScriptOutcome {
                    effects: vec![PluginEffect::PromptRewrite { messages }],
                    logs: context.take_logs(),
                    prng_seed: Some(context.prng_seed),
                })
            }
            PluginEvent::ChatCompletionPromptReady => {
                let updated = context.dispatch(
                    &installed.manifest.id,
                    "chat_completion_prompt_ready",
                    &input.context,
                    &input.payload,
                    limits,
                )?;
                let original = decode_chat(&input.payload)?;
                let modified = decode_chat(&updated)?;

                if original == modified {
                    Ok(ScriptOutcome {
                        effects: Vec::new(),
                        logs: context.take_logs(),
                        prng_seed: Some(context.prng_seed),
                    })
                } else {
                    Ok(ScriptOutcome {
                        effects: vec![PluginEffect::PromptRewrite {
                            messages: modified
                                .into_iter()
                                .map(|msg| PromptRewriteMessage {
                                    role: msg.role,
                                    content: msg.content,
                                })
                                .collect(),
                        }],
                        logs: context.take_logs(),
                        prng_seed: Some(context.prng_seed),
                    })
                }
            }
            PluginEvent::StBridgeLifecycle => {
                // Dispatch batch of lifecycle events
                if let Some(events) = input.payload.get("events").and_then(|v| v.as_array()) {
                    for event_obj in events {
                        if let Some(event_name) = event_obj.get("name").and_then(|v| v.as_str()) {
                            let empty_args = serde_json::json!([]);
                            let args = event_obj.get("args").unwrap_or(&empty_args);
                            context.dispatch(
                                &installed.manifest.id,
                                event_name,
                                &input.context,
                                args,
                                limits,
                            )?;
                        }
                    }
                }
                Ok(ScriptOutcome {
                    effects: vec![PluginEffect::Observe {
                        value: serde_json::json!({}),
                    }],
                    logs: context.take_logs(),
                    prng_seed: Some(context.prng_seed),
                })
            }
            _ => Err(PluginError::UnsupportedStBridgeEvent),
        }
    }
}

impl BridgeContext {
    fn new(
        context_key: &ContextKey,
        plugin_id: &str,
        source: &str,
        initial_snapshot: &JsonValue,
        limits: ScriptLimits,
    ) -> Result<Self, PluginError> {
        let runtime =
            Runtime::new().map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
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
            intrinsic::Promise,
        )>(&runtime)
        .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
        let listeners = Rc::new(RefCell::new(HashMap::new()));
        let snapshot = Rc::new(RefCell::new(initial_snapshot.clone()));
        let last_branch_id = Rc::new(RefCell::new(None));
        let app_ready_emitted = Rc::new(Cell::new(false));
        let abandoned = Rc::new(Cell::new(false));
        let prng_seed = context_key.prng_seed();
        let prng = Rc::new(RefCell::new(Xoshiro128PlusPlus::from_seed(prng_seed)));
        let next_timer_id = Rc::new(Cell::new(1));
        let logs = Rc::new(RefCell::new(Vec::new()));
        let warned_delayed_timer = Rc::new(Cell::new(false));
        context.with(|ctx| {
            install_globals(
                &ctx,
                Rc::clone(&listeners),
                Rc::clone(&snapshot),
                prng,
                next_timer_id,
                Rc::clone(&logs),
                warned_delayed_timer,
            )
            .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
            let globals = ctx.globals();
            globals
                .remove("eval")
                .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
            let promise = Module::evaluate(ctx.clone(), plugin_id, source).map_err(|error| {
                PluginError::StBridgeInitialization {
                    plugin: plugin_id.to_owned(),
                    message: error.to_string(),
                }
            })?;
            if promise.state() == PromiseState::Pending {
                return Err(PluginError::StBridgeInitialization {
                    plugin: plugin_id.to_owned(),
                    message: "asynchronous module initialization is unsupported".to_owned(),
                });
            }
            promise
                .finish::<()>()
                .catch(&ctx)
                .map_err(|error| map_caught(&ctx, plugin_id, &ticks, error, true))
        })?;
        Ok(Self {
            context,
            listeners,
            snapshot,
            ticks,
            last_branch_id,
            app_ready_emitted,
            abandoned,
            prng_seed,
            logs,
        })
    }

    fn take_logs(&self) -> Vec<crate::ScriptLog> {
        std::mem::take(&mut *self.logs.borrow_mut())
    }

    fn dispatch(
        &mut self,
        plugin_id: &str,
        event_name: &str,
        snapshot: &JsonValue,
        payload: &JsonValue,
        limits: ScriptLimits,
    ) -> Result<JsonValue, PluginError> {
        *self.snapshot.borrow_mut() = snapshot.clone();
        self.ticks.set(limits.interrupt_ticks);
        let event_listeners = self
            .listeners
            .borrow()
            .get(event_name)
            .cloned()
            .unwrap_or_default();
        self.context.with(|ctx| {
            let argument = ctx
                .json_parse(
                    serde_json::to_string(payload)
                        .map_err(|e| PluginError::StBridgePayload(e.to_string()))?,
                )
                .map_err(|e| PluginError::StBridgePayload(e.to_string()))?;
            for saved in event_listeners {
                let listener = saved
                    .restore(&ctx)
                    .map_err(|e| PluginError::StBridgeHandler {
                        plugin: plugin_id.to_owned(),
                        message: e.to_string(),
                    })?;
                let result = if event_name == "chat_completion_prompt_ready" {
                    listener
                        .call::<_, Value>((argument.clone(),))
                        .catch(&ctx)
                        .map_err(|error| map_caught(&ctx, plugin_id, &self.ticks, error, false))?
                } else {
                    let values = payload.as_array().cloned().unwrap_or_default();
                    let mut args = Args::new(ctx.clone(), values.len());
                    for value in values {
                        let parsed = ctx
                            .json_parse(
                                serde_json::to_string(&value)
                                    .map_err(|e| PluginError::StBridgePayload(e.to_string()))?,
                            )
                            .map_err(|e| PluginError::StBridgePayload(e.to_string()))?;
                        args.push_arg(parsed)
                            .map_err(|e| PluginError::StBridgePayload(e.to_string()))?;
                    }
                    listener
                        .call_arg::<Value>(args)
                        .catch(&ctx)
                        .map_err(|error| map_caught(&ctx, plugin_id, &self.ticks, error, false))?
                };
                let promise = result.as_promise();
                drain_pending_jobs(&ctx, limits.microtask_jobs, &self.abandoned)?;
                if let Some(promise) = promise {
                    match promise.state() {
                        PromiseState::Resolved => {}
                        PromiseState::Rejected => {
                            return Err(PluginError::StBridgeHandler {
                                plugin: plugin_id.to_owned(),
                                message: "listener promise rejected".to_owned(),
                            });
                        }
                        PromiseState::Pending => {
                            self.abandoned.set(true);
                            return Err(PluginError::StBridgeAsyncTimeout);
                        }
                    }
                }
            }
            let text = ctx
                .json_stringify(argument)
                .map_err(|e| PluginError::StBridgePayload(e.to_string()))?
                .ok_or_else(|| PluginError::StBridgePayload("payload is not JSON".to_owned()))?
                .to_string()
                .map_err(|e| PluginError::StBridgePayload(e.to_string()))?;
            serde_json::from_str(&text).map_err(|e| PluginError::StBridgePayload(e.to_string()))
        })
    }
}

fn drain_pending_jobs(
    ctx: &Ctx<'_>,
    max_jobs: usize,
    abandoned: &Cell<bool>,
) -> Result<(), PluginError> {
    let mut jobs = 0;
    while ctx.execute_pending_job() {
        jobs += 1;
        if jobs >= max_jobs {
            abandoned.set(true);
            return Err(PluginError::StBridgeAsyncTimeout);
        }
    }
    Ok(())
}

fn install_timer<'js>(
    ctx: &Ctx<'js>,
    globals: &Object<'js>,
    name: &'static str,
    next_timer_id: Rc<Cell<u32>>,
    logs: Rc<RefCell<Vec<crate::ScriptLog>>>,
    warned_delayed_timer: Rc<Cell<bool>>,
) -> rquickjs::Result<()> {
    let schedule: Function = ctx.eval("(callback) => Promise.resolve().then(callback)")?;
    globals.set(
        name,
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, callback: Function<'js>, delay: Option<i32>| {
                if delay.unwrap_or(0) > 0 {
                    if !warned_delayed_timer.replace(true) {
                        logs.borrow_mut().push(crate::ScriptLog {
                            level: "warn".to_owned(),
                            message: format!(
                                "`{name}` with delay is unsupported; use `Promise.resolve()` for deferred work"
                            ),
                        });
                    }
                    return Err(Exception::throw_type(
                        &ctx,
                        &format!("{name} with delay is unsupported"),
                    ));
                }
                let _: Value = schedule.call((callback,))?;
                let id = next_timer_id.get();
                next_timer_id.set(id.wrapping_add(1));
                Ok(id)
            },
        )?,
    )
}

fn install_globals<'js>(
    ctx: &Ctx<'js>,
    listeners: Rc<RefCell<HashMap<String, Vec<Listener>>>>,
    snapshot: Rc<RefCell<JsonValue>>,
    prng: Rc<RefCell<Xoshiro128PlusPlus>>,
    next_timer_id: Rc<Cell<u32>>,
    logs: Rc<RefCell<Vec<crate::ScriptLog>>>,
    warned_delayed_timer: Rc<Cell<bool>>,
) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    let event_types = Object::new(ctx.clone())?;
    event_types.set("APP_READY", "app_ready")?;
    event_types.set("CHAT_CHANGED", "chat_id_changed")?;
    event_types.set("GENERATION_STARTED", "generation_started")?;
    event_types.set("MESSAGE_SENT", "message_sent")?;
    event_types.set("MESSAGE_RECEIVED", "message_received")?;
    event_types.set("GENERATION_ENDED", "generation_ended")?;
    event_types.set(
        "CHAT_COMPLETION_PROMPT_READY",
        "chat_completion_prompt_ready",
    )?;
    event_types.set("USER_MESSAGE_RENDERED", "user_message_rendered")?;
    event_types.set("CHARACTER_MESSAGE_RENDERED", "character_message_rendered")?;
    event_types.set("TOOL_CALLS_RENDERED", "tool_calls_rendered")?;
    freeze(ctx, event_types.clone())?;

    let event_source = Object::new(ctx.clone())?;
    event_source.set(
        "on",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, event: String, listener: Function<'js>| {
                let valid_lifecycle = [
                    "app_ready",
                    "chat_id_changed",
                    "generation_started",
                    "message_sent",
                    "message_received",
                    "generation_ended",
                    "chat_completion_prompt_ready",
                ];
                let render_events = [
                    "user_message_rendered",
                    "character_message_rendered",
                    "tool_calls_rendered",
                ];

                if valid_lifecycle.contains(&event.as_str()) {
                    listeners
                        .borrow_mut()
                        .entry(event)
                        .or_default()
                        .push(Persistent::save(&ctx, listener));
                    Ok(())
                } else if render_events.contains(&event.as_str()) {
                    Ok(())
                } else {
                    Err(Exception::throw_type(&ctx, "unsupported event type"))
                }
            },
        )?,
    )?;
    freeze(ctx, event_source.clone())?;

    let silly_tavern = Object::new(ctx.clone())?;
    silly_tavern.set(
        "getContext",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            let json = serde_json::to_string(&*snapshot.borrow())
                .map_err(|_| Exception::throw_type(&ctx, "context snapshot is not JSON"))?;
            let value = ctx.json_parse(json)?;
            deep_freeze(&ctx, value)
        })?,
    )?;
    freeze(ctx, silly_tavern.clone())?;

    globals.set("event_types", event_types)?;
    globals.set("eventSource", event_source)?;
    globals.set("SillyTavern", silly_tavern)?;

    let math: Object = globals.get("Math")?;
    math.set(
        "random",
        Function::new(ctx.clone(), move || prng.borrow_mut().next_f64())?,
    )?;

    install_timer(
        ctx,
        &globals,
        "setTimeout",
        Rc::clone(&next_timer_id),
        Rc::clone(&logs),
        Rc::clone(&warned_delayed_timer),
    )?;
    install_timer(
        ctx,
        &globals,
        "setInterval",
        next_timer_id,
        logs,
        warned_delayed_timer,
    )?;

    globals.set("clearTimeout", Function::new(ctx.clone(), || ())?)?;
    globals.set("clearInterval", Function::new(ctx.clone(), || ())?)?;
    Ok(())
}

fn freeze<'js>(ctx: &Ctx<'js>, value: Object<'js>) -> rquickjs::Result<()> {
    let object: Object = ctx.globals().get("Object")?;
    let freeze: Function = object.get("freeze")?;
    freeze.call::<_, Value>((value,))?;
    Ok(())
}

fn deep_freeze<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Value<'js>> {
    let freeze: Function = ctx.eval(
        "(function freeze(value) { if (value !== null && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value); } return value; })",
    )?;
    freeze.call((value,))
}

#[derive(Deserialize)]
struct StMessage {
    #[serde(default)]
    is_user: bool,
    #[serde(default)]
    is_system: bool,
    mes: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeChatMessage {
    role: ChatRole,
    content: String,
}

fn decode_chat(payload: &JsonValue) -> Result<Vec<ChatMessage>, PluginError> {
    let messages = serde_json::from_value::<Vec<BridgeChatMessage>>(
        payload
            .get("chat")
            .cloned()
            .ok_or_else(|| PluginError::StBridgePayload("missing chat array".to_owned()))?,
    )
    .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
    Ok(messages
        .into_iter()
        .map(|message| ChatMessage {
            role: message.role,
            content: message.content,
        })
        .collect())
}

fn map_caught<'js>(
    ctx: &Ctx<'js>,
    plugin_id: &str,
    ticks: &Cell<u64>,
    error: CaughtError<'js>,
    initialization: bool,
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
    if initialization {
        PluginError::StBridgeInitialization {
            plugin: plugin_id.to_owned(),
            message,
        }
    } else {
        PluginError::StBridgeHandler {
            plugin: plugin_id.to_owned(),
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Xoshiro128PlusPlus;

    #[test]
    fn seeded_prng_replays_to_same_values() {
        let recorded_seed = 0x0123_4567_89ab_cdef;
        let mut original = Xoshiro128PlusPlus::from_seed(recorded_seed);
        let expected = (0..5).map(|_| original.next_f64()).collect::<Vec<_>>();
        let mut replay = Xoshiro128PlusPlus::from_seed(recorded_seed);
        let actual = (0..5).map(|_| replay.next_f64()).collect::<Vec<_>>();

        assert_eq!(actual, expected);
    }
}
