use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use stcli_core::{ArtifactKind, EngineInspection, EngineQuery, EngineResult, StcliEngine, Store};
use stcli_testkit::fixtures;
use stcli_tui::{
    App, ChatFocus, Config, Effect, ImportArtifactState, ModalTarget, Popup, render as render_ui,
};
use tempfile::tempdir;

fn press(app: &mut App, code: KeyCode) -> Effect {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

async fn execute(app: &mut App, effect: Effect) -> EngineResult {
    let Effect::Execute(command) = effect else {
        panic!("expected command execution");
    };
    let result = app.engine.execute(command, |_| {}).await.unwrap();
    app.finish_command(Ok(result.clone()));
    result
}

fn scripted_preset(name: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "name": name,
        "temperature": 0.7,
        "top_p": 0.9,
        "max_tokens": 512,
        "prompts": [{"identifier": "main", "role": "system", "content": "test"}],
        "prompt_order": [{"character_id": 100001, "order": [
            {"identifier": "main", "enabled": true}
        ]}],
        "extensions": {"regex_scripts": [
            {"id": "one", "scriptName": "One", "placement": [1]},
            {"id": "two", "scriptName": "Two", "placement": [2]}
        ]}
    }))
    .unwrap()
}

fn render(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| render_ui(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[tokio::test]
async fn preset_management_covers_import_inspection_filtering_and_navigation() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let first_path = directory.path().join("scripted-preset.json");
    let second_path = directory.path().join("second-preset.json");
    fs::write(&first_path, scripted_preset("Scripted")).unwrap();
    fs::write(&second_path, scripted_preset("Second")).unwrap();

    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let session = store
        .create_session(stcli_testkit::configuration(character.revision_hash), 0)
        .unwrap();
    drop(store);

    let mut app = App::load(StcliEngine::new(database.clone()), Config::default(), None).unwrap();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Char('i'));

    let Some(Popup::ImportArtifact(state)) = &mut app.popup else {
        panic!("expected preset import dialog");
    };
    state.input = first_path.display().to_string();
    let effect = press(&mut app, KeyCode::Enter);
    let EngineResult::ArtifactBundle { primary, .. } = execute(&mut app, effect).await else {
        panic!("expected artifact import result");
    };
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected preset picker after import");
    };
    assert_eq!(state.selected, 1);
    assert_eq!(state.rows[0].record.revision_hash, primary.revision_hash);
    assert_eq!(
        app.toast.as_ref().map(|toast| toast.message.as_str()),
        Some("Imported preset 'Scripted' (contains 2 untrusted scripts)")
    );
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('d'));
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected filtered preset picker");
    };
    assert!(state.filtering);
    assert_eq!(state.filter, "d");
    assert_eq!(state.selected, 1);
    assert!(render(&mut app).contains("Scripted"));
    press(&mut app, KeyCode::Esc);
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected picker after clearing filter");
    };
    assert!(!state.filtering);
    assert!(state.filter.is_empty());
    assert_eq!(state.selected, 1);

    press(&mut app, KeyCode::Char('d'));
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected preset picker");
    };
    assert!(state.show_details);
    assert_eq!(state.rows[0].summary.prompt_count, 1);
    assert_eq!(
        state.rows[0].summary.order_profile,
        "Chat Completion (100001)"
    );
    assert!(state.rows[0].summary.system_prompt_enabled);
    assert_eq!(state.rows[0].summary.scripts.len(), 2);
    assert_eq!(state.rows[0].summary.scripts[0].digest.len(), 12);
    assert!(
        state.rows[0].summary.scripts[0]
            .digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    let rendered = render(&mut app);
    assert!(rendered.contains("Generation Parameters"));
    assert!(rendered.contains("Temperature: 0.7"));
    assert!(rendered.contains("One · UserInput"));
    assert!(rendered.contains("[inert — requires grant]"));
    assert!(rendered.contains("Enter select · i import · d details · / filter · Esc close"));
    press(&mut app, KeyCode::Tab);
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected preset picker");
    };
    assert!(!state.show_details);
    press(&mut app, KeyCode::Tab);
    assert!(matches!(app.popup, Some(Popup::Presets(_))));

    press(&mut app, KeyCode::Esc);
    assert!(app.popup.is_none());

    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.popup.is_none());
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('n'));
    let Some(Popup::NewSession(state)) = &mut app.popup else {
        panic!("expected new session modal");
    };
    assert_eq!(state.selected_preset, 1);
    state.focused_field = 2;
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Enter);
    let Some(Popup::ImportArtifact(state)) = &mut app.popup else {
        panic!("expected preset import dialog");
    };
    assert_eq!(
        state.expected_kind,
        Some(ArtifactKind::ChatCompletionPreset)
    );
    state.input = second_path.display().to_string();
    let effect = press(&mut app, KeyCode::Enter);
    execute(&mut app, effect).await;
    let Some(Popup::NewSession(state)) = &app.popup else {
        panic!("expected new session modal after import");
    };
    assert_eq!(state.presets[state.selected_preset - 1].label, "Second");

    let mut chat_app = App::load(
        StcliEngine::new(database),
        Config::default(),
        Some(session.session.session_id),
    )
    .unwrap();
    chat_app.chat_focus = ChatFocus::History;
    press(&mut chat_app, KeyCode::Char('P'));
    press(&mut chat_app, KeyCode::Down);
    let effect = press(&mut chat_app, KeyCode::Enter);
    assert!(matches!(
        effect,
        Effect::Execute(stcli_core::EngineCommand::UpdateConfiguration { .. })
    ));
}

