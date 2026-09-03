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
    collections::{BTreeMap, HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    rc::Rc,
    sync::{OnceLock, mpsc},
};

use rquickjs::{
    CatchResultExt, CaughtError, Coerced, Context, Ctx, Exception, FromJs, Function, Module,
    Object, Persistent, Runtime, Value,
    context::intrinsic,
    function::{Args, Opt, Rest},
    promise::PromiseState,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::{
    ChatMessage, ChatRole, ContentHash, EgressInvocation, EgressReceipt, EgressRequest, EntityId,
    InstalledPlugin, PluginEffect, PluginError, PluginEvent, PluginInput, PromptRewriteMessage,
    ScriptLimits, ScriptOutcome, StateKey, VariableScope,
};

type Listener = Persistent<Function<'static>>;
type StorageHydrate = Persistent<Function<'static>>;

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
    commands: Rc<RefCell<HashSet<String>>>,
    snapshot: Rc<RefCell<JsonValue>>,
    storage_hydrate: StorageHydrate,
    ticks: Rc<Cell<u64>>,
    last_branch_id: Rc<RefCell<Option<EntityId>>>,
    app_ready_emitted: Rc<Cell<bool>>,
    abandoned: Rc<Cell<bool>>,
    initialization_error: Option<String>,
    prng_seed: u64,
    logs: Rc<RefCell<Vec<crate::ScriptLog>>>,
    effects: Rc<RefCell<BridgeEffectState>>,
}

struct BridgeEffectState {
    caller: String,
    egress: Option<EgressInvocation>,
    inference: Option<crate::InferenceInvocation>,
    egress_receipts: Vec<EgressReceipt>,
    inference_receipts: Vec<crate::InferenceReceipt>,
    state_writes: BTreeMap<StateKey, JsonValue>,
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
    egress: Option<EgressInvocation>,
    inference: Option<crate::InferenceInvocation>,
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
    egress: Option<EgressInvocation>,
    inference: Option<crate::InferenceInvocation>,
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
            egress,
            inference,
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
                request.egress,
                request.inference,
            );
            let result = match result {
                Err(PluginError::StBridgeAsyncTimeout) => Ok(ScriptOutcome {
                    effects: Vec::new(),
                    logs: vec![crate::ScriptLog {
                        level: "warn".to_owned(),
                        message: "st-bridge async callback exceeded 64 microtasks; dispatch effects were discarded".to_owned(),
                    }],
                    egress_receipts: Vec::new(),
                    inference_receipts: Vec::new(),
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
        egress: Option<EgressInvocation>,
        inference: Option<crate::InferenceInvocation>,
    ) -> Result<ScriptOutcome, PluginError> {
        let key = ContextKey::from_request(&installed, &input)?;
        if !self.contexts.contains_key(&key) {
            let context = BridgeContext::new(
                &key,
                &installed.manifest.id,
                source,
                &input.context,
                &input.state,
                limits,
            )?;
            self.contexts.insert(key.clone(), context);
        }

        let context = self
            .contexts
            .get_mut(&key)
            .ok_or(PluginError::StBridgeWorkerStopped)?;
        if let Some(message) = &context.initialization_error {
            return Err(PluginError::StBridgeInitialization {
                plugin: installed.manifest.id.clone(),
                message: message.clone(),
            });
        }
        *context.effects.borrow_mut() = BridgeEffectState {
            caller: installed.manifest.id.clone(),
            egress,
            inference,
            egress_receipts: Vec::new(),
            inference_receipts: Vec::new(),
            state_writes: BTreeMap::new(),
        };
        context.hydrate_storage(&input.state)?;
        if context.abandoned.get() {
            return Ok(ScriptOutcome {
                effects: Vec::new(),
                logs: context.take_logs(),
                egress_receipts: Vec::new(),
                inference_receipts: Vec::new(),
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

        let mut outcome = match input.event {
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
                    egress_receipts: Vec::new(),
                    inference_receipts: Vec::new(),
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
                        egress_receipts: Vec::new(),
                        inference_receipts: Vec::new(),
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
                        egress_receipts: Vec::new(),
                        inference_receipts: Vec::new(),
                        prng_seed: Some(context.prng_seed),
                    })
                }
            }
            PluginEvent::StBridgeLifecycle => {
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
                    egress_receipts: Vec::new(),
                    inference_receipts: Vec::new(),
                    prng_seed: Some(context.prng_seed),
                })
            }
            PluginEvent::Command => {
                let command = input
                    .payload
                    .get("command")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| {
                        PluginError::StBridgePayload(
                            "command payload missing 'command' string".to_owned(),
                        )
                    })?;
                let named = input
                    .payload
                    .get("named")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let unnamed = input
                    .payload
                    .get("unnamed")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                let output = context.invoke_command(
                    &installed.manifest.id,
                    command,
                    &input.context,
                    &named,
                    unnamed,
                    limits,
                )?;
                Ok(ScriptOutcome {
                    effects: vec![PluginEffect::Observe {
                        value: serde_json::json!({"output": output}),
                    }],
                    logs: context.take_logs(),
                    egress_receipts: Vec::new(),
                    inference_receipts: Vec::new(),
                    prng_seed: Some(context.prng_seed),
                })
            }
            _ => Err(PluginError::UnsupportedStBridgeEvent),
        }?;
        let mut effects = context.effects.borrow_mut();
        outcome.egress_receipts = std::mem::take(&mut effects.egress_receipts);
        outcome.inference_receipts = std::mem::take(&mut effects.inference_receipts);
        outcome.effects.extend(
            std::mem::take(&mut effects.state_writes)
                .into_iter()
                .map(|(key, value)| PluginEffect::StateWrite { key, value }),
        );
        Ok(outcome)
    }
}

