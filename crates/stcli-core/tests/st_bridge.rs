use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use serde_json::json;
use stcli_core::{
    CredentialError, CredentialResolver, EGRESS_REQUEST_DOMAIN, EgressAllowance, EgressBroker,
    EgressRequest, EgressResponse, EgressSecretInjection, EgressTransport, EngineCommand,
    EngineResult, EntityId, InstalledPlugin, PluginEffect, PluginEvent, PluginGrant, PluginHost,
    PluginInput, PluginLimits, PluginPin, PluginReceipt, PluginRegistry, StcliEngine, Store,
    StubTransport, canonical_json_hash, content_blob_hash, plugin_digest,
};
use stcli_testkit::{MockProvider, configuration as base_configuration, fixtures};
use tempfile::tempdir;

const PLUGIN_ID: &str = "org.stcli.st-bridge-proof";
const SOURCE: &str = r#"
let lifecycleOrder = [];
let initCount = 0;
let appReadyCount = 0;
let chatChangedCount = 0;
let generationStartedCount = 0;
let messageSentCount = 0;
let messageReceivedCount = 0;
let generationEndedCount = 0;
let promptReadyCount = 0;
let userRenderedCount = 0;
let characterRenderedCount = 0;
let toolRenderedCount = 0;

initCount += 1;

eventSource.on(event_types.APP_READY, () => {
  appReadyCount += 1;
  lifecycleOrder.push('app_ready');
});

eventSource.on(event_types.CHAT_CHANGED, () => {
  chatChangedCount += 1;
  lifecycleOrder.push('chat_id_changed');
});

eventSource.on(event_types.GENERATION_STARTED, () => {
  generationStartedCount += 1;
  lifecycleOrder.push('generation_started');
});

eventSource.on(event_types.MESSAGE_SENT, () => {
  messageSentCount += 1;
  lifecycleOrder.push('message_sent');
});

eventSource.on(event_types.MESSAGE_RECEIVED, () => {
  messageReceivedCount += 1;
  lifecycleOrder.push('message_received');
});

eventSource.on(event_types.GENERATION_ENDED, () => {
  generationEndedCount += 1;
  lifecycleOrder.push('generation_ended');
});

eventSource.on(event_types.USER_MESSAGE_RENDERED, () => {
  userRenderedCount += 1;
});

eventSource.on(event_types.CHARACTER_MESSAGE_RENDERED, () => {
  characterRenderedCount += 1;
});

eventSource.on(event_types.TOOL_CALLS_RENDERED, () => {
  toolRenderedCount += 1;
});

async function namedInterceptor(chat, contextSize, abortSignal, generationType) {
  await Promise.resolve();
  lifecycleOrder.push('interceptor');

  // Mutate the supplied chat in place: remove, edit, and insert.
  chat.splice(0, 1);
  if (chat.length > 0) {
    chat[0].mes = chat[0].mes + ' [interceptor-edit]';
  }
  chat.push({
    name: 'System',
    is_user: false,
    is_system: true,
    mes: `interceptor: init=${initCount}`,
    extra: {},
    index: chat.length
  });
}

globalThis.namedInterceptor = namedInterceptor;

eventSource.on(event_types.CHAT_COMPLETION_PROMPT_READY, async (eventData) => {
  await Promise.resolve();
  promptReadyCount += 1;
  lifecycleOrder.push('chat_completion_prompt_ready');

  const context = SillyTavern.getContext();
  eventData.chat.push({
    role: 'system',
    content: `prompt-ready: order=${lifecycleOrder.join(',')} renders=${userRenderedCount},${characterRenderedCount},${toolRenderedCount}`
  });
});
"#;

fn write_bridge_plugin(directory: &Path) -> PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("extension.js"), SOURCE).unwrap();
    let manifest = json!({
        "schema": "stcli.plugin-manifest/v1",
        "id": PLUGIN_ID,
        "version": "0.1.0",
        "engine": ">=0.1.0, <0.2.0",
        "runtime": "st-bridge",
        "component": "extension.js",
        "component_sha256": plugin_digest(SOURCE.as_bytes()),
        "dependencies": [],
        "license": "MIT",
        "subscriptions": ["generate-interceptor", "chat-completion-prompt-ready", "st-bridge-lifecycle"],
        "prompt_slots": [],
        "commands": [],
        "macros": [],
        "settings_schema": null,
        "requested_capabilities": ["contribute-prompt", "observe-lifecycle", "read-session"],
        "before": [],
        "after": [],
        "generate_interceptor": "namedInterceptor"
    });
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    directory.to_owned()
}

