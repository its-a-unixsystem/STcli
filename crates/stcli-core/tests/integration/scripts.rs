use std::path::{Path, PathBuf};

use serde_json::json;
use stcli_core::{
    PluginCapability, PluginError, PluginEvent, PluginGrant, PluginHost, PluginInput, PluginLimits,
    PluginPin, PluginRegistry, PromptSlot, ScriptLimits, SessionConfiguration, Store,
    VariableScope, plugin_digest,
};
use stcli_testkit::{MockProvider, configuration as base_configuration, fixtures};
use tempfile::tempdir;

const PLUGIN_ID: &str = "org.stcli.script-proof";

fn write_script_plugin(
    directory: &Path,
    script: &str,
    subscriptions: &[&str],
    slots: &[&str],
    capabilities: &[&str],
) -> PathBuf {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join("script.js"), script).unwrap();
    let manifest = json!({
        "schema": "stcli.plugin-manifest/v1",
        "id": PLUGIN_ID,
        "version": "0.1.0",
        "engine": ">=0.1.0, <0.2.0",
        "runtime": "script",
        "component": "script.js",
        "component_sha256": plugin_digest(script.as_bytes()),
        "dependencies": [],
        "license": "MIT",
        "subscriptions": subscriptions,
        "prompt_slots": slots,
        "commands": [],
        "macros": [],
        "settings_schema": null,
        "requested_capabilities": capabilities,
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

fn execute_script(
    directory: &Path,
    script: &str,
    slots: &[&str],
    capabilities: &[&str],
    limits: PluginLimits,
) -> Result<stcli_core::PluginReceipt, PluginError> {
    let plugin = write_script_plugin(directory, script, &["pre-prompt"], slots, capabilities);
    let installed = PluginRegistry::new(directory.join("registry"))
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
    PluginHost::new(limits).execute(
        &installed,
        &grant,
        PluginInput {
            event: PluginEvent::PrePrompt,
            plugin_id: installed.manifest.id.clone(),
            settings: grant.settings.clone(),
            context: json!({}),
            payload: json!(null),
            artifact: json!(null),
            state: json!({}),
            session: json!(null),
        },
    )
}

fn configuration(
    character_revision: stcli_core::ContentHash,
    pin: PluginPin,
) -> SessionConfiguration {
    let mut configuration = base_configuration(character_revision);
    configuration.plugins = vec![pin];
    configuration
}

#[tokio::test]
async fn script_plugin_persists_state_and_injects_prompt_across_turns() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let plugin = write_script_plugin(
        &directory.path().join("plugin"),
        r#"
function prePrompt() {
  var hour = stcli.state.get('hour');
  if (hour === undefined) hour = 8;
  hour = (hour + 1) % 24;
  stcli.state.set('hour', hour);
  stcli.prompt.inject('after-character-definitions', '[Current time: ' + hour + ':00]');
}
"#,
        &["pre-prompt"],
        &["after-character-definitions"],
        &["write-own-state", "contribute-prompt"],
    );
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&plugin).unwrap();
    let mock = MockProvider::spawn(["ok", "ok"]).await.unwrap();
    let mut store = Store::open(data.join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let pin = PluginPin {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    };
    let mut configuration = configuration(character.revision_hash, pin);
    configuration.provider = mock.provider_settings();
    let created = store.create_session(configuration, 0).unwrap();

    let first = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "What time is it?".to_owned(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(
        first
            .attempt
            .require_primary()
            .unwrap()
            .1
            .segments
            .iter()
            .any(|segment| {
                segment.source == "runtime-plugin:org.stcli.script-proof#1"
                    && segment.content == "[Current time: 9:00]"
            })
    );
    assert_eq!(
        store
            .state_transaction(created.session.session_id)
            .unwrap()
            .get(VariableScope::Local, "org.stcli.script-proof.hour")
            .unwrap()
            .value,
        9
    );
    assert_eq!(
        first.attempt.effect_receipt.as_ref().unwrap().plugins.len(),
        1
    );

    let second = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "And now?".to_owned(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(
        second
            .attempt
            .require_primary()
            .unwrap()
            .1
            .segments
            .iter()
            .any(|segment| {
                segment.source == "runtime-plugin:org.stcli.script-proof#1"
                    && segment.content == "[Current time: 10:00]"
            })
    );
    assert_eq!(
        store
            .state_transaction(created.session.session_id)
            .unwrap()
            .get(VariableScope::Local, "org.stcli.script-proof.hour")
            .unwrap()
            .value,
        10
    );
    assert_eq!(
        second
            .attempt
            .effect_receipt
            .as_ref()
            .unwrap()
            .plugins
            .len(),
        1
    );
    mock.shutdown().await;
}

#[test]
fn documented_turn_counter_plugin_executes_as_written() {
    let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/turn-counter");
    let directory = tempdir().unwrap();
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .install(&plugin)
        .unwrap();
    let grant = PluginGrant {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.clone(),
        component_sha256: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({"start": 10}),
        egress_allow_list: Vec::new(),
        enabled: true,
    };
    let run = |state| {
        PluginHost::new(PluginLimits::default())
            .execute(
                &installed,
                &grant,
                PluginInput {
                    event: PluginEvent::PrePrompt,
                    plugin_id: installed.manifest.id.clone(),
                    settings: grant.settings.clone(),
                    context: json!({}),
                    payload: json!(null),
                    artifact: json!(null),
                    state,
                    session: json!(null),
                },
            )
            .unwrap()
    };

    let first = run(json!({}));
    assert_eq!(
        serde_json::to_value(&first.effects).unwrap(),
        json!([
            {
                "effect": "state-write",
                "key": {
                    "scope": "local",
                    "name": "org.stcli.turn-counter.turns"
                },
                "value": 11
            },
            {
                "effect": "prompt",
                "contribution": {
                    "slot": "after-character-definitions",
                    "name": "org.stcli.turn-counter#1",
                    "role": "system",
                    "content": "[Turn 11]",
                    "depth": null,
                    "order": 1,
                    "outlet": null
                }
            }
        ])
    );
    assert_eq!(first.script_logs.len(), 1);
    assert_eq!(first.script_logs[0].level, "info");
    assert_eq!(first.script_logs[0].message, "turn 11");

    let second = run(json!({"turns": 11}));
    assert_eq!(
        serde_json::to_value(&second.effects).unwrap(),
        json!([
            {
                "effect": "state-write",
                "key": {
                    "scope": "local",
                    "name": "org.stcli.turn-counter.turns"
                },
                "value": 12
            },
            {
                "effect": "prompt",
                "contribution": {
                    "slot": "after-character-definitions",
                    "name": "org.stcli.turn-counter#1",
                    "role": "system",
                    "content": "[Turn 12]",
                    "depth": null,
                    "order": 1,
                    "outlet": null
                }
            }
        ])
    );
}

#[test]
fn script_syntax_and_runtime_errors_are_reported() {
    let directory = tempdir().unwrap();
    assert!(matches!(
        execute_script(
            &directory.path().join("syntax"),
            "function (",
            &[],
            &[],
            PluginLimits::default(),
        ),
        Err(PluginError::ScriptTrap { .. })
    ));

    let error = execute_script(
        &directory.path().join("runtime"),
        "function prePrompt(){ throw new Error('boom'); }",
        &[],
        &[],
        PluginLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PluginError::ScriptTrap { message, .. } if message == "boom"
    ));

    assert!(matches!(
        execute_script(
            &directory.path().join("missing"),
            "var loaded = true;",
            &[],
            &[],
            PluginLimits::default(),
        ),
        Err(PluginError::ScriptHookMissing { .. })
    ));
}

#[test]
fn script_step_limit_stops_infinite_loop() {
    let directory = tempdir().unwrap();
    let limits = PluginLimits {
        script: ScriptLimits {
            interrupt_ticks: 1,
            ..ScriptLimits::default()
        },
        ..PluginLimits::default()
    };
    assert!(matches!(
        execute_script(
            directory.path(),
            "function prePrompt(){ while(true){} }",
            &[],
            &[],
            limits,
        ),
        Err(PluginError::ScriptStepLimit)
    ));
}

#[test]
fn script_cannot_reach_host_globals() {
    let directory = tempdir().unwrap();
    let script = r#"
function prePrompt() {
  var names = ['require', 'process', 'fetch', 'Deno', 'XMLHttpRequest', 'eval', 'Date', 'console'];
  for (var i = 0; i < names.length; i++) {
    if (typeof globalThis[names[i]] !== 'undefined') throw new Error(names[i] + ' is reachable');
  }
  if (typeof Math.random !== 'undefined') throw new Error('Math.random is reachable');
}
"#;
    let receipt =
        execute_script(directory.path(), script, &[], &[], PluginLimits::default()).unwrap();
    assert!(receipt.effects.is_empty());
}

#[test]
fn script_state_and_slot_violations_are_denied() {
    let directory = tempdir().unwrap();
    let invalid_state = execute_script(
        &directory.path().join("invalid-state"),
        "function prePrompt(){ stcli.state.set('../escape', 1); }",
        &[],
        &["write-own-state"],
        PluginLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        invalid_state,
        PluginError::ScriptTrap { message, .. } if message.contains("invalid state key")
    ));

    let unknown_slot = execute_script(
        &directory.path().join("unknown-slot"),
        "function prePrompt(){ stcli.prompt.inject('nope', 'x'); }",
        &[],
        &["contribute-prompt"],
        PluginLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        unknown_slot,
        PluginError::ScriptTrap { message, .. } if message.contains("unknown prompt slot")
    ));

    assert!(matches!(
        execute_script(
            &directory.path().join("closed-slot"),
            "function prePrompt(){ stcli.prompt.inject('before-history', 'x'); }",
            &["after-character-definitions"],
            &["contribute-prompt"],
            PluginLimits::default(),
        ),
        Err(PluginError::ClosedPromptSlot(PromptSlot::BeforeHistory))
    ));

    assert!(matches!(
        execute_script(
            &directory.path().join("missing-capability"),
            "function prePrompt(){ stcli.state.set('hour', 1); }",
            &[],
            &[],
            PluginLimits::default(),
        ),
        Err(PluginError::CapabilityDenied(
            PluginCapability::WriteOwnState
        ))
    ));
}
