use ratatui::{Terminal, backend::TestBackend};
use stcli_core::{StcliEngine, Store};
use stcli_testkit::{MockProvider, configuration, fixtures};
use stcli_tui::{App, Config, render};

#[tokio::test]
async fn submitted_roll_macro_is_sent_and_displayed_as_its_outcome() {
    // Regression test for issue #133: the TUI bubble must show the same expanded
    // user input that the provider receives, not the raw roll macro.
    let provider = MockProvider::spawn(["Response"]).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut session_configuration = configuration(character.revision_hash);
    session_configuration.provider = provider.provider_settings();
    let created = store.create_session(session_configuration, 0).unwrap();

    let completed = store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Rolled {{roll:d20}}".to_owned(),
            |_| {},
        )
        .await
        .unwrap();
    let provider_messages = completed
        .attempt
        .effect_receipt
        .as_ref()
        .unwrap()
        .provider_request["messages"]
        .as_array()
        .unwrap();
    let expanded_input = provider_messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .unwrap();
    assert!(expanded_input.starts_with("Rolled "));
    assert!(!expanded_input.contains("{{roll"));
    drop(store);

    let mut app = App::load(
        StcliEngine::new(database),
        Config::default(),
        Some(created.session.session_id),
    )
    .unwrap();
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, &mut app)).unwrap();
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();

    assert!(rendered.contains(expanded_input));
    assert!(!rendered.contains("{{roll:d20}}"));
    provider.shutdown().await;
}
