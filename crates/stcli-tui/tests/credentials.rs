use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use stcli_core::StcliEngine;
use stcli_tui::{App, Config, ModalTarget, Popup, render};
use tempfile::tempdir;

fn rendered(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn provider_profile_accepts_a_masked_credential_store_secret() {
    // Regression test: TUI secret entry must never render the raw API key.
    let directory = tempdir().unwrap();
    let mut app = App::load(
        StcliEngine::new(directory.path().join("stcli.sqlite3")),
        Config::default(),
        None,
    )
    .unwrap();
    app.open_provider_profile_popup(None, ModalTarget::Sessions);
    let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
        panic!("expected provider profile popup");
    };
    state.focused_field = 5;

    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
        panic!("expected provider profile popup");
    };
    assert!(state.use_credential_store);
    state.credential_key = "openrouter".to_owned();
    state.credential_secret = "raw-secret-value".to_owned();

    let output = rendered(&mut app);
    assert!(output.contains("Credential Alias"));
    assert!(output.contains("API Key (Secret)"));
    assert!(output.contains("••••••••••••••••"));
    assert!(!output.contains("raw-secret-value"));
}
