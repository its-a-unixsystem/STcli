use std::{
    collections::BTreeSet,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::{Compression, write::ZlibEncoder};

use serde_json::{Value, json};
use stcli_core::{
    ARTIFACT_CODEC_INTERFACE_VERSION, ArtifactCodecError, ArtifactKind, EngineCommand,
    EngineInspection, EngineQuery, EngineResult, PluginCapability, StcliEngine, Store,
    plugin_digest,
};
use stcli_testkit::configuration;
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
fn charx_with_lorebook() -> Vec<u8> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    archive.start_file("card.json", options).unwrap();
    archive.write_all(&character_card_v3()).unwrap();
    archive
        .start_file("lorebooks/world/lorebook.json", options)
        .unwrap();
    archive
        .write_all(br#"{"entries":{"0":{"key":["harbor"],"content":"A quiet harbor."}}}"#)
        .unwrap();
    archive.start_file("assets/avatar.png", options).unwrap();
    archive
        .write_all(b"\x89PNG\r\n\x1a\ncodec fixture")
        .unwrap();
    archive.finish().unwrap().into_inner()
}
fn charx_with_identical_embedded_and_file_lorebooks() -> Vec<u8> {
    let lorebook: Value = serde_json::from_slice(&lorebook()).unwrap();
    let mut card: Value = serde_json::from_slice(&character_card_v3()).unwrap();
    card["data"]["character_book"] = lorebook.clone();
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    archive.start_file("card.json", options).unwrap();
    archive
        .write_all(&serde_json::to_vec(&card).unwrap())
        .unwrap();
    archive
        .start_file("lorebooks/world/lorebook.json", options)
        .unwrap();
    archive
        .write_all(&serde_json::to_vec(&lorebook).unwrap())
        .unwrap();
    archive.start_file("assets/avatar.png", options).unwrap();
    archive
        .write_all(b"\x89PNG\r\n\x1a\ncodec fixture")
        .unwrap();
    archive.finish().unwrap().into_inner()
}
fn charx_with_invalid_asset() -> Vec<u8> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    archive.start_file("card.json", options).unwrap();
    archive.write_all(&character_card_v3()).unwrap();
    archive.start_file("assets/avatar.png", options).unwrap();
    archive.write_all(b"not an image").unwrap();
    archive.finish().unwrap().into_inner()
}

fn append_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(kind);
    hasher.update(data);
    png.extend_from_slice(&hasher.finalize().to_be_bytes());
}

fn png_card(animated: bool) -> Vec<u8> {
    let mut metadata = b"chara\0".to_vec();
    metadata.extend_from_slice(STANDARD.encode(character_card_v3()).as_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    append_png_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    if animated {
        append_png_chunk(&mut png, b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]);
    }
    append_png_chunk(&mut png, b"tEXt", &metadata);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&[0, 0, 0, 0, 0]).unwrap();
    append_png_chunk(&mut png, b"IDAT", &encoder.finish().unwrap());
    append_png_chunk(&mut png, b"IEND", &[]);
    png
}

fn character_card_v1() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name": "V1 fixture",
        "description": "",
        "personality": "",
        "scenario": "",
        "first_mes": "Hello",
        "mes_example": ""
    }))
    .unwrap()
}

fn character_card_v2() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "data": {
            "name": "V2 fixture",
            "description": "",
            "personality": "",
            "scenario": "",
            "first_mes": "Hello",
            "mes_example": "",
            "alternate_greetings": [],
            "extensions": {}
        }
    }))
    .unwrap()
}

fn lorebook() -> Vec<u8> {
    br#"{"entries":{"0":{"key":["harbor"],"content":"A quiet harbor."}}}"#.to_vec()
}

fn expected_flat_bundle(source: &[u8], format: &str) -> (Vec<u8>, usize, usize) {
    match format {
        "json" => (source.to_vec(), 0, 0),
        "png" | "apng" => (character_card_v3(), 0, 1),
        "webp"
            if source == include_bytes!("fixtures/artifacts/card-v2-exif.webp").as_slice() =>
        {
            (
                br#"{"spec":"chara_card_v2","spec_version":"2.0","data":{"name":"iTXt V2","first_mes":"Hello iTXt"}}"#.to_vec(),
                0,
                1,
            )
        }
        "webp"
            if source == include_bytes!("fixtures/artifacts/card-v3-xmp.webp").as_slice() =>
        {
            (
                br#"{"spec":"chara_card_v3","spec_version":"3.0","data":{"name":"PNG V3","first_mes":"Hello V3"}}"#.to_vec(),
                0,
                1,
            )
        }
        "charx" => {
            let mut archive = ZipArchive::new(Cursor::new(source)).unwrap();
            let mut payload = Vec::new();
            archive
                .by_name("card.json")
                .unwrap()
                .read_to_end(&mut payload)
                .unwrap();
            let file_lorebooks = (0..archive.len())
                .filter(|index| {
                    archive
                        .by_index(*index)
                        .unwrap()
                        .name()
                        .starts_with("lorebooks/")
                })
                .count();
            let assets = (0..archive.len())
                .filter(|index| {
                    archive
                        .by_index(*index)
                        .unwrap()
                        .name()
                        .starts_with("assets/")
                })
                .count();
            let embedded = usize::from(
                serde_json::from_slice::<Value>(&payload)
                    .unwrap()
                    .pointer("/data/character_book")
                    .is_some(),
            );
            (payload, file_lorebooks + embedded, assets)
        }
        _ => panic!("missing expected flat bundle for {format}"),
    }
}

