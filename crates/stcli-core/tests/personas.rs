use std::fs;

use serde_json::json;
use stcli_core::PersonaStore;
use tempfile::tempdir;

#[test]
fn persona_store_round_trips_sillytavern_backup_format() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("personas.json"),
        serde_json::to_vec_pretty(&json!({
            "personas": {
                "alice.png": "Alice",
                "bob.png": "Bob"
            },
            "persona_descriptions": {
                "alice.png": {
                    "description": "A curious archivist.",
                    "position": 0,
                    "depth": 2
                },
                "bob.png": {
                    "description": "A patient navigator.",
                    "position": 1
                }
            },
            "default_persona": "alice.png"
        }))
        .unwrap(),
    )
    .unwrap();

    let mut store = PersonaStore::load(directory.path()).unwrap();
    assert_eq!(store.personas().len(), 2);
    assert_eq!(store.get("alice.png").unwrap().name, "Alice");
    assert_eq!(
        store.get("alice.png").unwrap().description,
        "A curious archivist."
    );
    assert_eq!(store.default_persona(), Some("alice.png"));

    store
        .update("alice.png", "Alice Prime", "An updated archivist.")
        .unwrap();
    store.save(directory.path()).unwrap();

    let reloaded = PersonaStore::load(directory.path()).unwrap();
    let alice = reloaded.get("alice.png").unwrap();
    assert_eq!(alice.name, "Alice Prime");
    assert_eq!(alice.description, "An updated archivist.");
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.path().join("personas.json")).unwrap()).unwrap();
    assert_eq!(saved["persona_descriptions"]["alice.png"]["depth"], 2);
    assert_eq!(saved["default_persona"], "alice.png");
}
