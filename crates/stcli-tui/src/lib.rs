mod app;
mod clipboard;
pub mod config;
mod markdown;
mod terminal;
mod theme;
mod ui;

use std::time::Duration;

use anyhow::Result;
use app::GenerationState;
use crossterm::event::{self, Event, KeyEventKind};
use stcli_core::{AppPaths, EngineCommand, EngineResult, EntityId, ProviderEvent, StcliEngine};
use terminal::TerminalSession;
use tokio::sync::mpsc;

pub use app::{
    App, ChatFocus, ClonePresetState, Effect, GenerationSettingsState, ImportArtifactState,
    ImportPersonasState, ModalTarget, PersonaEditorState, PersonasState, Popup, PresetOption,
    PresetPickerState, PresetScriptSummary, PresetSummary, Screen,
};
pub use config::{Config, ThemeChoice, TuiSettings};
pub use ui::render;

pub fn run(paths: &AppPaths, direct_session: Option<EntityId>) -> Result<()> {
    paths.ensure_exists()?;
    let config = Config::load(&paths.config)?;
    let engine = StcliEngine::new(paths.database()).with_config_directory(&paths.config);
    let mut app = App::load(engine, config, direct_session)?;
    app.set_config_dir(paths.config.clone());
    let mut terminal = TerminalSession::enter()?;
    run_loop(terminal.terminal(), app)
}

enum RuntimeEvent {
    Provider(ProviderEvent),
    GenerationFinished(Result<EngineResult, String>),
    CommandFinished(Result<EngineResult, String>),
    CancelledForExit(Result<EngineResult, String>),
}

#[derive(Default)]
struct ExitProgress {
    generation_finished: bool,
    cancellation_finished: bool,
}

impl ExitProgress {
    fn generation_finished(&mut self) -> bool {
        self.generation_finished = true;
        self.cancellation_finished
    }

    fn cancellation_finished(&mut self) -> bool {
        self.cancellation_finished = true;
        self.generation_finished
    }
}

fn run_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    mut app: App,
) -> Result<()> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut exit_progress: Option<ExitProgress> = None;
    loop {
        app.tick();
        terminal.draw(|frame| ui::render(frame, &mut app))?;

        while let Ok(runtime_event) = receiver.try_recv() {
            match runtime_event {
                RuntimeEvent::Provider(event) => app.handle_provider_event(event),
                RuntimeEvent::GenerationFinished(result) => {
                    app.finish_generation(result);
                    if exit_progress
                        .as_mut()
                        .is_some_and(ExitProgress::generation_finished)
                    {
                        return Ok(());
                    }
                }
                RuntimeEvent::CommandFinished(result) => {
                    app.finish_command(result);
                }
                RuntimeEvent::CancelledForExit(result) => match result {
                    Ok(_) => {
                        if exit_progress
                            .as_mut()
                            .is_some_and(ExitProgress::cancellation_finished)
                        {
                            return Ok(());
                        }
                    }
                    Err(error) => {
                        if exit_progress
                            .as_ref()
                            .is_some_and(|progress| progress.generation_finished)
                        {
                            return Ok(());
                        }
                        exit_progress = None;
                        app.finish_command(Err(error));
                    }
                },
            }
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let effect = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Event::Mouse(mouse) => app.handle_mouse(mouse),
            Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Paste(_) => {
                Effect::None
            }
            Event::Key(_) => Effect::None,
        };
        match effect {
            Effect::None => {}
            Effect::Quit => return Ok(()),
            Effect::Copy(content) => match clipboard::copy(&content) {
                Ok(()) => app.show_info("Copied original message content"),
                Err(error) => app.show_error(error.to_string()),
            },
            Effect::Start(command) => {
                if app.generation.is_none() {
                    let streaming = app
                        .history
                        .as_ref()
                        .is_some_and(|history| history.configuration.configuration.provider.stream);
                    let continues = matches!(&command, EngineCommand::Continue { .. });
                    app.generation = Some(GenerationState {
                        partial: String::new(),
                        reasoning: String::new(),
                        streaming,
                        pending_input: None,
                        continues,
                    });
                }
                spawn_engine(
                    app.engine.clone(),
                    command,
                    sender.clone(),
                    Completion::Generation,
                );
            }
            Effect::Execute(command) => {
                spawn_engine(
                    app.engine.clone(),
                    command,
                    sender.clone(),
                    Completion::Command,
                );
            }
            Effect::CancelAndQuit(command) => {
                exit_progress = Some(ExitProgress::default());
                spawn_engine(
                    app.engine.clone(),
                    command,
                    sender.clone(),
                    Completion::Exit,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Completion {
    Generation,
    Command,
    Exit,
}

fn spawn_engine(
    engine: StcliEngine,
    command: EngineCommand,
    sender: mpsc::UnboundedSender<RuntimeEvent>,
    completion: Completion,
) {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("TUI worker runtime must initialize");
        let event_sender = sender.clone();
        let result = runtime
            .block_on(engine.execute(command, move |event| {
                let _ = event_sender.send(RuntimeEvent::Provider(event.clone()));
            }))
            .map_err(|error| error.to_string());
        let event = match completion {
            Completion::Generation => RuntimeEvent::GenerationFinished(result),
            Completion::Command => RuntimeEvent::CommandFinished(result),
            Completion::Exit => RuntimeEvent::CancelledForExit(result),
        };
        let _ = sender.send(event);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use stcli_core::Store;
    use stcli_testkit::{configuration, fixtures};
    use tempfile::tempdir;

    #[test]
    fn app_loads_a_direct_session_through_inspection() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = Store::open(&database).unwrap();
        let character = store
            .import_artifact(fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(configuration(character.revision_hash), 0)
            .unwrap();
        let app = App::load(
            StcliEngine::new(database),
            Config::default(),
            Some(created.session.session_id),
        )
        .unwrap();
        assert_eq!(app.screen, app::Screen::Chat);
        assert_eq!(
            app.history.unwrap().session.session_id,
            created.session.session_id
        );
    }

    #[test]
    fn confirmed_exit_waits_for_cancel_and_generation_completion() {
        let mut cancel_first = ExitProgress::default();
        assert!(!cancel_first.cancellation_finished());
        assert!(cancel_first.generation_finished());

        let mut generation_first = ExitProgress::default();
        assert!(!generation_first.generation_finished());
        assert!(generation_first.cancellation_finished());
    }
}