fn write_test_bridge_plugin(directory: &Path, id: &str, source: &str) -> PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("extension.js"), source).unwrap();
    let manifest = json!({
        "schema": "stcli.plugin-manifest/v1",
        "id": id,
        "version": "0.1.0",
        "engine": ">=0.1.0, <0.2.0",
        "runtime": "st-bridge",
        "component": "extension.js",
        "component_sha256": plugin_digest(source.as_bytes()),
        "dependencies": [],
        "license": "MIT",
        "subscriptions": ["generate-interceptor"],
        "prompt_slots": [],
        "commands": [],
        "macros": [],
        "settings_schema": null,
        "requested_capabilities": ["contribute-prompt"],
        "before": [],
        "after": [],
        "generate_interceptor": "namedInterceptor"
    });
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    directory.to_owned()
}

fn authorize(installed: &InstalledPlugin) -> PluginGrant {
    PluginGrant {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.clone(),
        component_sha256: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    }
}

fn interceptor_input(installed: &InstalledPlugin, session_id: EntityId) -> PluginInput {
    PluginInput {
        event: PluginEvent::GenerateInterceptor,
        plugin_id: installed.manifest.id.clone(),
        settings: json!({}),
        context: json!({}),
        payload: json!({"chat": [], "has_user_message": false}),
        artifact: json!(null),
        state: json!(null),
        session: json!({"session_id": session_id, "branch_id": EntityId::new()}),
    }
}

#[test]
fn bridge_dispatches_lifecycle_events_and_interceptor_with_async_settlement() {
    let directory = tempdir().unwrap();
    let plugin = write_bridge_plugin(&directory.path().join("plugin"));
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let grant = PluginGrant {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.clone(),
        component_sha256: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    };
    let session_id = EntityId::new();
    let branch_id = EntityId::new();

    // First call: APP_READY + CHAT_CHANGED + interceptor
    let first = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::GenerateInterceptor,
                plugin_id: installed.manifest.id.clone(),
                settings: grant.settings.clone(),
                context: json!({
                    "name2": "Alice",
                    "chat": [{"role": "assistant", "content": "Welcome."}],
                }),
                payload: json!({
                    "chat": [
                        {"name": "Alice", "is_user": false, "is_system": false, "mes": "Welcome.", "extra": {}, "index": 0},
                        {"name": "User", "is_user": true, "is_system": false, "mes": "Hello", "extra": {}, "index": 1}
                    ],
                    "has_user_message": true,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": branch_id}),
            },
        )
        .unwrap();

    // Verify interceptor returned PromptRewrite with modified messages
    assert_eq!(first.effects.len(), 1);
    match &first.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            assert_eq!(messages.len(), 2);
            assert_eq!(messages[0].content, "Hello [interceptor-edit]");
            assert!(messages[1].content.contains("interceptor: init=1"));
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }

    // Second call with prompt-ready: reuses context, increments counters
    let second = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::ChatCompletionPromptReady,
                plugin_id: installed.manifest.id.clone(),
                settings: grant.settings.clone(),
                context: json!({
                    "name2": "Alice",
                    "chat": [{"role": "assistant", "content": "Welcome."}],
                }),
                payload: json!({
                    "chat": [{"role": "user", "content": "Modified"}],
                    "dryRun": false,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": branch_id}),
            },
        )
        .unwrap();

    match &second.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            assert_eq!(messages.len(), 2);
            let content = &messages[1].content;
            assert!(content.contains("prompt-ready:"));
            assert!(content.contains("order=app_ready,chat_id_changed,generation_started,message_sent,interceptor,chat_completion_prompt_ready"));
            assert!(content.contains("renders=0,0,0"));
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }

    // Third call: lifecycle observation only (no rewrite)
    let third = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::StBridgeLifecycle,
                plugin_id: installed.manifest.id.clone(),
                settings: grant.settings.clone(),
                context: json!({
                    "name2": "Alice",
                    "chat": [{"role": "assistant", "content": "Welcome."}, {"role": "assistant", "content": "Generated."}],
                }),
                payload: json!({
                    "events": [
                        {"name": "message_received", "args": [2, "continue"]},
                        {"name": "generation_ended", "args": [2]}
                    ]
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": branch_id}),
            },
        )
        .unwrap();

    assert_eq!(third.effects.len(), 1);
    match &third.effects[0] {
        PluginEffect::Observe { .. } => {}
        effect => panic!("expected Observe, got {effect:?}"),
    }
}

