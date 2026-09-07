use std::{path::PathBuf, time::Duration};

use serde_json::json;
use stcli_core::{
    CapsuleKind, PluginCapability, PluginDependency, PluginError, PluginEvent, PluginGrant,
    PluginHost, PluginInput, PluginLimits, PluginPin, PluginRegistry, SessionConfiguration, Store,
    TurnError, order_plugins,
};
use stcli_testkit::{configuration as base_configuration, fixtures};
use tempfile::tempdir;

fn proof_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/proof")
}
#[test]
fn declared_interaction_schema_cannot_escape_its_package() {
    use std::fs;

    let directory = tempdir().unwrap();
    let package = directory.path().join("package");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("component.wasm"), b"component").unwrap();
    fs::write(directory.path().join("outside.json"), b"{}").unwrap();
    let digest = stcli_core::plugin_digest(b"component");
    fs::write(
        package.join("manifest.json"),
        serde_json::to_vec(&json!({
            "schema": "stcli.plugin-manifest/v1",
            "id": "escape-proof",
            "version": "1.0.0",
            "engine": ">=0.1.0, <0.2.0",
            "runtime": "wasm",
            "component": "component.wasm",
            "component_sha256": digest,
            "dependencies": [],
            "license": "MIT",
            "subscriptions": [],
            "prompt_slots": [],
            "commands": [],
            "macros": [],
            "settings_schema": "../outside.json",
            "requested_capabilities": []
        }))
        .unwrap(),
    )
    .unwrap();

    let error = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&package)
        .unwrap_err();
    assert!(matches!(error, PluginError::UnsafePath(path) if path == "../outside.json"));
}
#[cfg(feature = "scripting")]
#[test]
fn declared_interaction_projects_pins_without_losing_runtime_state() {
    use stcli_core::{PluginRuntime, StateKey, VariableScope};

    let package = PluginRegistry::new(tempdir().unwrap().path())
        .doctor(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../extensions/memory"))
        .unwrap();
    assert_eq!(package.manifest.runtime, PluginRuntime::StBridge);
    let grant = PluginGrant {
        id: package.manifest.id.clone(),
        version: package.manifest.version.clone(),
        component_sha256: package.manifest.component_sha256.clone(),
        capabilities: package.manifest.requested_capabilities.clone(),
        settings: json!({"promptWords": 123}),
        egress_allow_list: Vec::new(),
        enabled: true,
    };
    let receipt = PluginHost::new(Default::default())
        .execute(
            &package,
            &grant,
            PluginInput {
                event: PluginEvent::PrePrompt,
                plugin_id: String::new(),
                settings: json!({}),
                context: json!({}),
                payload: serde_json::Value::Null,
                state: json!({"settings": {"promptWords": 9, "checkpoints": [{"sentinel": true}], "unknown": "kept"}}),
                artifact: serde_json::Value::Null,
                session: json!({"session_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"}),
            },
        )
        .unwrap();
    assert_eq!(receipt.input.state["settings"]["promptWords"], 123);
    assert_eq!(
        receipt.input.state["settings"]["checkpoints"][0]["sentinel"],
        true
    );
    assert_eq!(receipt.input.state["settings"]["unknown"], "kept");
    assert!(receipt.effects.iter().any(|effect| matches!(
        effect,
        stcli_core::PluginEffect::StateWrite {
            key: StateKey { scope: VariableScope::Local, name },
            value,
        } if name == "extension.memory.settings"
            && value["promptWords"] == 123
            && value["checkpoints"][0]["sentinel"] == true
            && value["unknown"] == "kept"
    )));
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
async fn proof_component_contributes_only_granted_recorded_effects() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&proof_directory()).unwrap();
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
    let created = store
        .create_session(configuration(character.revision_hash, pin), 0)
        .unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Use {{proof-greeting}}",
        )
        .unwrap();
    assert_eq!(dry_run.prompt_plan.plugin_receipts.len(), 1);
    assert!(dry_run.prompt_plan.segments.iter().any(|segment| {
        segment.source == "runtime-plugin:proof-note"
            && segment.content == "The proof Plugin is active."
    }));
    assert_eq!(
        dry_run.prompt_plan.messages.last().unwrap().content,
        "Use Hello from Wasm"
    );
    assert!(dry_run.prompt_plan.state_mutations.iter().any(|mutation| {
        mutation.key.name == "org.stcli.proof.invoked"
            && mutation
                .after
                .as_ref()
                .is_some_and(|cell| cell.value == true)
    }));
    assert!(
        store
            .state_transaction(created.session.session_id)
            .unwrap()
            .get(stcli_core::VariableScope::Local, "org.stcli.proof.invoked")
            .is_none()
    );

    store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Use {{proof-greeting}}".to_owned(),
            |_| {},
        )
        .await
        .unwrap_err();
    let turn = store
        .turns_for_branch(created.branch.branch_id)
        .unwrap()
        .pop()
        .unwrap();
    let attempt = store
        .attempts_for_turn(turn.turn_id)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(attempt.effect_receipt.as_ref().unwrap().plugins.len(), 1);
    let capsule = store
        .export_turn_capsule(attempt.attempt_id, CapsuleKind::Portable, false)
        .unwrap();
    let command = store
        .invoke_plugin_command(
            created.session.session_id,
            None,
            "org.stcli.proof",
            "proof-set",
            json!({"value": 1}),
        )
        .unwrap();
    assert_eq!(command.receipt.event, PluginEvent::Command);
    assert_eq!(command.state_mutations.len(), 1);
    assert_eq!(
        store
            .state_transaction(created.session.session_id)
            .unwrap()
            .get(
                stcli_core::VariableScope::Local,
                "org.stcli.proof.command-value"
            )
            .unwrap()
            .value,
        "set by command"
    );
    assert!(
        store
            .trace_events(Some(created.session.session_id))
            .unwrap()
            .iter()
            .any(|event| event.event_type == "plugin.command")
    );

    let projection = store.session(created.session.session_id).unwrap().unwrap();
    let mut configuration = store
        .configuration(&projection.current_config_hash)
        .unwrap()
        .unwrap()
        .configuration;
    configuration.plugins[0]
        .capabilities
        .remove(&PluginCapability::RegisterCommand);
    store
        .update_session_configuration(created.session.session_id, configuration)
        .unwrap();
    assert!(matches!(
        store.invoke_plugin_command(
            created.session.session_id,
            None,
            "org.stcli.proof",
            "proof-set",
            json!(null)
        ),
        Err(TurnError::Plugin(PluginError::CapabilityDenied(
            PluginCapability::RegisterCommand
        )))
    ));

    let replay = store.replay_turn_capsule(&capsule).unwrap();
    assert_eq!(replay.provider_calls, 0);
    assert_eq!(replay.plugin_executions, 0);
}

