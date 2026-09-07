use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use stcli_core::{EngineInspection, EngineQuery, InteractionValue, StcliEngine, Store};
use stcli_testkit::{configuration, fixtures};
use stcli_tui::{App, Config, Effect, InteractionDraftValue, Popup, render as render_ui};
use tempfile::tempdir;

fn press(app: &mut App, code: KeyCode) -> Effect {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press_ctrl(app: &mut App, character: char) -> Effect {
    app.handle_key(KeyEvent::new(
        KeyCode::Char(character),
        KeyModifiers::CONTROL,
    ))
}

fn render_at(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render_ui(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn render(app: &mut App) -> String {
    render_at(app, 120, 40)
}

#[tokio::test]
async fn extension_interaction_surface_opens_as_generic_form() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins {
            plugin_id: Some("memory".to_owned()),
        })
        .unwrap()
    else {
        panic!("plugins")
    };
    let memory = plugins.into_iter().next().unwrap();
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.plugins.push(stcli_core::PluginPin {
        id: memory.manifest.id.clone(),
        version: memory.manifest.version.to_string(),
        component_hash: memory.manifest.component_sha256.clone(),
        capabilities: stcli_core::st_bridge_capability_tier(),
        settings: serde_json::json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    });
    let created = store.create_session(config, 0).unwrap();
    drop(store);
    let mut app = App::load(
        StcliEngine::new(database),
        Config::default(),
        Some(created.session.session_id),
    )
    .unwrap();
    press(&mut app, KeyCode::Esc);
    let effect = press(&mut app, KeyCode::Char('E'));
    assert!(matches!(effect, Effect::None));
    press(&mut app, KeyCode::Enter);
    assert!(render(&mut app).contains("Freeze automatic refresh"));
    assert!(render(&mut app).contains("Summarize now"));
    assert!(matches!(app.popup, Some(Popup::InteractionForm(_))));
}

#[tokio::test]
async fn extension_interaction_drafts_cancel_and_save_through_typed_targets() {
    // Regression test for ticket 02: native drafts stay local and preserve multiline text.
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins {
            plugin_id: Some("memory".to_owned()),
        })
        .unwrap()
    else {
        panic!("plugins")
    };
    let memory = plugins.into_iter().next().unwrap();
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.plugins.push(stcli_core::PluginPin {
        id: memory.manifest.id,
        version: memory.manifest.version.to_string(),
        component_hash: memory.manifest.component_sha256,
        capabilities: stcli_core::st_bridge_capability_tier(),
        settings: serde_json::json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    });
    let created = store.create_session(config, 0).unwrap();
    drop(store);
    let mut app = App::load(
        StcliEngine::new(&database),
        Config::default(),
        Some(created.session.session_id),
    )
    .unwrap();
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('E'));
    press(&mut app, KeyCode::Enter);
    let before = match app
        .engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record.revision_hash,
        _ => panic!(),
    };

    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Tab);
    let Some(Popup::InteractionForm(state)) = &mut app.popup else {
        panic!("form")
    };
    state.focused = 2;
    state.cursor_position = match &state.drafts[2].value {
        InteractionDraftValue::Text(value) => value.chars().count(),
        _ => 0,
    };
    press(&mut app, KeyCode::Enter);
    for character in "Extra".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    assert!(render_at(&mut app, 80, 24).contains("Summary prompt"));
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('E'));
    press(&mut app, KeyCode::Enter);
    let unchanged = match app
        .engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record.revision_hash,
        _ => panic!(),
    };
    assert_eq!(unchanged, before);

    let Some(Popup::InteractionForm(state)) = &mut app.popup else {
        panic!("form")
    };
    state.drafts[1].value = InteractionDraftValue::Boolean(true);
    state.drafts[2].value = InteractionDraftValue::Text("First line\nSecond line".to_owned());
    state.drafts[3].value = InteractionDraftValue::Number("123".to_owned());
    state.drafts[8].value = InteractionDraftValue::Text("Memory: {{summary}}".to_owned());
    state.drafts[9].value = InteractionDraftValue::Choice(InteractionValue::Number(1.into()));
    state.focused = state.drafts.len() + 1;
    let blocked = press(&mut app, KeyCode::Enter);
    assert!(matches!(blocked, Effect::None));
    let Some(Popup::InteractionForm(state)) = &mut app.popup else {
        panic!("form")
    };
    assert_eq!(
        state.notice.as_deref(),
        Some("Save or discard edits before running this action")
    );
    let effect = press_ctrl(&mut app, 's');
    assert!(matches!(app.popup, Some(Popup::InteractionForm(_))));
    let Effect::Execute(command) = effect else {
        panic!("save command")
    };
    let result = app.engine.execute(command, |_| {}).await.unwrap();
    assert!(app.finish_command(Ok(result)));
    let Some(Popup::InteractionForm(state)) = &app.popup else {
        panic!("saved form")
    };
    assert!(!state.dirty());
    assert!(
        matches!(&state.drafts[2].value, InteractionDraftValue::Text(value) if value == "First line\nSecond line")
    );
    let configuration = match app
        .engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record,
        _ => panic!(),
    };
    assert_ne!(configuration.revision_hash, before);
    assert_eq!(
        configuration.configuration.plugins[0].settings["promptWords"],
        123
    );
    assert_eq!(
        configuration.configuration.plugins[0].settings["prompt"],
        "First line\nSecond line"
    );
}
