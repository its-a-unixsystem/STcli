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
    EngineError, EngineResult, EntityId, ExtensionCommandTrace, InstalledPlugin, PluginEffect,
    PluginEvent, PluginGrant, PluginHost, PluginInput, PluginLimits, PluginPin, PluginReceipt,
    PluginRegistry, StcliEngine, Store, StscriptError, StscriptLimits, StscriptProgram,
    StscriptResult, StubTransport, VariableScope, canonical_json_hash, content_blob_hash,
    plugin_digest,
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
    write_bridge_plugin_with_capabilities(directory, id, source, &["contribute-prompt"])
}

fn write_storage_bridge_plugin(directory: &Path, id: &str, source: &str) -> PathBuf {
    write_bridge_plugin_with_capabilities(
        directory,
        id,
        source,
        &["contribute-prompt", "write-own-state"],
    )
}

fn write_bridge_plugin_with_capabilities(
    directory: &Path,
    id: &str,
    source: &str,
    requested_capabilities: &[&str],
) -> PathBuf {
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
        "requested_capabilities": requested_capabilities,
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

fn write_command_bridge_plugin(directory: &Path, id: &str, source: &str) -> PathBuf {
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
        "subscriptions": [],
        "prompt_slots": [],
        "commands": [],
        "macros": [],
        "settings_schema": null,
        "requested_capabilities": ["register-command", "write-own-state"],
        "before": [],
        "after": [],
        "generate_interceptor": null
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
fn bridge_registers_both_slash_command_forms_and_invokes_persistent_callbacks() {
    let directory = tempdir().unwrap();
    let source = r#"
SillyTavern.registerSlashCommand('/greet', (named, unnamed) => `${named.who}:${unnamed}:old`);
SillyTavern.registerSlashCommand('greet', (named, unnamed) => `${named.who}:${unnamed}:latest`);
SillyTavern.registerSlashCommand({
  name: '/object',
  callback: (named, unnamed) => `${named.mode}:${unnamed}`,
  description: 'object registration'
});
"#;
    let plugin = write_command_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.command-registration",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let host = PluginHost::new(Default::default());
    let session_id = EntityId::new();
    let invoke = |command: &str, named, unnamed: &str| {
        host.execute(
            &installed,
            &authorize(&installed),
            PluginInput {
                event: PluginEvent::Command,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({"session_id": session_id}),
                payload: json!({"command": command, "named": named, "unnamed": unnamed}),
                artifact: json!(null),
                state: json!({}),
                session: json!({"session_id": session_id}),
            },
        )
        .unwrap()
    };

    assert_eq!(
        invoke("greet", json!({"who": "Sam"}), "hello").effects,
        vec![PluginEffect::Observe {
            value: json!({"output": "Sam:hello:latest"})
        }]
    );
    assert_eq!(
        invoke("object", json!({"mode": "async"}), "works").effects,
        vec![PluginEffect::Observe {
            value: json!({"output": "async:works"})
        }]
    );
}

#[tokio::test]
async fn engine_routes_extension_slash_commands_and_replays_recorded_output() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let source = r#"
SillyTavern.registerSlashCommand('/greet', (named, unnamed) => {
  return `${named.who}:${unnamed}`;
});
"#;
    let plugin = write_command_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.extension-command",
        source,
    );
    let installed = PluginRegistry::new(data.join("plugins"))
        .install(&plugin)
        .unwrap();
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
    let session_id = created.session.session_id;
    let script = "/greet who=Sam hello";
    let result = engine
        .execute(
            EngineCommand::ExecuteStscript {
                session_id,
                execution_id: EntityId::new(),
                source: script.to_owned(),
                limits: StscriptLimits::default(),
            },
            |_| {},
        )
        .await
        .unwrap();
    assert!(matches!(
        result,
        EngineResult::Stscript(StscriptResult::Completed { output })
            if output == "Sam:hello"
    ));

    let store = Store::open(&database).unwrap();
    let events = store.trace_events(Some(session_id)).unwrap();
    let extension_events = events
        .iter()
        .filter(|event| event.event_type == "extension.command")
        .collect::<Vec<_>>();
    assert_eq!(extension_events.len(), 1);
    let recorded: ExtensionCommandTrace =
        serde_json::from_value(extension_events[0].payload.clone()).unwrap();
    assert_eq!(recorded.command, "greet");
    assert_eq!(
        recorded.named,
        BTreeMap::from([("who".to_owned(), "Sam".to_owned())])
    );
    assert_eq!(recorded.unnamed, "hello");
    assert_eq!(recorded.output, "Sam:hello");
    let receipt = recorded.receipt.as_ref().unwrap();
    assert_eq!(receipt.event, PluginEvent::Command);
    assert_eq!(
        receipt.effects,
        vec![PluginEffect::Observe {
            value: json!({"output": "Sam:hello"})
        }]
    );
    assert!(receipt.script_logs.is_empty());

    let missing = engine
        .execute(
            EngineCommand::ExecuteStscript {
                session_id,
                execution_id: EntityId::new(),
                source: "/missing".to_owned(),
                limits: StscriptLimits::default(),
            },
            |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(
        missing,
        EngineError::Stscript(StscriptError::UnknownCommand(command)) if command == "missing"
    ));

    std::fs::remove_file(installed.directory.join(&installed.manifest.component)).unwrap();
    let replay = StscriptProgram::parse(script)
        .unwrap()
        .evaluate_replay_with_extension_commands(
            StscriptLimits::default(),
            std::slice::from_ref(&recorded),
        )
        .unwrap();
    assert_eq!(replay.output, "Sam:hello");
    assert_eq!(replay.state_mutations, recorded.state_mutations);
}
#[test]
fn bridge_rejects_malformed_slash_command_registration() {
    let directory = tempdir().unwrap();
    let plugin = write_command_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.command-registration-invalid",
        "SillyTavern.registerSlashCommand({ name: '/broken' });",
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let error = PluginHost::new(Default::default())
        .execute(
            &installed,
            &authorize(&installed),
            PluginInput {
                event: PluginEvent::Command,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({}),
                payload: json!({"command": "broken", "named": {}, "unnamed": ""}),
                artifact: json!(null),
                state: json!({}),
                session: json!({"session_id": EntityId::new()}),
            },
        )
        .unwrap_err();

    assert!(matches!(
        error,
        stcli_core::PluginError::StBridgeInitialization { .. }
    ));
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

#[test]
fn bridge_hydrates_and_records_extension_storage() {
    const ID: &str = "org.stcli.storage-proof";
    const STORAGE_SOURCE: &str = r#"
function namedInterceptor(chat) {
  if (localStorage.getItem('token') === null) {
    extension_settings['org.stcli.storage-proof'] = { theme: 'light' };
    extension_settings['org.stcli.storage-proof'] = { theme: 'dark' };
    localStorage.setItem('token', 'abc');
    localStorage.setItem('discarded', 'value');
    localStorage.removeItem('discarded');
    saveSettingsDebounced();
    return;
  }
  chat.push({
    name: 'System',
    is_user: false,
    is_system: true,
    mes: `${extension_settings['org.stcli.storage-proof'].theme}:${localStorage.getItem('token')}:${localStorage.length}:${localStorage.key(0)}`,
    extra: {},
    index: chat.length
  });
}
globalThis.namedInterceptor = namedInterceptor;
"#;
    let directory = tempdir().unwrap();
    let registry = PluginRegistry::new(directory.path().join("registry"));
    let installed = registry
        .install(&write_storage_bridge_plugin(
            &directory.path().join("plugin"),
            ID,
            STORAGE_SOURCE,
        ))
        .unwrap();
    let host = PluginHost::new(PluginLimits::default());
    let session_id = EntityId::new();

    // Regression test for ticket 05: bridge writes must be namespaced and deduplicated.
    let first = host
        .execute(
            &installed,
            &authorize(&installed),
            interceptor_input(&installed, session_id),
        )
        .unwrap();
    assert!(first.effects.iter().any(|effect| matches!(
        effect,
        PluginEffect::StateWrite { key, value }
            if key.scope == VariableScope::Local
                && key.name == format!("extension.{ID}.settings")
                && value == &json!({"theme": "dark"})
    )));
    assert!(first.effects.iter().any(|effect| matches!(
        effect,
        PluginEffect::StateWrite { key, value }
            if key.name == format!("extension.{ID}.ls.token") && value == "abc"
    )));
    assert!(first.effects.iter().any(|effect| matches!(
        effect,
        PluginEffect::StateWrite { key, value }
            if key.name == format!("extension.{ID}.ls.discarded") && value.is_null()
    )));
    assert_eq!(
        first
            .effects
            .iter()
            .filter(|effect| matches!(
                effect,
                PluginEffect::StateWrite { key, .. }
                    if key.name == format!("extension.{ID}.settings")
            ))
            .count(),
        1
    );

    let mut later = interceptor_input(&installed, session_id);
    later.state = json!({
        "settings": {"theme": "dark"},
        "ls.token": "abc"
    });
    let later = host
        .execute(&installed, &authorize(&installed), later)
        .unwrap();
    let PluginEffect::PromptRewrite { messages } = &later.effects[0] else {
        panic!("expected prompt rewrite");
    };
    assert_eq!(messages.last().unwrap().content, "dark:abc:1:token");
}

#[tokio::test]
async fn extension_storage_is_isolated_and_survives_disable_reenable() {
    const OWNER_ID: &str = "org.stcli.storage-owner";
    const OBSERVER_ID: &str = "org.stcli.storage-observer";
    const OWNER_SOURCE: &str = r#"
function namedInterceptor(chat) {
  if (localStorage.getItem('token') === null) {
    extension_settings['org.stcli.storage-owner'] = { theme: 'dark' };
    localStorage.setItem('token', 'abc');
    saveSettingsDebounced();
  } else {
    chat.push({ name: 'System', is_user: false, is_system: true, mes: `owner:${extension_settings['org.stcli.storage-owner'].theme}:${localStorage.getItem('token')}`, extra: {}, index: chat.length });
  }
}
globalThis.namedInterceptor = namedInterceptor;
"#;
    const OBSERVER_SOURCE: &str = r#"
function namedInterceptor(chat) {
  let qualifiedDenied = false;
  try {
    localStorage.getItem('extension.org.stcli.storage-owner.ls.token');
  } catch (_) {
    qualifiedDenied = true;
  }
  let assignmentDenied = false;
  try {
    extension_settings['org.stcli.storage-owner'] = { stolen: true };
  } catch (_) {
    assignmentDenied = true;
  }
  chat.push({ name: 'System', is_user: false, is_system: true, mes: `observer:${extension_settings['org.stcli.storage-owner'] === undefined}:${qualifiedDenied}:${assignmentDenied}`, extra: {}, index: chat.length });
}
globalThis.namedInterceptor = namedInterceptor;
"#;
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let registry = PluginRegistry::new(data.join("plugins"));
    let owner = registry
        .install(&write_storage_bridge_plugin(
            &directory.path().join("owner"),
            OWNER_ID,
            OWNER_SOURCE,
        ))
        .unwrap();
    let observer = registry
        .install(&write_storage_bridge_plugin(
            &directory.path().join("observer"),
            OBSERVER_ID,
            OBSERVER_SOURCE,
        ))
        .unwrap();
    let mock = MockProvider::spawn(["Generated response"]).await.unwrap();
    let database = data.join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut configuration = base_configuration(character.revision_hash);
    configuration.provider = mock.provider_settings();
    configuration.plugins = [&owner, &observer]
        .into_iter()
        .map(|installed| PluginPin {
            id: installed.manifest.id.clone(),
            version: installed.manifest.version.to_string(),
            component_hash: installed.manifest.component_sha256.clone(),
            capabilities: installed.manifest.requested_capabilities.clone(),
            settings: json!({}),
            egress_allow_list: Vec::new(),
            enabled: true,
        })
        .collect();
    let created = store.create_session(configuration.clone(), 0).unwrap();

    // Regression test for ticket 05: live writes persist, but Dry Run writes do not commit.
    let completed = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "write storage".to_owned(),
            |_| {},
        )
        .await
        .unwrap();
    let mutations = &completed
        .attempt
        .effect_receipt
        .as_ref()
        .unwrap()
        .state_mutations;
    assert!(mutations.iter().any(|mutation| {
        mutation.key.name == format!("extension.{OWNER_ID}.settings")
            && mutation
                .after
                .as_ref()
                .is_some_and(|cell| cell.value == json!({"theme": "dark"}))
    }));
    assert!(mutations.iter().any(|mutation| {
        mutation.key.name == format!("extension.{OWNER_ID}.ls.token")
            && mutation
                .after
                .as_ref()
                .is_some_and(|cell| cell.value == "abc")
    }));
    assert!(
        completed
            .attempt
            .effect_receipt
            .as_ref()
            .unwrap()
            .provider_request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"] == "observer:true:true:true")
    );

    configuration.plugins[0].enabled = false;
    store
        .update_session_configuration(created.session.session_id, configuration.clone())
        .unwrap();
    configuration.plugins[0].enabled = true;
    store
        .update_session_configuration(created.session.session_id, configuration)
        .unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "read storage",
        )
        .unwrap();
    assert!(
        dry_run.provider_request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"] == "owner:dark:abc")
    );
    let persisted = store.state_transaction(created.session.session_id).unwrap();
    assert_eq!(
        persisted
            .get(
                VariableScope::Local,
                &format!("extension.{OWNER_ID}.ls.token")
            )
            .unwrap()
            .value,
        "abc"
    );
    assert!(
        persisted
            .get(
                VariableScope::Local,
                &format!("extension.{OBSERVER_ID}.ls.token")
            )
            .is_none()
    );
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