#[test]
fn artifact_import_rejects_wrong_kinds_without_writing() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let card_path = directory.path().join("character.json");
    fs::write(&card_path, fixtures::minimal_card()).unwrap();
    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    app.popup = Some(Popup::ImportArtifact(ImportArtifactState {
        expected_kind: Some(ArtifactKind::ChatCompletionPreset),
        return_to: ModalTarget::Sessions,
        input: card_path.display().to_string(),
    }));

    let effect = press(&mut app, KeyCode::Enter);

    assert!(matches!(effect, Effect::None));
    let Some(Popup::ImportArtifact(state)) = &app.popup else {
        panic!("expected import dialog to remain open");
    };
    assert_eq!(state.input, card_path.display().to_string());
    assert_eq!(
        app.toast.as_ref().map(|toast| toast.message.as_str()),
        Some("File is a character-card-v2, expected a chat-completion-preset")
    );
    let EngineInspection::Artifacts(records) = app
        .engine
        .inspect(EngineQuery::Artifacts {
            kind: Some(ArtifactKind::CharacterCardV2),
        })
        .unwrap()
    else {
        panic!("expected artifact records");
    };
    assert!(records.is_empty());

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let preset_path = directory.path().join("preset.json");
    fs::write(&preset_path, fixtures::preset()).unwrap();
    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    app.open_new_session_popup();
    let Some(Popup::ImportArtifact(state)) = &mut app.popup else {
        panic!("expected character import dialog");
    };
    state.input = preset_path.display().to_string();

    let effect = press(&mut app, KeyCode::Enter);

    assert!(matches!(effect, Effect::None));
    assert!(matches!(app.popup, Some(Popup::ImportArtifact(_))));
    assert_eq!(
        app.toast.as_ref().map(|toast| toast.message.as_str()),
        Some("File is a chat-completion-preset, expected a character-card")
    );
    let EngineInspection::Artifacts(records) = app
        .engine
        .inspect(EngineQuery::Artifacts {
            kind: Some(ArtifactKind::ChatCompletionPreset),
        })
        .unwrap()
    else {
        panic!("expected artifact records");
    };
    assert!(records.is_empty());
}
