use std::{collections::BTreeMap, fs, path::PathBuf};

use serde_json::Value;
use stcli_core::{Store, canonical_json};
use stcli_testkit::{MockProviderProcess, TestHome, configuration, fixtures, stcli_cmd};

const REGENERATE_ENV: &str = "STCLI_REGENERATE_PROTOCOL_SAMPLES";
const FIXED_PROVIDER_URL: &str = "https://127.0.0.1:443";
const FIXED_CERTIFICATE: &str = "CANONICAL TEST CERTIFICATE";
const CLI_ENVELOPE_SCHEMA: &str = include_str!("../../../schemas/cli-envelope.schema.json");
const CLI_EVENT_SCHEMA: &str = include_str!("../../../schemas/cli-event.schema.json");
const TURN_CAPSULE_SCHEMA: &str = include_str!("../../../schemas/turn-capsule.schema.json");

#[tokio::test(flavor = "multi_thread")]
async fn real_cli_workflows_match_canonical_protocol_samples() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let paths = home.paths();
    paths.ensure_exists().unwrap();
    let mut store = Store::open(paths.database()).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut session_configuration = configuration(character.revision_hash);
    session_configuration.provider = provider.provider_settings();
    session_configuration.provider.stream = true;
    let created = store.create_session(session_configuration, 0).unwrap();
    drop(store);

    let session_id = created.session.session_id.to_string();
    let send = stcli_cmd(&home)
        .args([
            "--output",
            "json",
            "message",
            "send",
            "--session",
            &session_id,
            "Hello",
        ])
        .output()
        .unwrap();
    assert!(
        send.status.success(),
        "{}",
        String::from_utf8_lossy(&send.stderr)
    );
    let stream = json_lines(&send.stdout);
    let success_envelope = stream.last().unwrap().clone();
    let attempt_id = success_envelope["data"]["attempt"]["attempt_id"]
        .as_str()
        .unwrap();

    let failure = stcli_cmd(&home)
        .args([
            "--output",
            "json",
            "session",
            "show",
            "00000000000000000000000000",
        ])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    let failure_envelope = json_lines(&failure.stderr).pop().unwrap();

    let portable_path = home.root().join("portable.json");
    export_capsule(&home, &session_id, attempt_id, &portable_path, false);
    let portable = read_json(&portable_path);
    let thin_path = home.root().join("thin.json");
    export_capsule(&home, &session_id, attempt_id, &thin_path, true);
    let thin = read_json(&thin_path);

    let mut normalizer = Normalizer::default();
    let success_envelope = normalizer.normalize(success_envelope);
    let failure_envelope = normalizer.normalize(failure_envelope);
    let stream = stream
        .into_iter()
        .map(|value| normalizer.normalize(value))
        .collect::<Vec<_>>();
    let portable = normalizer.normalize(portable);
    let thin = normalizer.normalize(thin);

    let envelope_schema = serde_json::from_str(CLI_ENVELOPE_SCHEMA).unwrap();
    assert_valid(&envelope_schema, &success_envelope);
    assert_valid(&envelope_schema, &failure_envelope);
    let (stream_envelope, events) = stream.split_last().unwrap();
    assert_valid(&envelope_schema, stream_envelope);
    let event_schema = serde_json::from_str(CLI_EVENT_SCHEMA).unwrap();
    for event in events {
        assert_valid(&event_schema, event);
    }
    let streamed_text = events
        .iter()
        .filter(|event| event["event_type"] == "provider.text-delta")
        .map(|event| event["data"]["text"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(
        streamed_text,
        stream_envelope["data"]["candidate"]["content"]
    );
    let capsule_schema = serde_json::from_str(TURN_CAPSULE_SCHEMA).unwrap();
    assert_valid(&capsule_schema, &portable);
    assert_valid(&capsule_schema, &thin);
    assert_eq!(portable["kind"], "portable");
    assert_eq!(thin["kind"], "thin");

    let samples = BTreeMap::from([
        ("envelope-success.json", json_bytes(success_envelope)),
        ("envelope-error.json", json_bytes(failure_envelope)),
        ("turn-stream.jsonl", json_lines_bytes(stream)),
        ("turn-capsule-portable.json", json_bytes(portable)),
        ("turn-capsule-thin.json", json_bytes(thin)),
    ]);

    let sample_directory = sample_directory();
    if std::env::var_os(REGENERATE_ENV).as_deref() == Some("1".as_ref()) {
        fs::create_dir_all(&sample_directory).unwrap();
        for (name, bytes) in &samples {
            fs::write(sample_directory.join(name), bytes).unwrap();
        }
    } else {
        for (name, actual) in &samples {
            let path = sample_directory.join(name);
            let expected = fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "failed to read canonical protocol sample {}: {error}; regenerate with {REGENERATE_ENV}=1",
                    path.display()
                )
            });
            assert_eq!(
                &expected,
                actual,
                "canonical protocol sample {} changed; review the payload diff and affected schema identifier, then regenerate with {REGENERATE_ENV}=1",
                path.display()
            );
        }
    }

    provider.shutdown();
}

