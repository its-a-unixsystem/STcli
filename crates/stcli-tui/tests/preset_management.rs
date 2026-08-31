use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use stcli_core::{ArtifactKind, EngineInspection, EngineQuery, EngineResult, StcliEngine, Store};
use stcli_testkit::fixtures;
use stcli_tui::{
    App, ChatFocus, Config, Effect, ImportArtifactState, ModalTarget, Popup, PresetPickerState,
    render as render_ui,
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
async fn preset_import_applies_custom_name_from_the_tui_form() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let preset_path = directory.path().join("embedded-name.json");
    fs::write(&preset_path, scripted_preset("Embedded")).unwrap();
    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Char('i'));

    let Some(Popup::ImportArtifact(state)) = &mut app.popup else {
        panic!("expected preset import dialog");
    };
    state.input = preset_path.display().to_string();
    assert!(render(&mut app).contains("Name"));
    press(&mut app, KeyCode::Tab);
    for character in "Custom".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Up);

    let effect = press(&mut app, KeyCode::Enter);
    let EngineResult::ArtifactBundle { primary, .. } = execute(&mut app, effect).await else {
        panic!("expected artifact import result");
    };

    let EngineInspection::ArtifactSource(source) = app
        .engine
        .inspect(EngineQuery::ArtifactSource {
            revision_hash: primary.revision_hash,
        })
        .unwrap()
    else {
        panic!("expected imported preset source");
    };
    let source: serde_json::Value = serde_json::from_slice(&source).unwrap();
    assert_eq!(source["preset_name"], "Custom");
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected preset picker after import");
    };
    assert_eq!(state.rows[state.selected - 1].label, "Custom");
}

#[tokio::test]
async fn preset_import_uses_filename_stem_when_name_is_omitted() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let preset_path = directory.path().join("filename-fallback.json");
    let mut preset: serde_json::Value =
        serde_json::from_slice(&scripted_preset("Embedded")).unwrap();
    preset.as_object_mut().unwrap().remove("name");
    fs::write(&preset_path, serde_json::to_vec(&preset).unwrap()).unwrap();
    let mut store = Store::open(&database).unwrap();
    store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    drop(store);
    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Char('i'));
    let Some(Popup::ImportArtifact(state)) = &mut app.popup else {
        panic!("expected preset import dialog");
    };
    state.input = preset_path.display().to_string();

    let effect = press(&mut app, KeyCode::Enter);
    let EngineResult::ArtifactBundle { primary, .. } = execute(&mut app, effect).await else {
        panic!("expected artifact import result");
    };

    let EngineInspection::ArtifactSource(source) = app
        .engine
        .inspect(EngineQuery::ArtifactSource {
            revision_hash: primary.revision_hash,
        })
        .unwrap()
    else {
        panic!("expected imported preset source");
    };
    let source: serde_json::Value = serde_json::from_slice(&source).unwrap();
    assert_eq!(source["preset_name"], "filename-fallback");
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected preset picker after import");
    };
    assert_eq!(state.rows[state.selected - 1].label, "filename-fallback");

    app.open_new_session_popup();
    let Some(Popup::NewSession(state)) = &app.popup else {
        panic!("expected new session form");
    };
    assert!(
        state
            .presets
            .iter()
            .any(|preset| preset.label == "filename-fallback")
    );
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
    assert!(
        rendered.contains("Enter select · c copy · i import · d details · / filter · PgUp/PgDn scroll details · Esc close")
    );
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

#[tokio::test]
async fn prompt_order_toggle_works_from_sessions_and_new_session() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    store
        .import_artifact(&scripted_preset("Toggle preset"))
        .unwrap();
    drop(store);
    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Right);
    let sessions_effect = press(&mut app, KeyCode::Char(' '));
    assert!(matches!(
        &sessions_effect,
        Effect::Execute(stcli_core::EngineCommand::UpdatePromptOrder {
            session_id: None,
            ..
        })
    ));

    app.open_new_session_popup();
    let Some(Popup::NewSession(state)) = &mut app.popup else {
        panic!("expected new session modal");
    };
    state.selected_preset = 1;
    state.focused_field = 2;
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Right);
    let effect = press(&mut app, KeyCode::Char(' '));
    assert!(matches!(
        &effect,
        Effect::Execute(stcli_core::EngineCommand::UpdatePromptOrder {
            session_id: None,
            ..
        })
    ));
    execute(&mut app, effect).await;
    let Some(Popup::NewSession(state)) = &app.popup else {
        panic!("expected preserved new session modal");
    };
    assert_eq!(state.selected_preset, 2);
    assert_eq!(
        app.toast.as_ref().map(|toast| toast.message.as_str()),
        Some("Updated preset prompt order")
    );
}

