use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use stcli_core::{
    ArtifactKind, ArtifactRecord, AttemptStatus, BranchHistory, BranchProjection,
    CandidateProjection, ContentHash, EngineCommand, EngineInspection, EngineQuery, EngineResult,
    EntityId, ProviderEvent, ProviderSettings, ProviderTemplate, SessionConfiguration,
    SessionSummary, StcliEngine, decode_artifact, validate_provider_settings,
};

use crate::{config::Config, theme::Theme};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    Sessions,
    Chat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatFocus {
    Composer,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Modified,
    Created,
    Name,
    Turns,
    Tokens,
}

impl SortKey {
    fn next(self) -> Self {
        match self {
            Self::Modified => Self::Created,
            Self::Created => Self::Name,
            Self::Name => Self::Turns,
            Self::Turns => Self::Tokens,
            Self::Tokens => Self::Modified,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PresetOption {
    pub record: ArtifactRecord,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct CharacterOption {
    pub revision_hash: ContentHash,
    pub name: String,
    pub greeting_count: usize,
}

#[derive(Clone, Debug)]
pub struct NewSessionState {
    pub characters: Vec<CharacterOption>,
    pub selected_character: usize,
    pub providers: Vec<String>,
    pub selected_provider: usize,
    pub presets: Vec<PresetOption>,
    pub selected_preset: usize,
    pub persona: String,
    pub selected_greeting: usize,
    pub focused_field: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ImportCharacterState {
    pub input: String,
    pub return_to_new_session: Option<Box<NewSessionState>>,
}

#[derive(Clone, Debug)]
pub struct NewProviderProfileState {
    pub templates: Vec<ProviderTemplate>,
    pub selected_template: usize,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub chat_path: String,
    pub api_key_env: String,
    pub stream: bool,
    pub timeout_seconds: u64,
    pub focused_field: usize,
    pub return_to_new_session: Option<Box<NewSessionState>>,
    pub return_to_providers: bool,
}

#[derive(Clone, Debug)]
pub enum Popup {
    Help,
    Branches {
        rows: Vec<BranchProjection>,
        selected: usize,
    },
    Providers {
        names: Vec<String>,
        selected: usize,
    },
    Presets {
        rows: Vec<PresetOption>,
        selected: usize,
    },
    Rename {
        session_id: EntityId,
        input: String,
    },
    ConfirmExit,
    ConfirmDelete {
        session_id: EntityId,
        name: String,
    },
    NewSession(Box<NewSessionState>),
    ImportCharacter(ImportCharacterState),
    NewProviderProfile(Box<NewProviderProfileState>),
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub message: String,
    pub error: bool,
    expires: Instant,
}

#[derive(Clone, Debug)]
pub struct GenerationState {
    pub partial: String,
    pub reasoning: String,
    pub streaming: bool,
    pub pending_input: Option<String>,
    pub continues: bool,
}

#[derive(Clone, Debug)]
pub enum Effect {
    None,
    Start(EngineCommand),
    Execute(EngineCommand),
    CancelAndQuit(EngineCommand),
    Copy(String),
    Quit,
}

#[derive(Clone, Debug)]
pub enum HitAction {
    Session(usize),
    Message(usize),
    CandidatePrevious,
    CandidateNext,
    GreetingPrevious,
    GreetingNext,
    PopupRow(usize),
    Composer,
    Stop,
    Regenerate,
    Continue,
}

#[derive(Clone, Debug)]
pub struct HitTarget {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub action: HitAction,
}

#[derive(Clone, Debug)]
pub enum SessionListEntry {
    Session(usize),
    Branch {
        session_index: usize,
        branch: BranchProjection,
    },
}

pub struct App {
    pub engine: StcliEngine,
    pub config: Config,
    pub theme: Theme,
    pub screen: Screen,
    pub sessions: Vec<SessionSummary>,
    pub selected_session: usize,
    pub show_branches: bool,
    pub session_branches: HashMap<EntityId, Vec<BranchProjection>>,
    pub sort: SortKey,
    pub filter: String,
    pub filtering: bool,
    pub history: Option<BranchHistory>,
    pub chat_focus: ChatFocus,
    pub focused_message: usize,
    pub scroll: u16,
    pub follow: bool,
    pub composer: String,
    pub popup: Option<Popup>,
    pub toast: Option<Toast>,
    pub generation: Option<GenerationState>,
    deletion_pending: bool,
    pub hit_targets: Vec<HitTarget>,
    toast_timeout: Duration,
    pub config_dir: Option<PathBuf>,
}

impl App {
    pub fn load(
        engine: StcliEngine,
        config: Config,
        direct_session: Option<EntityId>,
    ) -> anyhow::Result<Self> {
        let theme = Theme::resolve(config.tui.theme);
        let toast_timeout = Duration::from_secs(config.tui.toast_timeout.max(1));
        let mut app = Self {
            engine,
            config,
            theme,
            screen: Screen::Sessions,
            sessions: Vec::new(),
            selected_session: 0,
            show_branches: false,
            session_branches: HashMap::new(),
            sort: SortKey::Modified,
            filter: String::new(),
            filtering: false,
            history: None,
            chat_focus: ChatFocus::Composer,
            focused_message: 0,
            scroll: 0,
            follow: true,
            composer: String::new(),
            popup: None,
            toast: None,
            generation: None,
            deletion_pending: false,
            hit_targets: Vec::new(),
            toast_timeout,
            config_dir: None,
        };
        app.reload_sessions()?;
        if let Some(session_id) = direct_session
            && let Err(error) = app.open_session(session_id)
        {
            app.show_error(error.to_string());
        }
        Ok(app)
    }

    pub fn set_config_dir(&mut self, path: PathBuf) {
        self.config_dir = Some(path);
    }

    pub fn reload_config(&mut self) -> anyhow::Result<()> {
        if let Some(dir) = &self.config_dir {
            self.config = Config::load(dir)?;
        }
        Ok(())
    }

    pub fn reload_sessions(&mut self) -> anyhow::Result<()> {
        let EngineInspection::Sessions(mut sessions) =
            self.engine.inspect(EngineQuery::Sessions)?
        else {
            unreachable!("sessions query returned another inspection type")
        };
        sort_sessions(&mut sessions, self.sort);
        self.sessions = sessions;
        if self.show_branches {
            self.reload_branches();
        }
        self.selected_session = self
            .selected_session
            .min(self.session_list_entries().len().saturating_sub(1));
        Ok(())
    }

    pub fn open_session(&mut self, session_id: EntityId) -> anyhow::Result<()> {
        let EngineInspection::Session(session) =
            self.engine.inspect(EngineQuery::Session { session_id })?
        else {
            unreachable!("session query returned another inspection type")
        };
        self.open_branch(session_id, session.root_branch_id)
    }

    pub fn open_branch(&mut self, session_id: EntityId, branch_id: EntityId) -> anyhow::Result<()> {
        let EngineInspection::BranchHistory(history) =
            self.engine.inspect(EngineQuery::BranchHistory {
                session_id,
                branch_id,
            })?
        else {
            unreachable!("branch history query returned another inspection type")
        };
        self.focused_message = message_count(&history).saturating_sub(1);
        self.history = Some(*history);
        self.chat_focus = ChatFocus::Composer;
        self.screen = Screen::Chat;
        self.scroll = u16::MAX;
        self.follow = true;
        Ok(())
    }

    pub fn reload_history(&mut self) -> anyhow::Result<()> {
        let Some(history) = &self.history else {
            return Ok(());
        };
        let session_id = history.session.session_id;
        let branch_id = history.branch.branch_id;
        let EngineInspection::BranchHistory(history) =
            self.engine.inspect(EngineQuery::BranchHistory {
                session_id,
                branch_id,
            })?
        else {
            unreachable!("branch history query returned another inspection type")
        };
        self.focused_message = self
            .focused_message
            .min(message_count(&history).saturating_sub(1));
        self.history = Some(*history);
        Ok(())
    }
    pub fn filtered_sessions(&self) -> Vec<&SessionSummary> {
        self.sessions
            .iter()
            .filter(|session| {
                self.filter.is_empty()
                    || fuzzy_match(
                        &self.filter,
                        &format!(
                            "{} {} {} {}",
                            session.display_name,
                            session.session_id,
                            session.character_label,
                            session.persona_label
                        ),
                    )
            })
            .collect()
    }

    pub fn session_list_entries(&self) -> Vec<SessionListEntry> {
        let filtered = self.filtered_sessions();
        let mut entries = Vec::new();
        for (i, session) in filtered.iter().enumerate() {
            entries.push(SessionListEntry::Session(i));
            if self.show_branches
                && let Some(branches) = self.session_branches.get(&session.session_id)
            {
                for branch in branches {
                    entries.push(SessionListEntry::Branch {
                        session_index: i,
                        branch: branch.clone(),
                    });
                }
            }
        }
        entries
    }

    fn reload_branches(&mut self) {
        self.session_branches.clear();
        for session in &self.sessions {
            if let Ok(EngineInspection::Branches(branches)) =
                self.engine.inspect(EngineQuery::Branches {
                    session_id: session.session_id,
                })
            {
                self.session_branches.insert(session.session_id, branches);
            }
        }
    }

    pub fn tick(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.expires <= Instant::now())
        {
            self.toast = None;
        }
    }

    pub fn show_error(&mut self, message: impl Into<String>) {
        self.toast = Some(Toast {
            message: message.into(),
            error: true,
            expires: Instant::now() + self.toast_timeout,
        });
    }

    pub fn show_info(&mut self, message: impl Into<String>) {
        self.toast = Some(Toast {
            message: message.into(),
            error: false,
            expires: Instant::now() + self.toast_timeout,
        });
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if self.deletion_pending {
            return Effect::None;
        }
        if let Some(popup) = self.popup.take() {
            return self.handle_popup(key, popup);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.request_quit();
        }
        match self.screen {
            Screen::Sessions => {
                if !self.filtering && key.code == KeyCode::Char('q') {
                    self.request_quit()
                } else {
                    self.handle_sessions_key(key)
                }
            }
            Screen::Chat => self.handle_chat_key(key),
        }
    }

    fn request_quit(&mut self) -> Effect {
        if self.generation.is_some() {
            self.popup = Some(Popup::ConfirmExit);
            Effect::None
        } else {
            Effect::Quit
        }
    }

    fn handle_sessions_key(&mut self, key: KeyEvent) -> Effect {
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter.push(character);
                }
                _ => {}
            }
            self.selected_session = self
                .selected_session
                .min(self.session_list_entries().len().saturating_sub(1));
            return Effect::None;
        }
        let list_len = self.session_list_entries().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected_session = self.selected_session.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected_session = (self.selected_session + 1).min(list_len.saturating_sub(1));
            }
            KeyCode::Home | KeyCode::Char('g') => self.selected_session = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected_session = list_len.saturating_sub(1),
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Esc if !self.filter.is_empty() => self.filter.clear(),
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                if let Err(error) = self.reload_sessions() {
                    self.show_error(error.to_string());
                }
            }
            KeyCode::Char('b') => {
                self.show_branches = !self.show_branches;
                if self.show_branches {
                    self.reload_branches();
                }
            }
            KeyCode::Char('x') => return self.delete_session_list_entry(),
            KeyCode::Char('r') => self.start_rename(),
            KeyCode::Char('n') => self.open_new_session_popup(),
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Enter => {
                let entries = self.session_list_entries();
                if let Some(entry) = entries.get(self.selected_session) {
                    match entry {
                        SessionListEntry::Session(i) => {
                            let filtered = self.filtered_sessions();
                            if let Some(session_id) = filtered.get(*i).map(|s| s.session_id)
                                && let Err(error) = self.open_session(session_id)
                            {
                                self.show_error(error.to_string());
                            }
                        }
                        SessionListEntry::Branch {
                            session_index,
                            branch,
                        } => {
                            let filtered = self.filtered_sessions();
                            if let Some(session_id) =
                                filtered.get(*session_index).map(|s| s.session_id)
                                && let Err(error) = self.open_branch(session_id, branch.branch_id)
                            {
                                self.show_error(error.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        Effect::None
    }

    fn handle_chat_key(&mut self, key: KeyEvent) -> Effect {
        if self.generation.is_some() {
            return match key.code {
                KeyCode::Esc => self.running_attempt().map_or(Effect::None, |attempt_id| {
                    Effect::Execute(EngineCommand::Cancel { attempt_id })
                }),
                KeyCode::Char('q') => self.request_quit(),
                KeyCode::Up | KeyCode::PageUp => {
                    self.scroll_up(if key.code == KeyCode::PageUp { 10 } else { 1 });
                    Effect::None
                }
                KeyCode::Down | KeyCode::PageDown => {
                    self.scroll_down(if key.code == KeyCode::PageDown { 10 } else { 1 });
                    Effect::None
                }
                KeyCode::End => {
                    self.follow_to_bottom();
                    Effect::None
                }
                _ => Effect::None,
            };
        }
        match self.chat_focus {
            ChatFocus::Composer => self.handle_composer_key(key),
            ChatFocus::History => self.handle_history_key(key),
        }
    }

    fn handle_composer_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab | KeyCode::Up => {
                self.chat_focus = ChatFocus::History;
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.composer.push('\n');
            }
            KeyCode::Enter => {
                if self.composer.trim().is_empty() {
                    if let Some(turn_id) = self.unanswered_turn_id() {
                        return Effect::Start(EngineCommand::Regenerate { turn_id });
                    }
                } else {
                    let history = self.history.as_ref().expect("chat has history");
                    let content = std::mem::take(&mut self.composer);
                    self.generation = Some(GenerationState {
                        partial: String::new(),
                        reasoning: String::new(),
                        streaming: history.configuration.configuration.provider.stream,
                        pending_input: Some(content.clone()),
                        continues: false,
                    });
                    return Effect::Start(EngineCommand::Send {
                        session_id: history.session.session_id,
                        branch_id: history.branch.branch_id,
                        content,
                    });
                }
            }
            KeyCode::Backspace => {
                self.composer.pop();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.push(character);
            }
            _ => {}
        }
        Effect::None
    }

    fn handle_history_key(&mut self, key: KeyEvent) -> Effect {
        match key.code {
            KeyCode::Char('q') => return self.request_quit(),
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Esc => {
                self.screen = Screen::Sessions;
                self.history = None;
            }
            KeyCode::Enter => {
                if let Some(turn_id) = self.focused_unanswered_turn_id() {
                    return Effect::Start(EngineCommand::Regenerate { turn_id });
                }
                self.chat_focus = ChatFocus::Composer;
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp => {
                self.scroll_up(if key.code == KeyCode::PageUp { 10 } else { 1 });
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => {
                if matches!(key.code, KeyCode::Down | KeyCode::Char('j'))
                    && let Some(history) = &self.history
                    && self.focused_message >= message_count(history).saturating_sub(1)
                {
                    self.chat_focus = ChatFocus::Composer;
                    return Effect::None;
                }
                self.scroll_down(if key.code == KeyCode::PageDown { 10 } else { 1 });
            }
            KeyCode::End | KeyCode::Char('G') => self.follow_to_bottom(),
            KeyCode::Home | KeyCode::Char('g') => {
                self.scroll = 0;
                self.follow = false;
            }
            KeyCode::Tab => {
                if let Some(history) = &self.history {
                    self.focused_message =
                        (self.focused_message + 1) % message_count(history).max(1);
                }
            }
            KeyCode::BackTab => {
                if let Some(history) = &self.history {
                    self.focused_message = self
                        .focused_message
                        .checked_sub(1)
                        .unwrap_or(message_count(history).saturating_sub(1));
                }
            }
            KeyCode::Char('b') => self.open_branch_popup(),
            KeyCode::Char('p') => self.open_provider_popup(),
            KeyCode::Char('P') => self.open_preset_popup(),
            KeyCode::Char('c') => {
                if let Some(content) = self.focused_content() {
                    return Effect::Copy(content.to_owned());
                }
            }
            KeyCode::Char('r') => {
                if let Some(turn_id) = self.current_turn_id() {
                    return Effect::Start(EngineCommand::Regenerate { turn_id });
                }
            }
            KeyCode::Char('e') => {
                if let Some(turn_id) = self.current_turn_id()
                    && self.current_candidate_id().is_some()
                {
                    return Effect::Start(EngineCommand::Continue { turn_id });
                }
            }
            KeyCode::Char('x') => return self.delete_focused(),
            KeyCode::Left => return self.navigate_focused(-1),
            KeyCode::Right => return self.navigate_focused(1),
            _ => {}
        }
        Effect::None
    }

    fn scroll_up(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_sub(amount);
        self.follow = false;
    }

    fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    fn follow_to_bottom(&mut self) {
        self.scroll = u16::MAX;
        self.follow = true;
    }

    fn handle_popup(&mut self, key: KeyEvent, mut popup: Popup) -> Effect {
        if key.code == KeyCode::Esc {
            match popup {
                Popup::ImportCharacter(mut state) => {
                    if let Some(session_state) = state.return_to_new_session.take() {
                        self.popup = Some(Popup::NewSession(session_state));
                        return Effect::None;
                    }
                }
                Popup::NewProviderProfile(mut state) => {
                    if let Some(session_state) = state.return_to_new_session.take() {
                        self.popup = Some(Popup::NewSession(session_state));
                        return Effect::None;
                    } else if state.return_to_providers {
                        self.open_provider_popup();
                        return Effect::None;
                    }
                }
                _ => {}
            }
            return Effect::None;
        }
        match &mut popup {
            Popup::Help => {}
            Popup::ConfirmExit => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if let Some(attempt_id) = self.running_attempt() {
                        return Effect::CancelAndQuit(EngineCommand::Cancel { attempt_id });
                    }
                    return Effect::Quit;
                }
                KeyCode::Char('n') | KeyCode::Char('N') => return Effect::None,
                _ => return Effect::None,
            },
            Popup::ConfirmDelete { session_id, .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.deletion_pending = true;
                    return Effect::Execute(EngineCommand::PurgeSession {
                        session_id: *session_id,
                    });
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter => {
                    return Effect::None;
                }
                _ => return Effect::None,
            },
            Popup::Branches { rows, selected } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(rows.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let (Some(history), Some(branch)) = (&self.history, rows.get(*selected)) {
                        let session_id = history.session.session_id;
                        let branch_id = branch.branch_id;
                        if let Err(error) = self.open_branch(session_id, branch_id) {
                            self.show_error(error.to_string());
                        }
                    }
                    return Effect::None;
                }
                _ => {}
            },
            Popup::Providers { names, selected } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1).min(names.len()),
                KeyCode::Char('a') | KeyCode::Char('n') => {
                    self.open_new_provider_profile_popup(None, true);
                    return Effect::None;
                }
                KeyCode::Enter => {
                    if *selected == names.len() {
                        self.open_new_provider_profile_popup(None, true);
                        return Effect::None;
                    }
                    if let (Some(history), Some(name)) = (&self.history, names.get(*selected))
                        && let Some(provider) = self.config.core.providers.get(name)
                    {
                        let mut configuration = history.configuration.configuration.clone();
                        configuration.provider = provider.clone();
                        return Effect::Execute(EngineCommand::UpdateConfiguration {
                            session_id: history.session.session_id,
                            configuration: Box::new(configuration),
                        });
                    }
                    return Effect::None;
                }
                _ => {}
            },
            Popup::Rename { session_id, input } => match key.code {
                KeyCode::Enter => {
                    return Effect::Execute(EngineCommand::RenameSession {
                        session_id: *session_id,
                        name: std::mem::take(input),
                    });
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(character);
                }
                _ => {}
            },
            Popup::Presets { rows, selected } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1).min(rows.len()),
                KeyCode::Enter => {
                    if let Some(history) = &self.history {
                        let mut configuration = history.configuration.configuration.clone();
                        configuration.prompt_preset_revision = selected
                            .checked_sub(1)
                            .and_then(|index| rows.get(index))
                            .map(|row| row.record.revision_hash.clone());
                        return Effect::Execute(EngineCommand::UpdateConfiguration {
                            session_id: history.session.session_id,
                            configuration: Box::new(configuration),
                        });
                    }
                    return Effect::None;
                }
                _ => {}
            },
            Popup::ImportCharacter(state) => match key.code {
                KeyCode::Backspace => {
                    state.input.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.input.push(character);
                }
                KeyCode::Enter => {
                    let path_str = state.input.trim();
                    if path_str.is_empty() {
                        self.show_error("Path cannot be empty");
                        self.popup = Some(Popup::ImportCharacter(state.clone()));
                        return Effect::None;
                    }
                    let expanded = if let Some(stripped) = path_str.strip_prefix("~/") {
                        if let Ok(home) = std::env::var("HOME") {
                            Path::new(&home).join(stripped)
                        } else {
                            PathBuf::from(path_str)
                        }
                    } else {
                        PathBuf::from(path_str)
                    };
                    if !expanded.exists() {
                        self.show_error(format!("File does not exist: {}", expanded.display()));
                        self.popup = Some(Popup::ImportCharacter(state.clone()));
                        return Effect::None;
                    }
                    let source = match fs::read(&expanded) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            self.show_error(format!("Failed to read file: {error}"));
                            self.popup = Some(Popup::ImportCharacter(state.clone()));
                            return Effect::None;
                        }
                    };
                    self.popup = Some(Popup::ImportCharacter(state.clone()));
                    return Effect::Execute(EngineCommand::ImportArtifact { source });
                }
                _ => {}
            },
            Popup::NewProviderProfile(state) => {
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.submit_provider_profile(state.as_ref().clone());
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        state.focused_field = (state.focused_field + 1) % 9;
                    }
                    KeyCode::Char('j') if !(1..=5).contains(&state.focused_field) => {
                        state.focused_field = (state.focused_field + 1) % 9;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        state.focused_field = (state.focused_field + 8) % 9;
                    }
                    KeyCode::Char('k') if !(1..=5).contains(&state.focused_field) => {
                        state.focused_field = (state.focused_field + 8) % 9;
                    }
                    _ => match state.focused_field {
                        0 => match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                state.selected_template = state.selected_template.saturating_sub(1);
                                Self::apply_template_to_profile_state(state);
                            }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                state.selected_template =
                                    (state.selected_template + 1).min(state.templates.len());
                                Self::apply_template_to_profile_state(state);
                            }
                            KeyCode::Enter => state.focused_field = 1,
                            _ => {}
                        },
                        1 => match key.code {
                            KeyCode::Backspace => {
                                state.name.pop();
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.name.push(c);
                            }
                            KeyCode::Enter => state.focused_field = 2,
                            _ => {}
                        },
                        2 => match key.code {
                            KeyCode::Backspace => {
                                state.base_url.pop();
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.base_url.push(c);
                            }
                            KeyCode::Enter => state.focused_field = 3,
                            _ => {}
                        },
                        3 => match key.code {
                            KeyCode::Backspace => {
                                state.model.pop();
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.model.push(c);
                            }
                            KeyCode::Enter => state.focused_field = 4,
                            _ => {}
                        },
                        4 => match key.code {
                            KeyCode::Backspace => {
                                state.chat_path.pop();
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.chat_path.push(c);
                            }
                            KeyCode::Enter => state.focused_field = 5,
                            _ => {}
                        },
                        5 => match key.code {
                            KeyCode::Backspace => {
                                state.api_key_env.pop();
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.api_key_env.push(c);
                            }
                            KeyCode::Enter => state.focused_field = 6,
                            _ => {}
                        },
                        6 => match key.code {
                            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                                state.stream = !state.stream;
                            }
                            KeyCode::Enter => state.focused_field = 7,
                            _ => {}
                        },
                        7 => {
                            if key.code == KeyCode::Enter {
                                return self.submit_provider_profile(state.as_ref().clone());
                            }
                        }
                        8 => {
                            if key.code == KeyCode::Enter {
                                if let Some(session_state) = state.return_to_new_session.take() {
                                    self.popup = Some(Popup::NewSession(session_state));
                                } else if state.return_to_providers {
                                    self.open_provider_popup();
                                } else {
                                    self.popup = None;
                                }
                                return Effect::None;
                            }
                        }
                        _ => {}
                    },
                }
            }
            Popup::NewSession(state) => {
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.submit_new_session(state.as_ref().clone());
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        state.focused_field = (state.focused_field + 1) % 7;
                    }
                    KeyCode::Char('j') if state.focused_field != 3 => {
                        state.focused_field = (state.focused_field + 1) % 7;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        state.focused_field = (state.focused_field + 6) % 7;
                    }
                    KeyCode::Char('k') if state.focused_field != 3 => {
                        state.focused_field = (state.focused_field + 6) % 7;
                    }
                    _ => match state.focused_field {
                        0 => match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                state.selected_character =
                                    state.selected_character.saturating_sub(1);
                                state.selected_greeting = 0;
                            }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                state.selected_character =
                                    (state.selected_character + 1).min(state.characters.len());
                                state.selected_greeting = 0;
                            }
                            KeyCode::Enter => {
                                if state.characters.is_empty()
                                    || state.selected_character == state.characters.len()
                                {
                                    self.popup =
                                        Some(Popup::ImportCharacter(ImportCharacterState {
                                            input: String::new(),
                                            return_to_new_session: Some(state.clone()),
                                        }));
                                    return Effect::None;
                                }
                                state.focused_field = 1;
                            }
                            _ => {}
                        },
                        1 => match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                state.selected_provider = state.selected_provider.saturating_sub(1);
                            }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                state.selected_provider =
                                    (state.selected_provider + 1).min(state.providers.len());
                            }
                            KeyCode::Enter => {
                                if state.providers.is_empty()
                                    || state.selected_provider == state.providers.len()
                                {
                                    self.open_new_provider_profile_popup(
                                        Some(state.clone()),
                                        false,
                                    );
                                    return Effect::None;
                                }
                                state.focused_field = 2;
                            }
                            _ => {}
                        },
                        2 => match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                state.selected_preset = state.selected_preset.saturating_sub(1);
                            }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                state.selected_preset =
                                    (state.selected_preset + 1).min(state.presets.len());
                            }
                            KeyCode::Enter => state.focused_field = 3,
                            _ => {}
                        },
                        3 => match key.code {
                            KeyCode::Backspace => {
                                state.persona.pop();
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                state.persona.push(c);
                            }
                            KeyCode::Enter => state.focused_field = 4,
                            _ => {}
                        },
                        4 => match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                state.selected_greeting = state.selected_greeting.saturating_sub(1);
                            }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                let max = state
                                    .characters
                                    .get(state.selected_character)
                                    .map(|c| c.greeting_count)
                                    .unwrap_or(1);
                                state.selected_greeting =
                                    (state.selected_greeting + 1).min(max.saturating_sub(1));
                            }
                            KeyCode::Enter => state.focused_field = 5,
                            _ => {}
                        },
                        5 => {
                            if key.code == KeyCode::Enter {
                                return self.submit_new_session(state.as_ref().clone());
                            }
                        }
                        6 => {
                            if key.code == KeyCode::Enter {
                                self.popup = None;
                                return Effect::None;
                            }
                        }
                        _ => {}
                    },
                }
            }
        }
        self.popup = Some(popup);
        Effect::None
    }

    fn open_branch_popup(&mut self) {
        let Some(history) = &self.history else { return };
        match self.engine.inspect(EngineQuery::Branches {
            session_id: history.session.session_id,
        }) {
            Ok(EngineInspection::Branches(rows)) => {
                let selected = rows
                    .iter()
                    .position(|row| row.branch_id == history.branch.branch_id)
                    .unwrap_or(0);
                self.popup = Some(Popup::Branches { rows, selected });
            }
            Err(error) => self.show_error(error.to_string()),
            _ => unreachable!(),
        }
    }

    fn open_provider_popup(&mut self) {
        let names = self
            .config
            .core
            .providers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let selected = self
            .history
            .as_ref()
            .and_then(|history| {
                names.iter().position(|name| {
                    self.config.core.providers.get(name)
                        == Some(&history.configuration.configuration.provider)
                })
            })
            .unwrap_or(0);
        self.popup = Some(Popup::Providers { names, selected });
    }

    pub fn query_character_options(&self) -> Vec<CharacterOption> {
        let mut options = Vec::new();
        for kind in [
            ArtifactKind::CharacterCardV3,
            ArtifactKind::CharacterCardV2,
            ArtifactKind::CharacterCardV1,
        ] {
            if let Ok(EngineInspection::Artifacts(records)) = self
                .engine
                .inspect(EngineQuery::Artifacts { kind: Some(kind) })
            {
                for record in records {
                    let hash_str = record.revision_hash.to_string();
                    let mut name = hash_str[hash_str.len().saturating_sub(12)..].to_owned();
                    let mut greeting_count = 1;
                    if let Ok(EngineInspection::ArtifactSource(source)) =
                        self.engine.inspect(EngineQuery::ArtifactSource {
                            revision_hash: record.revision_hash.clone(),
                        })
                        && let Ok(decoded) = decode_artifact(&source)
                    {
                        greeting_count = decoded.greetings.len().max(1);
                        if let Some(n) = decoded
                            .semantic
                            .get("data")
                            .and_then(|d| d.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            if !n.trim().is_empty() {
                                name = n.to_owned();
                            }
                        } else if let Some(n) =
                            decoded.semantic.get("name").and_then(|n| n.as_str())
                            && !n.trim().is_empty()
                        {
                            name = n.to_owned();
                        }
                    }
                    options.push(CharacterOption {
                        revision_hash: record.revision_hash,
                        name,
                        greeting_count,
                    });
                }
            }
        }
        options
    }

    pub fn query_preset_options(&self) -> anyhow::Result<Vec<PresetOption>> {
        let EngineInspection::Artifacts(records) = self.engine.inspect(EngineQuery::Artifacts {
            kind: Some(ArtifactKind::ChatCompletionPreset),
        })?
        else {
            unreachable!("artifacts query returned another inspection type")
        };
        Ok(records
            .into_iter()
            .map(|record| {
                let label = self
                    .engine
                    .inspect(EngineQuery::ArtifactSource {
                        revision_hash: record.revision_hash.clone(),
                    })
                    .ok()
                    .and_then(|inspection| match inspection {
                        EngineInspection::ArtifactSource(source) => decode_artifact(&source).ok(),
                        _ => None,
                    })
                    .and_then(|artifact| {
                        artifact
                            .semantic
                            .get("name")
                            .and_then(|name| name.as_str())
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| {
                        let hash = record.revision_hash.to_string();
                        format!(
                            "{} · {}",
                            record.source_format,
                            &hash[hash.len().saturating_sub(12)..]
                        )
                    });
                PresetOption { record, label }
            })
            .collect())
    }

    pub fn open_new_session_popup(&mut self) {
        let characters = self.query_character_options();
        let presets = match self.query_preset_options() {
            Ok(presets) => presets,
            Err(error) => {
                self.show_error(error.to_string());
                Vec::new()
            }
        };
        let state = Box::new(NewSessionState {
            selected_character: 0,
            characters,
            providers: self.config.core.providers.keys().cloned().collect(),
            selected_provider: 0,
            presets,
            selected_preset: 0,
            persona: "User".to_owned(),
            selected_greeting: 0,
            focused_field: 0,
        });
        self.popup = if state.characters.is_empty() {
            Some(Popup::ImportCharacter(ImportCharacterState {
                input: String::new(),
                return_to_new_session: Some(state),
            }))
        } else {
            Some(Popup::NewSession(state))
        };
    }

    pub fn open_new_provider_profile_popup(
        &mut self,
        return_to_new_session: Option<Box<NewSessionState>>,
        return_to_providers: bool,
    ) {
        let templates = match self
            .config_dir
            .as_ref()
            .map(|directory| stcli_core::Config::load_provider_templates(directory))
        {
            Some(Ok(templates)) => templates.into_values().collect(),
            Some(Err(error)) => {
                self.show_error(error.to_string());
                Vec::new()
            }
            None => Vec::new(),
        };
        self.popup = Some(Popup::NewProviderProfile(Box::new(
            NewProviderProfileState {
                templates,
                selected_template: 0,
                name: String::new(),
                base_url: "https://".to_owned(),
                model: String::new(),
                chat_path: "/v1/chat/completions".to_owned(),
                api_key_env: String::new(),
                stream: true,
                timeout_seconds: 120,
                focused_field: 1,
                return_to_new_session,
                return_to_providers,
            },
        )));
    }

    fn apply_template_to_profile_state(state: &mut NewProviderProfileState) {
        if state.selected_template == 0 {
            return;
        }
        if let Some(template) = state.templates.get(state.selected_template - 1) {
            state.base_url = template.base_url.clone();
            state.chat_path = template.chat_completions_path.clone();
            state.model = template.default_model.clone();
            state.api_key_env = template.api_key_env.clone().unwrap_or_default();
            state.stream = template.stream;
            state.timeout_seconds = template.timeout_seconds;
            if state.name.is_empty() {
                state.name = template.id.clone();
            }
        }
    }

    fn submit_provider_profile(&mut self, mut state: NewProviderProfileState) -> Effect {
        if state.name.trim().is_empty() {
            self.show_error("Profile name cannot be empty");
            self.popup = Some(Popup::NewProviderProfile(Box::new(state)));
            return Effect::None;
        }
        let settings = ProviderSettings {
            id: state.name.clone(),
            base_url: state.base_url.trim().to_owned(),
            chat_completions_path: state.chat_path.trim().to_owned(),
            api_key_env: if state.api_key_env.trim().is_empty() {
                None
            } else {
                Some(state.api_key_env.trim().to_owned())
            },
            static_headers: BTreeMap::new(),
            timeout_seconds: state.timeout_seconds,
            ca_certificate_pem: None,
            model: state.model.trim().to_owned(),
            stream: state.stream,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        };
        if let Err(error) = validate_provider_settings(&settings) {
            self.show_error(format!("Invalid settings: {error}"));
            self.popup = Some(Popup::NewProviderProfile(Box::new(state)));
            return Effect::None;
        }
        let config_dir = match &self.config_dir {
            Some(dir) => dir.clone(),
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };
        if let Err(error) =
            stcli_core::Config::add_provider_profile(&config_dir, &state.name, settings)
        {
            self.show_error(format!("Failed to save profile: {error}"));
            self.popup = Some(Popup::NewProviderProfile(Box::new(state)));
            return Effect::None;
        }
        if let Err(error) = self.reload_config() {
            self.show_error(format!("Failed to reload config: {error}"));
        }
        let profile_name = state.name.clone();
        if let Some(mut session_state) = state.return_to_new_session.take() {
            session_state.providers = self.config.core.providers.keys().cloned().collect();
            if let Some(pos) = session_state
                .providers
                .iter()
                .position(|p| p == &profile_name)
            {
                session_state.selected_provider = pos;
            }
            self.show_info(format!("Created provider profile '{profile_name}'"));
            self.popup = Some(Popup::NewSession(session_state));
        } else if state.return_to_providers {
            let names = self
                .config
                .core
                .providers
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let selected = names.iter().position(|p| p == &profile_name).unwrap_or(0);
            self.show_info(format!("Created provider profile '{profile_name}'"));
            self.popup = Some(Popup::Providers { names, selected });
        } else {
            self.show_info(format!("Created provider profile '{profile_name}'"));
            self.popup = None;
        }
        Effect::None
    }

    fn submit_new_session(&mut self, state: NewSessionState) -> Effect {
        if state.characters.is_empty() || state.selected_character >= state.characters.len() {
            self.show_error("Please select or import a character card first");
            self.popup = Some(Popup::NewSession(Box::new(state)));
            return Effect::None;
        }
        if state.providers.is_empty() || state.selected_provider >= state.providers.len() {
            self.show_error("Please select or add a provider profile first");
            self.popup = Some(Popup::NewSession(Box::new(state)));
            return Effect::None;
        }
        let character = &state.characters[state.selected_character];
        let provider_name = &state.providers[state.selected_provider];
        let Some(provider_settings) = self.config.core.providers.get(provider_name) else {
            self.show_error(format!("Provider profile '{provider_name}' not found"));
            self.popup = Some(Popup::NewSession(Box::new(state)));
            return Effect::None;
        };
        let prompt_preset_revision =
            if state.selected_preset > 0 && state.selected_preset <= state.presets.len() {
                Some(
                    state.presets[state.selected_preset - 1]
                        .record
                        .revision_hash
                        .clone(),
                )
            } else {
                None
            };
        let configuration = SessionConfiguration {
            compatibility_profile: "sillytavern-1.18-core".to_owned(),
            character_revision: character.revision_hash.clone(),
            persona_name: if state.persona.trim().is_empty() {
                "User".to_owned()
            } else {
                state.persona.clone()
            },
            lorebook_revisions: vec![],
            prompt_preset_revision,
            provider: provider_settings.clone(),
            tokenizer: "tiktoken:o200k_base".to_owned(),
            generation_settings: serde_json::json!({}),
            plugins: vec![],
            script_grants: vec![],
        };
        let greeting_index = state.selected_greeting;
        self.popup = None;
        Effect::Execute(EngineCommand::CreateSession {
            configuration: Box::new(configuration),
            greeting_index,
        })
    }

    fn open_preset_popup(&mut self) {
        let rows = match self.query_preset_options() {
            Ok(rows) => rows,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        let selected = self
            .history
            .as_ref()
            .and_then(|history| {
                history
                    .configuration
                    .configuration
                    .prompt_preset_revision
                    .as_ref()
            })
            .and_then(|current| {
                rows.iter()
                    .position(|row| &row.record.revision_hash == current)
            })
            .map_or(0, |index| index + 1);
        self.popup = Some(Popup::Presets { rows, selected });
    }

    fn delete_session_list_entry(&mut self) -> Effect {
        let entries = self.session_list_entries();
        let Some(entry) = entries.get(self.selected_session) else {
            return Effect::None;
        };
        match entry {
            SessionListEntry::Session(i) => {
                let filtered = self.filtered_sessions();
                if let Some(session) = filtered.get(*i) {
                    self.popup = Some(Popup::ConfirmDelete {
                        session_id: session.session_id,
                        name: session.display_name.clone(),
                    });
                }
            }
            SessionListEntry::Branch { branch, .. } => {
                if branch.parent_branch_id.is_none() {
                    self.show_info("The root branch cannot be deleted; delete the session instead");
                    return Effect::None;
                }
                self.deletion_pending = true;
                return Effect::Execute(EngineCommand::DeleteBranch {
                    branch_id: branch.branch_id,
                });
            }
        }
        Effect::None
    }

    fn start_rename(&mut self) {
        let entries = self.session_list_entries();
        let Some(SessionListEntry::Session(i)) = entries.get(self.selected_session) else {
            self.show_error("Can only rename sessions, not branches");
            return;
        };
        let filtered = self.filtered_sessions();
        if let Some(session) = filtered.get(*i) {
            self.popup = Some(Popup::Rename {
                session_id: session.session_id,
                input: session.display_name.clone(),
            });
        }
    }

    fn delete_focused(&mut self) -> Effect {
        let Some(history) = &self.history else {
            return Effect::None;
        };
        match resolve_focus(history, self.focused_message) {
            Some(FocusedSlot::UserMessage(turn_index)) => {
                let turn = &history.turns[turn_index];
                Effect::Execute(EngineCommand::DeleteTurn {
                    turn_id: turn.turn.turn_id,
                })
            }
            Some(FocusedSlot::AssistantMessage(turn_index)) => {
                let turn = &history.turns[turn_index];
                match turn.turn.selected_candidate_id {
                    Some(candidate_id) => {
                        Effect::Execute(EngineCommand::DeleteCandidate { candidate_id })
                    }
                    None => Effect::Execute(EngineCommand::DeleteTurn {
                        turn_id: turn.turn.turn_id,
                    }),
                }
            }
            _ => Effect::None,
        }
    }

    fn navigate_focused(&mut self, direction: isize) -> Effect {
        let Some(history) = &self.history else {
            return Effect::None;
        };
        match resolve_focus(history, self.focused_message) {
            Some(FocusedSlot::Greeting) => {
                let greeting = history
                    .greeting
                    .as_ref()
                    .expect("Greeting offset is present");
                let next = greeting.index as isize + direction;
                if !(0..greeting.total as isize).contains(&next) {
                    self.show_error("Greeting selection is out of range");
                    return Effect::None;
                }
                Effect::Execute(EngineCommand::SelectGreeting {
                    session_id: history.session.session_id,
                    branch_id: history.branch.branch_id,
                    greeting_index: next as usize,
                })
            }
            Some(FocusedSlot::AssistantMessage(turn_index)) => {
                let turn = &history.turns[turn_index];
                let current = turn.turn.selected_candidate_id.and_then(|id| {
                    turn.candidates
                        .iter()
                        .position(|candidate| candidate.candidate_id == id)
                });
                match (current, direction) {
                    (Some(index), -1) if index > 0 => {
                        Effect::Execute(EngineCommand::SelectCandidate {
                            turn_id: turn.turn.turn_id,
                            candidate_id: turn.candidates[index - 1].candidate_id,
                        })
                    }
                    (Some(index), 1) if index + 1 < turn.candidates.len() => {
                        Effect::Execute(EngineCommand::SelectCandidate {
                            turn_id: turn.turn.turn_id,
                            candidate_id: turn.candidates[index + 1].candidate_id,
                        })
                    }
                    (Some(index), 1)
                        if index + 1 == turn.candidates.len() && self.generation.is_none() =>
                    {
                        Effect::Start(EngineCommand::GenerateSwipe {
                            turn_id: turn.turn.turn_id,
                        })
                    }
                    _ => Effect::None,
                }
            }
            _ => Effect::None,
        }
    }

    pub fn handle_provider_event(&mut self, event: ProviderEvent) {
        if matches!(&event, ProviderEvent::Started)
            && let Err(error) = self.reload_history()
        {
            self.show_error(error.to_string());
        }
        if let Some(generation) = &mut self.generation {
            match event {
                ProviderEvent::TextDelta { text } => generation.partial.push_str(&text),
                ProviderEvent::ReasoningDelta { text } => generation.reasoning.push_str(&text),
                ProviderEvent::Started | ProviderEvent::Usage { .. } | ProviderEvent::Completed => {
                }
            }
        }
    }

    pub fn finish_generation(&mut self, result: Result<EngineResult, String>) {
        match result {
            Ok(_) => {
                self.generation = None;
                if let Err(error) = self.reload_history() {
                    self.show_error(error.to_string());
                }
                if let Some(history) = &self.history {
                    self.focused_message = message_count(history).saturating_sub(1);
                }
                self.follow = true;
            }
            Err(error) => {
                let cancelled = is_cancelled_error(&error);
                if let Some(generation) = self.generation.take()
                    && !cancelled
                    && let Some(input) = generation.pending_input
                {
                    self.composer = input;
                }
                if cancelled {
                    self.show_info(
                        "Generation stopped; partial output was retained when available",
                    );
                } else {
                    self.show_error(error);
                }
                if let Err(error) = self.reload_history() {
                    self.show_error(error.to_string());
                }
            }
        }
    }

    pub fn finish_command(&mut self, result: Result<EngineResult, String>) -> bool {
        self.deletion_pending = false;
        match result {
            Ok(EngineResult::CreatedSession(created)) => {
                self.popup = None;
                if let Err(error) = self.reload_sessions() {
                    self.show_error(error.to_string());
                }
                if let Err(error) = self.open_session(created.session.session_id) {
                    self.show_error(error.to_string());
                }
                true
            }
            Ok(EngineResult::ArtifactBundle {
                primary,
                supplementary_artifacts,
                asset_count,
            }) => {
                let status = format!(
                    "Imported character card ({} supplementary Artifacts, {asset_count} assets)",
                    supplementary_artifacts.len()
                );
                if let Some(Popup::ImportCharacter(mut state)) = self.popup.take() {
                    if let Some(mut session_state) = state.return_to_new_session.take() {
                        session_state.characters = self.query_character_options();
                        if let Some(pos) = session_state
                            .characters
                            .iter()
                            .position(|character| character.revision_hash == primary.revision_hash)
                        {
                            session_state.selected_character = pos;
                            session_state.selected_greeting = 0;
                        }
                        self.show_info(status);
                        self.popup = Some(Popup::NewSession(session_state));
                    } else {
                        self.show_info(status);
                        self.popup = None;
                    }
                } else {
                    self.show_info(status);
                    self.popup = None;
                }
                true
            }
            Ok(_) => {
                self.popup = None;
                match self.screen {
                    Screen::Sessions => {
                        if let Err(error) = self.reload_sessions() {
                            self.show_error(error.to_string());
                        }
                    }
                    Screen::Chat => {
                        if let Err(error) = self.reload_history() {
                            self.show_error(error.to_string());
                        }
                    }
                }
                true
            }
            Err(error) => {
                self.show_error(error);
                false
            }
        }
    }

    pub fn focused_content(&self) -> Option<&str> {
        let history = self.history.as_ref()?;
        match resolve_focus(history, self.focused_message)? {
            FocusedSlot::Greeting => history
                .greeting
                .as_ref()
                .map(|greeting| greeting.content.as_str()),
            FocusedSlot::UserMessage(turn_index) => {
                Some(&history.turns[turn_index].turn.user_content)
            }
            FocusedSlot::AssistantMessage(turn_index) => {
                selected_candidate(&history.turns[turn_index]).map(|candidate| {
                    candidate
                        .rendered_content
                        .as_deref()
                        .unwrap_or(&candidate.content)
                })
            }
        }
    }

    pub fn running_attempt(&self) -> Option<EntityId> {
        self.history
            .as_ref()?
            .turns
            .iter()
            .rev()
            .flat_map(|turn| turn.attempts.iter().rev())
            .find(|attempt| attempt.status == AttemptStatus::Running)
            .map(|attempt| attempt.attempt_id)
    }

    fn unanswered_turn_id(&self) -> Option<EntityId> {
        let turn = self.history.as_ref()?.turns.last()?;
        turn.candidates.is_empty().then_some(turn.turn.turn_id)
    }

    fn focused_unanswered_turn_id(&self) -> Option<EntityId> {
        let history = self.history.as_ref()?;
        let FocusedSlot::UserMessage(turn_index) = resolve_focus(history, self.focused_message)?
        else {
            return None;
        };
        let turn = &history.turns[turn_index];
        (turn_index + 1 == history.turns.len() && turn.candidates.is_empty())
            .then_some(turn.turn.turn_id)
    }

    fn current_turn_id(&self) -> Option<EntityId> {
        self.history
            .as_ref()?
            .turns
            .last()
            .map(|turn| turn.turn.turn_id)
    }

    fn current_candidate_id(&self) -> Option<EntityId> {
        self.history
            .as_ref()?
            .turns
            .last()
            .and_then(|turn| turn.turn.selected_candidate_id)
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> Effect {
        match event.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_context(-3);
                Effect::None
            }
            MouseEventKind::ScrollDown => {
                self.scroll_context(3);
                Effect::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let action = self
                    .hit_targets
                    .iter()
                    .rev()
                    .find(|target| {
                        event.column >= target.x
                            && event.column < target.x + target.width
                            && event.row >= target.y
                            && event.row < target.y + target.height
                    })
                    .map(|target| target.action.clone());
                self.apply_hit(action)
            }
            _ => Effect::None,
        }
    }

    fn scroll_context(&mut self, amount: isize) {
        match &mut self.popup {
            Some(Popup::Branches { rows, selected }) => {
                *selected = selected
                    .saturating_add_signed(amount)
                    .min(rows.len().saturating_sub(1));
                return;
            }
            Some(Popup::Providers { names, selected }) => {
                *selected = selected
                    .saturating_add_signed(amount)
                    .min(names.len().saturating_sub(1));
                return;
            }
            Some(Popup::Presets { rows, selected }) => {
                *selected = selected.saturating_add_signed(amount).min(rows.len());
                return;
            }
            Some(
                Popup::Help
                | Popup::ConfirmExit
                | Popup::ConfirmDelete { .. }
                | Popup::Rename { .. }
                | Popup::NewSession(_)
                | Popup::ImportCharacter(_)
                | Popup::NewProviderProfile(_),
            ) => return,
            None => {}
        }
        match self.screen {
            Screen::Sessions => {
                self.selected_session = self
                    .selected_session
                    .saturating_add_signed(amount)
                    .min(self.filtered_sessions().len().saturating_sub(1));
            }
            Screen::Chat if amount < 0 => self.scroll_up(amount.unsigned_abs() as u16),
            Screen::Chat => self.scroll_down(amount as u16),
        }
    }

    fn apply_hit(&mut self, action: Option<HitAction>) -> Effect {
        match action {
            Some(HitAction::Session(index)) => {
                self.selected_session = index;
                if let Some(session_id) = self
                    .filtered_sessions()
                    .get(index)
                    .map(|row| row.session_id)
                    && let Err(error) = self.open_session(session_id)
                {
                    self.show_error(error.to_string());
                }
                Effect::None
            }
            Some(HitAction::Message(index)) => {
                self.focused_message = index;
                self.chat_focus = ChatFocus::History;
                Effect::None
            }
            Some(HitAction::Composer) => {
                self.chat_focus = ChatFocus::Composer;
                Effect::None
            }
            Some(HitAction::CandidatePrevious) | Some(HitAction::GreetingPrevious) => {
                self.navigate_focused(-1)
            }
            Some(HitAction::CandidateNext) | Some(HitAction::GreetingNext) => {
                self.navigate_focused(1)
            }
            Some(HitAction::Stop) => self.running_attempt().map_or(Effect::None, |attempt_id| {
                Effect::Execute(EngineCommand::Cancel { attempt_id })
            }),
            Some(HitAction::Regenerate) => self.current_turn_id().map_or(Effect::None, |turn_id| {
                Effect::Start(EngineCommand::Regenerate { turn_id })
            }),
            Some(HitAction::Continue) => {
                match (self.current_turn_id(), self.current_candidate_id()) {
                    (Some(turn_id), Some(_)) => Effect::Start(EngineCommand::Continue { turn_id }),
                    _ => Effect::None,
                }
            }
            Some(HitAction::PopupRow(index)) => {
                if let Some(
                    Popup::Branches { selected, .. }
                    | Popup::Providers { selected, .. }
                    | Popup::Presets { selected, .. },
                ) = &mut self.popup
                {
                    *selected = index;
                }
                self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            }
            None => Effect::None,
        }
    }
}