fn export_capsule(
    home: &TestHome,
    session_id: &str,
    attempt_id: &str,
    path: &std::path::Path,
    thin: bool,
) {
    let mut args = vec![
        "--output",
        "json",
        "turn",
        "export",
        "--session",
        session_id,
        attempt_id,
        "--file",
        path.to_str().unwrap(),
    ];
    if thin {
        args.push("--thin");
    }
    let output = stcli_cmd(home).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn sample_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../compat/protocol-samples")
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn assert_valid(schema: &Value, instance: &Value) {
    jsonschema::validator_for(schema)
        .unwrap()
        .validate(instance)
        .unwrap();
}

fn json_lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn json_bytes(value: Value) -> Vec<u8> {
    let mut bytes = canonical_json(&value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn json_lines_bytes(values: impl IntoIterator<Item = Value>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend(canonical_json(&value).unwrap());
        bytes.push(b'\n');
    }
    bytes
}

#[derive(Default)]
struct Normalizer {
    entity_ids: BTreeMap<String, String>,
    hashes: BTreeMap<String, String>,
}

impl Normalizer {
    fn normalize(&mut self, value: Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|value| self.normalize(value))
                    .collect(),
            ),
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        let value = if key == "rng_seed" {
                            Value::from(0)
                        } else if key == "base_url" {
                            Value::String(FIXED_PROVIDER_URL.to_owned())
                        } else if key == "ca_certificate_pem" && !value.is_null() {
                            Value::String(FIXED_CERTIFICATE.to_owned())
                        } else {
                            self.normalize(value)
                        };
                        (key, value)
                    })
                    .collect(),
            ),
            Value::String(value) => Value::String(self.normalize_string(value)),
            value => value,
        }
    }

    fn normalize_string(&mut self, value: String) -> String {
        if is_entity_id(&value) {
            return stable_entity_id(&mut self.entity_ids, value);
        }
        if is_hash(&value) {
            return stable_hash(&mut self.hashes, value);
        }
        value
    }
}

fn stable_entity_id(values: &mut BTreeMap<String, String>, value: String) -> String {
    let next = format!("{:026}", values.len() + 1);
    values.entry(value).or_insert(next).clone()
}

fn stable_hash(values: &mut BTreeMap<String, String>, value: String) -> String {
    let next = format!("sha256:{:064x}", values.len() + 1);
    values.entry(value).or_insert(next).clone()
}

fn is_entity_id(value: &str) -> bool {
    value.len() == 26
        && value.bytes().all(|b| {
            matches!(
                b,
                b'0'..=b'9'
                    | b'A'..=b'H'
                    | b'J'
                    | b'K'
                    | b'M'..=b'N'
                    | b'P'..=b'T'
                    | b'V'..=b'Z'
            )
        })
}

fn is_hash(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