#[tokio::test]
async fn preset_copy_opens_patch_form_and_selects_imported_clone() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let source_bytes = scripted_preset("Source");
    let mut store = Store::open(&database).unwrap();
    let source = store.import_artifact(&source_bytes).unwrap();
    drop(store);
    let mut app = App::load(StcliEngine::new(database.clone()), Config::default(), None).unwrap();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Down);

    press(&mut app, KeyCode::Char('c'));

    let Some(Popup::ClonePreset(state)) = &mut app.popup else {
        panic!("expected preset clone form");
    };
    assert_eq!(state.name, "Source-copy");
    assert_eq!(state.temperature, "0.7");
    assert_eq!(state.max_context, "8192");
    assert_eq!(state.max_tokens, "512");
    assert!(state.use_sysprompt);
    state.name = "Tuned Source".to_owned();
    state.temperature = "0.9".to_owned();
    state.max_context = "16384".to_owned();
    state.max_tokens = "1024".to_owned();
    state.use_sysprompt = false;

    let effect = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let EngineResult::ArtifactBundle { primary, .. } = execute(&mut app, effect).await else {
        panic!("expected artifact import result");
    };

    assert_ne!(primary.revision_hash, source.revision_hash);
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected preset picker after clone");
    };
    assert_eq!(state.rows[state.selected - 1].label, "Tuned Source");
    assert_eq!(
        state.rows[state.selected - 1].record.revision_hash,
        primary.revision_hash
    );
    let EngineInspection::ArtifactSource(clone_bytes) = app
        .engine
        .inspect(EngineQuery::ArtifactSource {
            revision_hash: primary.revision_hash,
        })
        .unwrap()
    else {
        panic!("expected cloned artifact source");
    };
    let source_json: serde_json::Value = serde_json::from_slice(&source_bytes).unwrap();
    let clone_json: serde_json::Value = serde_json::from_slice(&clone_bytes).unwrap();
    assert_eq!(clone_json["preset_name"], "Tuned Source");
    assert_eq!(clone_json["prompts"], source_json["prompts"]);
    assert_eq!(clone_json["prompt_order"], source_json["prompt_order"]);
    assert_eq!(clone_json["extensions"], source_json["extensions"]);
}

#[test]
fn artifact_import_rejects_wrong_kinds_without_writing() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let card_path = directory.path().join("character.json");
    fs::write(&card_path, fixtures::minimal_card()).unwrap();
    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    let mut import_state = ImportArtifactState::new(
        Some(ArtifactKind::ChatCompletionPreset),
        ModalTarget::Sessions,
        directory.path().to_path_buf(),
    );
    import_state.input = card_path.display().to_string();
    app.popup = Some(Popup::ImportArtifact(import_state));

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

fn scrollable_preset() -> Vec<u8> {
    let prompts: Vec<_> = (1..=40)
        .map(|index| {
            serde_json::json!({
                "identifier": format!("slot-{index:02}"),
                "role": "system",
                "content": "test",
            })
        })
        .collect();
    let order: Vec<_> = (1..=40)
        .map(|index| serde_json::json!({"identifier": format!("slot-{index:02}"), "enabled": true}))
        .collect();
    serde_json::to_vec(&serde_json::json!({
        "name": "Scrollable",
        "prompts": prompts,
        "prompt_order": [{"character_id": 100001, "order": order}],
    }))
    .unwrap()
}

fn preset_picker(app: &App) -> &PresetPickerState {
    let Some(Popup::Presets(state)) = &app.popup else {
        panic!("expected preset picker");
    };
    state
}

#[test]
fn preset_details_scrolling_clamps_and_resets_on_selection_change() {
    // Regression test for .scratch/tui-preset-management/issues/03: the detail
    // inspector rendered a static paragraph with no way to read overflowing metadata.
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    store.import_artifact(&scrollable_preset()).unwrap();
    drop(store);

    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(preset_picker(&app).details_scroll, 0);
    assert!(!render(&mut app).contains("40. slot-40"));

    press(&mut app, KeyCode::PageDown);
    press(&mut app, KeyCode::PageDown);
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert_eq!(preset_picker(&app).details_scroll, 20);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(preset_picker(&app).details_scroll, 10);
    press(&mut app, KeyCode::PageUp);
    press(&mut app, KeyCode::PageUp);
    app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert_eq!(preset_picker(&app).details_scroll, 0);

    for _ in 0..10 {
        press(&mut app, KeyCode::PageDown);
    }
    // 120x40 terminal: 56 content lines against 27 visible detail rows.
    let rendered = render(&mut app);
    assert_eq!(preset_picker(&app).details_scroll, 29);
    assert!(rendered.contains("40. slot-40"));
    assert!(rendered.contains("Generation Parameters"));
    assert!(!rendered.contains("1. slot-01"));

    press(&mut app, KeyCode::Up);
    assert_eq!(preset_picker(&app).selected, 0);
    assert_eq!(preset_picker(&app).details_scroll, 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(preset_picker(&app).details_scroll, 0);

    // Shift+Down falls through to list navigation while details are hidden.
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Up);
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
    assert_eq!(preset_picker(&app).selected, 1);
}

#[test]
fn preset_details_scroll_resets_when_filter_swaps_the_same_index() {
    // Filtering can land a different preset on the same selection index; the
    // scroll offset must reset with it (ticket 03, AC1).
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    store.import_artifact(&scrollable_preset()).unwrap();
    let mut other =
        serde_json::from_slice::<serde_json::Value>(&scripted_preset("Second")).unwrap();
    other["name"] = serde_json::json!("Tiny");
    store
        .import_artifact(&serde_json::to_vec(&other).unwrap())
        .unwrap();
    drop(store);

    let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('d'));
    for _ in 0..3 {
        press(&mut app, KeyCode::PageDown);
    }
    assert!(preset_picker(&app).details_scroll > 0);

    // Filter down to the single other preset; it takes over index 1.
    press(&mut app, KeyCode::Char('/'));
    for character in "Tiny".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let state = preset_picker(&app);
    assert_eq!(state.selected, 1);
    assert_eq!(state.details_scroll, 0);
}