pub fn selected_candidate(turn: &stcli_core::EngineTurn) -> Option<&CandidateProjection> {
    let selected = turn.turn.selected_candidate_id?;
    turn.candidates
        .iter()
        .find(|candidate| candidate.candidate_id == selected)
}

fn turn_has_assistant_slot(turn: &stcli_core::EngineTurn) -> bool {
    !turn.candidates.is_empty()
}

#[derive(Clone, Copy, Debug)]
enum FocusedSlot {
    Greeting,
    UserMessage(usize),
    AssistantMessage(usize),
}

fn resolve_focus(history: &BranchHistory, focused_message: usize) -> Option<FocusedSlot> {
    let greeting_offset = usize::from(history.greeting.is_some());
    if focused_message < greeting_offset {
        return Some(FocusedSlot::Greeting);
    }
    let mut index = greeting_offset;
    for (i, turn) in history.turns.iter().enumerate() {
        if focused_message == index {
            return Some(FocusedSlot::UserMessage(i));
        }
        index += 1;
        if turn_has_assistant_slot(turn) {
            if focused_message == index {
                return Some(FocusedSlot::AssistantMessage(i));
            }
            index += 1;
        }
    }
    None
}

fn message_count(history: &BranchHistory) -> usize {
    let greeting = usize::from(history.greeting.is_some());
    let turn_slots: usize = history
        .turns
        .iter()
        .map(|turn| if turn_has_assistant_slot(turn) { 2 } else { 1 })
        .sum();
    greeting + turn_slots
}