impl BridgeContext {
    fn new(
        context_key: &ContextKey,
        plugin_id: &str,
        source: &str,
        initial_snapshot: &JsonValue,
        initial_state: &JsonValue,
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
            intrinsic::Proxy,
        )>(&runtime)
        .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
        let listeners = Rc::new(RefCell::new(HashMap::new()));
        let commands = Rc::new(RefCell::new(HashSet::new()));
        let snapshot = Rc::new(RefCell::new(initial_snapshot.clone()));
        let last_branch_id = Rc::new(RefCell::new(None));
        let app_ready_emitted = Rc::new(Cell::new(false));
        let abandoned = Rc::new(Cell::new(false));
        let prng_seed = context_key.prng_seed();
        let prng = Rc::new(RefCell::new(Xoshiro128PlusPlus::from_seed(prng_seed)));
        let next_timer_id = Rc::new(Cell::new(1));
        let logs = Rc::new(RefCell::new(Vec::new()));
        let warned_delayed_timer = Rc::new(Cell::new(false));
        let effects = Rc::new(RefCell::new(BridgeEffectState {
            caller: plugin_id.to_owned(),
            egress: None,
            inference: None,
            egress_receipts: Vec::new(),
            inference_receipts: Vec::new(),
            state_writes: BTreeMap::new(),
        }));
        let storage_hydrate = context.with(|ctx| {
            let storage_hydrate = install_globals(
                &ctx,
                plugin_id,
                Rc::clone(&listeners),
                Rc::clone(&commands),
                Rc::clone(&snapshot),
                prng,
                next_timer_id,
                Rc::clone(&logs),
                warned_delayed_timer,
                Rc::clone(&effects),
            )
            .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
            hydrate_storage(&ctx, &storage_hydrate, initial_state)?;
            Ok::<_, PluginError>(storage_hydrate)
        })?;
        let initialization = context.with(|ctx| {
            let globals = ctx.globals();
            globals
                .remove("eval")
                .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
            let promise = Module::evaluate(ctx.clone(), plugin_id, source)
                .catch(&ctx)
                .map_err(|error| map_caught(&ctx, plugin_id, &ticks, error, true))?;
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
        });
        let initialization_error = initialization.err().map(|error| match error {
            PluginError::StBridgeInitialization { message, .. } => message,
            error => error.to_string(),
        });
        Ok(Self {
            context,
            listeners,
            commands,
            snapshot,
            storage_hydrate,
            ticks,
            last_branch_id,
            initialization_error,
            app_ready_emitted,
            abandoned,
            prng_seed,
            logs,
            effects,
        })
    }

    fn take_logs(&self) -> Vec<crate::ScriptLog> {
        std::mem::take(&mut *self.logs.borrow_mut())
    }

    fn hydrate_storage(&self, state: &JsonValue) -> Result<(), PluginError> {
        self.context
            .with(|ctx| hydrate_storage(&ctx, &self.storage_hydrate, state))
    }

    fn invoke_command(
        &mut self,
        plugin_id: &str,
        command: &str,
        snapshot: &JsonValue,
        named: &JsonValue,
        unnamed: &str,
        limits: ScriptLimits,
    ) -> Result<String, PluginError> {
        *self.snapshot.borrow_mut() = snapshot.clone();
        self.ticks.set(limits.interrupt_ticks);
        if !self.commands.borrow().contains(command) {
            return Err(PluginError::StBridgeCommandNotRegistered(
                command.to_owned(),
            ));
        }
        self.context.with(|ctx| {
            let callbacks: Object = ctx
                .globals()
                .get("__stcliSlashCommands")
                .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
            let callback: Function =
                callbacks
                    .get(command)
                    .map_err(|error| PluginError::StBridgeHandler {
                        plugin: plugin_id.to_owned(),
                        message: error.to_string(),
                    })?;
            let named = ctx
                .json_parse(
                    serde_json::to_string(named)
                        .map_err(|error| PluginError::StBridgePayload(error.to_string()))?,
                )
                .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
            let mut result = callback
                .call::<_, Value>((named, unnamed))
                .catch(&ctx)
                .map_err(|error| map_caught(&ctx, plugin_id, &self.ticks, error, false))?;
            let promise = result.as_promise();
            drain_pending_jobs(&ctx, limits.microtask_jobs, &self.abandoned)?;
            if let Some(promise) = promise {
                result = match promise.state() {
                    PromiseState::Resolved => promise
                        .result::<Value>()
                        .ok_or_else(|| {
                            PluginError::StBridgePayload(
                                "slash command promise has no result".to_owned(),
                            )
                        })?
                        .map_err(|error| PluginError::StBridgeHandler {
                            plugin: plugin_id.to_owned(),
                            message: error.to_string(),
                        })?,
                    PromiseState::Rejected => {
                        return Err(PluginError::StBridgeHandler {
                            plugin: plugin_id.to_owned(),
                            message: "slash command promise rejected".to_owned(),
                        });
                    }
                    PromiseState::Pending => {
                        self.abandoned.set(true);
                        return Err(PluginError::StBridgeAsyncTimeout);
                    }
                };
            }
            if result.is_undefined() {
                Ok(String::new())
            } else {
                Coerced::<String>::from_js(&ctx, result)
                    .map(|value| value.0)
                    .map_err(|error| PluginError::StBridgePayload(error.to_string()))
            }
        })
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

impl Drop for BridgeContext {
    fn drop(&mut self) {
        self.listeners.borrow_mut().clear();
        self.commands.borrow_mut().clear();
        self.context.with(clear_bridge_globals);
    }
}

fn clear_bridge_globals(ctx: Ctx<'_>) {
    let _ = ctx.catch();
    let _ = ctx.eval::<(), _>(
        "for (const key of Object.getOwnPropertyNames(globalThis)) { if (key !== 'globalThis') delete globalThis[key]; }",
    );
    ctx.run_gc();
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

fn valid_storage_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn hydrate_storage(
    ctx: &Ctx<'_>,
    hydrate: &StorageHydrate,
    state: &JsonValue,
) -> Result<(), PluginError> {
    let settings = state
        .get("settings")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let storage = state
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| {
            let key = name.strip_prefix("ls.")?;
            if !valid_storage_key(key) {
                return None;
            }
            value.as_str().map(|value| (key, value))
        })
        .collect::<BTreeMap<_, _>>();
    let settings = serde_json::to_string(&settings)
        .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
    let storage = serde_json::to_string(&storage)
        .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
    let hydrate = hydrate
        .clone()
        .restore(ctx)
        .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
    hydrate
        .call::<_, ()>((settings, storage))
        .map_err(|error| PluginError::StBridgePayload(error.to_string()))
}

fn install_storage<'js>(
    ctx: &Ctx<'js>,
    globals: &Object<'js>,
    plugin_id: &str,
) -> rquickjs::Result<StorageHydrate> {
    globals.set("__stcliExtensionId", plugin_id)?;
    let hydrate: Function = ctx.eval(
        r#"
(function() {
    const extensionId = globalThis.__stcliExtensionId;
    const writeState = globalThis.__stcliWriteExtensionState;
    delete globalThis.__stcliExtensionId;
    delete globalThis.__stcliWriteExtensionState;

    let settings = {};
    const values = new Map();
    const storageKey = value => {
        const key = String(value);
        if (!/^[A-Za-z0-9_-]+$/.test(key)) {
            throw new TypeError(`invalid localStorage key '${key}'`);
        }
        return key;
    };
    const saveSettingsDebounced = () => {
        writeState('settings', settings);
    };
    const extensionSettings = new Proxy({}, {
        get(_target, property) {
            return property === extensionId ? settings : undefined;
        },
        set(_target, property, value) {
            if (property !== extensionId) {
                throw new TypeError('extension_settings access is limited to the current Extension');
            }
            settings = value;
            saveSettingsDebounced();
            return true;
        },
        ownKeys() {
            return [extensionId];
        },
        getOwnPropertyDescriptor(_target, property) {
            return property === extensionId
                ? { configurable: true, enumerable: true }
                : undefined;
        }
    });
    const localStorage = {
        getItem(key) {
            key = storageKey(key);
            return values.has(key) ? values.get(key) : null;
        },
        setItem(key, value) {
            key = storageKey(key);
            value = String(value);
            values.set(key, value);
            writeState(`ls.${key}`, value);
        },
        removeItem(key) {
            key = storageKey(key);
            values.delete(key);
            writeState(`ls.${key}`, null);
        },
        clear() {
            for (const key of values.keys()) {
                writeState(`ls.${key}`, null);
            }
            values.clear();
        },
        key(index) {
            index = Number(index);
            if (!Number.isInteger(index) || index < 0) return null;
            return Array.from(values.keys())[index] ?? null;
        }
    };
    Object.defineProperty(localStorage, 'length', {
        enumerable: true,
        get() {
            return values.size;
        }
    });
    globalThis.extension_settings = extensionSettings;
    globalThis.localStorage = localStorage;
    globalThis.saveSettingsDebounced = saveSettingsDebounced;

    return function hydrate(settingsJson, storageJson) {
        const hydratedSettings = JSON.parse(settingsJson);
        settings = hydratedSettings === null ? {} : hydratedSettings;
        values.clear();
        for (const [key, value] of Object.entries(JSON.parse(storageJson))) {
            values.set(key, value);
        }
    };
})()
"#,
    )?;
    Ok(Persistent::save(ctx, hydrate))
}