#[test]
fn denied_effect_fails_before_attempt_creation() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&proof_directory()).unwrap();
    let mut store = Store::open(data.join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let pin = PluginPin {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256,
        capabilities: [
            PluginCapability::RegisterMacro,
            PluginCapability::RegisterCommand,
            PluginCapability::ContributePrompt,
        ]
        .into_iter()
        .collect(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    };
    let created = store
        .create_session(configuration(character.revision_hash, pin), 0)
        .unwrap();

    let error = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Plugin(PluginError::CapabilityDenied(
            PluginCapability::WriteOwnState
        ))
    ));
    assert!(
        store
            .turns_for_branch(created.branch.branch_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dependency_cycles_and_digest_tampering_are_rejected() {
    let directory = tempdir().unwrap();
    let registry = PluginRegistry::new(directory.path().join("plugins"));
    let first = registry.doctor(&proof_directory()).unwrap();
    let mut second = first.clone();
    second.manifest.id = "org.stcli.second".to_owned();
    second.manifest.after.insert(first.manifest.id.clone());
    let mut first = first;
    first.manifest.after.insert(second.manifest.id.clone());
    assert!(matches!(
        order_plugins(&[first, second]),
        Err(PluginError::DependencyCycle)
    ));

    let mut later = registry.doctor(&proof_directory()).unwrap();
    later.manifest.id = "later".to_owned();
    later.manifest.loading_order = Some(20);
    later.manifest.dependencies = vec![PluginDependency {
        id: "not-installed".to_owned(),
        version: semver::VersionReq::STAR,
        optional: true,
    }];
    let mut earlier = later.clone();
    earlier.manifest.id = "earlier".to_owned();
    earlier.manifest.loading_order = Some(10);
    earlier.manifest.dependencies.clear();
    let ordered = order_plugins(&[later, earlier]).unwrap();
    assert_eq!(
        ordered
            .iter()
            .map(|plugin| plugin.manifest.id.as_str())
            .collect::<Vec<_>>(),
        ["earlier", "later"]
    );

    let copied = directory.path().join("tampered");
    std::fs::create_dir_all(&copied).unwrap();
    std::fs::copy(
        proof_directory().join("manifest.json"),
        copied.join("manifest.json"),
    )
    .unwrap();
    std::fs::write(copied.join("component.wasm"), b"not a component").unwrap();
    assert!(matches!(
        registry.doctor(&copied),
        Err(PluginError::DigestMismatch)
    ));
}

fn execute_mode(
    installed: &stcli_core::InstalledPlugin,
    mode: &str,
    limits: PluginLimits,
    event: PluginEvent,
) -> Result<stcli_core::PluginReceipt, PluginError> {
    let grant = PluginGrant {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.clone(),
        component_sha256: installed.manifest.component_sha256.clone(),
        capabilities: installed.manifest.requested_capabilities.clone(),
        settings: json!({"mode": mode}),
        egress_allow_list: Vec::new(),
        enabled: true,
    };
    PluginHost::new(limits).execute(
        installed,
        &grant,
        PluginInput {
            event,
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

#[test]
fn wasm_receipts_keep_the_legacy_input_shape() {
    // Regression test: ScriptHost state snapshots must not alter Wasm receipt bytes.
    let directory = tempdir().unwrap();
    let installed = PluginRegistry::new(directory.path().join("registry"))
        .doctor(&proof_directory())
        .unwrap();
    let receipt = execute_mode(
        &installed,
        "",
        PluginLimits::default(),
        PluginEvent::PrePrompt,
    )
    .unwrap();

    assert!(
        serde_json::to_value(receipt.input)
            .unwrap()
            .get("state")
            .is_none()
    );
}

#[test]
fn engine_state_abort_failure_and_resource_boundaries_are_enforced() {
    let directory = tempdir().unwrap();
    let registry = PluginRegistry::new(directory.path().join("registry"));
    let installed = registry.doctor(&proof_directory()).unwrap();

    assert!(matches!(
        execute_mode(
            &installed,
            "wrong-state",
            PluginLimits::default(),
            PluginEvent::PrePrompt
        ),
        Err(PluginError::StateScopeDenied)
    ));
    assert!(matches!(
        execute_mode(
            &installed,
            "abort",
            PluginLimits::default(),
            PluginEvent::PrePrompt
        ),
        Err(PluginError::AbortPhaseDenied)
    ));
    assert!(matches!(
        execute_mode(
            &installed,
            "failure",
            PluginLimits::default(),
            PluginEvent::PrePrompt
        ),
        Err(PluginError::Guest(_))
    ));

    let limits = PluginLimits {
        component_bytes: 1,
        ..PluginLimits::default()
    };
    assert!(matches!(
        execute_mode(&installed, "", limits, PluginEvent::PrePrompt),
        Err(PluginError::ComponentLimit)
    ));
    let limits = PluginLimits {
        input_bytes: 1,
        ..PluginLimits::default()
    };
    assert!(matches!(
        execute_mode(&installed, "", limits, PluginEvent::PrePrompt),
        Err(PluginError::InputLimit)
    ));
    let limits = PluginLimits {
        output_bytes: 100,
        ..PluginLimits::default()
    };
    assert!(matches!(
        execute_mode(&installed, "huge-output", limits, PluginEvent::PrePrompt),
        Err(PluginError::OutputLimit)
    ));
    let limits = PluginLimits {
        fuel: 1,
        ..PluginLimits::default()
    };
    assert!(matches!(
        execute_mode(&installed, "", limits, PluginEvent::PrePrompt),
        Err(PluginError::Wasmtime(_))
    ));
    let limits = PluginLimits {
        fuel: u64::MAX,
        timeout: Duration::from_millis(1),
        ..PluginLimits::default()
    };
    assert!(matches!(
        execute_mode(&installed, "spin", limits, PluginEvent::PrePrompt),
        Err(PluginError::Wasmtime(_))
    ));

    let incompatible = directory.path().join("incompatible");
    std::fs::create_dir_all(&incompatible).unwrap();
    std::fs::copy(
        proof_directory().join("component.wasm"),
        incompatible.join("component.wasm"),
    )
    .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(proof_directory().join("manifest.json")).unwrap())
            .unwrap();
    manifest["engine"] = json!(">=99.0.0");
    std::fs::write(
        incompatible.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        registry.doctor(&incompatible),
        Err(PluginError::EngineVersion)
    ));
}

#[test]
fn disabled_and_removed_plugins_do_not_execute() {
    let directory = tempdir().unwrap();
    let data = directory.path().join("data");
    let registry = PluginRegistry::new(data.join("plugins"));
    let installed = registry.install(&proof_directory()).unwrap();
    let mut store = Store::open(data.join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let pin = PluginPin {
        id: installed.manifest.id.clone(),
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256,
        capabilities: installed.manifest.requested_capabilities,
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: false,
    };
    let created = store
        .create_session(configuration(character.revision_hash, pin), 0)
        .unwrap();
    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();
    assert!(dry_run.prompt_plan.plugin_receipts.is_empty());
    assert!(
        store
            .invoke_plugin_command(
                created.session.session_id,
                None,
                "org.stcli.proof",
                "proof-set",
                json!(null)
            )
            .is_err()
    );

    let empty_registry = PluginRegistry::new(directory.path().join("removal-registry"));
    empty_registry.install(&proof_directory()).unwrap();
    assert!(empty_registry.remove("org.stcli.proof").unwrap());
    assert!(empty_registry.list().unwrap().is_empty());
}