fn is_cancelled_error(error: &str) -> bool {
    error.to_ascii_lowercase().contains("cancelled")
}

fn sort_sessions(sessions: &mut [SessionSummary], sort: SortKey) {
    sessions.sort_by(|left, right| {
        let ordering = match sort {
            SortKey::Modified => left.modified_at_ms.cmp(&right.modified_at_ms),
            SortKey::Created => left.created_at_ms.cmp(&right.created_at_ms),
            SortKey::Name => right.display_name.cmp(&left.display_name),
            SortKey::Turns => left.turn_count.cmp(&right.turn_count),
            SortKey::Tokens => left.token_count.cmp(&right.token_count),
        };
        ordering
            .reverse()
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
}

fn fuzzy_match(needle: &str, haystack: &str) -> bool {
    let mut characters = needle.chars().flat_map(char::to_lowercase);
    let mut expected = characters.next();
    if expected.is_none() {
        return true;
    }
    for character in haystack.chars().flat_map(char::to_lowercase) {
        if Some(character) == expected {
            expected = characters.next();
            if expected.is_none() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    fn execute_command(app: &mut App, effect: Effect) {
        let Effect::Execute(command) = effect else {
            panic!("expected command execution");
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime
            .block_on(app.engine.execute(command, |_| {}))
            .map_err(|error| error.to_string());
        app.finish_command(result);
    }

    fn append_unanswered_turn(app: &mut App) -> EntityId {
        let history = app.history.as_mut().expect("history loaded");
        let turn_id = EntityId::new();
        history.turns.push(stcli_core::EngineTurn {
            turn: stcli_core::TurnProjection {
                turn_id,
                session_id: history.session.session_id,
                branch_id: history.branch.branch_id,
                user_content: "Hello?".to_owned(),
                selected_candidate_id: None,
                hidden: false,
                created_event_id: EntityId::new().to_string(),
            },
            candidates: Vec::new(),
            attempts: Vec::new(),
        });
        turn_id
    }

    fn append_answered_turn(app: &mut App) -> EntityId {
        let turn_id = append_unanswered_turn(app);
        let turn = app
            .history
            .as_mut()
            .expect("history loaded")
            .turns
            .last_mut()
            .expect("turn appended");
        let candidate_id = EntityId::new();
        turn.turn.selected_candidate_id = Some(candidate_id);
        turn.candidates.push(CandidateProjection {
            candidate_id,
            turn_id,
            attempt_id: None,
            parent_candidate_id: None,
            origin: stcli_core::CandidateOrigin::Generated,
            content: "Hello.".to_owned(),
            rendered_content: None,
            hidden: false,
            created_event_id: EntityId::new().to_string(),
        });
        turn_id
    }

    fn app_with_session() -> (App, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);
        let app = App::load(
            StcliEngine::new(database),
            Config::default(),
            Some(created.session.session_id),
        )
        .unwrap();
        (app, directory)
    }

    #[test]
    fn enter_on_selected_unanswered_user_message_starts_response() {
        // Regression: Enter on the last user message must generate its missing response.
        let (mut app, _directory) = app_with_session();
        let turn_id = append_unanswered_turn(&mut app);
        app.chat_focus = ChatFocus::History;
        app.focused_message = message_count(app.history.as_ref().unwrap()).saturating_sub(1);

        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            effect,
            Effect::Start(EngineCommand::Regenerate { turn_id: actual }) if actual == turn_id
        ));
    }

    #[test]
    fn enter_in_empty_composer_after_user_message_starts_response() {
        // Regression: an empty composer must submit the unanswered last user message.
        let (mut app, _directory) = app_with_session();
        let turn_id = append_unanswered_turn(&mut app);
        app.chat_focus = ChatFocus::Composer;

        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            effect,
            Effect::Start(EngineCommand::Regenerate { turn_id: actual }) if actual == turn_id
        ));
    }

    #[test]
    fn enter_in_empty_composer_after_assistant_message_does_nothing() {
        // Regression: an empty composer must not generate after an assistant message.
        let (mut app, _directory) = app_with_session();
        append_answered_turn(&mut app);
        app.chat_focus = ChatFocus::Composer;

        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(effect, Effect::None));
    }

    #[test]
    fn enter_on_assistant_history_message_focuses_composer_without_response() {
        // Regression: Enter on an assistant message must not start generation.
        let (mut app, _directory) = app_with_session();
        append_answered_turn(&mut app);
        app.chat_focus = ChatFocus::History;
        app.focused_message = message_count(app.history.as_ref().unwrap()).saturating_sub(1);

        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(effect, Effect::None));
        assert_eq!(app.chat_focus, ChatFocus::Composer);
    }

    #[test]
    fn composer_accepts_quit_key_as_message_text() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::load(
            StcliEngine::new(directory.path().join("stcli.sqlite3")),
            Config::default(),
            None,
        )
        .unwrap();
        app.screen = Screen::Chat;

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

        assert!(matches!(effect, Effect::None));
        assert_eq!(app.composer, "q");
    }

    #[test]
    fn history_quit_key_requests_exit() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::load(
            StcliEngine::new(directory.path().join("stcli.sqlite3")),
            Config::default(),
            None,
        )
        .unwrap();
        app.screen = Screen::Chat;
        app.chat_focus = ChatFocus::History;

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

        assert!(matches!(effect, Effect::Quit));
    }

    #[test]
    fn cancelled_engine_errors_are_recognized_case_insensitively() {
        assert!(is_cancelled_error("attempt 01 is Cancelled, not running"));
    }

    #[test]
    fn confirm_exit_dismisses_on_negative_reply() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::load(
            StcliEngine::new(directory.path().join("stcli.sqlite3")),
            Config::default(),
            None,
        )
        .unwrap();
        app.popup = Some(Popup::ConfirmExit);

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        assert!(matches!(effect, Effect::None));
        assert!(app.popup.is_none());
    }

    #[test]
    fn fuzzy_filter_is_ordered_and_case_insensitive() {
        assert!(fuzzy_match("MRA", "Mira Roleplay"));
        assert!(!fuzzy_match("xyz", "Mira Roleplay"));
    }

    #[test]
    fn sort_keys_form_a_complete_cycle() {
        let mut key = SortKey::Modified;
        for _ in 0..5 {
            key = key.next();
        }
        assert_eq!(key, SortKey::Modified);
    }

    #[test]
    fn reasoning_delta_updates_only_the_active_generation() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::load(
            StcliEngine::new(directory.path().join("stcli.sqlite3")),
            Config::default(),
            None,
        )
        .unwrap();
        app.generation = Some(GenerationState {
            partial: String::new(),
            reasoning: String::new(),
            streaming: true,
            pending_input: None,
            continues: false,
        });

        app.handle_provider_event(ProviderEvent::ReasoningDelta {
            text: "Thinking live".to_owned(),
        });

        assert_eq!(app.generation.as_ref().unwrap().reasoning, "Thinking live");
        assert!(app.generation.as_ref().unwrap().partial.is_empty());
    }

    #[test]
    fn finish_generation_focuses_last_message_and_enables_follow() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);

        let mut app = App::load(
            StcliEngine::new(database),
            Config::default(),
            Some(created.session.session_id),
        )
        .unwrap();

        // Precondition: history is loaded and has at least the greeting.
        assert!(app.history.is_some());

        // Simulate mid-generation state where the user scrolled away.
        app.focused_message = 0;
        app.follow = false;
        app.generation = Some(GenerationState {
            partial: String::new(),
            streaming: false,
            reasoning: String::new(),
            pending_input: None,
            continues: false,
        });

        // Regression: finish_generation must move focus to the last message
        // and re-enable follow so the regenerated response is visible.
        app.finish_generation(Ok(EngineResult::DeletedTurn(stcli_core::DeletionReceipt {
            entity_id: EntityId::new(),
            deleted: false,
        })));

        let history = app.history.as_ref().expect("history reloaded");
        assert_eq!(
            app.focused_message,
            message_count(history).saturating_sub(1),
        );
        assert!(app.follow);
    }

    #[test]
    fn down_at_last_message_transitions_to_composer() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);

        let mut app = App::load(
            StcliEngine::new(database),
            Config::default(),
            Some(created.session.session_id),
        )
        .unwrap();

        app.chat_focus = ChatFocus::History;
        let history = app.history.as_ref().unwrap();
        app.focused_message = message_count(history).saturating_sub(1);

        let effect = app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

        assert!(matches!(effect, Effect::None));
        assert_eq!(app.chat_focus, ChatFocus::Composer);
    }

    #[test]
    fn j_at_last_message_transitions_to_composer() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);

        let mut app = App::load(
            StcliEngine::new(database),
            Config::default(),
            Some(created.session.session_id),
        )
        .unwrap();

        app.chat_focus = ChatFocus::History;
        let history = app.history.as_ref().unwrap();
        app.focused_message = message_count(history).saturating_sub(1);

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));

        assert!(matches!(effect, Effect::None));
        assert_eq!(app.chat_focus, ChatFocus::Composer);
    }
    #[test]
    fn session_without_turns_uses_greeting_preview() {
        // Regression test for issue 16: zero-turn sessions preview their greeting.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);

        let app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();

        assert_eq!(app.sessions[0].turn_count, 0);
        assert_eq!(app.sessions[0].last_message_preview, "Welcome.");
    }

    #[test]
    fn delete_session_list_entry_returns_session_and_branch_effects() {
        // Regression test for issue 16: sessions confirm purge while branches tombstone directly.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        let branch = store
            .create_branch(
                created.session.session_id,
                created.branch.branch_id,
                created.branch.greeting_index,
            )
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();

        let session_effect = app.delete_session_list_entry();
        assert!(matches!(session_effect, Effect::None));
        assert!(app.popup.is_some());
        let confirmed = app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(matches!(
            confirmed,
            Effect::Execute(EngineCommand::PurgeSession { session_id })
                if session_id == created.session.session_id
        ));

        app.show_branches = true;
        app.reload_branches();
        let branch_index = app
            .session_list_entries()
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    SessionListEntry::Branch { branch: row, .. }
                        if row.branch_id == branch.branch_id
                )
            })
            .unwrap();
        app.selected_session = branch_index;
        let branch_effect = app.delete_session_list_entry();
        assert!(matches!(
            branch_effect,
            Effect::Execute(EngineCommand::DeleteBranch { branch_id })
                if branch_id == branch.branch_id
        ));
    }
    #[test]
    fn deleting_session_does_not_report_session_not_found() {
        // Regression test: a successful session purge must not surface a stale not-found error.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();

        app.delete_session_list_entry();
        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        execute_command(&mut app, effect);

        assert!(app.toast.is_none());
        assert!(
            app.sessions
                .iter()
                .all(|session| session.session_id != created.session.session_id)
        );
    }

    #[test]
    fn deleting_child_branch_does_not_report_branch_not_found() {
        // Regression test: deleting a listed child branch must leave session summaries reloadable.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        let child = store
            .create_branch(
                created.session.session_id,
                created.branch.branch_id,
                created.branch.greeting_index,
            )
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.show_branches = true;
        app.reload_sessions().unwrap();
        app.selected_session = app
            .session_list_entries()
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    SessionListEntry::Branch { branch, .. }
                        if branch.branch_id == child.branch_id
                )
            })
            .unwrap();

        let effect = app.delete_session_list_entry();
        execute_command(&mut app, effect);

        assert!(app.toast.is_none());
        assert!(
            app.session_branches
                .get(&created.session.session_id)
                .is_some_and(|branches| {
                    branches
                        .iter()
                        .all(|branch| branch.branch_id != child.branch_id)
                })
        );
    }

    #[test]
    fn deletion_commands_are_not_redispatched_while_pending() {
        // Regression test: key repeats must not dispatch a second delete that reports not found.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();

        app.delete_session_list_entry();
        let first = app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(matches!(
            first,
            Effect::Execute(EngineCommand::PurgeSession { .. })
        ));

        let repeated = app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(matches!(repeated, Effect::None));
        assert!(app.popup.is_none());
    }

    #[test]
    fn deleting_root_branch_is_rejected_before_dispatch() {
        // Regression test: the root branch cannot be tombstoned without invalidating its session.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.show_branches = true;
        app.reload_sessions().unwrap();
        app.selected_session = app
            .session_list_entries()
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    SessionListEntry::Branch { branch, .. }
                        if branch.branch_id == created.branch.branch_id
                )
            })
            .unwrap();

        let effect = app.delete_session_list_entry();

        assert!(matches!(effect, Effect::None));
        assert!(app.toast.as_ref().is_some_and(|toast| !toast.error));
    }

    #[test]
    fn session_list_entries_flatten_sessions_and_branches() {
        // Regression test for issue 16: the navigation list preserves session/branch order.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(stcli_testkit::configuration(character.revision_hash), 0)
            .unwrap();
        let child = store
            .create_branch(
                created.session.session_id,
                created.branch.branch_id,
                created.branch.greeting_index,
            )
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.show_branches = true;
        app.reload_branches();

        let entries = app.session_list_entries();

        assert_eq!(entries.len(), 3);
        assert!(matches!(entries[0], SessionListEntry::Session(0)));
        assert!(matches!(
            &entries[1],
            SessionListEntry::Branch {
                session_index: 0,
                branch,
            } if branch.branch_id == created.branch.branch_id
        ));
        assert!(matches!(
            &entries[2],
            SessionListEntry::Branch {
                session_index: 0,
                branch,
            } if branch.branch_id == child.branch_id
        ));
    }

    #[test]
    fn empty_session_rename_dispatches_clear() {
        // Regression test for issue 16: submitting an empty rename clears custom_name.
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::load(
            StcliEngine::new(directory.path().join("stcli.sqlite3")),
            Config::default(),
            None,
        )
        .unwrap();
        let session_id = EntityId::new();
        app.popup = Some(Popup::Rename {
            session_id,
            input: String::new(),
        });

        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            effect,
            Effect::Execute(EngineCommand::RenameSession { session_id: id, name })
                if id == session_id && name.is_empty()
        ));
    }

    #[test]
    fn rename_completion_stays_on_sessions_screen() {
        // Regression test: renaming a Session must not open it in Chat.
        let (mut app, _directory) = app_with_session();
        let session_id = app.history.as_ref().unwrap().session.session_id;
        app.screen = Screen::Sessions;
        app.history = None;

        execute_command(
            &mut app,
            Effect::Execute(EngineCommand::RenameSession {
                session_id,
                name: "Renamed".to_owned(),
            }),
        );

        assert_eq!(app.screen, Screen::Sessions);
        assert_eq!(app.sessions[0].display_name, "Renamed");
    }

    #[test]
    fn new_session_popup_opens_with_n_key_and_shows_fields() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        drop(store);

        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        assert_eq!(app.screen, Screen::Sessions);

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(matches!(effect, Effect::None));

        let Some(Popup::NewSession(state)) = &app.popup else {
            panic!("expected Popup::NewSession");
        };
        assert_eq!(state.characters.len(), 1);
        assert_eq!(state.characters[0].revision_hash, character.revision_hash);
        assert_eq!(state.persona, "User");
        assert_eq!(state.selected_greeting, 0);
        assert_eq!(state.focused_field, 0);
    }
    #[test]
    fn new_session_selector_cycles_with_space() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.open_new_session_popup();
        let Some(Popup::NewSession(state)) = &mut app.popup else {
            panic!("expected Popup::NewSession");
        };
        state.characters.push(state.characters[0].clone());

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        let Some(Popup::NewSession(state)) = &app.popup else {
            panic!("expected Popup::NewSession");
        };
        assert_eq!(state.focused_field, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

        let Some(Popup::NewSession(state)) = &app.popup else {
            panic!("expected Popup::NewSession");
        };
        assert_eq!(state.selected_character, 1);
    }

    #[test]
    fn new_session_navigates_to_import_character_and_resumes_with_selection() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let card_path = directory.path().join("character.json");
        fs::write(&card_path, stcli_testkit::fixtures::minimal_card()).unwrap();

        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        let Some(Popup::ImportCharacter(import_state)) = &mut app.popup else {
            panic!("expected Popup::ImportCharacter");
        };
        assert!(import_state.return_to_new_session.is_some());

        for c in card_path.to_str().unwrap().chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        execute_command(&mut app, effect);

        let Some(Popup::NewSession(session_state)) = &app.popup else {
            panic!("expected resumed Popup::NewSession");
        };
        assert_eq!(session_state.characters.len(), 1);
        assert_eq!(session_state.selected_character, 0);
    }

    #[test]
    fn new_provider_profile_saves_and_reloads_config_and_resumes_session() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        drop(store);
        let config_dir = directory.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();

        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.set_config_dir(config_dir.clone());
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let Some(Popup::NewProviderProfile(state)) = &mut app.popup else {
            panic!("expected Popup::NewProviderProfile");
        };
        assert_eq!(state.focused_field, 1);

        for c in "openrouter-test".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for _ in 0..8 {
            app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for c in "https://openrouter.ai".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for c in "anthropic/claude-3.5-sonnet".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(effect, Effect::None));

        assert!(app.config.core.providers.contains_key("openrouter-test"));

        let Some(Popup::NewSession(session_state)) = &app.popup else {
            panic!("expected resumed Popup::NewSession");
        };
        assert_eq!(session_state.providers, vec!["openrouter-test"]);
        assert_eq!(session_state.selected_provider, 0);
    }

    #[test]
    fn provider_template_timeout_is_persisted() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let config_dir = directory.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("provider-templates.toml"),
            r#"
