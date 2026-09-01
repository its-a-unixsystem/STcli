use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use stcli_core::{EngineCommand, EngineResult, StcliEngine, Store};
use stcli_testkit::{MockProvider, configuration, fixtures};
use stcli_tui::{App, ChatFocus, Config, Effect, Popup};
use tempfile::tempdir;

fn press(app: &mut App, code: KeyCode) -> Effect {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[tokio::test]
async fn chat_generation_settings_override_is_used_by_regeneration() {
    // Regression test: an incompatible preset reasoning level must be recoverable in Chat.
    let provider = MockProvider::spawn(["First response", "Regenerated response"])
        .await
        .unwrap();
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.provider = provider.provider_settings();
    config.generation_settings = json!({
        "reasoning_effort": "auto",
        "temperature": 0.7,
        "max_tokens": 512
    });
    let created = store.create_session(config, 0).unwrap();
    store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello".to_owned(),
            |_| {},
        )
        .await
        .unwrap();
    drop(store);

    let mut app = App::load(
        StcliEngine::new(database),
        Config::default(),
        Some(created.session.session_id),
    )
    .unwrap();
    app.chat_focus = ChatFocus::History;

    assert!(matches!(press(&mut app, KeyCode::Char('s')), Effect::None));
    let Some(Popup::GenerationSettings(state)) = &mut app.popup else {
        panic!("expected generation settings popup");
    };
    assert_eq!(state.reasoning_effort, "auto");
    assert_eq!(state.temperature, "0.7");
    assert_eq!(state.max_tokens, "512");
    state.reasoning_effort = "high".to_owned();
    state.temperature = "0.3".to_owned();
    state.max_tokens = "1024".to_owned();

    let effect = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let Effect::Execute(EngineCommand::UpdateConfiguration { configuration, .. }) = effect else {
        panic!("expected configuration update");
    };
    assert_eq!(
        configuration.generation_settings["reasoning_effort"],
        "high"
    );
    assert_eq!(configuration.generation_settings["temperature"], 0.3);
    assert_eq!(configuration.generation_settings["max_tokens"], 1024);
    let result = app
        .engine
        .execute(
            EngineCommand::UpdateConfiguration {
                session_id: created.session.session_id,
                configuration,
            },
            |_| {},
        )
        .await
        .unwrap();
    app.finish_command(Ok(result));
    assert!(app.popup.is_none());
    assert_eq!(
        app.toast.as_ref().map(|toast| toast.message.as_str()),
        Some("Updated generation settings")
    );

    let Effect::Start(command @ EngineCommand::Regenerate { .. }) =
        press(&mut app, KeyCode::Char('r'))
    else {
        panic!("expected regeneration command");
    };
    assert!(matches!(
        app.engine.execute(command, |_| {}).await,
        Ok(EngineResult::CompletedTurn(_))
    ));
}