#[allow(clippy::too_many_arguments)]
fn install_globals<'js>(
    ctx: &Ctx<'js>,
    plugin_id: &str,
    listeners: Rc<RefCell<HashMap<String, Vec<Listener>>>>,
    commands: Rc<RefCell<HashSet<String>>>,
    snapshot: Rc<RefCell<JsonValue>>,
    prng: Rc<RefCell<Xoshiro128PlusPlus>>,
    next_timer_id: Rc<Cell<u32>>,
    logs: Rc<RefCell<Vec<crate::ScriptLog>>>,
    warned_delayed_timer: Rc<Cell<bool>>,
    effects: Rc<RefCell<BridgeEffectState>>,
) -> rquickjs::Result<StorageHydrate> {
    let globals = ctx.globals();
    let state_effects = Rc::clone(&effects);
    let state_prefix = format!("extension.{plugin_id}.");
    globals.set(
        "__stcliWriteExtensionState",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, relative: String, value: Value<'js>| {
                let valid = relative == "settings"
                    || relative.strip_prefix("ls.").is_some_and(valid_storage_key);
                if !valid {
                    return Err(Exception::throw_type(&ctx, "invalid extension state key"));
                }
                let text = ctx.json_stringify(value)?.ok_or_else(|| {
                    Exception::throw_type(&ctx, "extension state value must be JSON-serializable")
                })?;
                let value =
                    serde_json::from_str::<JsonValue>(&text.to_string()?).map_err(|_| {
                        Exception::throw_type(
                            &ctx,
                            "extension state value must be JSON-serializable",
                        )
                    })?;
                state_effects.borrow_mut().state_writes.insert(
                    StateKey {
                        scope: VariableScope::Local,
                        name: format!("{state_prefix}{relative}"),
                    },
                    value,
                );
                Ok(())
            },
        )?,
    )?;
    let storage_hydrate = install_storage(ctx, &globals, plugin_id)?;
    globals.set("__stcliSlashCommands", Object::new(ctx.clone())?)?;
    let console = Object::new(ctx.clone())?;
    for level in ["log", "warn", "error"] {
        let logs = Rc::clone(&logs);
        console.set(
            level,
            Function::new(
                ctx.clone(),
                move |call_ctx: Ctx<'js>, args: Rest<Value<'js>>| {
                    let message = args
                        .0
                        .into_iter()
                        .filter_map(|value| Coerced::<String>::from_js(&call_ctx, value).ok())
                        .map(|value| value.0)
                        .collect::<Vec<_>>()
                        .join(" ");
                    logs.borrow_mut().push(crate::ScriptLog {
                        level: level.to_owned(),
                        message,
                    });
                    Ok::<(), rquickjs::Error>(())
                },
            )?,
        )?;
    }
    globals.set("console", console)?;
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
    let on_listeners = Rc::clone(&listeners);
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
                    on_listeners
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
    let off_listeners = Rc::clone(&listeners);
    event_source.set(
        "off",
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
                if valid_lifecycle.contains(&event.as_str()) {
                    let mut map = off_listeners.borrow_mut();
                    if let Some(handlers) = map.get_mut(event.as_str()) {
                        let target = Persistent::save(&ctx, listener);
                        handlers.retain(|saved| {
                            let saved_fn = saved.clone().restore(&ctx);
                            let target_fn = target.clone().restore(&ctx);
                            match (saved_fn, target_fn) {
                                (Ok(a), Ok(b)) => a != b,
                                _ => true,
                            }
                        });
                    }
                }
                Ok::<(), rquickjs::Error>(())
            },
        )?,
    )?;
    event_source.set(
        "emit",
        Function::new(ctx.clone(), |_: String| {
            // Extensions should not emit events directly; no-op with warning.
            Ok::<(), rquickjs::Error>(())
        })?,
    )?;
    freeze(ctx, event_source.clone())?;

    let silly_tavern = Object::new(ctx.clone())?;
    globals.set(
        "__stcliGetContextSnapshot",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            let json = serde_json::to_string(&*snapshot.borrow())
                .map_err(|_| Exception::throw_type(&ctx, "context snapshot is not JSON"))?;
            let value: Value = ctx.json_parse(json)?;
            deep_freeze(&ctx, value)
        })?,
    )?;
    globals.set(
        "__stcliWarnFrozenWrite",
        Function::new(ctx.clone(), {
            let warn_logs = Rc::clone(&logs);
            move || {
                warn_logs.borrow_mut().push(crate::ScriptLog {
                    level: "warn".to_owned(),
                    message: "write to frozen getContext() snapshot ignored".to_owned(),
                });
            }
        })?,
    )?;
    let get_context_fn: Function = ctx.eval::<Function, _>(
        r#"
(function() {
    const rawSnapshot = globalThis.__stcliGetContextSnapshot;
    const warnFrozen = globalThis.__stcliWarnFrozenWrite;
    delete globalThis.__stcliGetContextSnapshot;
    delete globalThis.__stcliWarnFrozenWrite;
    return function getContext() {
        const target = rawSnapshot();
        if (typeof Proxy === 'undefined') return target;
        let warned = false;
        return new Proxy(target, {
            set() { if (!warned) { warned = true; warnFrozen(); } return true; },
            deleteProperty() { if (!warned) { warned = true; warnFrozen(); } return true; }
        });
    };
})();
"#,
    )?;
    silly_tavern.set("getContext", get_context_fn)?;

    // Helper: create a no-op stub that warns once.
    let make_stub = {
        let stub_logs = Rc::clone(&logs);
        move |ctx: &Ctx<'js>, name: &'static str| -> rquickjs::Result<Function<'js>> {
            let warned = Rc::new(Cell::new(false));
            let stub_logs = Rc::clone(&stub_logs);
            Function::new(ctx.clone(), move |_ctx: Ctx<'js>| {
                if !warned.replace(true) {
                    stub_logs.borrow_mut().push(crate::ScriptLog {
                        level: "warn".to_owned(),
                        message: format!("`{name}` is not yet supported in this runtime"),
                    });
                }
                Ok::<Value<'js>, rquickjs::Error>(Value::new_undefined(_ctx.clone()))
            })
        }
    };

    silly_tavern.set("setExtensionPrompt", make_stub(ctx, "setExtensionPrompt")?)?;
    silly_tavern.set(
        "registerSlashCommand",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, registration: Value<'js>, callback: Opt<Value<'js>>| {
                let (name, callback) = if let Some(name) = registration.as_string() {
                    let name = name.to_string()?;
                    let callback = callback.0.and_then(Value::into_function).ok_or_else(|| {
                        Exception::throw_type(
                            &ctx,
                            "registerSlashCommand callback must be a function",
                        )
                    })?;
                    (name, callback)
                } else if let Some(registration) = registration.as_object() {
                    let name = registration.get::<_, String>("name").map_err(|_| {
                        Exception::throw_type(
                            &ctx,
                            "registerSlashCommand command object requires a string name",
                        )
                    })?;
                    let callback = registration.get::<_, Function>("callback").map_err(|_| {
                        Exception::throw_type(
                            &ctx,
                            "registerSlashCommand command object requires a callback function",
                        )
                    })?;
                    (name, callback)
                } else {
                    return Err(Exception::throw_type(
                        &ctx,
                        "registerSlashCommand requires a name or command object",
                    ));
                };
                let name = name.trim().trim_start_matches('/');
                if name.is_empty() || name.chars().any(char::is_whitespace) {
                    return Err(Exception::throw_type(
                        &ctx,
                        "registerSlashCommand name must be a non-empty command name",
                    ));
                }
                ctx.globals()
                    .get::<_, Object>("__stcliSlashCommands")?
                    .set(name, callback)?;
                commands.borrow_mut().insert(name.to_owned());
                Ok(())
            },
        )?,
    )?;
    silly_tavern.set(
        "executeSlashCommands",
        make_stub(ctx, "executeSlashCommands")?,
    )?;
    silly_tavern.set(
        "substituteParams",
        Function::new(ctx.clone(), {
            let warned = Rc::new(Cell::new(false));
            let logs = Rc::clone(&logs);
            move |_ctx: Ctx<'js>, text: String| {
                if !warned.replace(true) {
                    logs.borrow_mut().push(crate::ScriptLog {
                        level: "warn".to_owned(),
                        message:
                            "`substituteParams` is not yet supported in this runtime; returning input unchanged"
                                .to_owned(),
                    });
                }
                Ok::<String, rquickjs::Error>(text)
            }
        })?,
    )?;
    silly_tavern.set(
        "getTokenCount",
        Function::new(ctx.clone(), {
            let warned = Rc::new(Cell::new(false));
            let logs = Rc::clone(&logs);
            move |_ctx: Ctx<'js>, _text: String| {
                if !warned.replace(true) {
                    logs.borrow_mut().push(crate::ScriptLog {
                        level: "warn".to_owned(),
                        message:
                            "`getTokenCount` is not yet supported in this runtime; returning 0"
                                .to_owned(),
                    });
                }
                Ok::<i64, rquickjs::Error>(0)
            }
        })?,
    )?;
    silly_tavern.set(
        "saveSettingsDebounced",
        globals.get::<_, Function>("saveSettingsDebounced")?,
    )?;
    silly_tavern.set("saveMetadata", make_stub(ctx, "saveMetadata")?)?;
    silly_tavern.set("updateChatMetadata", make_stub(ctx, "updateChatMetadata")?)?;
    silly_tavern.set(
        "generateQuietPrompt",
        ctx.eval::<Function, _>("(prompt, options) => Promise.resolve(__stcliInfer(String(prompt), JSON.stringify(options || {})))")?,
    )?;
    silly_tavern.set(
        "generateRaw",
        ctx.eval::<Function, _>("(prompt, options) => Promise.resolve(__stcliInfer(String(prompt), JSON.stringify(options || {})))")?,
    )?;

    freeze(ctx, silly_tavern.clone())?;

    globals.set("event_types", event_types)?;
    globals.set("eventSource", event_source)?;
    globals.set("SillyTavern", silly_tavern)?;

    // Wrap SillyTavern in a Proxy so unknown members are control-flow-safe
    // no-op stubs that warn once per property name.
    globals.set(
        "__stcliWarnUnknownMember",
        Function::new(ctx.clone(), {
            let member_logs = Rc::clone(&logs);
            move |name: String| {
                member_logs.borrow_mut().push(crate::ScriptLog {
                    level: "warn".to_owned(),
                    message: format!(
                        "`SillyTavern.{name}` is not supported in this runtime; no-op"
                    ),
                });
            }
        })?,
    )?;
    ctx.eval::<Value, _>(
        r#"
(function() {
    if (typeof Proxy === 'undefined') {
        delete globalThis.__stcliWarnUnknownMember;
        return;
    }
    const warnUnknown = globalThis.__stcliWarnUnknownMember;
    delete globalThis.__stcliWarnUnknownMember;
    const target = globalThis.SillyTavern;
    const warned = new Set();
    let warnedWrite = false;
    globalThis.SillyTavern = new Proxy(target, {
        get(t, prop) {
            if (typeof prop === 'string' && !(prop in t)) {
                if (!warned.has(prop)) {
                    warned.add(prop);
                    warnUnknown(String(prop));
                }
                return function() { return undefined; };
            }
            return t[prop];
        },
        set() {
            if (!warnedWrite) {
                warnedWrite = true;
                warnUnknown("<assignment>");
            }
            return true;
        }
    });
})();
"#,
    )?;

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
        Rc::clone(&logs),
        warned_delayed_timer,
    )?;

    globals.set("clearTimeout", Function::new(ctx.clone(), || ())?)?;
    globals.set("clearInterval", Function::new(ctx.clone(), || ())?)?;
    install_egress(ctx, &globals, effects.clone(), Rc::clone(&logs))?;
    install_inference(ctx, &globals, effects, Rc::clone(&logs))?;
    Ok(storage_hydrate)
}

