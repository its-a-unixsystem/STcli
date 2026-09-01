use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{OnceLock, mpsc},
};

use rquickjs::{
    CatchResultExt, CaughtError, Context, Ctx, Exception, Function, Module, Object, Persistent,
    Runtime, Value, context::intrinsic, promise::PromiseState,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::{
    ChatMessage, ChatRole, ContentHash, EntityId, InstalledPlugin, PluginEffect, PluginError,
    PluginEvent, PluginInput, PromptContribution, PromptSlot, ScriptLimits, ScriptOutcome,
};

type Listener = Persistent<Function<'static>>;

#[derive(Clone, Eq, Hash, PartialEq)]
struct ContextKey {
    session_id: EntityId,
    plugin_id: String,
    component_sha256: ContentHash,
}

struct BridgeContext {
    context: Context,
    listeners: Rc<RefCell<Vec<Listener>>>,
    snapshot: Rc<RefCell<JsonValue>>,
    ticks: Rc<Cell<u64>>,
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
            let result = self.execute(
                request.installed,
                request.input,
                &request.source,
                request.limits,
            );
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
        if input.event != PluginEvent::ChatCompletionPromptReady {
            return Err(PluginError::UnsupportedStBridgeEvent);
        }
        let session_id = input
            .session
            .get("session_id")
            .and_then(JsonValue::as_str)
            .and_then(|value| value.parse::<EntityId>().ok())
            .ok_or(PluginError::StBridgeSessionIdentity)?;
        let key = ContextKey {
            session_id,
            plugin_id: installed.manifest.id.clone(),
            component_sha256: installed.manifest.component_sha256.clone(),
        };
        let original = decode_chat(&input.payload)?;
        if !self.contexts.contains_key(&key) {
            let context =
                BridgeContext::new(&installed.manifest.id, source, &input.context, limits)?;
            self.contexts.insert(key.clone(), context);
        }
        let context = self
            .contexts
            .get_mut(&key)
            .ok_or(PluginError::StBridgeWorkerStopped)?;
        let updated = context.dispatch(
            &installed.manifest.id,
            &input.context,
            &input.payload,
            limits,
        )?;
        let chat = decode_chat(&updated)?;
        if chat.len() < original.len() || chat[..original.len()] != original {
            return Err(PluginError::UnsupportedStBridgeMutation);
        }
        let effects = chat[original.len()..]
            .iter()
            .enumerate()
            .map(|(index, message)| PluginEffect::Prompt {
                contribution: PromptContribution {
                    slot: PromptSlot::InChat,
                    name: format!("{}#{}", installed.manifest.id, index + 1),
                    role: role_name(message.role).to_owned(),
                    content: message.content.clone(),
                    depth: None,
                    order: index + 1,
                    outlet: None,
                },
            })
            .collect();
        Ok(ScriptOutcome {
            effects,
            logs: Vec::new(),
        })
    }
}

impl BridgeContext {
    fn new(
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
        let listeners = Rc::new(RefCell::new(Vec::new()));
        let snapshot = Rc::new(RefCell::new(initial_snapshot.clone()));
        context.with(|ctx| {
            install_globals(&ctx, Rc::clone(&listeners), Rc::clone(&snapshot))
                .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
            let globals = ctx.globals();
            globals
                .remove("eval")
                .map_err(|error| PluginError::ScriptRuntime(error.to_string()))?;
            globals
                .get::<_, Object>("Math")
                .and_then(|math| math.remove("random"))
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
        })
    }

    fn dispatch(
        &mut self,
        plugin_id: &str,
        snapshot: &JsonValue,
        payload: &JsonValue,
        limits: ScriptLimits,
    ) -> Result<JsonValue, PluginError> {
        *self.snapshot.borrow_mut() = snapshot.clone();
        self.ticks.set(limits.interrupt_ticks);
        self.context.with(|ctx| {
            let json = serde_json::to_string(payload)
                .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
            let argument = ctx
                .json_parse(json)
                .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
            for listener in self.listeners.borrow().iter() {
                let listener = listener.clone().restore(&ctx).map_err(|error| {
                    PluginError::StBridgeHandler {
                        plugin: plugin_id.to_owned(),
                        message: error.to_string(),
                    }
                })?;
                listener
                    .call::<_, Value>((argument.clone(),))
                    .catch(&ctx)
                    .map_err(|error| map_caught(&ctx, plugin_id, &self.ticks, error, false))?;
            }
            let text = ctx
                .json_stringify(argument)
                .map_err(|error| PluginError::StBridgePayload(error.to_string()))?
                .ok_or_else(|| PluginError::StBridgePayload("payload is not JSON".to_owned()))?
                .to_string()
                .map_err(|error| PluginError::StBridgePayload(error.to_string()))?;
            serde_json::from_str(&text)
                .map_err(|error| PluginError::StBridgePayload(error.to_string()))
        })
    }
}

fn install_globals<'js>(
    ctx: &Ctx<'js>,
    listeners: Rc<RefCell<Vec<Listener>>>,
    snapshot: Rc<RefCell<JsonValue>>,
) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    let event_types = Object::new(ctx.clone())?;
    event_types.set(
        "CHAT_COMPLETION_PROMPT_READY",
        "chat-completion-prompt-ready",
    )?;
    freeze(ctx, event_types.clone())?;

    let event_source = Object::new(ctx.clone())?;
    event_source.set(
        "on",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, event: String, listener: Function<'js>| {
                if event != "chat-completion-prompt-ready" {
                    return Err(Exception::throw_type(&ctx, "unsupported event type"));
                }
                listeners
                    .borrow_mut()
                    .push(Persistent::save(&ctx, listener));
                Ok(())
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

fn role_name(role: ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    }
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
