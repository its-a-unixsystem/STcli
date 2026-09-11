use std::{collections::BTreeSet, fs, path::Path};

use serde_json::json;
use stcli_core::{
    EngineCommand, EngineError, EngineInspection, EngineQuery, PluginCapability, PluginError,
    StcliEngine, Store, plugin_digest,
};
use tempfile::tempdir;

fn nemo_directory() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/nemo-directives")
}

fn write_plugin(directory: &Path, id: &str, script: &str, capabilities: &[&str]) {
    fs::create_dir_all(directory).unwrap();
    fs::write(directory.join("script.js"), script).unwrap();
    let digest = plugin_digest(script.as_bytes());
    fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "stcli.plugin-manifest/v1",
            "id": id,
            "version": "1.0.0",
            "engine": ">=0.1.0, <0.2.0",
            "runtime": "script",
            "component": "script.js",
            "component_sha256": digest,
            "dependencies": [],
            "license": "MIT",
            "subscriptions": ["inspect-artifact"],
            "prompt_slots": [],
            "commands": [],
            "macros": [],
            "settings_schema": null,
            "requested_capabilities": capabilities,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn preset() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name": "Inspection fixture",
        "prompts": [{"identifier": "main", "role": "system", "content": "Stay in character."}],
        "prompt_order": [{"character_id": 100001, "order": [{"identifier": "main", "enabled": true}]}]
    }))
    .unwrap()
}