#[tokio::test]
async fn dry_run_applies_interceptor_and_prompt_ready_mutations() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let plugin = write_bridge_plugin(&directory.path().join("plugin"));
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&plugin).unwrap();
    let database = data.join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    drop(store);

    let mut configuration = base_configuration(character.revision_hash);
    configuration.plugins = vec![PluginPin {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    }];
    let engine = StcliEngine::new(&database);
    let EngineResult::CreatedSession(created) = engine
        .execute(
            EngineCommand::CreateSession {
                configuration: Box::new(configuration),
                greeting_index: 0,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected create result");
    };

    let EngineResult::DryRun(result) = engine
        .execute(
            EngineCommand::DryRunSend {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "Hello".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected Dry Run result");
    };

    // Verify both interceptor and prompt-ready mutations are in the provider request
    let messages = result.provider_request["messages"].as_array().unwrap();

    // Find interceptor mutation
    let has_interceptor = messages.iter().any(|msg| {
        msg["content"]
            .as_str()
            .is_some_and(|c| c.contains("interceptor: init=1"))
    });
    assert!(
        has_interceptor,
        "interceptor mutation missing from provider request"
    );

    // Find prompt-ready mutation with lifecycle order
    let has_prompt_ready = messages.iter().any(|msg| {
        msg["content"].as_str().is_some_and(|c| {
            c.contains("prompt-ready:") && c.contains("order=") && c.contains("interceptor")
        })
    });
    assert!(
        has_prompt_ready,
        "prompt-ready mutation missing from provider request"
    );

    // Verify both mutations are in PromptPlan receipts
    assert_eq!(result.prompt_plan.plugin_receipts.len(), 2);
    assert_eq!(
        result.prompt_plan.plugin_receipts[0].event,
        PluginEvent::GenerateInterceptor
    );
    assert_eq!(
        result.prompt_plan.plugin_receipts[1].event,
        PluginEvent::ChatCompletionPromptReady
    );

    // Verify no turn was committed
    let store = Store::open(&database).unwrap();
    assert!(
        store
            .turns_for_branch(created.branch.branch_id)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn live_attempt_records_lifecycle_and_rerun_reuses_recorded_prompt() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let plugin = write_bridge_plugin(&directory.path().join("plugin"));
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&plugin).unwrap();
    let mock = MockProvider::spawn(["Generated response"]).await.unwrap();
    let database = data.join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut configuration = base_configuration(character.revision_hash);
    configuration.provider = mock.provider_settings();
    configuration.plugins = vec![PluginPin {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    }];
    let created = store.create_session(configuration, 0).unwrap();
    let completed = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello".to_owned(),
            |_| {},
        )
        .await
        .unwrap();
    let attempt_id = completed.attempt.attempt_id;
    let persisted = store.attempt(attempt_id).unwrap().unwrap();
    let original_plan = persisted.prompt_plan.clone();
    let original_request = persisted
        .effect_receipt
        .as_ref()
        .unwrap()
        .provider_request
        .clone();
    let plugins = &completed.attempt.effect_receipt.as_ref().unwrap().plugins;
    let lifecycle = plugins
        .iter()
        .find(|receipt| receipt.event == PluginEvent::StBridgeLifecycle)
        .unwrap();
    let names = lifecycle.input.payload["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["message_received", "generation_ended"]);

    // Make the installed component unavailable; recorded rerun must not initialize QuickJS.
    std::fs::remove_file(installed.directory.join(&installed.manifest.component)).unwrap();
    let rerun = store.dry_run_rerun(attempt_id).unwrap();
    assert_eq!(rerun.prompt_plan, original_plan);
    assert_eq!(rerun.provider_request, original_request);
}

#[test]
fn prng_seed_is_recorded_in_replayed_receipt() {
    let source = r#"
function namedInterceptor(chat) {
  const values = Array.from({ length: 5 }, () => Math.random());
  chat.push({ is_user: false, is_system: false, mes: JSON.stringify(values) });
}
globalThis.namedInterceptor = namedInterceptor;
"#;
    let directory = tempdir().unwrap();
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.seeded-prng-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &authorize(&installed),
            interceptor_input(&installed, EntityId::new()),
        )
        .unwrap();

    assert_ne!(receipt.prng_seed, Some(0));
    let values = match &receipt.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            serde_json::from_str::<Vec<f64>>(&messages[0].content).unwrap()
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    };
    assert_eq!(values.len(), 5);
    assert!(values.iter().all(|value| (0.0..1.0).contains(value)));
    let replayed: PluginReceipt =
        serde_json::from_value(serde_json::to_value(&receipt).unwrap()).unwrap();
    assert_eq!(replayed.prng_seed, receipt.prng_seed);
    assert_eq!(replayed.effects, receipt.effects);
}