[fast]
name = "Fast"
id = "fast"
base_url = "https://fast.example.com"
chat_completions_path = "/v1/chat/completions"
default_model = "fast-model"
stream = true
timeout_seconds = 45
"#,
        )
        .unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.set_config_dir(config_dir.clone());
        app.open_new_provider_profile_popup(None, false);
        let Some(Popup::NewProviderProfile(state)) = &mut app.popup else {
            panic!("expected Popup::NewProviderProfile");
        };
        state.focused_field = 0;

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        assert!(matches!(effect, Effect::None));
        let config = stcli_core::Config::load(&config_dir).unwrap();
        assert_eq!(config.providers["fast"].timeout_seconds, 45);
    }

    #[test]
    fn submitting_new_session_creates_session_and_transitions_to_chat() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let config_dir = directory.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();

        let mut store = stcli_core::Store::open(&database).unwrap();
        let _character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        drop(store);

        let provider_settings = ProviderSettings {
            id: "my-provider".to_owned(),
            base_url: "https://api.example.com".to_owned(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 60,
            ca_certificate_pem: None,
            model: "test-model".to_owned(),
            stream: false,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        };
        stcli_core::Config::add_provider_profile(&config_dir, "my-provider", provider_settings)
            .unwrap();

        let mut app = App::load(
            StcliEngine::new(database),
            Config::load(&config_dir).unwrap(),
            None,
        )
        .unwrap();
        app.set_config_dir(config_dir);

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(
            effect,
            Effect::Execute(EngineCommand::CreateSession { .. })
        ));

        execute_command(&mut app, effect);

        assert_eq!(app.screen, Screen::Chat);
        assert!(app.history.is_some());
        assert!(app.popup.is_none());
    }
}