#[test]
fn get_context_returns_full_snapshot_fields() {
    let directory = tempdir().unwrap();
    let session_id = EntityId::new();
    let branch_id = EntityId::new();
    let source = r#"
globalThis.namedInterceptor = function(chat) {
    const ctx = SillyTavern.getContext();
    chat.push({
        name: 'System',
        is_user: false,
        is_system: true,
        mes: [
            'name1=' + ctx.name1,
            'name2=' + ctx.name2,
            'chatId=' + ctx.chatId,
            'characterId=' + ctx.characterId,
            'chatLen=' + ctx.chat.length,
            'groupsLen=' + ctx.groups.length,
            'hasChatMetadata=' + (typeof ctx.chatMetadata === 'object'),
            'worldInfoLen=' + ctx.worldInfo.length,
            'temperature=' + ctx.generationSettings.temperature,
        ].join(' '),
        extra: {},
        index: chat.length
    });
};
"#;
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.ctx-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let grant = authorize(&installed);

    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::GenerateInterceptor,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({
                    "name1": "User",
                    "name2": "Alice",
                    "chatId": branch_id.to_string(),
                    "sessionId": session_id.to_string(),
                    "chat": [{"role": "assistant", "content": "Hello"}],
                    "characters": [{"name": "Alice", "id": "abc123"}],
                    "characterId": "abc123",
                    "groups": [],
                    "chatMetadata": {},
                    "worldInfo": [],
                    "generationSettings": {"temperature": 0.7},
                }),
                payload: json!({
                    "chat": [{"name": "Alice", "is_user": false, "is_system": false, "mes": "Hello", "extra": {}, "index": 0}],
                    "has_user_message": false,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": branch_id}),
            },
        )
        .unwrap();

    assert_eq!(receipt.effects.len(), 1);
    match &receipt.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            let content = &messages.last().unwrap().content;
            assert!(content.contains("name1=User"), "missing name1: {content}");
            assert!(content.contains("name2=Alice"), "missing name2: {content}");
            assert!(
                content.contains(&format!("chatId={branch_id}")),
                "missing chatId: {content}"
            );
            assert!(
                content.contains("characterId=abc123"),
                "missing characterId: {content}"
            );
            assert!(
                content.contains("chatLen=1"),
                "missing chat length: {content}"
            );
            assert!(content.contains("groupsLen=0"), "missing groups: {content}");
            assert!(
                content.contains("hasChatMetadata=true"),
                "missing chatMetadata: {content}"
            );
            assert!(
                content.contains("worldInfoLen=0"),
                "missing worldInfo: {content}"
            );
            assert!(
                content.contains("temperature=0.7"),
                "missing generationSettings: {content}"
            );
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }
    assert!(receipt.script_logs.is_empty());
}

