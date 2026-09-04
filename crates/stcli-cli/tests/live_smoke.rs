use serde_json::Value;
use stcli_core::{
    AttemptStatus, CapsuleKind, Config, EntityId, InferenceStatus, PluginEvent, ProviderSettings,
    Store, validate_inference_receipt,
};
use stcli_testkit::{TestHome, stcli_cmd};
use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Output,
};

const API_KEY_ENV: &str = "STCLI_LIVE_API_KEY";
const GENERATION_SETTINGS: &str = r#"{"max_tokens":512,"stream_options":{"include_usage":true}}"#;
const EXTENSION_GENERATION_SETTINGS: &str =
    r#"{"max_tokens":64,"stream_options":{"include_usage":true}}"#;

struct LiveConfiguration {
    base_url: String,
    api_key: String,
    model: String,
}

impl LiveConfiguration {
    fn from_environment() -> Option<Self> {
        Some(Self {
            base_url: nonempty_environment("STCLI_LIVE_BASE_URL")?,
            api_key: nonempty_environment(API_KEY_ENV)?,
            model: nonempty_environment("STCLI_LIVE_MODEL")?,
        })
    }
}

fn nonempty_environment(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn example(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../examples/{name}"))
        .to_string_lossy()
        .into_owned()
}

fn metamorph_lifecycle_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/stcli-core/tests/fixtures/real_extensions/metamorph-lifecycle")
}

fn run(home: &TestHome, api_key: &str, args: &[&dyn AsRef<OsStr>]) -> Output {
    let args: Vec<&OsStr> = args.iter().map(|arg| arg.as_ref()).collect();
    stcli_cmd(home)
        .env(API_KEY_ENV, api_key)
        .args([OsStr::new("--output"), OsStr::new("json")])
        .args(args)
        .output()
        .unwrap()
}

fn json_lines(output: &Output, api_key: &str) -> Vec<Value> {
    assert_secret_absent(&output.stdout, api_key);
    assert_secret_absent(&output.stderr, api_key);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn envelope(lines: &[Value]) -> &Value {
    let envelope = lines.last().unwrap();
    assert_eq!(envelope["schema"], "stcli.cli/v1");
    assert_eq!(envelope["ok"], true);
    envelope
}

fn envelope_data(output: &Output, api_key: &str) -> Value {
    let lines = json_lines(output, api_key);
    envelope(&lines)["data"].clone()
}

fn assert_completed_stream(output: &Output, api_key: &str) -> Value {
    let lines = json_lines(output, api_key);
    let envelope = envelope(&lines);
    let events = &lines[..lines.len() - 1];
    assert_eq!(events.first().unwrap()["event_type"], "provider.started");
    assert_eq!(events.last().unwrap()["event_type"], "provider.completed");
    assert!(
        events
            .iter()
            .any(|event| event["event_type"] == "provider.text-delta")
    );
    assert!(
        events
            .iter()
            .any(|event| event["event_type"] == "provider.usage")
    );

    envelope["data"].clone()
}

fn assert_secret_absent(bytes: &[u8], api_key: &str) {
    assert!(
        !bytes
            .windows(api_key.len())
            .any(|window| window == api_key.as_bytes())
    );
}

fn assert_files_do_not_contain_secret(path: &Path, api_key: &str) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            assert_files_do_not_contain_secret(&entry.unwrap().path(), api_key);
        }
    } else {
        assert_secret_absent(&fs::read(path).unwrap(), api_key);
    }
}

fn receipt_has_usage(receipt: &Value) -> bool {
    receipt["chunks"]
        .as_array()
        .is_some_and(|chunks| chunks.iter().any(|chunk| !chunk["usage"].is_null()))
}

fn projection_hash(database: &Path, attempt_id: EntityId) -> String {
    Store::open(database)
        .unwrap()
        .export_turn_capsule(attempt_id, CapsuleKind::Thin, false)
        .unwrap()
        .result
        .projection_hash
        .unwrap()
        .to_string()
}

#[test]
fn real_provider_completes_two_streamed_turns_without_persisting_credentials() {
    let Some(configuration) = LiveConfiguration::from_environment() else {
        return;
    };
    let home = TestHome::new().unwrap();
    let database = home.paths().database();

    let imported = run(
        &home,
        &configuration.api_key,
        &[&"artifact", &"import", &example("character.json")],
    );
    let character = envelope_data(&imported, &configuration.api_key);
    let character_hash = character["primary"]["revision_hash"].as_str().unwrap();

    let created = run(
        &home,
        &configuration.api_key,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--provider-base-url",
            &configuration.base_url,
            &"--provider-api-key-env",
            &API_KEY_ENV,
            &"--model",
            &configuration.model,
            &"--generation-settings",
            &GENERATION_SETTINGS,
        ],
    );
    let session = envelope_data(&created, &configuration.api_key);
    let session_id = session["session"]["session_id"].as_str().unwrap();

    let first = run(
        &home,
        &configuration.api_key,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"Reply briefly.",
        ],
    );
    let first = assert_completed_stream(&first, &configuration.api_key);
    assert!(!first["candidate"]["content"].as_str().unwrap().is_empty());

    let second = run(
        &home,
        &configuration.api_key,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"Continue briefly.",
        ],
    );
    let second = assert_completed_stream(&second, &configuration.api_key);
    assert!(!second["candidate"]["content"].as_str().unwrap().is_empty());

    let attempt_ids = [&first, &second].map(|data| {
        data["attempt"]["attempt_id"]
            .as_str()
            .unwrap()
            .parse::<EntityId>()
            .unwrap()
    });
    let store = Store::open(&database).unwrap();
    for attempt_id in attempt_ids {
        let attempt = store.attempt(attempt_id).unwrap().unwrap();
        assert_eq!(attempt.status, AttemptStatus::Completed);
        assert!(receipt_has_usage(
            attempt.provider_receipt.as_ref().unwrap()
        ));
    }
    drop(store);

    let hash_before = projection_hash(&database, attempt_ids[1]);
    let rebuilt = run(&home, &configuration.api_key, &[&"session", &"rebuild"]);
    envelope_data(&rebuilt, &configuration.api_key);
    assert_eq!(projection_hash(&database, attempt_ids[1]), hash_before);
    assert_files_do_not_contain_secret(home.root(), &configuration.api_key);
}