async fn assert_codec_parity(
    source: Vec<u8>,
    expected_format: &str,
    exact_export: bool,
) -> Vec<u8> {
    let (expected_payload, expected_supplementary, expected_assets) =
        expected_flat_bundle(&source, expected_format);
    let expected = stcli_core::decode_artifact(&expected_payload).unwrap();
    let expected_source_format = match expected_format {
        "png" | "apng" => "png",
        "webp" => "webp",
        _ => "json",
    };
    let revision_source = if expected_format == "charx" {
        expected_payload.as_slice()
    } else {
        source.as_slice()
    };

    let codec_directory = tempdir().unwrap();
    let database = codec_directory.path().join("codec.sqlite3");
    let engine = StcliEngine::new(&database);
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
        panic!("unexpected import result");
    };
    assert_eq!(primary.kind, expected.kind);
    assert_eq!(primary.source_format, expected_source_format);
    assert_eq!(
        primary.revision_hash,
        stcli_core::identity::artifact_revision_hash(
            expected.kind.as_str(),
            expected_source_format,
            revision_source,
        )
    );
    assert_eq!(
        primary.semantic_hash,
        stcli_core::artifact_semantic_hash(&expected.semantic).unwrap()
    );
    assert_eq!(
        primary.source_blob_hash,
        stcli_core::content_blob_hash(&expected_payload)
    );
    assert_eq!(supplementary_artifacts.len(), expected_supplementary);
    assert!(supplementary_artifacts.iter().all(
        |artifact| artifact.kind == ArtifactKind::Lorebook && artifact.source_format == "json"
    ));
    assert_eq!(asset_count, expected_assets);
    let EngineInspection::ArtifactCodecProvenance(Some(provenance)) = engine
        .inspect(EngineQuery::ArtifactCodecProvenance {
            revision_hash: primary.revision_hash.clone(),
        })
        .unwrap()
    else {
        panic!("codec provenance was not recorded");
    };
    assert_eq!(provenance.plugin_id, "org.stcli.sillytavern-codec");
    assert_eq!(provenance.format, expected_format);
    assert_eq!(
        provenance.supplementary_artifacts.len(),
        supplementary_artifacts.len()
    );
    let EngineInspection::ArtifactSource(exported) = engine
        .inspect(EngineQuery::ArtifactSource {
            revision_hash: primary.revision_hash,
        })
        .unwrap()
    else {
        panic!("unexpected export result");
    };
    if exact_export {
        assert_eq!(exported, source);
    }
    exported
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

#[tokio::test]
async fn bundled_codec_preserves_json_artifact_parity() {
    assert_codec_parity(character_card_v1(), "json", true).await;
    assert_codec_parity(character_card_v2(), "json", true).await;
    assert_codec_parity(character_card_v3(), "json", true).await;
    assert_codec_parity(lorebook(), "json", true).await;
    assert_codec_parity(preset(0), "json", true).await;
}

#[tokio::test]
async fn bundled_codec_preserves_png_apng_and_webp_parity() {
    assert_codec_parity(png_card(false), "png", true).await;
    assert_codec_parity(png_card(true), "apng", true).await;
    assert_codec_parity(
        include_bytes!("fixtures/artifacts/card-v2-exif.webp").to_vec(),
        "webp",
        true,
    )
    .await;
    assert_codec_parity(
        include_bytes!("fixtures/artifacts/card-v3-xmp.webp").to_vec(),
        "webp",
        true,
    )
    .await;
}

#[tokio::test]
async fn bundled_codec_preserves_charx_assets_and_supplementary_artifacts() {
    let exported = assert_codec_parity(charx_with_lorebook(), "charx", false).await;
    let mut archive = ZipArchive::new(Cursor::new(exported)).unwrap();
    assert!(archive.by_name("card.json").is_ok());
    assert!(archive.by_name("lorebooks/world/lorebook.json").is_ok());
    assert!(archive.by_name("assets/avatar.png").is_ok());
}