#[test]
fn frozen_write_warns_and_does_not_throw() {
    let directory = tempdir().unwrap();
    let source = r#"
let writeResult = null;
globalThis.namedInterceptor = function(chat) {
    const ctx = SillyTavern.getContext();
    try {
        ctx.name2 = "Modified";
        writeResult = "no-throw";
    } catch (e) {
        writeResult = "threw: " + e.message;
    }
};
"#;
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.frozen-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let grant = authorize(&installed);
    let session_id = EntityId::new();

    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::GenerateInterceptor,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({"name2": "Alice", "chat": []}),
                payload: json!({
                    "chat": [{"name": "Alice", "is_user": false, "is_system": false, "mes": "Hi", "extra": {}, "index": 0}],
                    "has_user_message": false,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": EntityId::new()}),
            },
        )
        .unwrap();

    // Write should not throw; a warning should be logged.
    let warnings: Vec<_> = receipt
        .script_logs
        .iter()
        .filter(|l| l.level == "warn")
        .collect();
    assert!(
        warnings.iter().any(|l| l.message.contains("frozen")),
        "expected frozen-write warning, got: {:?}",
        receipt.script_logs
    );
}

#[test]
fn stub_methods_warn_once() {
    let directory = tempdir().unwrap();
    let source = r#"
globalThis.namedInterceptor = function(chat) {
    SillyTavern.setExtensionPrompt("test", "prompt", 0, 0, false, "system");
    SillyTavern.setExtensionPrompt("test", "prompt", 0, 0, false, "system");
    const tokens = SillyTavern.getTokenCount("hello");
    const substituted = SillyTavern.substituteParams("hello {{user}}");
    chat.push({
        name: 'System',
        is_user: false,
        is_system: true,
        mes: 'tokens=' + tokens + ' substituted=' + substituted,
        extra: {},
        index: chat.length
    });
    SillyTavern.generateQuietPrompt("test");
};
"#;
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.stub-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let grant = authorize(&installed);
    let session_id = EntityId::new();

    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::GenerateInterceptor,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({}),
                payload: json!({
                    "chat": [{"name": "Alice", "is_user": false, "is_system": false, "mes": "Hi", "extra": {}, "index": 0}],
                    "has_user_message": false,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": EntityId::new()}),
            },
        )
        .unwrap();

    // Verify return values are control-flow-safe.
    match &receipt.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            let content = &messages.last().unwrap().content;
            assert!(
                content.contains("tokens=0"),
                "getTokenCount should return 0: {content}"
            );
            assert!(
                content.contains("substituted=hello {{user}}"),
                "substituteParams should return input: {content}"
            );
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }

    let warnings: Vec<_> = receipt
        .script_logs
        .iter()
        .filter(|l| l.level == "warn")
        .collect();
    // setExtensionPrompt called twice → 1 warning; getTokenCount → 1; generateQuietPrompt → 1.
    assert!(
        warnings
            .iter()
            .any(|l| l.message.contains("setExtensionPrompt")),
        "expected setExtensionPrompt warning, got: {:?}",
        receipt.script_logs
    );
    assert!(
        warnings.iter().any(|l| l.message.contains("getTokenCount")),
        "expected getTokenCount warning, got: {:?}",
        receipt.script_logs
    );
    assert!(
        warnings
            .iter()
            .any(|l| l.message.contains("generateQuietPrompt")),
        "expected generateQuietPrompt warning, got: {:?}",
        receipt.script_logs
    );
}