#[test]
fn immediate_timer_resolves_as_microtask() {
    let source = r#"
function namedInterceptor(chat) {
  setTimeout(() => {
    chat.push({ is_user: false, is_system: false, mes: "42" });
  }, 0);
}
globalThis.namedInterceptor = namedInterceptor;
"#;
    let directory = tempdir().unwrap();
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.immediate-timer-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &authorize(&installed),
            interceptor_input(&installed, EntityId::new()),
        )
        .unwrap();

    match &receipt.effects[0] {
        PluginEffect::PromptRewrite { messages } => assert_eq!(messages[0].content, "42"),
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }
}

#[test]
fn delayed_timer_rejected_with_warning() {
    let source = r#"
for (let attempt = 0; attempt < 2; attempt += 1) {
  try {
    setTimeout(() => {}, 100);
  } catch {}
}
function namedInterceptor() {}
globalThis.namedInterceptor = namedInterceptor;
"#;
    let directory = tempdir().unwrap();
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.delayed-timer-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &authorize(&installed),
            interceptor_input(&installed, EntityId::new()),
        )
        .unwrap();

    assert_eq!(receipt.script_logs.len(), 1);
    assert_eq!(receipt.script_logs[0].level, "warn");
    assert!(
        receipt.script_logs[0]
            .message
            .contains("`setTimeout` with delay is unsupported")
    );
}

#[test]
fn abandoned_async_work_records_warning_and_no_effect() {
    let source = r#"
function namedInterceptor() {
  return new Promise(() => {});
}
globalThis.namedInterceptor = namedInterceptor;
"#;
    let directory = tempdir().unwrap();
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.async-timeout-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &authorize(&installed),
            interceptor_input(&installed, EntityId::new()),
        )
        .unwrap();

    assert!(receipt.effects.is_empty());
    assert_ne!(receipt.prng_seed, Some(0));
    assert_eq!(receipt.script_logs.len(), 1);
    assert!(
        receipt.script_logs[0]
            .message
            .contains("async callback exceeded 64 microtasks")
    );
}

struct MapCredentialResolver {
    secrets: BTreeMap<String, String>,
}

impl CredentialResolver for MapCredentialResolver {
    fn get(&self, key: &str) -> Result<String, CredentialError> {
        self.secrets
            .get(key)
            .cloned()
            .ok_or(CredentialError::Missing)
    }
}

#[derive(Default)]
struct CapturingTransport {
    requests: Mutex<Vec<EgressRequest>>,
    responses: BTreeMap<String, EgressResponse>,
}

impl EgressTransport for CapturingTransport {
    fn roundtrip(
        &self,
        request: &EgressRequest,
    ) -> Result<EgressResponse, stcli_core::EgressTransportError> {
        self.requests.lock().push(request.clone());
        Ok(self
            .responses
            .get(&request.url)
            .cloned()
            .unwrap_or(EgressResponse {
                status: 200,
                status_text: "OK".to_owned(),
                headers: BTreeMap::new(),
                body: String::new(),
            }))
    }
}

const EGRESS_SOURCE: &str = r#"
async function namedInterceptor(chat) {
  const response = await fetch("https://api.example.com/v1/data");
  const body = await response.text();
  chat.push({
    name: 'System',
    is_user: false,
    is_system: true,
    mes: 'fetch:' + response.status + ':' + body,
    extra: {},
    index: chat.length
  });
}
globalThis.namedInterceptor = namedInterceptor;
"#;

