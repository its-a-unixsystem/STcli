//! Tests for provider profile loading from config.toml, CLI flag overrides,
//! session update with profile switching, and error diagnostics.

use serde_json::Value;
use stcli_core::Config;
use stcli_testkit::TestHome;
use std::{
    ffi::OsStr,
    fs,
    process::{Command, Output},
};

fn stcli_cmd(home: &TestHome) -> Command {
    let mut command = Command::new(home.stcli_binary());
    command.env("STCLI_HOME", home.root());
    command.env("STCLI_REGEX_WORKER", home.stcli_binary());
    command
}

fn run(home: &TestHome, args: &[&dyn AsRef<OsStr>]) -> Output {
    let args: Vec<&OsStr> = args.iter().map(|arg| arg.as_ref()).collect();
    stcli_cmd(home)
        .args([OsStr::new("--output"), OsStr::new("json")])
        .args(&args)
        .output()
        .unwrap()
}

fn envelope_data(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let envelope: Value = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .next_back()
        .map(|line| serde_json::from_str(line).unwrap())
        .unwrap();
    assert_eq!(envelope["ok"], true, "envelope not ok: {envelope}");
    envelope["data"].clone()
}

fn error_message(output: &Output) -> String {
    String::from_utf8(output.stderr.clone())
        .unwrap()
        .trim()
        .to_owned()
}

fn write_config_toml(home: &TestHome, content: &str) {
    let config_dir = home.root().join("config");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), content).unwrap();
}

fn import_character(home: &TestHome) -> String {
    let example =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/character.json");
    let output = run(home, &[&"artifact", &"import", &example.to_str().unwrap()]);
    let data = envelope_data(&output);
    data["primary"]["revision_hash"]
        .as_str()
        .unwrap()
        .to_owned()
}

const PROVIDER_CONFIG: &str = r#"
[providers.openrouter]
id = "openrouter"
base_url = "https://openrouter.ai"
chat_completions_path = "/api/v1/chat/completions"
timeout_seconds = 60
model = "anthropic/claude-3.5-sonnet"
stream = true

[providers.local]
id = "local"
base_url = "https://localhost:5001"
chat_completions_path = "/v1/chat/completions"
timeout_seconds = 30
model = "local-model"
stream = false
api_key_env = "LOCAL_API_KEY"
"#;

/// AC1: Config and config.toml loading are moved into stcli-core and shared.
#[test]
fn core_config_loads_provider_profiles() {
    let home = TestHome::new().unwrap();
    write_config_toml(&home, PROVIDER_CONFIG);

    let config = Config::load(&home.root().join("config")).unwrap();
    assert_eq!(config.providers.len(), 2);
    assert_eq!(
        config.providers["openrouter"].model,
        "anthropic/claude-3.5-sonnet"
    );
    assert!(!config.providers["local"].stream);
}

/// AC2: `stcli session create --provider-profile <name>` populates provider settings.
#[test]
fn session_create_uses_provider_profile_from_config() {
    let home = TestHome::new().unwrap();
    write_config_toml(&home, PROVIDER_CONFIG);
    let character = import_character(&home);

    let output = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character.as_str(),
            &"--provider-profile",
            &"openrouter",
        ],
    );
    let data = envelope_data(&output);
    let provider = &data["configuration"]["configuration"]["provider"];
    assert_eq!(provider["id"], "openrouter");
    assert_eq!(provider["base_url"], "https://openrouter.ai");
    assert_eq!(
        provider["chat_completions_path"],
        "/api/v1/chat/completions"
    );
    assert_eq!(provider["model"], "anthropic/claude-3.5-sonnet");
    assert_eq!(provider["stream"], true);
    assert_eq!(provider["timeout_seconds"], 60);
}

/// AC3: Explicit CLI flags override individual fields of the selected profile.
#[test]
fn explicit_flags_override_provider_profile_fields() {
    let home = TestHome::new().unwrap();
    write_config_toml(&home, PROVIDER_CONFIG);
    let character = import_character(&home);

    let output = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character.as_str(),
            &"--provider-profile",
            &"openrouter",
            &"--model",
            &"custom-model-override",
            &"--provider-stream",
            &"false",
        ],
    );
    let data = envelope_data(&output);
    let provider = &data["configuration"]["configuration"]["provider"];
    assert_eq!(provider["base_url"], "https://openrouter.ai");
    assert_eq!(provider["model"], "custom-model-override");
    assert_eq!(provider["stream"], false);
}

