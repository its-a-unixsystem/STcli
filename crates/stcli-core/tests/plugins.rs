use std::{path::PathBuf, time::Duration};

use serde_json::json;
use stcli_core::{
    CapsuleKind, PluginCapability, PluginError, PluginEvent, PluginGrant, PluginHost, PluginInput,
    PluginLimits, PluginPin, PluginRegistry, SessionConfiguration, Store, TurnError, order_plugins,
};
use stcli_testkit::{configuration as base_configuration, fixtures};
use tempfile::tempdir;

fn proof_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/proof")
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
