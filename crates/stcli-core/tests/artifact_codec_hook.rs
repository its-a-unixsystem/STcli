use std::{collections::BTreeSet, fs, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};
use stcli_core::{
    ArtifactKind, EngineCommand, EngineResult, PluginCapability, StcliEngine, Store, plugin_digest,
};
use tempfile::tempdir;

fn write_codec(directory: &Path, source: &[u8], payload: &[u8], asset: &[u8]) {
    fs::create_dir_all(directory).unwrap();
    let script = format!(
        "function inspectArtifact(input) {{\n  if (input.payload.source !== '{}') throw new Error('unexpected source');\n  stcli.output({{ kind: 'charx', payload: '{}', assets: [{{ logical_path: 'assets/avatar.png', bytes: '{}' }}], format: 'json' }});\n}}",
        BASE64.encode(source),
        BASE64.encode(payload),
        BASE64.encode(asset),
    );
    fs::write(directory.join("script.js"), &script).unwrap();
    fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "stcli.plugin-manifest/v1",
            "id": "org.stcli.artifact-codec",
            "version": "1.0.0",
            "engine": ">=0.1.0, <0.2.0",
            "runtime": "script",
            "component": "script.js",
            "component_sha256": plugin_digest(script.as_bytes()),
            "dependencies": [],
            "license": "MIT",
            "subscriptions": ["inspect-artifact"],
            "prompt_slots": [],
            "commands": [],
            "macros": [],
            "settings_schema": null,
            "requested_capabilities": ["artifact-codec", "inspect-artifact"],
        }))
        .unwrap(),
    )
    .unwrap();
}

fn character_card_v3() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "spec": "chara_card_v3",
        "spec_version": "3.0",
        "data": {
            "name": "Codec fixture",
            "description": "",
            "creator": "",
            "character_version": "",
            "mes_example": "",
            "system_prompt": "",
            "post_history_instructions": "",
            "first_mes": "Hello",
            "personality": "",
            "scenario": "",
            "creator_notes": "",
            "tags": [],
            "alternate_greetings": [],
            "group_only_greetings": [],
            "extensions": {},
            "assets": [{
                "type": "icon",
                "uri": "embeded://assets/avatar.png",
                "name": "main",
                "ext": "png"
            }]
        }
    }))
    .unwrap()
}

fn preset(description_bytes: usize) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name": "Core fixture",
        "description": "x".repeat(description_bytes),
        "prompts": [{"identifier": "main", "role": "system", "content": "Stay in character."}],
        "prompt_order": [{"character_id": 100001, "order": [{"identifier": "main", "enabled": true}]}]
    }))
    .unwrap()
}

async fn install_and_register(engine: &StcliEngine, directory: &Path) {
    let EngineResult::InstalledPlugin(installed) = engine
        .execute(
            EngineCommand::InstallPlugin {
                directory: directory.to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected plugin install result");
    };
    engine
        .execute(
            EngineCommand::RegisterArtifactInspector {
                id: installed.manifest.id,
                version: installed.manifest.version.to_string(),
                digest: installed.manifest.component_sha256,
                capabilities: [
                    PluginCapability::ArtifactCodec,
                    PluginCapability::InspectArtifact,
                ]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            },
            |_| {},
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn engine_import_decodes_artifact_through_registered_codec() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let plugin = directory.path().join("codec");
    let source = b"external codec container";
    let payload = character_card_v3();
    let asset = b"\x89PNG\r\n\x1a\ncodec fixture";
    write_codec(&plugin, source, &payload, asset);
    let engine = StcliEngine::new(&database);
    install_and_register(&engine, &plugin).await;

    let EngineResult::ArtifactBundle {
        primary,
        supplementary_artifacts,
        asset_count,
    } = engine
        .execute(
            EngineCommand::ImportArtifact {
                source: source.to_vec(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected artifact import result");
    };

    assert_eq!(primary.kind, ArtifactKind::CharacterCardV3);
    assert_eq!(primary.source_format, "json");
    assert!(supplementary_artifacts.is_empty());
    assert_eq!(asset_count, 1);
    let store = Store::open(&database).unwrap();
    assert_eq!(
        store.export_artifact(&primary.revision_hash).unwrap(),
        payload
    );
    assert_eq!(
        store
            .asset_references("artifact-revision", &primary.revision_hash.to_string())
            .unwrap()[0]
            .logical_path,
        "assets/avatar.png"
    );
}

#[tokio::test]
async fn engine_import_uses_core_decoder_without_registered_codec() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let source = preset(0);

    let EngineResult::ArtifactBundle { primary, .. } = StcliEngine::new(&database)
        .execute(
            EngineCommand::ImportArtifact {
                source: source.clone(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected artifact import result");
    };

    assert_eq!(primary.kind, ArtifactKind::ChatCompletionPreset);
    assert_eq!(
        Store::open(&database)
            .unwrap()
            .export_artifact(&primary.revision_hash)
            .unwrap(),
        source
    );
}

#[tokio::test]
async fn engine_import_uses_core_decoder_above_codec_size_limit() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let plugin = directory.path().join("codec");
    let source = preset(2 * 1024 * 1024);
    write_codec(
        &plugin,
        b"codec must not receive oversized source",
        &character_card_v3(),
        b"\x89PNG\r\n\x1a\ncodec fixture",
    );
    let engine = StcliEngine::new(&database);
    install_and_register(&engine, &plugin).await;

    let EngineResult::ArtifactBundle { primary, .. } = engine
        .execute(
            EngineCommand::ImportArtifact {
                source: source.clone(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected artifact import result");
    };

    assert_eq!(primary.kind, ArtifactKind::ChatCompletionPreset);
    let exported = Store::open(&database)
        .unwrap()
        .export_artifact(&primary.revision_hash)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&exported).unwrap()["name"],
        "Core fixture"
    );
}
