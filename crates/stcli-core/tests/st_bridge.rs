use std::path::{Path, PathBuf};

use serde_json::json;
use stcli_core::{
    EngineCommand, EngineResult, EntityId, PluginEffect, PluginEvent, PluginGrant, PluginHost,
    PluginInput, PluginLimits, PluginPin, PluginRegistry, StcliEngine, Store, plugin_digest,
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
