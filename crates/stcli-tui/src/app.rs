use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use stcli_core::{
    ArtifactKind, ArtifactRecord, AttemptStatus, BranchHistory, BranchProjection,
    CandidateProjection, EngineCommand, EngineInspection, EngineQuery, EngineResult, EntityId,
    ProviderEvent, SessionSummary, StcliEngine, decode_artifact,
};
use std::collections::HashMap;

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
        };
        app.reload_sessions()?;
        if let Some(session_id) = direct_session
            && let Err(error) = app.open_session(session_id)
        {
            app.show_error(error.to_string());
        }
        Ok(app)
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
        self.history = Some(history);
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
        self.history = Some(history);
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
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(names.len().saturating_sub(1))
                }
                KeyCode::Enter => {
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
        if names.is_empty() {
            self.show_error("No provider profiles are configured");
        } else {
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
    }

    fn open_preset_popup(&mut self) {
        match self.engine.inspect(EngineQuery::Artifacts {
            kind: Some(ArtifactKind::ChatCompletionPreset),
        }) {
            Ok(EngineInspection::Artifacts(records)) => {
                let rows = records
                    .into_iter()
                    .map(|record| {
                        let label = self
                            .engine
                            .inspect(EngineQuery::ArtifactSource {
                                revision_hash: record.revision_hash.clone(),
                            })
                            .ok()
                            .and_then(|inspection| match inspection {
                                EngineInspection::ArtifactSource(source) => {
                                    decode_artifact(&source).ok()
                                }
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
                    .collect::<Vec<_>>();
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
            Err(error) => self.show_error(error.to_string()),
            _ => unreachable!(),
        }
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
                | Popup::Rename { .. },
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
}