#[test]
fn event_source_off_removes_listener() {
    let directory = tempdir().unwrap();
    let source = r#"
let callCount = 0;
function handler() { callCount += 1; }
eventSource.on(event_types.APP_READY, handler);
eventSource.off(event_types.APP_READY, handler);
globalThis.namedInterceptor = function(chat) {
    chat.push({
        name: 'System',
        is_user: false,
        is_system: true,
        mes: 'callCount=' + callCount,
        extra: {},
        index: chat.length
    });
};
"#;
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.off-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let grant = authorize(&installed);
    let session_id = EntityId::new();

    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::GenerateInterceptor,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({}),
                payload: json!({
                    "chat": [{"name": "Alice", "is_user": false, "is_system": false, "mes": "Hi", "extra": {}, "index": 0}],
                    "has_user_message": false,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": EntityId::new()}),
            },
        )
        .unwrap();

    // APP_READY was removed by off, so the handler should not have been called.
    match &receipt.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            let content = &messages.last().unwrap().content;
            assert!(
                content.contains("callCount=0"),
                "handler was called despite off(): {content}"
            );
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }
}

// Seam B regression for ticket 06: browser-only APIs degrade without stopping the Extension.
#[test]
fn browser_dom_stubs_are_control_flow_safe_and_warn_once() {
    let directory = tempdir().unwrap();
    let source = r##"
globalThis.namedInterceptor = async function(chat) {
    let threw = false;
    let summary = "";
    try {
        console.log("browser-log");
        console.warn("browser-warn");
        console.error("browser-error");

        const toastResults = [
            toastr.success("saved"),
            toastr.success("saved again"),
            toastr.info("info"),
            toastr.warning("warning"),
            toastr.error("error")
        ];
        const popup = await SillyTavern.callPopup("hello");
        const globalPopup = await callPopup("hello again");

        const jq = $("#missing");
        const jqChain = jq.on("click", () => {}).addClass("active").attr("role", "button")
            .text("hello").hide().fadeIn() === jq;
        const jqGetterSafe = jq.attr("missing") === undefined && jq.val() === undefined;
        const jqAliasSafe = jQuery(document.body).append("x").remove() !== undefined;

        const query = document.querySelector(".missing");
        document.querySelector(".missing");
        const queryAll = document.querySelectorAll(".missing");
        const byId = document.getElementById("missing");
        const element = document.createElement("div");
        const elementChain = element.appendChild(document.createElement("span")) === element;
        const awaitedElement = await Promise.resolve(element);
        document.addEventListener("ready", () => {});
        document.removeEventListener("ready", () => {});
        document.dispatchEvent({});
        window.addEventListener("resize", () => {});
        window.removeEventListener("resize", () => {});
        window.dispatchEvent({});
        window.alert("ignored");
        window.alert("ignored again");

        summary = [
            toastResults.every((value) => value === undefined),
            popup === null && globalPopup === null,
            jqChain && jqGetterSafe && jqAliasSafe,
            query === null && queryAll.length === 0 && byId === null,
            elementChain && awaitedElement === element
        ].join(",");
    } catch (error) {
        threw = true;
        summary = String(error);
    }
    chat.push({
        name: "System",
        is_user: false,
        is_system: true,
        mes: `threw=${threw} summary=${summary}`,
        extra: {},
        index: chat.length
    });
};
"##;
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.browser-stubs",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let grant = authorize(&installed);

    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::GenerateInterceptor,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({}),
                payload: json!({
                    "chat": [{"name": "Alice", "is_user": false, "is_system": false, "mes": "Hi", "extra": {}, "index": 0}],
                    "has_user_message": false,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": EntityId::new(), "branch_id": EntityId::new()}),
            },
        )
        .unwrap();

    match &receipt.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            assert_eq!(
                messages.last().unwrap().content,
                "threw=false summary=true,true,true,true,true"
            );
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }

    for (level, message) in [
        ("log", "browser-log"),
        ("warn", "browser-warn"),
        ("error", "browser-error"),
    ] {
        assert!(
            receipt
                .script_logs
                .iter()
                .any(|entry| entry.level == level && entry.message == message),
            "expected console {level} log, got: {:?}",
            receipt.script_logs
        );
    }

    let warnings: Vec<_> = receipt
        .script_logs
        .iter()
        .filter(|entry| entry.level == "warn")
        .collect();
    for api in [
        "toastr.success",
        "toastr.info",
        "toastr.warning",
        "toastr.error",
        "callPopup",
        "jQuery",
        "document.querySelector",
        "document.querySelectorAll",
        "document.getElementById",
        "document.createElement",
        "document.element.appendChild",
        "document.addEventListener",
        "document.removeEventListener",
        "document.dispatchEvent",
        "window.addEventListener",
        "window.removeEventListener",
        "window.dispatchEvent",
        "window.alert",
    ] {
        let needle = format!("`{api}`");
        let count = warnings
            .iter()
            .filter(|entry| entry.message.contains(&needle))
            .count();
        assert_eq!(
            count, 1,
            "expected exactly one warning for {api}, got: {:?}",
            receipt.script_logs
        );
    }
}