/// AC4 + AC5: Session update with --provider-profile switches the active provider
/// and creates a new SessionConfigurationRevision preserving other fields.
#[test]
fn session_update_switches_provider_profile() {
    let home = TestHome::new().unwrap();
    write_config_toml(&home, PROVIDER_CONFIG);
    let character = import_character(&home);

    // Create session with "openrouter" profile
    let create_output = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character.as_str(),
            &"--provider-profile",
            &"openrouter",
        ],
    );
    let create_data = envelope_data(&create_output);
    let session_id = create_data["session"]["session_id"].as_str().unwrap();
    let original_config_hash = create_data["session"]["current_config_hash"]
        .as_str()
        .unwrap()
        .to_owned();

    // Update session to "local" profile
    let update_output = run(
        &home,
        &[
            &"session",
            &"update",
            &session_id,
            &"--character",
            &character.as_str(),
            &"--provider-profile",
            &"local",
        ],
    );
    let update_data = envelope_data(&update_output);
    let new_config_hash = update_data["revision_hash"].as_str().unwrap();
    let provider = &update_data["configuration"]["provider"];

    // New config revision is different
    assert_ne!(new_config_hash, original_config_hash);
    // Provider switched to local
    assert_eq!(provider["id"], "local");
    assert_eq!(provider["base_url"], "https://localhost:5001");
    assert_eq!(provider["model"], "local-model");
    assert_eq!(provider["stream"], false);
    // Non-provider fields preserved
    assert_eq!(
        update_data["configuration"]["character_revision"],
        create_data["configuration"]["configuration"]["character_revision"]
    );
    assert_eq!(
        update_data["configuration"]["persona_name"],
        create_data["configuration"]["configuration"]["persona_name"]
    );
}

/// AC6: Unknown provider profile produces a descriptive error listing available profiles.
#[test]
fn unknown_provider_profile_lists_available_profiles() {
    let home = TestHome::new().unwrap();
    write_config_toml(&home, PROVIDER_CONFIG);
    let character = import_character(&home);

    let output = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character.as_str(),
            &"--provider-profile",
            &"nonexistent",
        ],
    );
    assert!(!output.status.success());
    let message = error_message(&output);
    assert!(
        message.contains("nonexistent"),
        "error should mention the requested profile: {message}"
    );
    assert!(
        message.contains("local"),
        "error should list available profiles: {message}"
    );
    assert!(
        message.contains("openrouter"),
        "error should list available profiles: {message}"
    );
}

/// AC6: No profiles configured produces a clear empty diagnostic.
#[test]
fn unknown_profile_with_no_config_shows_none_available() {
    let home = TestHome::new().unwrap();
    let character = import_character(&home);

    let output = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character.as_str(),
            &"--provider-profile",
            &"anything",
        ],
    );
    assert!(!output.status.success());
    let message = error_message(&output);
    assert!(
        message.contains("none configured"),
        "error should indicate no profiles exist: {message}"
    );
}

/// AC7: Existing CLI session creation without --provider-profile still works with defaults.
#[test]
fn session_create_without_profile_uses_flag_defaults() {
    let home = TestHome::new().unwrap();
    let character = import_character(&home);

    let output = run(
        &home,
        &[&"session", &"create", &"--character", &character.as_str()],
    );
    let data = envelope_data(&output);
    let provider = &data["configuration"]["configuration"]["provider"];
    assert_eq!(provider["base_url"], "https://127.0.0.1:3443");
    assert_eq!(provider["model"], "fixture-model");
    assert_eq!(provider["stream"], true);
    assert_eq!(provider["timeout_seconds"], 120);
}

#[test]
fn profile_cli_lifecycle_list_show_add_remove() {
    let home = TestHome::new().unwrap();
    write_config_toml(&home, PROVIDER_CONFIG);

    // List profiles
    let output = run(&home, &[&"profile", &"list"]);
    let data = envelope_data(&output);
    assert!(data.get("openrouter").is_some());
    assert!(data.get("local").is_some());

    // Show profile
    let output = run(&home, &[&"profile", &"show", &"openrouter"]);
    let data = envelope_data(&output);
    assert_eq!(data["model"], "anthropic/claude-3.5-sonnet");

    // Add profile from JSON file
    let new_profile_path = home.root().join("new_profile.json");
    fs::write(
        &new_profile_path,
        r#"{
            "id": "deepseek",
            "base_url": "https://api.deepseek.com",
            "chat_completions_path": "/chat/completions",
            "model": "deepseek-chat",
            "stream": true,
            "timeout_seconds": 90,
            "api_key_env": "DEEPSEEK_API_KEY"
        }"#,
    )
    .unwrap();

    let output = run(
        &home,
        &[
            &"profile",
            &"add",
            &"deepseek",
            &"--file",
            &new_profile_path.to_str().unwrap(),
        ],
    );
    assert!(output.status.success());

    // Show the newly added profile
    let output = run(&home, &[&"profile", &"show", &"deepseek"]);
    let data = envelope_data(&output);
    assert_eq!(data["model"], "deepseek-chat");
    assert_eq!(data["base_url"], "https://api.deepseek.com");

    // Remove profile
    let output = run(&home, &[&"profile", &"remove", &"deepseek"]);
    assert!(output.status.success());

    // Show removed profile fails
    let output = run(&home, &[&"profile", &"show", &"deepseek"]);
    assert!(!output.status.success());
}
