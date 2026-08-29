use std::{ffi::OsStr, panic};

use stcli_core::ContentHash;
use stcli_testkit::{
    EnvironmentGuard, MockProvider, MockProviderProcess, TestHome, configuration, fixtures,
    stcli_cmd,
};

#[test]
fn fixtures_expose_minimal_and_documented_cards() {
    let minimal = serde_json::from_str::<serde_json::Value>(fixtures::minimal_card()).unwrap();
    let documented = serde_json::from_str::<serde_json::Value>(fixtures::character()).unwrap();
    assert_eq!(minimal["data"]["name"], "Alice");
    assert_eq!(documented["spec"], "chara_card_v2");
}

#[test]
fn configuration_builds_the_canonical_session_revision_input() {
    let hash = ContentHash::new([7; 32]);
    let configuration = configuration(hash.clone());

    assert_eq!(configuration.character_revision, hash);
    assert_eq!(configuration.compatibility_profile, "sillytavern-1.18-core");
    assert_eq!(configuration.provider.id, "invalid-http");
}

#[test]
fn command_receives_isolated_home_and_worker_without_global_mutation() {
    let original_home = std::env::var_os("STCLI_HOME");
    let home = TestHome::new().unwrap();
    let command = stcli_cmd(&home);
    let environment = command
        .get_envs()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(
        environment.get(OsStr::new("STCLI_HOME")),
        Some(&home.root().as_os_str())
    );
    assert_eq!(
        environment.get(OsStr::new("STCLI_REGEX_WORKER")),
        Some(&home.stcli_binary().as_os_str())
    );
    assert_eq!(std::env::var_os("STCLI_HOME"), original_home);
}

#[test]
fn environment_guard_restores_values_and_recovers_after_panic() {
    const NAME: &str = "STCLI_TESTKIT_PANIC_SAFE_ENV";
    let original = std::env::var_os(NAME);

    let _ = panic::catch_unwind(|| {
        let mut environment = EnvironmentGuard::new();
        environment.set(NAME, "temporary");
        panic!("exercise unwind");
    });

    assert_eq!(std::env::var_os(NAME), original);
    let mut environment = EnvironmentGuard::new();
    environment.set(NAME, "restored");
    assert_eq!(std::env::var(NAME).unwrap(), "restored");
    drop(environment);
    assert_eq!(std::env::var_os(NAME), original);
}

#[tokio::test]
async fn mock_provider_is_ready_and_shuts_down() {
    let provider = MockProvider::spawn(["fixture response"]).await.unwrap();
    let health = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
        .get(provider.health_url())
        .send()
        .await
        .unwrap();

    assert!(health.status().is_success());
    provider.shutdown().await;
}

#[tokio::test]
async fn provider_test_process_is_ready_and_shuts_down() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let settings = provider.provider_settings();
    let client = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(
                settings.ca_certificate_pem.as_ref().unwrap().as_bytes(),
            )
            .unwrap(),
        )
        .build()
        .unwrap();

    assert!(
        client
            .get(provider.health_url())
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    provider.shutdown();
}