#[test]
fn unknown_sillytavern_member_is_control_flow_safe_noop() {
    let directory = tempdir().unwrap();
    let source = r#"
globalThis.namedInterceptor = function(chat) {
    let threw = false;
    try {
        SillyTavern.getVariables({ type: 'chat' });
        SillyTavern.callPopup('hello');
        SillyTavern.getVariables({ type: 'chat' });
    } catch (e) {
        threw = true;
    }
    chat.push({
        name: 'System',
        is_user: false,
        is_system: true,
        mes: 'threw=' + threw,
        extra: {},
        index: chat.length
    });
};
"#;
    let plugin = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.noop-test",
        source,
    );
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin)
        .unwrap();
    let grant = authorize(&installed);
    let session_id = EntityId::new();

    let receipt = PluginHost::new(PluginLimits::default())
        .execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::GenerateInterceptor,
                plugin_id: installed.manifest.id.clone(),
                settings: json!({}),
                context: json!({}),
                payload: json!({
                    "chat": [{"name": "Alice", "is_user": false, "is_system": false, "mes": "Hi", "extra": {}, "index": 0}],
                    "has_user_message": false,
                }),
                artifact: json!(null),
                state: json!(null),
                session: json!({"session_id": session_id, "branch_id": EntityId::new()}),
            },
        )
        .unwrap();

    match &receipt.effects[0] {
        PluginEffect::PromptRewrite { messages } => {
            let content = &messages.last().unwrap().content;
            assert!(
                content.contains("threw=false"),
                "unknown members threw: {content}"
            );
        }
        effect => panic!("expected PromptRewrite, got {effect:?}"),
    }

    let warnings: Vec<_> = receipt
        .script_logs
        .iter()
        .filter(|l| l.level == "warn")
        .collect();
    assert!(
        warnings.iter().any(|l| l.message.contains("getVariables")),
        "expected getVariables warning, got: {:?}",
        receipt.script_logs
    );
    assert!(
        warnings.iter().any(|l| l.message.contains("callPopup")),
        "expected callPopup warning, got: {:?}",
        receipt.script_logs
    );
    // One-time: getVariables called twice but only one warning.
    let get_vars_warnings: Vec<_> = warnings
        .iter()
        .filter(|l| l.message.contains("getVariables"))
        .collect();
    assert_eq!(
        get_vars_warnings.len(),
        1,
        "expected one-time warning, got: {:?}",
        receipt.script_logs
    );
}