fn write_egress_plugin(directory: &Path) -> PathBuf {
    let plugin = write_test_bridge_plugin(
        &directory.join("plugin"),
        "org.stcli.egress-proof",
        EGRESS_SOURCE,
    );
    // widen requested capabilities to include brokered-egress
    let manifest_path = plugin.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["requested_capabilities"] =
        json!(["contribute-prompt", "brokered-egress", "read-session"]);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    plugin
}

fn stub_broker() -> EgressBroker {
    let mut responses = BTreeMap::new();
    responses.insert(
        "https://api.example.com/v1/data".to_owned(),
        EgressResponse {
            status: 200,
            status_text: "OK".to_owned(),
            headers: BTreeMap::new(),
            body: "egress-body".to_owned(),
        },
    );
    let transport = StubTransport { responses };
    let credentials = MapCredentialResolver {
        secrets: BTreeMap::new(),
    };
    EgressBroker::stub(Arc::new(transport), Arc::new(credentials))
}

fn egress_pin(installed: &InstalledPlugin, allow_list: Vec<EgressAllowance>) -> PluginPin {
    PluginPin {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({}),
        egress_allow_list: allow_list,
        enabled: true,
    }
}

#[tokio::test]
async fn allowed_fetch_records_receipt_and_replays_offline() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let plugin = write_egress_plugin(&data);
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&plugin).unwrap();
    let mock = MockProvider::spawn(["Generated response"]).await.unwrap();
    let database = data.join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut configuration = base_configuration(character.revision_hash);
    configuration.provider = mock.provider_settings();
    configuration.plugins = vec![egress_pin(
        &installed,
        vec![EgressAllowance {
            domain: "api.example.com".to_owned(),
            secret: None,
        }],
    )];
    let created = store.create_session(configuration, 0).unwrap();
    store.set_egress_broker(stub_broker());
    let completed = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello".to_owned(),
            |_| {},
        )
        .await
        .unwrap();

    let attempt = &completed.attempt;
    let receipt = attempt
        .effect_receipt
        .as_ref()
        .unwrap()
        .plugins
        .iter()
        .find(|receipt| receipt.event == PluginEvent::GenerateInterceptor)
        .unwrap();
    assert_eq!(receipt.egress.len(), 1);
    let egress = &receipt.egress[0];
    assert_eq!(egress.url, "https://api.example.com/v1/data");
    assert_eq!(egress.method, "GET");
    assert_eq!(egress.status, 200);
    assert_eq!(egress.body, "egress-body");
    assert_eq!(egress.response_hash, content_blob_hash(b"egress-body"));
    assert_ne!(egress.request_hash, content_blob_hash(b""));

    let messages = &attempt.effect_receipt.as_ref().unwrap().provider_request["messages"];
    let contents = serde_json::to_string(messages).unwrap();
    assert!(contents.contains("fetch:200:egress-body"), "{contents}");

    // Offline replay: delete the component, dry_run_rerun must not re-execute.
    let attempt_id = attempt.attempt_id;
    let persisted = store.attempt(attempt_id).unwrap().unwrap();
    let original_plan = persisted.prompt_plan.clone();
    let original_request = persisted
        .effect_receipt
        .as_ref()
        .unwrap()
        .provider_request
        .clone();
    std::fs::remove_file(installed.directory.join(&installed.manifest.component)).unwrap();
    let rerun = store.dry_run_rerun(attempt_id).unwrap();
    assert_eq!(rerun.prompt_plan, original_plan);
    assert_eq!(rerun.provider_request, original_request);
}