async fn install_and_register(
    engine: &StcliEngine,
    directory: &Path,
    capabilities: BTreeSet<PluginCapability>,
) {
    let installed = match engine
        .execute(
            EngineCommand::InstallPlugin {
                directory: directory.to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    {
        stcli_core::EngineResult::InstalledPlugin(installed) => installed,
        result => panic!("unexpected install result: {result:?}"),
    };
    engine
        .execute(
            EngineCommand::RegisterArtifactInspector {
                id: installed.manifest.id,
                version: installed.manifest.version.to_string(),
                digest: installed.manifest.component_sha256,
                capabilities,
            },
            |_| {},
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn registered_artifact_inspector_returns_typed_output_without_trace_receipts() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let plugin = directory.path().join("inspector");
    write_plugin(
        &plugin,
        "org.stcli.inspector",
        "function inspectArtifact(input) { stcli.output({ preset: input.artifact.name, promptCount: input.artifact.prompts.length }); }",
        &["inspect-artifact"],
    );
    let engine = StcliEngine::new(&database);
    install_and_register(
        &engine,
        &plugin,
        [PluginCapability::InspectArtifact].into_iter().collect(),
    )
    .await;
    let revision = Store::open(&database)
        .unwrap()
        .import_artifact(&preset())
        .unwrap()
        .revision_hash;
    let trace_count = Store::open(&database)
        .unwrap()
        .trace_events(None)
        .unwrap()
        .len();

    let inspection = engine
        .inspect(EngineQuery::InspectArtifactWithPlugin {
            plugin_id: "org.stcli.inspector".to_owned(),
            revision_hash: revision.clone(),
        })
        .unwrap();

    let EngineInspection::PluginArtifactOutput(output) = inspection else {
        panic!("unexpected inspection result");
    };
    assert_eq!(output.plugin_id, "org.stcli.inspector");
    assert_eq!(output.revision_hash, revision);
    assert_eq!(
        output.value,
        json!({"preset": "Inspection fixture", "promptCount": 1})
    );
    assert_eq!(
        Store::open(&database)
            .unwrap()
            .trace_events(None)
            .unwrap()
            .len(),
        trace_count
    );
}

#[tokio::test]
async fn artifact_inspection_requires_registration_and_capability() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let plugin = directory.path().join("inspector");
    write_plugin(
        &plugin,
        "org.stcli.inspector",
        "function inspectArtifact(input) { stcli.output(input.artifact.name); }",
        &["inspect-artifact"],
    );
    let engine = StcliEngine::new(&database);
    let installed = match engine
        .execute(
            EngineCommand::InstallPlugin {
                directory: plugin.clone(),
            },
            |_| {},
        )
        .await
        .unwrap()
    {
        stcli_core::EngineResult::InstalledPlugin(installed) => installed,
        result => panic!("unexpected install result: {result:?}"),
    };
    let revision = Store::open(&database)
        .unwrap()
        .import_artifact(&preset())
        .unwrap()
        .revision_hash;

    assert!(matches!(
        engine.inspect(EngineQuery::InspectArtifactWithPlugin {
            plugin_id: installed.manifest.id.clone(),
            revision_hash: revision.clone(),
        }),
        Err(EngineError::ArtifactInspectorNotRegistered(id)) if id == installed.manifest.id
    ));

    engine
        .execute(
            EngineCommand::RegisterArtifactInspector {
                id: installed.manifest.id.clone(),
                version: installed.manifest.version.to_string(),
                digest: installed.manifest.component_sha256,
                capabilities: BTreeSet::new(),
            },
            |_| {},
        )
        .await
        .unwrap();
    assert!(matches!(
        engine.inspect(EngineQuery::InspectArtifactWithPlugin {
            plugin_id: installed.manifest.id,
            revision_hash: revision,
        }),
        Err(EngineError::Plugin(PluginError::CapabilityDenied(
            PluginCapability::InspectArtifact
        )))
    ));
}

#[tokio::test]
async fn artifact_inspection_rejects_mutation_effects() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let plugin = directory.path().join("mutating-inspector");
    write_plugin(
        &plugin,
        "org.stcli.mutating-inspector",
        "function inspectArtifact(input) { stcli.state.set('changed', true); stcli.output(input.artifact.name); }",
        &["inspect-artifact", "write-own-state"],
    );
    let engine = StcliEngine::new(&database);
    install_and_register(
        &engine,
        &plugin,
        [
            PluginCapability::InspectArtifact,
            PluginCapability::WriteOwnState,
        ]
        .into_iter()
        .collect(),
    )
    .await;
    let revision = Store::open(&database)
        .unwrap()
        .import_artifact(&preset())
        .unwrap()
        .revision_hash;

    assert!(matches!(
        engine.inspect(EngineQuery::InspectArtifactWithPlugin {
            plugin_id: "org.stcli.mutating-inspector".to_owned(),
            revision_hash: revision,
        }),
        Err(EngineError::Plugin(
            PluginError::ArtifactInspectionMutationDenied
        ))
    ));
}

#[tokio::test]
async fn nemo_directives_return_constraints_and_diagnostics_without_mutating_artifact() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    install_and_register(
        &engine,
        &nemo_directory(),
        [PluginCapability::InspectArtifact].into_iter().collect(),
    )
    .await;
    let source = serde_json::to_vec(&json!({
        "name": "Nemo fixture",
        "prompts": [
            {"identifier": "short", "name": "01. Short Style", "role": "system", "content": "{{// @mutual-exclusive-group style\n@warning Keep this short. }}"},
            {"identifier": "long", "name": "02. Long Style", "role": "system", "content": "{{// @exclusive-with-category style\n@conflicts-with 01. Short Style }}"},
            {"identifier": "plain", "name": "03. Plain", "role": "system", "content": "{{// @exclusive-with 02. Long Style\n@category format\n@max-one-per-category format\n@unknown ignored }}"},
            {"identifier": "rich", "name": "04. Rich", "role": "system", "content": "{{// @category format\n@deprecated Use Plain.\n@exclusive-with missing }}"}
        ],
        "prompt_order": [{"character_id": 100001, "order": [
            {"identifier": "short", "enabled": true},
            {"identifier": "long", "enabled": false},
            {"identifier": "plain", "enabled": true},
            {"identifier": "rich", "enabled": false}
        ]}]
    }))
    .unwrap();
    let revision = Store::open(&database)
        .unwrap()
        .import_artifact(&source)
        .unwrap()
        .revision_hash;

    let EngineInspection::PluginArtifactOutput(output) = engine
        .inspect(EngineQuery::InspectArtifactWithPlugin {
            plugin_id: "org.stcli.nemo-directives".to_owned(),
            revision_hash: revision.clone(),
        })
        .unwrap()
    else {
        panic!("unexpected inspection result");
    };
    let constraints = output.value["constraints"].as_array().unwrap();
    assert!(constraints.iter().any(|constraint| {
        constraint["kind"] == "named-group" && constraint["members"] == json!(["short", "long"])
    }));
    assert!(constraints.iter().any(|constraint| {
        constraint["kind"] == "exclusive-pair" && constraint["members"] == json!(["plain", "long"])
    }));
    assert!(constraints.iter().any(|constraint| {
        constraint["kind"] == "category-limit" && constraint["members"] == json!(["plain", "rich"])
    }));
    let diagnostics = output.value["diagnostics"].as_array().unwrap();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["identifier"] == "rich" && diagnostic["kind"] == "unresolved-reference"
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["identifier"] == "short" && diagnostic["kind"] == "warning"
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic["identifier"] == "rich" && diagnostic["kind"] == "deprecated"
    }));
    assert_eq!(
        Store::open(&database)
            .unwrap()
            .export_artifact(&revision)
            .unwrap(),
        source
    );
}