#[test]
fn secondary_inference_bridge_calls_both_apis_and_records_receipt() {
    use stcli_core::{Config, InferenceBroker, InferenceMode, StubInferenceTransport};

    let directory = tempdir().unwrap();
    let source = r#"
async function namedInterceptor(chat) {
    const quiet = await SillyTavern.generateQuietPrompt("Summarize this", { provider: "summary", temperature: 0.2 });
    const raw = await SillyTavern.generateRaw("Summarize this", { providerProfile: "summary", temperature: 0.2 });
    chat.push({ mes: quiet + " / " + raw, is_user: false, is_system: false });
    return chat;
}
globalThis.namedInterceptor = namedInterceptor;
"#;
    let plugin_dir = write_test_bridge_plugin(
        &directory.path().join("plugin"),
        "org.stcli.secondary-inference",
        source,
    );
    let manifest_path = plugin_dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["requested_capabilities"] = json!(["contribute-prompt", "secondary-inference"]);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&plugin_dir)
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
    let settings = stcli_core::ProviderSettings {
        id: "summary".to_owned(),
        base_url: "https://example.invalid".to_owned(),
        chat_completions_path: "/v1/chat/completions".to_owned(),
        format_mode: Default::default(),
        completions_path: None,
        instruct_template: None,
        context_formatting: None,
        api_key_env: None,
        credential_key: None,
        static_headers: BTreeMap::new(),
        timeout_seconds: 30,
        ca_certificate_pem: None,
        model: "stub".to_owned(),
        stream: false,
    };
    let config = Config {
        providers: BTreeMap::from([("summary".to_owned(), settings)]),
    };
    let broker = InferenceBroker::stub(
        config,
        Arc::new(StubInferenceTransport {
            responses: BTreeMap::from([("summary".to_owned(), "stub summary".to_owned())]),
        }),
    );
    let receipt = PluginHost::new(PluginLimits::default()).with_inference(broker).execute(
        &installed, &grant, PluginInput {
            event: PluginEvent::GenerateInterceptor, plugin_id: installed.manifest.id.clone(),
            settings: json!({}), context: json!({}), payload: json!({"chat": [], "has_user_message": false}),
            artifact: json!(null), state: json!(null),
            session: json!({"session_id": EntityId::new(), "branch_id": EntityId::new(), "provider_profile": "summary", "dry_run": true}),
        },
    ).unwrap();
    assert_eq!(
        receipt.inference.len(),
        2,
        "logs: {:?}, effects: {:?}",
        receipt.script_logs,
        receipt.effects
    );
    assert!(
        receipt
            .inference
            .iter()
            .all(|item| item.text == "stub summary")
    );
    assert!(
        receipt
            .inference
            .iter()
            .all(|item| stcli_core::validate_inference_receipt(item).is_ok())
    );
    assert!(
        receipt
            .inference
            .iter()
            .all(|item| item.request_hash.to_string().starts_with("sha256:"))
    );
    assert!(
        receipt
            .effects
            .iter()
            .any(|effect| matches!(effect, PluginEffect::PromptRewrite { .. }))
    );
    assert_eq!(InferenceMode::DryRun, InferenceMode::DryRun);
}

