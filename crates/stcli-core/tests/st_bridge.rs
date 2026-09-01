use std::path::{Path, PathBuf};

use serde_json::json;
use stcli_core::{
    EngineCommand, EngineResult, EntityId, PluginEffect, PluginEvent, PluginGrant, PluginHost,
    PluginInput, PluginLimits, PluginPin, PluginRegistry, StcliEngine, Store, plugin_digest,
};
use stcli_testkit::{configuration as base_configuration, fixtures};
use tempfile::tempdir;

const PLUGIN_ID: &str = "org.stcli.st-bridge-proof";
const SOURCE: &str = r#"
let initCount = 0;
initCount += 1;
let handlerCount = 0;

eventSource.on(event_types.CHAT_COMPLETION_PROMPT_READY, (payload) => {
  handlerCount += 1;
  const context = SillyTavern.getContext();
  try {
    context.name2 = 'Changed';
  } catch {}
  try {
    context.chat[0].content = 'Changed';
  } catch {}
  payload.chat.push({
    role: 'system',
    content: `init=${initCount} handler=${handlerCount} name=${context.name2} chat=${context.chat.length} first=${context.chat[0].content}`,
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
        "subscriptions": ["chat-completion-prompt-ready"],
        "prompt_slots": ["in-chat"],
        "commands": [],
        "macros": [],
        "settings_schema": null,
        "requested_capabilities": ["contribute-prompt", "read-session"],
        "before": [],
        "after": []
    });
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    directory.to_owned()
}

#[test]
fn bridge_context_initializes_once_and_reuses_registered_handler() {
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
    let execute = |name: &str| {
        PluginHost::new(PluginLimits::default())
            .execute(
                &installed,
                &grant,
                PluginInput {
                    event: PluginEvent::ChatCompletionPromptReady,
                    plugin_id: installed.manifest.id.clone(),
                    settings: grant.settings.clone(),
                    context: json!({
                        "name2": name,
                        "chat": [{"role": "assistant", "content": "Welcome."}],
                    }),
                    payload: json!({
                        "chat": [{"role": "user", "content": "Hello"}],
                    }),
                    artifact: json!(null),
                    state: json!(null),
                    session: json!({"session_id": session_id}),
                },
            )
            .unwrap()
    };

    let first = execute("Alice");
    let second = execute("Beatrice");

    let prompt_content = |receipt: &stcli_core::PluginReceipt| match &receipt.effects[..] {
        [PluginEffect::Prompt { contribution }] => contribution.content.clone(),
        effects => panic!("expected one prompt effect, got {effects:?}"),
    };
    assert_eq!(
        prompt_content(&first),
        "init=1 handler=1 name=Alice chat=1 first=Welcome."
    );
    assert_eq!(
        prompt_content(&second),
        "init=1 handler=2 name=Beatrice chat=1 first=Welcome."
    );
}

#[tokio::test]
async fn dry_run_dispatches_bridge_prompt_event_without_committing_a_turn() {
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

    let dry_run = |content: &str| {
        engine.execute(
            EngineCommand::DryRunSend {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: content.to_owned(),
            },
            |_| {},
        )
    };
    let EngineResult::DryRun(first) = dry_run("Hello").await.unwrap() else {
        panic!("unexpected first Dry Run result");
    };
    let first_literal = "init=1 handler=1 name=Alice chat=2 first=Welcome.";
    assert!(first.prompt_plan.segments.iter().any(|segment| {
        segment.source == format!("runtime-plugin:{PLUGIN_ID}#1")
            && segment.slot == "pluginInChat"
            && !segment.pruned
            && segment.content == first_literal
    }));
    assert_eq!(
        first.prompt_plan.messages.last().unwrap().content,
        first_literal
    );
    assert_eq!(
        first.provider_request["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &json!({"role": "system", "content": first_literal})
    );

    let EngineResult::DryRun(second) = dry_run("Again").await.unwrap() else {
        panic!("unexpected second Dry Run result");
    };
    assert_eq!(
        second.prompt_plan.messages.last().unwrap().content,
        "init=1 handler=2 name=Alice chat=2 first=Welcome."
    );

    let store = Store::open(&database).unwrap();
    assert!(
        store
            .turns_for_branch(created.branch.branch_id)
            .unwrap()
            .is_empty()
    );
}
