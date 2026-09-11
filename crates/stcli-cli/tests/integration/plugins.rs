use std::path::PathBuf;

use serde_json::{Value, json};
use stcli_core::{PluginPin, PluginRegistry, Store};
use stcli_testkit::{TestHome, configuration, fixtures, stcli_cmd};

fn proof_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/proof")
}

fn run(home: &TestHome, arguments: &[&str]) -> std::process::Output {
    stcli_cmd(home)
        .args(["--output", "json", "plugin"])
        .args(arguments)
        .output()
        .unwrap()
}

fn run_extension(home: &TestHome, arguments: &[&str]) -> std::process::Output {
    stcli_cmd(home)
        .args(["--output", "json", "extension"])
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn upgrade_disable_and_reference_aware_remove_are_explicit() {
    let home = TestHome::new().unwrap();
    let paths = home.paths();
    paths.ensure_exists().unwrap();
    let registry = PluginRegistry::new(paths.plugins());
    let installed = registry.install(&proof_directory()).unwrap();
    let upgrade_package = home.root().join("upgrade-package");
    std::fs::create_dir_all(&upgrade_package).unwrap();
    std::fs::copy(
        proof_directory().join("component.wasm"),
        upgrade_package.join("component.wasm"),
    )
    .unwrap();
    let mut upgrade_manifest: Value =
        serde_json::from_slice(&std::fs::read(proof_directory().join("manifest.json")).unwrap())
            .unwrap();
    upgrade_manifest["version"] = json!("1.0.1");
    std::fs::write(
        upgrade_package.join("manifest.json"),
        serde_json::to_vec(&upgrade_manifest).unwrap(),
    )
    .unwrap();
    let upgraded = registry.install(&upgrade_package).unwrap();
    let mut store = Store::open(paths.database()).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(
            {
                let mut configuration = configuration(character.revision_hash);
                configuration.plugins = vec![PluginPin {
                    id: installed.manifest.id.clone(),
                    version: installed.manifest.version.to_string(),
                    component_hash: installed.manifest.component_sha256.clone(),
                    capabilities: installed.manifest.requested_capabilities.clone(),
                    settings: json!({"retained": true}),
                    egress_allow_list: Vec::new(),
                    enabled: true,
                }];
                configuration
            },
            0,
        )
        .unwrap();
    let original_hash = created.session.current_config_hash;
    drop(store);

    let session = created.session.session_id.to_string();
    let digest = upgraded.manifest.component_sha256.to_string();
    let version = upgraded.manifest.version.to_string();
    let upgrade = run(
        &home,
        &[
            "upgrade",
            "--session",
            &session,
            "--version",
            &version,
            "--digest",
            &digest,
            "org.stcli.proof",
        ],
    );
    assert!(
        upgrade.status.success(),
        "{}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let envelope: Value = serde_json::from_slice(&upgrade.stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    let store = Store::open(paths.database()).unwrap();
    let projection = store.session(created.session.session_id).unwrap().unwrap();
    assert_ne!(projection.current_config_hash, original_hash);
    let configuration = store
        .configuration(&projection.current_config_hash)
        .unwrap()
        .unwrap()
        .configuration;
    assert_eq!(configuration.plugins[0].settings, json!({"retained": true}));
    drop(store);

    let invoke = run(
        &home,
        &[
            "invoke",
            "--session",
            &session,
            "--branch",
            &created.branch.branch_id.to_string(),
            "org.stcli.proof",
            "proof-set",
            "--arguments",
            r#"{"value":1}"#,
        ],
    );
    assert!(
        invoke.status.success(),
        "{}",
        String::from_utf8_lossy(&invoke.stderr)
    );
    let store = Store::open(paths.database()).unwrap();
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
    drop(store);

    let disable = run(
        &home,
        &["disable", "--session", &session, "org.stcli.proof"],
    );
    assert!(
        disable.status.success(),
        "{}",
        String::from_utf8_lossy(&disable.stderr)
    );
    let store = Store::open(paths.database()).unwrap();
    let projection = store.session(created.session.session_id).unwrap().unwrap();
    let configuration = store
        .configuration(&projection.current_config_hash)
        .unwrap()
        .unwrap()
        .configuration;
    assert!(!configuration.plugins[0].enabled);
    drop(store);

    let remove = run(&home, &["remove", "org.stcli.proof"]);
    assert!(!remove.status.success());
    assert!(String::from_utf8_lossy(&remove.stderr).contains("remains pinned"));

    let unreferenced_home = TestHome::new().unwrap();
    assert!(
        run(
            &unreferenced_home,
            &["install", proof_directory().to_str().unwrap()]
        )
        .status
        .success()
    );
    assert!(
        run(&unreferenced_home, &["remove", "org.stcli.proof"])
            .status
            .success()
    );
}

#[test]
fn native_extension_import_and_adoption_emit_structured_results() {
    let home = TestHome::new().unwrap();
    let source = home.root().join("CLI Extension");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("index.js"),
        "globalThis.interceptor = function (chat) { void chat; };",
    )
    .unwrap();
    std::fs::write(
        source.join("manifest.json"),
        serde_json::to_vec(&json!({
            "display_name": "CLI Extension",
            "version": "1.0.0",
            "js": "index.js",
            "css": "style.css",
            "generate_interceptor": "interceptor"
        }))
        .unwrap(),
    )
    .unwrap();

    let imported = run_extension(&home, &["import", source.to_str().unwrap()]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let envelope: Value = serde_json::from_slice(&imported.stdout).unwrap();
    assert_eq!(envelope["command"], "extension.import");
    assert_eq!(
        envelope["data"]["plugin"]["manifest"]["id"],
        "cli-extension"
    );
    assert_eq!(
        envelope["data"]["warnings"][0]["code"],
        "extension-import-field-ignored"
    );

    let paths = home.paths();
    let mut store = Store::open(paths.database()).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    drop(store);
    let session = created.session.session_id.to_string();
    let version = envelope["data"]["plugin"]["manifest"]["version"]
        .as_str()
        .unwrap();
    let digest = envelope["data"]["plugin"]["manifest"]["component_sha256"]
        .as_str()
        .unwrap();
    let adopted = run_extension(
        &home,
        &[
            "adopt",
            "--session",
            &session,
            "--version",
            version,
            "--digest",
            digest,
            "cli-extension",
        ],
    );
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let envelope: Value = serde_json::from_slice(&adopted.stdout).unwrap();
    assert_eq!(envelope["command"], "extension.adopt");
    let plugins = envelope["data"]["configuration"]["plugins"]
        .as_array()
        .unwrap();
    assert_eq!(plugins.len(), 1);
    let plugin = &plugins[0];
    assert_eq!(
        plugin["capabilities"],
        json!([
            "register-command",
            "write-own-state",
            "brokered-egress",
            "secondary-inference"
        ])
    );
    assert!(plugin.get("egress_allow_list").is_none());
    let store = Store::open(paths.database()).unwrap();
    let projection = store.session(created.session.session_id).unwrap().unwrap();
    let configuration = store
        .configuration(&projection.current_config_hash)
        .unwrap()
        .unwrap();
    assert!(
        configuration.configuration.plugins[0]
            .egress_allow_list
            .is_empty()
    );
}