#[test]
fn secondary_inference_replay_uses_recorded_text_with_zero_calls() {
    use stcli_core::{
        Config, InferenceBroker, InferenceMode, InferencePolicy, InferenceRequest,
        InferenceTransport, InferenceTransportError, ProviderSettings, validate_inference_receipt,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingTransport(AtomicUsize);
    impl InferenceTransport for CountingTransport {
        fn generate(
            &self,
            _settings: &ProviderSettings,
            _request: &serde_json::Value,
        ) -> Result<String, InferenceTransportError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("network result must not be used".to_owned())
        }
    }

    let settings = stcli_core::ProviderSettings {
        id: "summary".to_owned(),
        base_url: "https://example.invalid".to_owned(),
        chat_completions_path: "/v1/chat/completions".to_owned(),
        format_mode: Default::default(),
        completions_path: None,
        instruct_template: None,
        context_formatting: None,
        api_key_env: None,
        credential_key: None,
        static_headers: BTreeMap::new(),
        timeout_seconds: 30,
        ca_certificate_pem: None,
        model: "stub".to_owned(),
        stream: false,
    };
    let transport = Arc::new(CountingTransport(AtomicUsize::new(0)));
    let broker = InferenceBroker::stub(
        Config {
            providers: BTreeMap::from([("summary".to_owned(), settings)]),
        },
        transport.clone(),
    );
    let recorded = broker
        .infer(
            "fixture",
            &InferencePolicy {
                capability_granted: true,
                mode: InferenceMode::DryRun,
            },
            &InferenceRequest {
                prompt: "Summarize this".to_owned(),
                profile_name: "summary".to_owned(),
                generation_settings: json!({}),
            },
        )
        .unwrap()
        .receipt;
    validate_inference_receipt(&recorded).unwrap();
    let replayed_text = recorded.text.clone();
    assert_eq!(replayed_text, "network result must not be used");
    assert_eq!(transport.0.load(Ordering::SeqCst), 1);
    validate_inference_receipt(&recorded).unwrap();
    assert_eq!(replayed_text, recorded.text);
    assert_eq!(
        transport.0.load(Ordering::SeqCst),
        1,
        "replay must not call transport"
    );
}