#[tokio::test]
async fn dry_run_exercises_egress_offline() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let plugin = write_egress_plugin(&data);
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&plugin).unwrap();
    let database = data.join("stcli.sqlite3");
    let engine = StcliEngine::with_egress_broker(&database, stub_broker());
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut configuration = base_configuration(character.revision_hash);
    configuration.plugins = vec![egress_pin(
        &installed,
        vec![EgressAllowance {
            domain: "api.example.com".to_owned(),
            secret: None,
        }],
    )];
    let EngineResult::CreatedSession(created) = engine
        .execute(
            EngineCommand::CreateSession {
                configuration: Box::new(configuration),
                greeting_index: 0,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected create result");
    };
    let EngineResult::DryRun(result) = engine
        .execute(
            EngineCommand::DryRunSend {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "Hello".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected dry run result");
    };

    let contents = serde_json::to_string(&result.provider_request["messages"]).unwrap();
    assert!(contents.contains("fetch:200:egress-body"), "{contents}");
    let receipt = result
        .prompt_plan
        .plugin_receipts
        .iter()
        .find(|receipt| receipt.event == PluginEvent::GenerateInterceptor)
        .unwrap();
    assert_eq!(receipt.egress.len(), 1);
    assert_eq!(receipt.egress[0].status, 200);
    assert_eq!(receipt.egress[0].body, "egress-body");

    // Nothing was committed.
    let store = Store::open(&database).unwrap();
    assert!(
        store
            .turns_for_branch(created.branch.branch_id)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unallowed_domain_is_refused_non_fatally() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let plugin = write_egress_plugin(&data);
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&plugin).unwrap();
    let mock = MockProvider::spawn(["Generated response"]).await.unwrap();
    let database = data.join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut configuration = base_configuration(character.revision_hash);
    configuration.provider = mock.provider_settings();
    // Empty allow-list: the capability is granted but no domain is allowed.
    configuration.plugins = vec![egress_pin(&installed, Vec::new())];
    let created = store.create_session(configuration, 0).unwrap();
    store.set_egress_broker(stub_broker());
    let completed = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello".to_owned(),
            |_| {},
        )
        .await
        .unwrap();

    let attempt = &completed.attempt;
    let receipt = attempt
        .effect_receipt
        .as_ref()
        .unwrap()
        .plugins
        .iter()
        .find(|receipt| receipt.event == PluginEvent::GenerateInterceptor)
        .unwrap();
    assert!(receipt.egress.is_empty());
    let warn = receipt
        .script_logs
        .iter()
        .find(|log| log.level == "warn")
        .unwrap();
    assert!(
        warn.message
            .contains("'api.example.com' is not in the egress allow-list"),
        "{}",
        warn.message
    );

    let contents = serde_json::to_string(
        &attempt.effect_receipt.as_ref().unwrap().provider_request["messages"],
    )
    .unwrap();
    assert!(contents.contains("fetch:0:"), "{contents}");
}

#[tokio::test]
async fn secret_is_injected_out_of_band() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let plugin = write_egress_plugin(&data);
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&plugin).unwrap();
    let mock = MockProvider::spawn(["Generated response"]).await.unwrap();
    let database = data.join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut configuration = base_configuration(character.revision_hash);
    configuration.provider = mock.provider_settings();
    configuration.plugins = vec![egress_pin(
        &installed,
        vec![EgressAllowance {
            domain: "api.example.com".to_owned(),
            secret: Some(EgressSecretInjection {
                credential_key: "test-key".to_owned(),
                header: "Authorization".to_owned(),
                value_template: "Bearer {secret}".to_owned(),
            }),
        }],
    )];
    let created = store.create_session(configuration, 0).unwrap();
    let transport = Arc::new(CapturingTransport::default());
    let credentials = MapCredentialResolver {
        secrets: BTreeMap::from([("test-key".to_owned(), "s3cr3t".to_owned())]),
    };
    store.set_egress_broker(EgressBroker::stub(transport.clone(), Arc::new(credentials)));
    let completed = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello".to_owned(),
            |_| {},
        )
        .await
        .unwrap();

    let requests = transport.requests.lock();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers.get("Authorization"),
        Some(&"Bearer s3cr3t".to_owned())
    );
    drop(requests);

    // Everything the Plugin could observe (its prompt contribution) stays secret-free.
    let contents = serde_json::to_string(
        &completed
            .attempt
            .effect_receipt
            .as_ref()
            .unwrap()
            .provider_request["messages"],
    )
    .unwrap();
    assert!(!contents.contains("s3cr3t"), "{contents}");

    // The request hash covers names, not values: no secret entered the hash input.
    let receipt = completed
        .attempt
        .effect_receipt
        .as_ref()
        .unwrap()
        .plugins
        .iter()
        .find(|receipt| receipt.event == PluginEvent::GenerateInterceptor)
        .unwrap();
    let expected_hash = canonical_json_hash(
        EGRESS_REQUEST_DOMAIN,
        &json!({
            "method": "GET",
            "url": "https://api.example.com/v1/data",
            "body": null,
            "injected_headers": ["Authorization"],
        }),
    )
    .unwrap();
    assert_eq!(receipt.egress[0].request_hash, expected_hash);
    let receipt_json = serde_json::to_string(receipt).unwrap();
    assert!(!receipt_json.contains("s3cr3t"), "{receipt_json}");
}
