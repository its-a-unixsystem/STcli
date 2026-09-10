use std::{
    collections::BTreeSet,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use stcli_core::{
    ARTIFACT_CODEC_INTERFACE_VERSION, ArtifactCodecError, ArtifactKind, EngineCommand,
    EngineInspection, EngineQuery, EngineResult, PluginCapability, StcliEngine, Store,
    plugin_digest,
};
use tempfile::tempdir;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

fn codec_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/ccv3-codec")
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

fn charx() -> Vec<u8> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    archive.start_file("card.json", options).unwrap();
    archive.write_all(&character_card_v3()).unwrap();
    archive.start_file("assets/avatar.png", options).unwrap();
    archive
        .write_all(b"\x89PNG\r\n\x1a\ncodec fixture")
        .unwrap();
    archive.finish().unwrap().into_inner()
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
async fn wasm_codec_imports_and_exports_ccv3_with_recorded_provenance() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    install_and_register(&engine, &codec_directory()).await;
    let source = charx();

    let EngineResult::ArtifactBundle {
        primary,
        supplementary_artifacts,
        asset_count,
    } = engine
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

    assert_eq!(primary.kind, ArtifactKind::CharacterCardV3);
    assert_eq!(primary.source_format, "json");
    assert!(supplementary_artifacts.is_empty());
    assert_eq!(asset_count, 1);

    let EngineInspection::ArtifactCodecProvenance(Some(provenance)) = engine
        .inspect(EngineQuery::ArtifactCodecProvenance {
            revision_hash: primary.revision_hash.clone(),
        })
        .unwrap()
    else {
        panic!("codec provenance was not recorded");
    };
    assert_eq!(provenance.plugin_id, "org.stcli.ccv3-codec");
    assert_eq!(
        provenance.interface_version,
        ARTIFACT_CODEC_INTERFACE_VERSION
    );
    assert_eq!(provenance.format, "charx");
    assert_eq!(
        provenance
            .compatibility
            .iter()
            .map(|item| item.code.as_str())
            .collect::<Vec<_>>(),
        ["ccv3-charx", "ccv3-decoded"]
    );

    let EngineInspection::ArtifactSource(exported) = engine
        .inspect(EngineQuery::ArtifactSource {
            revision_hash: primary.revision_hash.clone(),
        })
        .unwrap()
    else {
        panic!("unexpected artifact export result");
    };
    let mut archive = ZipArchive::new(Cursor::new(exported)).unwrap();
    let mut card = Vec::new();
    archive
        .by_name("card.json")
        .unwrap()
        .read_to_end(&mut card)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&card).unwrap()["data"]["name"],
        "Codec fixture"
    );
    let mut avatar = Vec::new();
    archive
        .by_name("assets/avatar.png")
        .unwrap()
        .read_to_end(&mut avatar)
        .unwrap();
    assert_eq!(avatar, b"\x89PNG\r\n\x1a\ncodec fixture");

    engine
        .execute(
            EngineCommand::RemovePlugin {
                plugin_id: provenance.plugin_id,
            },
            |_| {},
        )
        .await
        .unwrap();
    let EngineInspection::Artifact(stored) = engine
        .inspect(EngineQuery::Artifact {
            revision_hash: primary.revision_hash,
        })
        .unwrap()
    else {
        panic!("unexpected artifact inspection");
    };
    assert_eq!(stored.kind, ArtifactKind::CharacterCardV3);
}

#[tokio::test]
async fn import_keeps_core_fallbacks_without_a_codec_and_above_the_codec_limit() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let source = preset(0);
    let engine = StcliEngine::new(&database);

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

    install_and_register(&engine, &codec_directory()).await;
    let oversized = preset(2 * 1024 * 1024);
    let EngineResult::ArtifactBundle { primary, .. } = engine
        .execute(EngineCommand::ImportArtifact { source: oversized }, |_| {})
        .await
        .unwrap()
    else {
        panic!("unexpected oversized artifact import result");
    };
    assert_eq!(primary.kind, ArtifactKind::ChatCompletionPreset);
}

#[tokio::test]
async fn codec_import_does_not_mutate_an_existing_flat_revision() {
    // Regression test: codec provenance and assets must not attach to an existing revision.
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let payload = character_card_v3();
    let existing = Store::open(&database)
        .unwrap()
        .import_artifact(&payload)
        .unwrap();
    install_and_register(&engine, &codec_directory()).await;

    let error = engine
        .execute(EngineCommand::ImportArtifact { source: charx() }, |_| {})
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        stcli_core::EngineError::ArtifactCodec(ArtifactCodecError::RevisionConflict(hash))
            if hash == existing.revision_hash
    ));
    let store = Store::open(&database).unwrap();
    assert_eq!(
        store.export_artifact(&existing.revision_hash).unwrap(),
        payload
    );
    assert!(
        store
            .artifact_codec_provenance(&existing.revision_hash)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .asset_references("artifact-revision", &existing.revision_hash.to_string())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn codec_registration_rejects_non_wasm_and_additional_capabilities() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let plugin = directory.path().join("script-codec");
    fs::create_dir(&plugin).unwrap();
    let script = "function inspectArtifact() { stcli.output({}); }";
    fs::write(plugin.join("script.js"), script).unwrap();
    fs::write(
        plugin.join("manifest.json"),
        serde_json::to_vec(&json!({
            "schema": "stcli.plugin-manifest/v1",
            "id": "org.stcli.script-codec",
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
            "requested_capabilities": [
                "artifact-codec",
                "inspect-artifact"
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let engine = StcliEngine::new(&database);
    let EngineResult::InstalledPlugin(installed) = engine
        .execute(EngineCommand::InstallPlugin { directory: plugin }, |_| {})
        .await
        .unwrap()
    else {
        panic!("unexpected plugin install result");
    };

    let error = engine
        .execute(
            EngineCommand::RegisterArtifactInspector {
                id: installed.manifest.id,
                version: installed.manifest.version.to_string(),
                digest: installed.manifest.component_sha256,
                capabilities: installed.manifest.requested_capabilities,
            },
            |_| {},
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        stcli_core::EngineError::ArtifactCodec(ArtifactCodecError::InvalidPluginContract(_))
    ));

    let wasm_plugin = directory.path().join("wasm-codec");
    fs::create_dir(&wasm_plugin).unwrap();
    fs::copy(
        codec_directory().join("component.wasm"),
        wasm_plugin.join("component.wasm"),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(codec_directory().join("manifest.json")).unwrap())
            .unwrap();
    manifest["requested_capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!("brokered-egress"));
    fs::write(
        wasm_plugin.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let EngineResult::InstalledPlugin(installed) = engine
        .execute(
            EngineCommand::InstallPlugin {
                directory: wasm_plugin,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected plugin install result");
    };
    let error = engine
        .execute(
            EngineCommand::RegisterArtifactInspector {
                id: installed.manifest.id,
                version: installed.manifest.version.to_string(),
                digest: installed.manifest.component_sha256,
                capabilities: installed.manifest.requested_capabilities,
            },
            |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        stcli_core::EngineError::ArtifactCodec(ArtifactCodecError::InvalidPluginContract(_))
    ));
}