#[test]
fn extension_real_provider_smoke() {
    let Some(configuration) = LiveConfiguration::from_environment() else {
        return;
    };
    let home = TestHome::new().unwrap();
    let database = home.paths().database();

    Config::add_provider_profile(
        &home.paths().config,
        "summary",
        ProviderSettings {
            id: "summary".to_owned(),
            base_url: configuration.base_url.clone(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
            api_key_env: Some(API_KEY_ENV.to_owned()),
            credential_key: None,
            static_headers: Default::default(),
            timeout_seconds: 120,
            ca_certificate_pem: None,
            model: configuration.model.clone(),
            stream: false,
        },
    )
    .unwrap();

    let fixture = metamorph_lifecycle_fixture();
    let extension = home.root().join("metamorph-lifecycle");
    fs::create_dir_all(&extension).unwrap();
    fs::copy(
        fixture.join("manifest.json"),
        extension.join("manifest.json"),
    )
    .unwrap();
    let mut script = fs::read_to_string(fixture.join("index.js")).unwrap();
    for (original, capped) in [
        (
            "{ provider: current.providerProfile, temperature: 0 }",
            "{ provider: current.providerProfile, temperature: 0, max_tokens: 64 }",
        ),
        (
            "{ providerProfile: current.providerProfile, temperature: 0 }",
            "{ providerProfile: current.providerProfile, temperature: 0, max_tokens: 64 }",
        ),
    ] {
        assert_eq!(
            script.matches(original).count(),
            1,
            "expected exactly one lifecycle fixture option object: {original}"
        );
        script = script.replace(original, capped);
    }
    fs::write(extension.join("index.js"), script).unwrap();

    let imported_character = run(
        &home,
        &configuration.api_key,
        &[&"artifact", &"import", &example("character.json")],
    );
    let character = envelope_data(&imported_character, &configuration.api_key);
    let character_hash = character["primary"]["revision_hash"].as_str().unwrap();
    let created = run(
        &home,
        &configuration.api_key,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--provider-base-url",
            &configuration.base_url,
            &"--provider-api-key-env",
            &API_KEY_ENV,
            &"--model",
            &configuration.model,
            &"--generation-settings",
            &EXTENSION_GENERATION_SETTINGS,
        ],
    );
    let session = envelope_data(&created, &configuration.api_key);
    let session_id = session["session"]["session_id"].as_str().unwrap();

    let imported_extension = run(
        &home,
        &configuration.api_key,
        &[&"extension", &"import", &extension],
    );
    let imported_extension = envelope_data(&imported_extension, &configuration.api_key);
    let manifest = &imported_extension["plugin"]["manifest"];
    let extension_id = manifest["id"].as_str().unwrap();
    let version = manifest["version"].as_str().unwrap();
    let digest = manifest["component_sha256"].as_str().unwrap();
    let adopted = run(
        &home,
        &configuration.api_key,
        &[
            &"extension",
            &"adopt",
            &"--session",
            &session_id,
            &"--version",
            &version,
            &"--digest",
            &digest,
            &"--settings",
            &r#"{"providerProfile":"summary"}"#,
            &extension_id,
        ],
    );
    envelope_data(&adopted, &configuration.api_key);

    let sent = run(
        &home,
        &configuration.api_key,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"Reply briefly.",
        ],
    );
    let completed = assert_completed_stream(&sent, &configuration.api_key);
    assert!(
        !completed["candidate"]["content"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    let attempt_id = completed["attempt"]["attempt_id"]
        .as_str()
        .unwrap()
        .parse::<EntityId>()
        .unwrap();

    let store = Store::open(&database).unwrap();
    let attempt = store.attempt(attempt_id).unwrap().unwrap();
    assert_eq!(attempt.status, AttemptStatus::Completed);
    let effect = attempt.effect_receipt.as_ref().unwrap();
    assert_eq!(effect.provider_request["max_tokens"], 64);
    let receipt = effect
        .plugins
        .iter()
        .find(|receipt| {
            receipt.id == extension_id && receipt.event == PluginEvent::GenerateInterceptor
        })
        .unwrap();
    assert_eq!(receipt.inference.len(), 2);
    for inference in &receipt.inference {
        assert_eq!(inference.status, InferenceStatus::Completed);
        assert!(inference.error.is_none());
        assert_eq!(inference.effective_settings["max_tokens"], 64);
        validate_inference_receipt(inference).unwrap();
    }
    assert!(receipt.egress.is_empty());
    assert_secret_absent(
        &serde_json::to_vec(&attempt).unwrap(),
        &configuration.api_key,
    );
    let capsule = store
        .export_turn_capsule(attempt_id, CapsuleKind::Thin, false)
        .unwrap();
    assert_secret_absent(
        &serde_json::to_vec(&capsule).unwrap(),
        &configuration.api_key,
    );
    drop(store);

    let hash_before = projection_hash(&database, attempt_id);
    let rebuilt = run(&home, &configuration.api_key, &[&"session", &"rebuild"]);
    envelope_data(&rebuilt, &configuration.api_key);
    assert_eq!(projection_hash(&database, attempt_id), hash_before);
    assert_files_do_not_contain_secret(home.root(), &configuration.api_key);
}