const DENIED_FETCH_JSON: &str =
    r#"{"ok":false,"status":0,"statusText":"egress denied","headers":{},"body":"","url":""}"#;

const EGRESS_SHIM: &str = r#"
(() => {
    const respond = (raw) => {
        const r = JSON.parse(raw);
        return {
            ok: r.ok,
            status: r.status,
            statusText: r.statusText,
            headers: r.headers,
            redirected: false,
            type: "basic",
            url: r.url,
            bodyUsed: false,
            text: () => Promise.resolve(r.body),
            json: () => Promise.resolve(JSON.parse(r.body)),
        };
    };
    globalThis.fetch = (url, options = {}) => {
        const request = JSON.stringify({
            url: String(url),
            method: (options.method || "GET").toUpperCase(),
            headers: options.headers || {},
            body: options.body == null ? null : String(options.body),
        });
        return Promise.resolve(respond(__stcliFetch(request)));
    };
    const ajax = (settings = {}) => globalThis.fetch(settings.url, {
        method: (settings.method || settings.type || "GET").toUpperCase(),
        headers: settings.headers || {},
        body: settings.data == null ? null : String(settings.data),
    }).then((response) => response.text().then((text) => {
        let data = text;
        if (settings.dataType === "json" && text) {
            try { data = JSON.parse(text); } catch { }
        }
        const jqXHR = { status: response.status, statusText: response.statusText, responseText: text };
        if (response.ok && settings.success) settings.success(data, "success", jqXHR);
        if (!response.ok && settings.error) settings.error(jqXHR, "error", response.statusText);
        if (settings.complete) settings.complete(jqXHR);
        return data;
    }));
    globalThis.$ = { ajax };
    globalThis.jQuery = globalThis.$;
})();
"#;