#[tokio::test]
async fn charx_allows_identical_embedded_and_file_lorebooks() {
    // Regression: embedded ownership is explicit and does not require globally unique bytes.
    assert_codec_parity(
        charx_with_identical_embedded_and_file_lorebooks(),
        "charx",
        false,
    )
    .await;
}

#[tokio::test]
async fn purge_retains_and_then_removes_codec_supplementary_revisions() {
    // Regression: provenance retains supplementary revisions only while its primary is reachable.
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineResult::ArtifactBundle {
        primary,
        supplementary_artifacts,
        ..
    } = engine
        .execute(
            EngineCommand::ImportArtifact {
                source: charx_with_lorebook(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected import result");
    };
    let supplementary_hashes = supplementary_artifacts
        .iter()
        .map(|artifact| artifact.revision_hash.clone())
        .collect::<Vec<_>>();
    let mut store = Store::open(&database).unwrap();
    let first = store
        .create_session(configuration(primary.revision_hash.clone()), 0)
        .unwrap();
    let second = store
        .create_session(configuration(primary.revision_hash.clone()), 0)
        .unwrap();

    store.purge_session(first.session.session_id).unwrap();
    for hash in &supplementary_hashes {
        assert!(store.artifact(hash).unwrap().is_some());
    }
    store.purge_session(second.session.session_id).unwrap();
    assert!(store.artifact(&primary.revision_hash).unwrap().is_none());
    for hash in &supplementary_hashes {
        assert!(store.artifact(hash).unwrap().is_none());
    }
}

#[tokio::test]
async fn rejected_codec_asset_commits_no_artifact_state() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);

    engine
        .execute(
            EngineCommand::ImportArtifact {
                source: charx_with_invalid_asset(),
            },
            |_| {},
        )
        .await
        .unwrap_err();

    assert!(
        Store::open(&database)
            .unwrap()
            .artifacts()
            .unwrap()
            .is_empty()
    );
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
    assert_eq!(provenance.plugin_id, "org.stcli.sillytavern-codec");
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
        ["sillytavern-charx", "sillytavern-charx-decoded"]
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

    // Regression test for issue #129: imported Artifact provenance pins its exact codec package.
    let error = engine
        .execute(
            EngineCommand::RemovePlugin {
                plugin_id: provenance.plugin_id,
            },
            |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(error, stcli_core::EngineError::PluginInUse(_)));
    let EngineInspection::ArtifactSource(exported) = engine
        .inspect(EngineQuery::ArtifactSource {
            revision_hash: primary.revision_hash,
        })
        .unwrap()
    else {
        panic!("unexpected artifact export");
    };
    assert!(!exported.is_empty());
}

#[tokio::test]
async fn core_bootstraps_json_but_requires_the_bundled_codec_for_external_containers() {
    // Regression test for issue #130: migrated formats must not fall back to native Core parsers.
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);

    engine
        .execute(
            EngineCommand::RemovePlugin {
                plugin_id: "org.stcli.sillytavern-codec".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap();

    let EngineResult::ArtifactBundle { primary, .. } = engine
        .execute(EngineCommand::ImportArtifact { source: preset(0) }, |_| {})
        .await
        .unwrap()
    else {
        panic!("unexpected artifact import result");
    };
    assert_eq!(primary.kind, ArtifactKind::ChatCompletionPreset);

    let error = engine
        .execute(
            EngineCommand::ImportArtifact {
                source: png_card(false),
            },
            |_| {},
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("plugin restore-defaults"),
        "expected an actionable codec repair diagnostic, got: {error}"
    );

    let EngineResult::ArtifactBundle { primary, .. } = engine
        .execute(
            EngineCommand::ImportArtifact {
                source: preset(2 * 1024 * 1024),
            },
            |_| {},
        )
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
    manifest["id"] = json!("org.example.extra-capability-codec");
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

#[tokio::test]
async fn import_rejects_ambiguous_codec_claims() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let competing = directory.path().join("competing-codec");
    fs::create_dir(&competing).unwrap();
    fs::copy(
        codec_directory().join("component.wasm"),
        competing.join("component.wasm"),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(codec_directory().join("manifest.json")).unwrap())
            .unwrap();
    manifest["id"] = json!("org.example.competing-codec");
    fs::write(
        competing.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    let engine = StcliEngine::new(&database);
    install_and_register(&engine, &competing).await;
    let error = engine
        .execute(EngineCommand::ImportArtifact { source: charx() }, |_| {})
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        stcli_core::EngineError::ArtifactCodec(ArtifactCodecError::AmbiguousFormatClaims(ids))
            if ids == ["org.example.competing-codec", "org.stcli.sillytavern-codec"]
    ));
}