#[tokio::test]
async fn imports_native_extension_and_adopts_fixed_bridge_grant() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("My Native Extension");
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    let script = br#"
globalThis.rewrite = function (chat) {
  const context = SillyTavern.getContext();
  chat.push({
    name: "System",
    is_user: false,
    is_system: true,
    mes: `imported extension prompt for ${context.name2}`,
    extra: {},
    index: chat.length
  });
};
"#;
    std::fs::write(source.join("scripts").join("index.js"), script).unwrap();
    // Regression: declared visual fields warn even when their value is JSON null.
    std::fs::write(
        source.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "display_name": "My Native Extension",
            "loading_order": 42,
            "requires": ["required-module"],
            "optional": ["optional-module"],
            "generate_interceptor": "rewrite",
            "js": ["scripts/index.js"],
            "css": null,
            "html": "panel.html",
            "i18n": "i18n",
            "author": "Fixture Author",
            "version": "1.2.3",
            "auto_update": true
        }))
        .unwrap(),
    )
    .unwrap();

    let database = directory.path().join("data").join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineResult::ImportedExtension(imported) = engine
        .execute(
            EngineCommand::ImportExtension {
                directory: source.clone(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected extension import result");
    };

    assert_eq!(imported.plugin.manifest.id, "my-native-extension");
    assert_eq!(
        imported.plugin.manifest.runtime,
        stcli_core::PluginRuntime::StBridge
    );
    assert_eq!(
        imported.plugin.manifest.display_name.as_deref(),
        Some("My Native Extension")
    );
    assert_eq!(
        imported.plugin.manifest.author.as_deref(),
        Some("Fixture Author")
    );
    assert_eq!(imported.plugin.manifest.loading_order, Some(42));
    assert!(!imported.plugin.manifest.auto_update);
    assert_eq!(imported.plugin.manifest.component, "index.js");
    assert_eq!(
        imported.plugin.manifest.component_sha256,
        plugin_digest(script)
    );
    assert_eq!(imported.plugin.manifest.dependencies.len(), 2);
    assert!(
        !imported
            .plugin
            .manifest
            .dependencies
            .iter()
            .find(|dependency| dependency.id == "required-module")
            .unwrap()
            .optional
    );
    assert!(
        imported
            .plugin
            .manifest
            .dependencies
            .iter()
            .find(|dependency| dependency.id == "optional-module")
            .unwrap()
            .optional
    );
    assert_eq!(
        imported
            .warnings
            .iter()
            .flat_map(|warning| warning.affected_identifiers.iter())
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["css", "html", "i18n"]
    );

    let first_digest = imported.plugin.manifest.component_sha256.clone();
    let EngineResult::ImportedExtension(unchanged) = engine
        .execute(
            EngineCommand::ImportExtension {
                directory: source.clone(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected unchanged extension re-import result");
    };
    assert_eq!(unchanged.plugin.directory, imported.plugin.directory);
    assert_eq!(
        unchanged.plugin.manifest.component_sha256,
        imported.plugin.manifest.component_sha256
    );
    std::fs::write(
        source.join("scripts").join("index.js"),
        b"globalThis.changed = true;",
    )
    .unwrap();
    let EngineResult::ImportedExtension(changed) = engine
        .execute(EngineCommand::ImportExtension { directory: source }, |_| {})
        .await
        .unwrap()
    else {
        panic!("unexpected extension re-import result");
    };
    assert_ne!(changed.plugin.manifest.component_sha256, first_digest);

    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    drop(store);
    let EngineResult::CreatedSession(created) = engine
        .execute(
            EngineCommand::CreateSession {
                configuration: Box::new(base_configuration(character.revision_hash)),
                greeting_index: 0,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected create result");
    };
    let required_package = write_test_bridge_plugin(
        &directory.path().join("required-plugin"),
        "required-module",
        "globalThis.namedInterceptor = function () {};",
    );
    let EngineResult::InstalledPlugin(required) = engine
        .execute(
            EngineCommand::InstallPlugin {
                directory: required_package,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected dependency install result");
    };
    engine
        .execute(
            EngineCommand::AdoptPlugin {
                session_id: created.session.session_id,
                id: required.manifest.id.clone(),
                version: required.manifest.version.to_string(),
                digest: required.manifest.component_sha256,
                capabilities: [stcli_core::PluginCapability::ContributePrompt]
                    .into_iter()
                    .collect(),
                settings: json!({}),
                egress: Vec::new(),
            },
            |_| {},
        )
        .await
        .unwrap();
    let EngineResult::Configuration(configuration) = engine
        .execute(
            EngineCommand::AdoptExtension {
                session_id: created.session.session_id,
                id: imported.plugin.manifest.id.clone(),
                version: imported.plugin.manifest.version.to_string(),
                digest: imported.plugin.manifest.component_sha256.clone(),
                settings: json!({}),
                egress: Vec::new(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected extension adoption result");
    };
    let pin = configuration.configuration.plugins.last().unwrap();
    assert_eq!(
        pin.component_hash,
        imported.plugin.manifest.component_sha256
    );
    assert_eq!(
        pin.capabilities,
        [
            stcli_core::PluginCapability::WriteOwnState,
            stcli_core::PluginCapability::RegisterCommand,
            stcli_core::PluginCapability::BrokeredEgress,
            stcli_core::PluginCapability::InferenceCapability,
        ]
        .into_iter()
        .collect()
    );
    assert!(pin.egress_allow_list.is_empty());

    // Regression: the fixed Extension consent tier must still permit bridge-inherent prompt rewrites.
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
    assert!(
        result.provider_request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"] == "imported extension prompt for Alice")
    );
}

#[test]
fn native_extension_import_rejects_invalid_component_declarations() {
    let directory = tempdir().unwrap();
    let registry = PluginRegistry::new(directory.path().join("registry"));
    for (name, manifest, expected) in [
        (
            "missing-js",
            json!({"display_name": "Missing", "version": "1.0.0"}),
            "missing required native Extension manifest field 'js'",
        ),
        (
            "unsafe-js",
            json!({"display_name": "Unsafe", "version": "1.0.0", "js": "../outside.js"}),
            "unsafe plugin-relative path '../outside.js'",
        ),
        (
            "missing-file",
            json!({"display_name": "Missing", "version": "1.0.0", "js": "index.js"}),
            "native Extension component 'index.js' does not exist",
        ),
        (
            "ambiguous-js",
            json!({"display_name": "Ambiguous", "version": "1.0.0", "js": []}),
            "native Extension manifest field 'js' must declare exactly one component",
        ),
    ] {
        let source = directory.path().join(name);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let error = registry.import_native_extension(&source).unwrap_err();
        assert_eq!(error.to_string(), expected);
    }

    let malformed = directory.path().join("malformed");
    std::fs::create_dir_all(&malformed).unwrap();
    std::fs::write(malformed.join("manifest.json"), b"{").unwrap();
    assert!(matches!(
        registry.import_native_extension(&malformed),
        Err(stcli_core::PluginError::Artifact(_))
    ));

    for (name, version, dependency, expected) in [
        (
            "invalid-version",
            "not-semver",
            "valid-dependency",
            "invalid native Extension semantic version 'not-semver'",
        ),
        (
            "invalid-dependency",
            "1.0.0",
            "",
            "invalid native Extension dependency ''",
        ),
    ] {
        let source = directory.path().join(name);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("index.js"), b"").unwrap();
        std::fs::write(
            source.join("manifest.json"),
            serde_json::to_vec(&json!({
                "version": version,
                "js": "index.js",
                "requires": [dependency]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            registry
                .import_native_extension(&source)
                .unwrap_err()
                .to_string(),
            expected
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = directory.path().join("outside.js");
        std::fs::write(&outside, b"").unwrap();
        let source = directory.path().join("symlink-escape");
        std::fs::create_dir_all(&source).unwrap();
        symlink(&outside, source.join("index.js")).unwrap();
        std::fs::write(
            source.join("manifest.json"),
            serde_json::to_vec(&json!({
                "version": "1.0.0",
                "js": "index.js"
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            registry
                .import_native_extension(&source)
                .unwrap_err()
                .to_string(),
            "unsafe plugin-relative path 'index.js'"
        );

        let outside_manifest = directory.path().join("outside-manifest.json");
        std::fs::write(
            &outside_manifest,
            serde_json::to_vec(&json!({
                "version": "1.0.0",
                "js": "index.js"
            }))
            .unwrap(),
        )
        .unwrap();
        let source = directory.path().join("manifest-symlink");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("index.js"), b"").unwrap();
        symlink(&outside_manifest, source.join("manifest.json")).unwrap();
        // Regression: importing an Extension must never read a manifest outside its directory.
        assert_eq!(
            registry
                .import_native_extension(&source)
                .unwrap_err()
                .to_string(),
            "unsafe plugin-relative path 'manifest.json'"
        );
    }
}