fn install_egress<'js>(
    ctx: &Ctx<'js>,
    globals: &Object<'js>,
    effects: Rc<RefCell<BridgeEffectState>>,
    logs: Rc<RefCell<Vec<crate::ScriptLog>>>,
) -> rquickjs::Result<()> {
    globals.set(
        "__stcliFetch",
        Function::new(ctx.clone(), move |request_json: String| -> String {
            let request: EgressRequest = match serde_json::from_str(&request_json) {
                Ok(request) => request,
                Err(_) => {
                    logs.borrow_mut().push(crate::ScriptLog {
                        level: "warn".to_owned(),
                        message: "egress denied: malformed fetch request".to_owned(),
                    });
                    return DENIED_FETCH_JSON.to_owned();
                }
            };
            let invocation = effects.borrow().egress.clone();
            let Some(invocation) = invocation else {
                logs.borrow_mut().push(crate::ScriptLog {
                    level: "warn".to_owned(),
                    message: "egress denied: egress is unavailable in this host".to_owned(),
                });
                return DENIED_FETCH_JSON.to_owned();
            };
            let caller = effects.borrow().caller.clone();
            let mut state = effects.borrow_mut();
            let outcome = invocation.broker.fetch(
                &caller,
                &invocation.policy,
                invocation.mode,
                &request,
                &mut logs.borrow_mut(),
            );
            if let Some(receipt) = outcome.receipt {
                state.egress_receipts.push(receipt);
            }
            serde_json::to_string(&serde_json::json!({
                "ok": outcome.ok,
                "status": outcome.response.status,
                "statusText": outcome.response.status_text,
                "headers": outcome.response.headers,
                "body": outcome.response.body,
                "url": request.url,
            }))
            .unwrap_or_else(|_| DENIED_FETCH_JSON.to_owned())
        })?,
    )?;
    ctx.eval::<Value, _>(EGRESS_SHIM)?;
    Ok(())
}

