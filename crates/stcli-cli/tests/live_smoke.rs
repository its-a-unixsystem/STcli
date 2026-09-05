use serde_json::Value;
use stcli_core::{
    AttemptKind, AttemptStatus, CapsuleKind, Config, EntityId, InferenceStatus, PluginEvent,
    ProviderSettings, Store, VariableScope, validate_inference_receipt,
    validate_persisted_inference_receipt,
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
const RAW_OUTPUT_ENV: &str = "STCLI_LIVE_SHOW_RAW";
const GENERATION_SETTINGS: &str = r#"{"max_tokens":512,"stream_options":{"include_usage":true}}"#;
const SUMMARIZE_GENERATION_SETTINGS: &str =
    r#"{"max_tokens":512,"max_context":8192,"stream_options":{"include_usage":true}}"#;
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

fn show_raw_output(label: &str, output: &Output, api_key: &str) {
    if nonempty_environment(RAW_OUTPUT_ENV).is_none() {
        return;
    }
    assert_secret_absent(&output.stdout, api_key);
    assert_secret_absent(&output.stderr, api_key);
    eprintln!("--- {label}: stdout ---");
    eprint!("{}", String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        eprintln!("--- {label}: stderr ---");
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
}

fn show_raw_json(label: &str, value: &Value, api_key: &str) {
    if nonempty_environment(RAW_OUTPUT_ENV).is_none() {
        return;
    }
    let bytes = serde_json::to_vec(value).unwrap();
    assert_secret_absent(&bytes, api_key);
    eprintln!("--- {label} ---");
    eprintln!("{}", serde_json::to_string_pretty(value).unwrap());
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

#[test]
fn summarize_extension_completes_four_live_roleplay_turns() {
    // Covers live Summarize generation and subsequent prompt injection over four roleplay Turns.
    let Some(configuration) = LiveConfiguration::from_environment() else {
        eprintln!(
            "skipping live Summarize test: STCLI_LIVE_BASE_URL, STCLI_LIVE_API_KEY, and STCLI_LIVE_MODEL are required"
        );
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
            &SUMMARIZE_GENERATION_SETTINGS,
        ],
    );
    let session = envelope_data(&created, &configuration.api_key);
    let session_id = session["session"]["session_id"]
        .as_str()
        .unwrap()
        .parse::<EntityId>()
        .unwrap();
    let branch_id = session["session"]["root_branch_id"]
        .as_str()
        .unwrap()
        .parse::<EntityId>()
        .unwrap();

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

    let listed = run(&home, &configuration.api_key, &[&"extension", &"list"]);
    let inventory = envelope_data(&listed, &configuration.api_key);
    let memory = inventory["installed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extension| extension["manifest"]["id"] == "memory")
        .expect("bundled memory Extension must be installed");
    let memory_version = memory["manifest"]["version"].as_str().unwrap();
    let memory_digest = memory["manifest"]["component_sha256"].as_str().unwrap();
    let session_id_text = session_id.to_string();
    let adopted = run(
        &home,
        &configuration.api_key,
        &[
            &"extension",
            &"adopt",
            &"--session",
            &session_id_text,
            &"--version",
            &memory_version,
            &"--digest",
            &memory_digest,
            &"--settings",
            &r#"{"providerProfile":"summary","promptInterval":4,"promptForceWords":0,"promptWords":80,"overrideResponseLength":0}"#,
            &"memory",
        ],
    );
    envelope_data(&adopted, &configuration.api_key);

    let messages = [
        "I arrive at the Grand Archive carrying a brass compass that points underground. I ask Elspeth to help identify it. Reply in character in at most two short sentences.",
        "I tell Elspeth the compass came from my missing mentor, and its lid bears the mark of the Aether Engines. I ask where the Archive keeps those records. Reply in at most two short sentences.",
        "I agree to follow Elspeth into the restricted stacks, keeping the compass wrapped in blue cloth. I ask what precautions we should take. Reply in at most two short sentences.",
        "At the sealed vault, I unwrap the compass and ask Elspeth to help compare it with the engine schematics. Continue our scene in at most two short sentences.",
    ];
    let mut primary_attempt_ids = Vec::new();
    let mut candidate_contents = Vec::new();
    let mut checkpoint_after_third = None;
    let mut summary_attempt_id = None;

    for (index, message) in messages.iter().enumerate() {
        let sent = run(
            &home,
            &configuration.api_key,
            &[&"message", &"send", &"--session", &session_id_text, message],
        );
        show_raw_output(
            &format!("Summarize Turn {} CLI JSONL", index + 1),
            &sent,
            &configuration.api_key,
        );
        let completed = assert_completed_stream(&sent, &configuration.api_key);
        let candidate_content = completed["candidate"]["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            !candidate_content.trim().is_empty(),
            "Turn {} returned blank Candidate content",
            index + 1
        );
        let attempt_id = completed["attempt"]["attempt_id"]
            .as_str()
            .unwrap()
            .parse::<EntityId>()
            .unwrap();
        primary_attempt_ids.push(attempt_id);
        candidate_contents.push(candidate_content);

        let store = Store::open(&database).unwrap();
        let attempt = store.attempt(attempt_id).unwrap().unwrap();
        assert_eq!(
            attempt.kind,
            AttemptKind::Primary,
            "Turn {} kind",
            index + 1
        );
        assert_eq!(
            attempt.status,
            AttemptStatus::Completed,
            "Turn {} status",
            index + 1
        );
        assert_eq!(
            attempt.effective_generation_settings.as_ref().unwrap()["values"]["max_tokens"],
            512,
            "Turn {} token cap",
            index + 1
        );
        assert_eq!(
            attempt.effect_receipt.as_ref().unwrap().provider_request["max_tokens"],
            512,
            "Turn {} submitted token cap",
            index + 1
        );
        assert_secret_absent(
            &serde_json::to_vec(&attempt).unwrap(),
            &configuration.api_key,
        );

        let background = store
            .background_attempts(session_id, Some(branch_id))
            .unwrap();
        let expected_background = usize::from(index >= 2);
        assert_eq!(
            background.len(),
            expected_background,
            "unexpected Background Attempt count after Turn {}",
            index + 1
        );

        if index == 2 {
            let effect = attempt.effect_receipt.as_ref().unwrap();
            let receipts: Vec<_> = effect
                .plugins
                .iter()
                .filter(|receipt| {
                    receipt.id == "memory" && receipt.event == PluginEvent::GenerateInterceptor
                })
                .collect();
            assert_eq!(receipts.len(), 1, "Turn 3 memory interceptor receipt");
            let receipt = receipts[0];
            assert_eq!(receipt.inference.len(), 1, "Turn 3 summary inference count");
            assert!(
                receipt.egress.is_empty(),
                "Turn 3 memory egress must be empty"
            );
            let inference = &receipt.inference[0];
            assert_eq!(inference.status, InferenceStatus::Completed);
            assert!(inference.error.is_none());
            assert_eq!(inference.caller, "memory");
            assert_eq!(inference.profile_name, "summary");
            let background_attempt = &background[0];
            show_raw_json(
                "Summarize Background Attempt provider response",
                background_attempt.provider_receipt.as_ref().unwrap(),
                &configuration.api_key,
            );
            assert!(
                !inference.text.trim().is_empty(),
                "Turn 3 live summary returned blank text (reasoning may have consumed output allowance)"
            );
            assert!(
                inference
                    .system_prompt
                    .as_deref()
                    .is_some_and(|prompt| !prompt.trim().is_empty()),
                "Turn 3 summary system prompt must be nonblank"
            );
            assert!(inference.prompt.contains(messages[0]));
            assert!(inference.prompt.contains(messages[1]));
            assert!(inference.prompt.contains(&candidate_contents[0]));
            assert!(inference.prompt.contains(&candidate_contents[1]));
            assert!(!inference.prompt.contains(messages[2]));
            validate_persisted_inference_receipt(&store, inference).unwrap();

            assert_eq!(background_attempt.kind, AttemptKind::Background);
            assert_eq!(background_attempt.status, AttemptStatus::Completed);
            assert_eq!(background_attempt.session_id, session_id);
            assert_eq!(background_attempt.branch_id, branch_id);
            assert_eq!(background_attempt.parent_attempt_id, Some(attempt_id));
            assert_eq!(background_attempt.caller.as_deref(), Some("memory"));
            assert_eq!(
                background_attempt.provider_profile.as_deref(),
                Some("summary")
            );
            assert_eq!(inference.attempt_id, Some(background_attempt.attempt_id));

            let state = store.state_transaction(session_id).unwrap();
            let settings = &state
                .get(VariableScope::Local, "extension.memory.settings")
                .expect("memory settings after Turn 3")
                .value;
            let checkpoints = settings["checkpoints"].as_array().unwrap();
            assert_eq!(checkpoints.len(), 1, "Turn 3 checkpoint count");
            let checkpoint = checkpoints[0].clone();
            let raw_summary = checkpoint["raw_summary"].as_str().unwrap();
            assert!(
                !raw_summary.trim().is_empty(),
                "Turn 3 summary must be nonblank"
            );
            assert_eq!(checkpoint["branch_id"], branch_id.to_string());
            assert_eq!(checkpoint["dialogue_cursor"], 5);
            let digest = checkpoint["history_prefix_digest"].as_str().unwrap();
            assert_eq!(digest.len(), 64);
            assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(
                checkpoint["attempt_id"],
                background_attempt.attempt_id.to_string()
            );
            assert_eq!(raw_summary, inference.text.trim());
            assert_secret_absent(
                &serde_json::to_vec(&background_attempt).unwrap(),
                &configuration.api_key,
            );
            assert_secret_absent(
                &serde_json::to_vec(&checkpoint).unwrap(),
                &configuration.api_key,
            );
            summary_attempt_id = Some(background_attempt.attempt_id);
            checkpoint_after_third = Some(checkpoint);
        }

        if index == 3 {
            assert_eq!(
                background[0].attempt_id,
                summary_attempt_id.unwrap(),
                "Turn 4 must reuse the Turn 3 Background Attempt"
            );
            let effect = attempt.effect_receipt.as_ref().unwrap();
            let receipts: Vec<_> = effect
                .plugins
                .iter()
                .filter(|receipt| {
                    receipt.id == "memory" && receipt.event == PluginEvent::GenerateInterceptor
                })
                .collect();
            assert_eq!(receipts.len(), 1, "Turn 4 memory interceptor receipt");
            assert!(
                receipts[0].inference.is_empty(),
                "Turn 4 must not request another summary"
            );
            assert!(
                receipts[0].egress.is_empty(),
                "Turn 4 memory egress must be empty"
            );

            let state = store.state_transaction(session_id).unwrap();
            let settings = &state
                .get(VariableScope::Local, "extension.memory.settings")
                .expect("memory settings after Turn 4")
                .value;
            let checkpoints = settings["checkpoints"].as_array().unwrap();
            assert_eq!(checkpoints.len(), 1, "Turn 4 checkpoint count");
            assert_eq!(
                checkpoints[0],
                checkpoint_after_third.as_ref().unwrap().clone(),
                "Turn 4 must preserve the Turn 3 checkpoint"
            );
            let raw_summary = checkpoints[0]["raw_summary"].as_str().unwrap();
            let expected_prompt = format!("[Summary: {raw_summary}]");
            let submitted_messages = effect.provider_request["messages"].as_array().unwrap();
            assert!(
                submitted_messages.iter().any(|message| message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains(&expected_prompt))),
                "Turn 4 submitted request must contain the persisted summary"
            );
            assert_secret_absent(
                &serde_json::to_vec(&checkpoints[0]).unwrap(),
                &configuration.api_key,
            );
        }
    }

    assert_eq!(
        primary_attempt_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4,
        "each Turn must use a distinct Primary Attempt"
    );
    let store = Store::open(&database).unwrap();
    let turns = store.turns_for_branch(branch_id).unwrap();
    assert_eq!(
        turns.len(),
        4,
        "summarization must not create a dialogue Turn"
    );
    for (index, turn) in turns.iter().enumerate() {
        assert_eq!(turn.user_content, messages[index]);
        let candidate = store
            .candidate(turn.selected_candidate_id.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(candidate.content, candidate_contents[index]);
        let attempts = store.attempts_for_turn(turn.turn_id).unwrap();
        assert_eq!(
            attempts.len(),
            1,
            "Turn {} primary Attempt count",
            index + 1
        );
        assert_eq!(attempts[0].attempt_id, primary_attempt_ids[index]);
        assert_eq!(attempts[0].kind, AttemptKind::Primary);
    }
    let background = store
        .background_attempts(session_id, Some(branch_id))
        .unwrap();
    assert_eq!(background.len(), 1);
    assert_eq!(background[0].attempt_id, summary_attempt_id.unwrap());
    assert_secret_absent(
        &serde_json::to_vec(&background).unwrap(),
        &configuration.api_key,
    );
    drop(store);

    assert_files_do_not_contain_secret(home.root(), &configuration.api_key);
}
