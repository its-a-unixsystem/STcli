use stcli_core::{
    EngineCommand, EngineInspection, EngineQuery, PluginCapability, StcliEngine, Store,
};
use stcli_testkit::{configuration, fixtures};
use tempfile::tempdir;

const NEMO_ID: &str = "org.stcli.nemo-directives";

#[test]
fn default_nemo_plugin_materializes_offline_and_idempotently() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);

    let first = engine
        .inspect(EngineQuery::Plugins { plugin_id: None })
        .unwrap();
    let EngineInspection::Plugins(first) = first else {
        panic!("unexpected inspection");
    };
    let nemo = first
        .iter()
        .find(|plugin| plugin.manifest.id == NEMO_ID)
        .unwrap();
    assert!(nemo.inspection_enabled);
    assert_eq!(nemo.manifest.version.to_string(), "1.0.0");

    let second = engine
        .inspect(EngineQuery::Plugins { plugin_id: None })
        .unwrap();
    let EngineInspection::Plugins(second) = second else {
        panic!("unexpected inspection");
    };
    assert_eq!(
        second
            .iter()
            .filter(|plugin| plugin.manifest.id == NEMO_ID)
            .count(),
        1
    );
}

#[tokio::test]
async fn removing_default_plugin_persists_until_explicit_reinstall() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins { plugin_id: None })
        .unwrap()
    else {
        panic!("unexpected inspection");
    };
    assert!(plugins.iter().any(|plugin| plugin.manifest.id == NEMO_ID));

    engine
        .execute(
            EngineCommand::RemovePlugin {
                plugin_id: NEMO_ID.to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap();
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins { plugin_id: None })
        .unwrap()
    else {
        panic!("unexpected inspection");
    };
    assert!(plugins.iter().all(|plugin| plugin.manifest.id != NEMO_ID));

    engine
        .execute(EngineCommand::RestoreDefaultPlugins, |_| {})
        .await
        .unwrap();
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins { plugin_id: None })
        .unwrap()
    else {
        panic!("unexpected inspection");
    };
    assert!(
        plugins
            .iter()
            .any(|plugin| { plugin.manifest.id == NEMO_ID && plugin.inspection_enabled })
    );
}

#[tokio::test]
async fn default_materialization_never_rewrites_session_plugin_pins() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins {
            plugin_id: Some(NEMO_ID.to_owned()),
        })
        .unwrap()
    else {
        panic!("unexpected inspection");
    };
    let nemo = plugins.into_iter().next().unwrap();
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    engine
        .execute(
            EngineCommand::AdoptPlugin {
                session_id: created.session.session_id,
                id: nemo.manifest.id,
                version: nemo.manifest.version.to_string(),
                digest: nemo.manifest.component_sha256,
                capabilities: [PluginCapability::InspectArtifact].into_iter().collect(),
                settings: serde_json::Value::Null,
                egress: Vec::new(),
            },
            |_| {},
        )
        .await
        .unwrap();
    let before = match engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record.configuration.plugins,
        _ => panic!("unexpected inspection"),
    };

    for _ in 0..2 {
        let _ = engine
            .inspect(EngineQuery::Plugins { plugin_id: None })
            .unwrap();
    }
    let after = match engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record.configuration.plugins,
        _ => panic!("unexpected inspection"),
    };
    assert_eq!(after, before);
}