fn install_inference<'js>(
    ctx: &Ctx<'js>,
    globals: &Object<'js>,
    effects: Rc<RefCell<BridgeEffectState>>,
    logs: Rc<RefCell<Vec<crate::ScriptLog>>>,
) -> rquickjs::Result<()> {
    globals.set("__stcliInfer", Function::new(ctx.clone(), move |prompt: String, options_json: String| {
        let options: JsonValue = serde_json::from_str(&options_json).unwrap_or(JsonValue::Object(serde_json::Map::new()));
        let Some(invocation) = effects.borrow().inference.clone() else {
            logs.borrow_mut().push(crate::ScriptLog { level: "warn".to_owned(), message: "`generateQuietPrompt`/`generateRaw` unavailable: secondary inference is unavailable in this host".to_owned() });
            return Ok::<String, rquickjs::Error>(String::new());
        };
        let object = options.as_object().cloned().unwrap_or_default();
        let profile = object.get("provider").or_else(|| object.get("providerProfile")).and_then(JsonValue::as_str).unwrap_or(&invocation.default_profile).to_owned();
        let request = crate::InferenceRequest { prompt, profile_name: profile, generation_settings: JsonValue::Object(object) };
        match invocation.broker.infer("st-bridge", &invocation.policy, &request) {
            Ok(response) => { effects.borrow_mut().inference_receipts.push(response.receipt); Ok(response.text) }
            Err(error) => { logs.borrow_mut().push(crate::ScriptLog { level: "warn".to_owned(), message: error.to_string() }); Ok(String::new()) }
        }
    })?)?;
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
