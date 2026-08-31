use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use stcli_core::{
    ArtifactKind, ArtifactRecord, AttemptStatus, BranchHistory, BranchProjection,
    CHAT_COMPLETION_CHARACTER_ID, CandidateProjection, ContentHash,
    DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID, EngineCommand, EngineInspection, EngineQuery, EngineResult,
    EntityId, Persona, PersonaStore, PresetPatch, PromptPreset, ProviderEvent, ProviderSettings,
    ProviderTemplate, RegexPlacement, SessionConfiguration, SessionSummary, StcliEngine,
    available_duplicated_session_name, clone_and_patch_preset, decode_artifact,
    transform_preset_content, validate_provider_settings,
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
    pub summary: PresetSummary,
}

#[derive(Clone, Debug)]
pub struct PromptOrderOption {
    pub identifier: String,
    pub preset_enabled: bool,
    pub override_enabled: Option<bool>,
    pub enabled: bool,
    pub marker: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct PresetConstraint {
    pub kind: String,
    pub name: String,
    pub members: Vec<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct PresetDiagnostic {
    pub identifier: String,
    pub severity: String,
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct PresetInspection {
    #[serde(default)]
    constraints: Vec<PresetConstraint>,
    #[serde(default)]
    diagnostics: Vec<PresetDiagnostic>,
}

#[derive(Clone, Debug, Default)]
pub struct PresetSummary {
    pub prompt_count: usize,
    pub order_profile: String,
    pub system_prompt_enabled: bool,
    pub prompt_order: Vec<PromptOrderOption>,
    pub temperature: Option<String>,
    pub top_p: Option<String>,
    pub max_tokens: Option<String>,
    pub scripts: Vec<PresetScriptSummary>,
    pub constraints: Vec<PresetConstraint>,
    pub diagnostics: Vec<PresetDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct PresetScriptSummary {
    pub name: String,
    pub placement: String,
    pub digest: String,
}

impl PresetSummary {
    fn from_semantic(value: &serde_json::Value, source_revision: &ContentHash) -> Self {
        let parsed = PromptPreset::parse(value, CHAT_COMPLETION_CHARACTER_ID).ok();
        let profiles = value
            .get("prompt_order")
            .and_then(serde_json::Value::as_array);
        let exact_profile = profiles.is_some_and(|profiles| {
            profiles.iter().any(|profile| {
                profile
                    .get("character_id")
                    .and_then(serde_json::Value::as_u64)
                    == Some(CHAT_COMPLETION_CHARACTER_ID)
            })
        });
        let fallback_profile_id = profiles.and_then(|profiles| {
            profiles.iter().find_map(|profile| {
                profile
                    .get("character_id")
                    .and_then(serde_json::Value::as_u64)
            })
        });
        let prompt_order = parsed
            .as_ref()
            .map(|preset| {
                preset
                    .order
                    .iter()
                    .map(|entry| PromptOrderOption {
                        identifier: entry.identifier.clone(),
                        preset_enabled: entry.enabled,
                        override_enabled: None,
                        enabled: entry.enabled,
                        marker: value
                            .get("prompts")
                            .and_then(serde_json::Value::as_array)
                            .into_iter()
                            .flatten()
                            .find(|prompt| {
                                prompt.get("identifier").and_then(serde_json::Value::as_str)
                                    == Some(entry.identifier.as_str())
                            })
                            .and_then(|prompt| prompt.get("marker"))
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let system_prompt_enabled = parsed.as_ref().is_some_and(|preset| {
            preset
                .order
                .iter()
                .any(|entry| entry.identifier == "main" && entry.enabled)
        });
        let scripts = transform_preset_content("", source_revision, value, &[])
            .scripts
            .into_iter()
            .map(|script| {
                let name = script
                    .metadata
                    .get("scriptName")
                    .or_else(|| script.metadata.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Unnamed script")
                    .to_owned();
                let placement = script
                    .placement
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_u64)
                    .map(regex_placement_name)
                    .collect::<Vec<_>>()
                    .join(", ");
                let digest = script.digest.to_string();
                let digest = digest
                    .strip_prefix("sha256:")
                    .unwrap_or(&digest)
                    .chars()
                    .take(12)
                    .collect();
                PresetScriptSummary {
                    name,
                    placement,
                    digest,
                }
            })
            .collect();
        Self {
            prompt_count: parsed.as_ref().map_or_else(
                || {
                    value
                        .get("prompts")
                        .and_then(serde_json::Value::as_array)
                        .map_or(0, Vec::len)
                },
                |preset| preset.prompts.len(),
            ),
            order_profile: if exact_profile {
                "Chat Completion (100001)".to_owned()
            } else {
                fallback_profile_id.map_or_else(
                    || "Fallback profile (100001 unavailable)".to_owned(),
                    |id| format!("Fallback profile ({id}; 100001 unavailable)"),
                )
            },
            system_prompt_enabled,
            prompt_order,
            temperature: summary_value(value.get("temperature")),
            top_p: summary_value(value.get("top_p")),
            max_tokens: summary_value(
                value
                    .get("max_tokens")
                    .or_else(|| value.get("openai_max_tokens")),
            ),
            scripts,
            constraints: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}
fn regex_placement_name(code: u64) -> String {
    [
        (RegexPlacement::UserInput, "UserInput"),
        (RegexPlacement::AiOutput, "AiOutput"),
        (RegexPlacement::SlashCommand, "SlashCommand"),
        (RegexPlacement::WorldInfo, "WorldInfo"),
        (RegexPlacement::Reasoning, "Reasoning"),
    ]
    .into_iter()
    .find_map(|(placement, name)| (placement.code() == code).then(|| name.to_owned()))
    .unwrap_or_else(|| code.to_string())
}

fn summary_value(value: Option<&serde_json::Value>) -> Option<String> {
    value.map(|value| match value {
        serde_json::Value::String(value) => value.clone(),
        value => value.to_string(),
    })
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
    pub personas: Vec<Persona>,
    pub selected_persona: usize,
    pub active_persona: Option<usize>,
    pub persona: String,
    pub persona_description: String,
    pub selected_greeting: usize,
    pub focused_field: usize,
}

#[derive(Clone, Debug)]
pub struct PresetPickerState {
    pub rows: Vec<PresetOption>,
    pub selected: usize,
    pub return_to: ModalTarget,
    pub filter: String,
    pub filtering: bool,
    pub show_details: bool,
    pub details_scroll: usize,
    pub order_focus: Option<usize>,
}

impl PresetPickerState {
    pub fn filtered_rows(&self) -> Vec<&PresetOption> {
        let filter = self.filter.to_lowercase();
        self.rows
            .iter()
            .filter(|row| filter.is_empty() || row.label.to_lowercase().contains(&filter))
            .collect()
    }

    fn selected_revision(&self) -> Option<ContentHash> {
        self.selected
            .checked_sub(1)
            .and_then(|index| self.filtered_rows().get(index).copied())
            .map(|row| row.record.revision_hash.clone())
    }

    fn select(&mut self, index: usize) {
        if self.selected != index {
            self.selected = index;
            self.details_scroll = 0;
            self.order_focus = None;
        }
    }

    fn select_filtered_revision(&mut self, revision: Option<&ContentHash>) {
        let selected = revision.and_then(|revision| {
            self.filtered_rows()
                .iter()
                .position(|row| &row.record.revision_hash == revision)
        });
        let index = selected.map_or_else(
            || usize::from(!self.filter.is_empty() && !self.filtered_rows().is_empty()),
            |index| index + 1,
        );
        self.select(index);
        if self.selected_revision().as_ref() != revision {
            self.details_scroll = 0;
        }
    }
}
#[derive(Clone, Debug)]
pub enum ModalTarget {
    Sessions,
    Chat,
    NewSession(Box<NewSessionState>),
    Providers {
        return_to: Box<ModalTarget>,
        selected_name: Option<String>,
        selected: usize,
    },
    Presets(Box<PresetPickerState>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportFocus {
    PathInput,
    NameInput,
    DirectoryList,
}

#[derive(Clone, Debug)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub struct FileBrowserState {
    pub directory: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub selected: usize,
    pub show_hidden: bool,
    pub access_denied: bool,
}

#[derive(Clone, Debug)]
pub struct ImportArtifactState {
    pub expected_kind: Option<ArtifactKind>,
    pub return_to: ModalTarget,
    pub input: String,
    pub name: String,
    pub focus: ImportFocus,
    pub browser: FileBrowserState,
    pub completion_hint: Option<String>,
}

const CHARACTER_EXTENSIONS: &[&str] = &["png", "apng", "webp", "charx", "json"];
const PRESET_EXTENSIONS: &[&str] = &["json"];

impl ImportArtifactState {
    pub fn new(
        expected_kind: Option<ArtifactKind>,
        return_to: ModalTarget,
        directory: PathBuf,
    ) -> Self {
        let mut state = Self {
            expected_kind,
            return_to,
            input: String::new(),
            name: String::new(),
            focus: ImportFocus::PathInput,
            browser: FileBrowserState {
                directory,
                entries: Vec::new(),
                selected: 0,
                show_hidden: false,
                access_denied: false,
            },
            completion_hint: None,
        };
        state.rescan();
        state
    }

    fn expects_character(&self) -> bool {
        match self.expected_kind {
            Some(kind) => {
                kind != ArtifactKind::ChatCompletionPreset && kind != ArtifactKind::Lorebook
            }
            None => matches!(&self.return_to, ModalTarget::NewSession(_)),
        }
    }

    fn expects_preset(&self) -> bool {
        self.expected_kind == Some(ArtifactKind::ChatCompletionPreset)
    }

    fn allowed_extensions(&self) -> Option<&'static [&'static str]> {
        match self.expected_kind {
            Some(ArtifactKind::ChatCompletionPreset) | Some(ArtifactKind::Lorebook) => {
                Some(PRESET_EXTENSIONS)
            }
            Some(_) => Some(CHARACTER_EXTENSIONS),
            None if self.expects_character() => Some(CHARACTER_EXTENSIONS),
            None => None,
        }
    }

    fn rescan(&mut self) {
        self.browser.entries = scan_directory(
            &self.browser.directory,
            self.browser.show_hidden,
            self.allowed_extensions(),
            &mut self.browser.access_denied,
        );
        self.browser.selected = 0;
        self.completion_hint = None;
    }

    fn navigate(&mut self, directory: PathBuf) {
        self.browser.directory = fs::canonicalize(&directory).unwrap_or(directory);
        self.rescan();
    }

    fn navigate_parent(&mut self) {
        if let Some(parent) = self.browser.directory.parent().map(Path::to_path_buf) {
            self.navigate(parent);
        }
    }

    fn resolve_input(&self) -> PathBuf {
        let expanded = expand_home_path(self.input.trim());
        if expanded.is_absolute() {
            expanded
        } else {
            self.browser.directory.join(expanded)
        }
    }

    fn focus_directory_list(&mut self) {
        if !self.input.trim().is_empty() {
            let expanded = self.resolve_input();
            if expanded.is_dir() {
                self.navigate(expanded);
                self.input.clear();
            }
        }
        self.focus = ImportFocus::DirectoryList;
        self.completion_hint = None;
    }

    /// Attempts shell-style segment completion. Returns `false` when no
    /// progress or new hint is possible, letting `Tab` shift focus instead.
    fn tab_complete(&mut self) -> bool {
        let input = self.input.clone();
        let (dir_part, prefix) = match input.rfind('/') {
            Some(index) => (&input[..=index], &input[index + 1..]),
            None => ("", input.as_str()),
        };
        let base = if dir_part.is_empty() {
            self.browser.directory.clone()
        } else {
            let expanded = expand_home_path(dir_part);
            if expanded.is_absolute() {
                expanded
            } else {
                self.browser.directory.join(expanded)
            }
        };
        let Ok(read) = fs::read_dir(&base) else {
            return false;
        };
        let mut matches: Vec<(String, bool)> = read
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.'))
                {
                    return None;
                }
                let is_dir = entry
                    .file_type()
                    .map(|file_type| {
                        file_type.is_dir()
                            || (file_type.is_symlink()
                                && entry.metadata().map(|meta| meta.is_dir()).unwrap_or(false))
                    })
                    .unwrap_or(false);
                Some((name, is_dir))
            })
            .collect();
        if matches.is_empty() {
            return false;
        }
        matches.sort();
        if matches.len() == 1 {
            let (name, is_dir) = &matches[0];
            let mut completed = format!("{dir_part}{name}");
            if *is_dir {
                completed.push('/');
            }
            if completed == self.input {
                return false;
            }
            self.input = completed;
            self.completion_hint = None;
            return true;
        }
        let common = longest_common_prefix(matches.iter().map(|(name, _)| name.as_str()));
        let progressed = common.len() > prefix.len();
        if progressed {
            self.input = format!("{dir_part}{common}");
        }
        let hint = format!("{} matches", matches.len());
        let new_hint = self.completion_hint.as_deref() != Some(hint.as_str());
        self.completion_hint = Some(hint);
        progressed || new_hint
    }
}

fn scan_directory(
    directory: &Path,
    show_hidden: bool,
    extensions: Option<&[&str]>,
    access_denied: &mut bool,
) -> Vec<DirectoryEntry> {
    let mut entries = Vec::new();
    if directory.parent().is_some() {
        entries.push(DirectoryEntry {
            name: "..".to_owned(),
            path: directory.join(".."),
            is_dir: true,
        });
    }
    let read = match fs::read_dir(directory) {
        Ok(read) => read,
        Err(_) => {
            *access_denied = true;
            return entries;
        }
    };
    *access_denied = false;
    let mut directories = Vec::new();
    let mut files = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_dir = file_type.is_dir()
            || (file_type.is_symlink()
                && entry.metadata().map(|meta| meta.is_dir()).unwrap_or(false));
        let path = entry.path();
        if is_dir {
            directories.push(DirectoryEntry {
                name,
                path,
                is_dir: true,
            });
        } else if extension_allowed(&name, extensions) {
            files.push(DirectoryEntry {
                name,
                path,
                is_dir: false,
            });
        }
    }
    let by_name = |left: &DirectoryEntry, right: &DirectoryEntry| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    };
    directories.sort_by(by_name);
    files.sort_by(by_name);
    entries.extend(directories);
    entries.extend(files);
    entries
}

fn extension_allowed(name: &str, extensions: Option<&[&str]>) -> bool {
    match extensions {
        None => true,
        Some(extensions) => Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extensions
                    .iter()
                    .any(|allowed| extension.eq_ignore_ascii_case(allowed))
            }),
    }
}

fn longest_common_prefix<'a>(mut names: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = names.next() else {
        return String::new();
    };
    let mut prefix = first.to_owned();
    for name in names {
        while !name.starts_with(&prefix) {
            prefix.pop();
        }
    }
    prefix
}

#[derive(Clone, Debug)]
pub struct ClonePresetState {
    pub source_revision: ContentHash,
    pub name: String,
    pub temperature: String,
    pub max_context: String,
    pub max_tokens: String,
    pub use_sysprompt: bool,
    pub focused_field: usize,
    pub picker: Box<PresetPickerState>,
}

#[derive(Clone, Debug)]
pub struct PersonasState {
    pub personas: Vec<Persona>,
    pub selected: usize,
    pub return_to: ModalTarget,
}

#[derive(Clone, Debug)]
pub struct PersonaEditorState {
    pub original_key: Option<String>,
    pub copy_source_key: Option<String>,
    pub name: String,
    pub description: String,
    pub focused_field: usize,
    pub manager: Box<PersonasState>,
    pub resume_new_session: bool,
}

#[derive(Clone, Debug)]
pub struct ImportPersonasState {
    pub input: String,
    pub manager: Box<PersonasState>,
}

#[derive(Clone, Debug)]
pub struct ProviderProfileState {
    pub templates: Vec<ProviderTemplate>,
    pub selected_template: usize,
    pub original_name: Option<String>,
    pub copy_source_name: Option<String>,
    pub original_settings: Option<ProviderSettings>,
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub chat_path: String,
    pub api_key_env: String,
    pub stream: bool,
    pub timeout_seconds: String,
    pub focused_field: usize,
    pub cursor_position: usize,
    pub return_to: ModalTarget,
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
        return_to: ModalTarget,
    },
    Presets(Box<PresetPickerState>),
    Rename {
        session_id: EntityId,
        input: String,
    },
    DuplicateSession {
        session_id: EntityId,
        input: String,
    },
    ConfirmExit,
    ConfirmDelete {
        session_id: EntityId,
        name: String,
    },
    ConfirmDeleteProvider {
        name: String,
        return_to: ModalTarget,
    },
    NewSession(Box<NewSessionState>),
    ImportArtifact(ImportArtifactState),
    ProviderProfile(Box<ProviderProfileState>),
    ClonePreset(Box<ClonePresetState>),
    Personas(Box<PersonasState>),
    PersonaEditor(Box<PersonaEditorState>),
    ImportPersonas(Box<ImportPersonasState>),
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
    import_browser_dir: Option<PathBuf>,
    pending_auto_disabled: Vec<String>,
    pending_override_message: Option<String>,
    pending_preset_toggle: Option<(String, bool)>,
    pending_directive_warnings: Vec<String>,
    pending_branch_creation: bool,
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
            import_browser_dir: None,
            pending_auto_disabled: Vec::new(),
            pending_directive_warnings: Vec::new(),
            pending_override_message: None,
            pending_preset_toggle: None,
            pending_branch_creation: false,
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
            KeyCode::Char('c') => self.start_duplicate_session(),
            KeyCode::Char('n') => self.open_new_session_popup(),
            KeyCode::Char('p') => self.open_provider_popup(ModalTarget::Sessions),
            KeyCode::Char('P') => self.open_preset_popup(ModalTarget::Sessions),
            KeyCode::Char('u') => self.open_personas(ModalTarget::Sessions),
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
        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.open_provider_popup(ModalTarget::Chat);
            return Effect::None;
        }
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
            KeyCode::Char('b') => return self.create_branch_from_focus(),
            KeyCode::Char('B') => self.open_branch_popup(),
            KeyCode::Char('p') => self.open_provider_popup(ModalTarget::Chat),
            KeyCode::Char('P') => self.open_preset_popup(ModalTarget::Chat),
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
                Popup::Presets(mut state) if state.filtering || !state.filter.is_empty() => {
                    let selected_revision = state.selected_revision();
                    state.filter.clear();
                    state.filtering = false;
                    state.select_filtered_revision(selected_revision.as_ref());
                    self.popup = Some(Popup::Presets(state));
                }
                Popup::Presets(state) => self.restore_modal(state.return_to),
                Popup::ImportArtifact(state) => self.restore_modal(state.return_to),
                Popup::ClonePreset(state) => self.popup = Some(Popup::Presets(state.picker)),
                Popup::Providers { return_to, .. } => self.restore_modal(return_to),
                Popup::ProviderProfile(state) => self.restore_modal(state.return_to),
                Popup::ConfirmDeleteProvider { return_to, .. } => self.restore_modal(return_to),
                Popup::Personas(state) => self.restore_modal(state.return_to),
                Popup::PersonaEditor(state) => {
                    if state.resume_new_session {
                        self.restore_modal(state.manager.return_to)
                    } else {
                        self.popup = Some(Popup::Personas(state.manager))
                    }
                }
                Popup::ImportPersonas(state) => self.popup = Some(Popup::Personas(state.manager)),
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
            Popup::ConfirmDeleteProvider { name, return_to } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let config_dir = self.profile_config_dir();
                    match stcli_core::Config::remove_provider_profile(&config_dir, name) {
                        Ok(true) => {
                            if let Err(error) = self.reload_config() {
                                self.show_error(format!("Failed to reload config: {error}"));
                            } else {
                                let deleted = name.clone();
                                if let ModalTarget::Providers { selected, .. } = return_to {
                                    *selected = (*selected)
                                        .min(self.config.core.providers.len().saturating_sub(1));
                                }
                                self.restore_modal(return_to.clone());
                                self.show_info(format!("Deleted provider profile '{deleted}'"));
                            }
                        }
                        Ok(false) => {
                            self.show_error(format!("Provider profile '{name}' was not found"));
                            self.restore_modal(return_to.clone());
                        }
                        Err(error) => {
                            self.show_error(format!("Failed to delete profile: {error}"));
                            self.popup = Some(Popup::ConfirmDeleteProvider {
                                name: name.clone(),
                                return_to: return_to.clone(),
                            });
                        }
                    }
                    return Effect::None;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter => {
                    self.restore_modal(return_to.clone());
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
            Popup::Providers {
                names,
                selected,
                return_to,
            } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1).min(names.len()),
                KeyCode::Char('a') | KeyCode::Char('n') => {
                    let target = Self::provider_modal_target(
                        return_to.clone(),
                        names.get(*selected).cloned(),
                        *selected,
                    );
                    self.open_provider_profile_popup(None, target);
                    return Effect::None;
                }
                KeyCode::Char('c') => {
                    if let Some(source_name) = names.get(*selected).cloned() {
                        let target = Self::provider_modal_target(
                            return_to.clone(),
                            Some(source_name.clone()),
                            *selected,
                        );
                        self.open_provider_profile_copy(source_name, target);
                    }
                    return Effect::None;
                }
                KeyCode::Char('e') | KeyCode::Char('m') => {
                    if let Some(name) = names.get(*selected).cloned() {
                        let target = Self::provider_modal_target(
                            return_to.clone(),
                            Some(name.clone()),
                            *selected,
                        );
                        self.open_provider_profile_popup(Some(name), target);
                    }
                    return Effect::None;
                }
                KeyCode::Char('x') | KeyCode::Char('d') => {
                    if let Some(name) = names.get(*selected).cloned() {
                        self.popup = Some(Popup::ConfirmDeleteProvider {
                            name,
                            return_to: Self::provider_modal_target(
                                return_to.clone(),
                                None,
                                *selected,
                            ),
                        });
                    }
                    return Effect::None;
                }
                KeyCode::Enter => {
                    if *selected == names.len() {
                        let target =
                            Self::provider_modal_target(return_to.clone(), None, *selected);
                        self.open_provider_profile_popup(None, target);
                        return Effect::None;
                    }
                    let Some(name) = names.get(*selected) else {
                        return Effect::None;
                    };
                    if let ModalTarget::NewSession(mut state) = return_to.clone() {
                        state.providers = names.clone();
                        state.selected_provider = *selected;
                        self.popup = Some(Popup::NewSession(state));
                        return Effect::None;
                    }
                    if let (Some(history), Some(provider)) =
                        (&self.history, self.config.core.providers.get(name))
                    {
                        let mut configuration = history.configuration.configuration.clone();
                        configuration.provider = provider.clone();
                        return Effect::Execute(EngineCommand::UpdateConfiguration {
                            session_id: history.session.session_id,
                            configuration: Box::new(configuration),
                        });
                    }
                    self.restore_modal(return_to.clone());
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
            Popup::DuplicateSession { session_id, input } => match key.code {
                KeyCode::Enter => {
                    return Effect::Execute(EngineCommand::DuplicateSession {
                        session_id: *session_id,
                        branch_id: None,
                        up_to_turn_id: None,
                        new_name: Some(std::mem::take(input)),
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
            Popup::Presets(state) => {
                if state.filtering {
                    let selected_revision = state.selected_revision();
                    match key.code {
                        KeyCode::Tab => {
                            state.show_details = !state.show_details;
                        }
                        KeyCode::Enter => state.filtering = false,
                        KeyCode::Backspace => {
                            state.filter.pop();
                        }
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            state.filter.push(character);
                        }
                        _ => {}
                    }
                    state.select_filtered_revision(selected_revision.as_ref());
                } else {
                    match key.code {
                        KeyCode::PageDown if state.show_details && state.order_focus.is_none() => {
                            state.details_scroll += 10;
                        }
                        KeyCode::PageUp if state.show_details && state.order_focus.is_none() => {
                            state.details_scroll = state.details_scroll.saturating_sub(10);
                        }
                        KeyCode::Down if state.show_details => {
                            let order_len = state
                                .selected
                                .checked_sub(1)
                                .and_then(|index| state.filtered_rows().get(index).copied())
                                .map_or(0, |row| row.summary.prompt_order.len());
                            if let Some(selected_order) = state.order_focus.as_mut() {
                                *selected_order =
                                    (*selected_order + 1).min(order_len.saturating_sub(1));
                            } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                                state.details_scroll += 1;
                            } else {
                                state.select((state.selected + 1).min(state.filtered_rows().len()));
                            }
                        }
                        KeyCode::Up if state.show_details => {
                            if let Some(selected_order) = state.order_focus.as_mut() {
                                *selected_order = selected_order.saturating_sub(1);
                            } else if key.modifiers.contains(KeyModifiers::SHIFT) {
                                state.details_scroll = state.details_scroll.saturating_sub(1);
                            } else {
                                state.select(state.selected.saturating_sub(1));
                            }
                        }
                        KeyCode::Right if state.show_details => {
                            if state
                                .selected
                                .checked_sub(1)
                                .and_then(|index| state.filtered_rows().get(index).copied())
                                .is_some_and(|row| !row.summary.prompt_order.is_empty())
                            {
                                state.order_focus = Some(0);
                            }
                        }
                        KeyCode::Left if state.order_focus.is_some() => {
                            state.order_focus = None;
                        }
                        KeyCode::Char(' ')
                            if state.show_details
                                && state.order_focus.is_some()
                                && key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            let selected_order = state.order_focus.expect("order focus exists");
                            let Some(entry) = state
                                .selected
                                .checked_sub(1)
                                .and_then(|index| state.filtered_rows().get(index).copied())
                                .and_then(|row| row.summary.prompt_order.get(selected_order))
                            else {
                                return Effect::None;
                            };
                            let Some(history) = &self.history else {
                                return Effect::None;
                            };
                            let identifier = entry.identifier.clone();
                            let enabled = Some(!entry.enabled);
                            let session_id = history.session.session_id;
                            self.pending_override_message =
                                Some("Updated Session Prompt Order Override".to_owned());
                            self.popup = Some(popup.clone());
                            return Effect::Execute(EngineCommand::UpdatePromptOrderOverride {
                                session_id,
                                identifier,
                                enabled,
                            });
                        }
                        KeyCode::Char('r') if state.show_details && state.order_focus.is_some() => {
                            let selected_order = state.order_focus.expect("order focus exists");
                            let Some(entry) = state
                                .selected
                                .checked_sub(1)
                                .and_then(|index| state.filtered_rows().get(index).copied())
                                .and_then(|row| row.summary.prompt_order.get(selected_order))
                            else {
                                return Effect::None;
                            };
                            if entry.override_enabled.is_none() {
                                return Effect::None;
                            }
                            let Some(history) = &self.history else {
                                return Effect::None;
                            };
                            let identifier = entry.identifier.clone();
                            let session_id = history.session.session_id;
                            self.pending_override_message =
                                Some("Reset Prompt Order Override to preset default".to_owned());
                            self.popup = Some(popup.clone());
                            return Effect::Execute(EngineCommand::UpdatePromptOrderOverride {
                                session_id,
                                identifier,
                                enabled: None,
                            });
                        }
                        KeyCode::Char(' ') if state.show_details && state.order_focus.is_some() => {
                            let selected_order = state.order_focus.expect("order focus exists");
                            let Some(row) = state
                                .selected
                                .checked_sub(1)
                                .and_then(|index| state.filtered_rows().get(index).copied())
                            else {
                                return Effect::None;
                            };
                            let Some(entry) = row.summary.prompt_order.get(selected_order) else {
                                return Effect::None;
                            };
                            let session_id = match &state.return_to {
                                ModalTarget::Chat => {
                                    let Some(history) = &self.history else {
                                        return Effect::None;
                                    };
                                    Some(history.session.session_id)
                                }
                                ModalTarget::Sessions | ModalTarget::NewSession(_) => None,
                                ModalTarget::Providers { .. } | ModalTarget::Presets(_) => {
                                    return Effect::None;
                                }
                            };
                            let revision_hash = row.record.revision_hash.clone();
                            let identifier = entry.identifier.clone();
                            let enabling = !entry.enabled;
                            let mut changes = BTreeMap::new();
                            changes.insert(identifier.clone(), enabling);
                            let mut auto_disabled = Vec::new();
                            if enabling {
                                for constraint in &row.summary.constraints {
                                    if !matches!(
                                        constraint.kind.as_str(),
                                        "named-group" | "exclusive-pair" | "category-limit"
                                    ) {
                                        continue;
                                    }
                                    if !constraint.members.contains(&identifier) {
                                        continue;
                                    }
                                    for sibling in &constraint.members {
                                        if sibling != &identifier
                                            && row.summary.prompt_order.iter().any(|entry| {
                                                entry.identifier == *sibling && entry.enabled
                                            })
                                        {
                                            changes.insert(sibling.clone(), false);
                                            if !auto_disabled.contains(sibling) {
                                                auto_disabled.push(sibling.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            let warnings = if enabling {
                                row.summary
                                    .diagnostics
                                    .iter()
                                    .filter(|diagnostic| {
                                        diagnostic.identifier == identifier
                                            || diagnostic.target.as_deref()
                                                == Some(identifier.as_str())
                                    })
                                    .map(|diagnostic| diagnostic.message.clone())
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            self.pending_auto_disabled = auto_disabled;
                            self.pending_preset_toggle = Some((identifier.clone(), enabling));
                            self.pending_directive_warnings = warnings;
                            self.popup = Some(popup.clone());
                            return Effect::Execute(EngineCommand::UpdatePromptOrder {
                                session_id,
                                revision_hash,
                                character_id: Some(CHAT_COMPLETION_CHARACTER_ID),
                                changes,
                            });
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            state.select(state.selected.saturating_sub(1))
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            state.select((state.selected + 1).min(state.filtered_rows().len()))
                        }
                        KeyCode::Char('n') if matches!(state.return_to, ModalTarget::Sessions) => {
                            let revision = state.selected_revision();
                            self.open_new_session_popup();
                            self.preselect_new_session_preset(revision.as_ref());
                            return Effect::None;
                        }
                        KeyCode::Char('/') => state.filtering = true,
                        KeyCode::Char('c') => {
                            self.open_clone_preset(state.as_ref().clone());
                            return Effect::None;
                        }
                        KeyCode::Char('i') => {
                            self.open_import_artifact(
                                Some(ArtifactKind::ChatCompletionPreset),
                                ModalTarget::Presets(state.clone()),
                            );
                            return Effect::None;
                        }
                        KeyCode::Char('d') | KeyCode::Tab => {
                            state.show_details = !state.show_details;
                            state.order_focus = None;
                        }
                        KeyCode::Enter => {
                            let revision = state.selected_revision();
                            match state.return_to.clone() {
                                ModalTarget::NewSession(mut session) => {
                                    session.presets = state.rows.clone();
                                    session.selected_preset = revision
                                        .as_ref()
                                        .and_then(|revision| {
                                            session.presets.iter().position(|preset| {
                                                &preset.record.revision_hash == revision
                                            })
                                        })
                                        .map_or(0, |index| index + 1);
                                    self.popup = Some(Popup::NewSession(session));
                                    return Effect::None;
                                }
                                ModalTarget::Chat => {
                                    if let Some(history) = &self.history {
                                        let mut configuration =
                                            history.configuration.configuration.clone();
                                        configuration.prompt_preset_revision = revision;
                                        return Effect::Execute(
                                            EngineCommand::UpdateConfiguration {
                                                session_id: history.session.session_id,
                                                configuration: Box::new(configuration),
                                            },
                                        );
                                    }
                                }
                                ModalTarget::Sessions => {
                                    self.popup = None;
                                    return Effect::None;
                                }
                                ModalTarget::Providers { .. } | ModalTarget::Presets(_) => {}
                            }
                            return Effect::None;
                        }
                        _ => {}
                    }
                }
            }
            Popup::ImportArtifact(state) => {
                match state.focus {
                    ImportFocus::PathInput => match key.code {
                        KeyCode::Backspace => {
                            state.input.pop();
                            state.completion_hint = None;
                        }
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            state.input.push(character);
                            state.completion_hint = None;
                        }
                        KeyCode::Tab => {
                            if state.expects_preset() {
                                state.focus = ImportFocus::NameInput;
                            } else if state.input.is_empty() || !state.tab_complete() {
                                state.focus_directory_list();
                            }
                        }
                        KeyCode::Down if state.expects_preset() => {
                            state.focus = ImportFocus::NameInput;
                        }
                        KeyCode::Down => state.focus_directory_list(),
                        KeyCode::Enter => {
                            return self.import_artifact_path(state.clone());
                        }
                        _ => {}
                    },
                    ImportFocus::NameInput => match key.code {
                        KeyCode::Backspace => {
                            state.name.pop();
                        }
                        KeyCode::Char(character)
                            if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            state.name.push(character);
                        }
                        KeyCode::Up => state.focus = ImportFocus::PathInput,
                        KeyCode::Tab | KeyCode::Down => state.focus_directory_list(),
                        KeyCode::Enter => {
                            return self.import_artifact_path(state.clone());
                        }
                        _ => {}
                    },
                    ImportFocus::DirectoryList => match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            if state.browser.selected == 0 {
                                state.focus = if state.expects_preset() {
                                    ImportFocus::NameInput
                                } else {
                                    ImportFocus::PathInput
                                };
                            } else {
                                state.browser.selected -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            state.browser.selected = (state.browser.selected + 1)
                                .min(state.browser.entries.len().saturating_sub(1));
                        }
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                            if let Some(entry) =
                                state.browser.entries.get(state.browser.selected).cloned()
                            {
                                if entry.is_dir {
                                    state.navigate(entry.path);
                                } else {
                                    return self.import_artifact_file(state.clone(), entry.path);
                                }
                            }
                        }
                        KeyCode::Backspace | KeyCode::Char('h')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            state.browser.show_hidden = !state.browser.show_hidden;
                            state.rescan();
                        }
                        KeyCode::Backspace | KeyCode::Left => state.navigate_parent(),
                        KeyCode::Char('h') => state.navigate_parent(),
                        KeyCode::Char('.') => {
                            state.browser.show_hidden = !state.browser.show_hidden;
                            state.rescan();
                        }
                        KeyCode::Tab => state.focus = ImportFocus::PathInput,
                        _ => {}
                    },
                }
                self.import_browser_dir = Some(state.browser.directory.clone());
            }
            Popup::Personas(state) => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    state.selected = state.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    state.selected =
                        (state.selected + 1).min(state.personas.len().saturating_sub(1));
                }
                KeyCode::Char('a') => {
                    self.open_persona_editor(state.as_ref().clone(), None, false, false);
                    return Effect::None;
                }
                KeyCode::Char('c') => {
                    if let Some(persona) = state.personas.get(state.selected).cloned() {
                        self.open_persona_editor(
                            state.as_ref().clone(),
                            Some(persona),
                            true,
                            false,
                        );
                    }
                    return Effect::None;
                }
                KeyCode::Char('e') => {
                    if let Some(persona) = state.personas.get(state.selected).cloned() {
                        self.open_persona_editor(
                            state.as_ref().clone(),
                            Some(persona),
                            false,
                            false,
                        );
                    }
                    return Effect::None;
                }
                KeyCode::Char('x') => {
                    self.delete_selected_persona(state.as_ref().clone());
                    return Effect::None;
                }
                KeyCode::Char('i') => {
                    self.popup = Some(Popup::ImportPersonas(Box::new(ImportPersonasState {
                        input: String::new(),
                        manager: Box::new(state.as_ref().clone()),
                    })));
                    return Effect::None;
                }
                KeyCode::Enter => {
                    self.select_persona_from_manager(state.as_ref().clone());
                    return Effect::None;
                }
                _ => {}
            },
            Popup::PersonaEditor(state) => {
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.save_persona_editor(state.as_ref().clone());
                    return Effect::None;
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        state.focused_field = (state.focused_field + 1) % 4;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        state.focused_field = (state.focused_field + 3) % 4;
                    }
                    _ => match state.focused_field {
                        0 | 1 => {
                            if key.code == KeyCode::Enter {
                                state.focused_field += 1;
                            } else {
                                let value = if state.focused_field == 0 {
                                    &mut state.name
                                } else {
                                    &mut state.description
                                };
                                match key.code {
                                    KeyCode::Backspace => {
                                        value.pop();
                                    }
                                    KeyCode::Char(character)
                                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                                    {
                                        value.push(character);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        2 if key.code == KeyCode::Enter => {
                            self.save_persona_editor(state.as_ref().clone());
                            return Effect::None;
                        }
                        3 if key.code == KeyCode::Enter => {
                            if state.resume_new_session {
                                self.restore_modal(state.manager.return_to.clone());
                            } else {
                                self.popup = Some(Popup::Personas(state.manager.clone()));
                            }
                            return Effect::None;
                        }
                        _ => {}
                    },
                }
            }
            Popup::ImportPersonas(state) => match key.code {
                KeyCode::Backspace => {
                    state.input.pop();
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.input.push(character);
                }
                KeyCode::Enter => {
                    self.import_persona_backup(state.as_ref().clone());
                    return Effect::None;
                }
                _ => {}
            },
            Popup::ClonePreset(state) => {
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.submit_cloned_preset(state.as_ref().clone());
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        state.focused_field = (state.focused_field + 1) % 7;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        state.focused_field = (state.focused_field + 6) % 7;
                    }
                    _ => match state.focused_field {
                        0..=3 => {
                            if key.code == KeyCode::Enter {
                                state.focused_field += 1;
                            } else {
                                let value = match state.focused_field {
                                    0 => &mut state.name,
                                    1 => &mut state.temperature,
                                    2 => &mut state.max_context,
                                    3 => &mut state.max_tokens,
                                    _ => unreachable!(),
                                };
                                match key.code {
                                    KeyCode::Backspace => {
                                        value.pop();
                                    }
                                    KeyCode::Char(character)
                                        if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                                    {
                                        value.push(character);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        4 => match key.code {
                            KeyCode::Left
                            | KeyCode::Right
                            | KeyCode::Char(' ')
                            | KeyCode::Enter => {
                                state.use_sysprompt = !state.use_sysprompt;
                                if key.code == KeyCode::Enter {
                                    state.focused_field = 5;
                                }
                            }
                            _ => {}
                        },
                        5 if key.code == KeyCode::Enter => {
                            return self.submit_cloned_preset(state.as_ref().clone());
                        }
                        6 if key.code == KeyCode::Enter => {
                            self.popup = Some(Popup::Presets(state.picker.clone()));
                            return Effect::None;
                        }
                        _ => {}
                    },
                }
            }
            Popup::ProviderProfile(state) => {
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.submit_provider_profile(state.as_ref().clone());
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        Self::focus_profile_field(state, (state.focused_field + 1) % 10);
                    }
                    KeyCode::Char('j')
                        if !(1..=5).contains(&state.focused_field) && state.focused_field != 7 =>
                    {
                        Self::focus_profile_field(state, (state.focused_field + 1) % 10);
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        Self::focus_profile_field(state, (state.focused_field + 9) % 10);
                    }
                    KeyCode::Char('k')
                        if !(1..=5).contains(&state.focused_field) && state.focused_field != 7 =>
                    {
                        Self::focus_profile_field(state, (state.focused_field + 9) % 10);
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
                            KeyCode::Enter => Self::focus_profile_field(state, 1),
                            _ => {}
                        },
                        1..=5 => {
                            if key.code == KeyCode::Enter {
                                Self::focus_profile_field(state, state.focused_field + 1);
                            } else {
                                Self::handle_profile_text_input(state, key);
                            }
                        }
                        6 => match key.code {
                            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                                state.stream = !state.stream;
                            }
                            KeyCode::Enter => Self::focus_profile_field(state, 7),
                            _ => {}
                        },
                        7 => {
                            if key.code == KeyCode::Enter {
                                Self::focus_profile_field(state, 8);
                            } else {
                                Self::handle_profile_text_input(state, key);
                            }
                        }
                        8 => {
                            if key.code == KeyCode::Enter {
                                return self.submit_provider_profile(state.as_ref().clone());
                            }
                        }
                        9 => {
                            if key.code == KeyCode::Enter {
                                self.restore_modal(state.return_to.clone());
                                return Effect::None;
                            }
                        }
                        _ => {}
                    },
                }
            }
            Popup::NewSession(state) => {
                if key.code == KeyCode::Char('p') {
                    self.open_provider_popup(ModalTarget::NewSession(state.clone()));
                    return Effect::None;
                }
                if matches!(key.code, KeyCode::Char('e') | KeyCode::Char('m'))
                    && state.focused_field == 1
                    && let Some(name) = state.providers.get(state.selected_provider).cloned()
                {
                    self.open_provider_profile_popup(
                        Some(name),
                        ModalTarget::NewSession(state.clone()),
                    );
                    return Effect::None;
                }
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.submit_new_session(state.as_ref().clone());
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => {
                        state.focused_field = (state.focused_field + 1) % 7;
                    }
                    KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
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
                                    self.open_import_artifact(
                                        None,
                                        ModalTarget::NewSession(state.clone()),
                                    );
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
                                    self.open_provider_profile_popup(
                                        None,
                                        ModalTarget::NewSession(state.clone()),
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
                                    (state.selected_preset + 1).min(state.presets.len() + 1);
                            }
                            KeyCode::Enter if state.selected_preset == state.presets.len() + 1 => {
                                self.open_import_artifact(
                                    Some(ArtifactKind::ChatCompletionPreset),
                                    ModalTarget::NewSession(state.clone()),
                                );
                                return Effect::None;
                            }
                            KeyCode::Enter => {
                                self.open_preset_popup(ModalTarget::NewSession(state.clone()));
                                return Effect::None;
                            }
                            _ => {}
                        },
                        3 => match key.code {
                            KeyCode::Left | KeyCode::Char('h') => {
                                state.selected_persona = state.selected_persona.saturating_sub(1);
                                Self::apply_new_session_persona(state);
                            }
                            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                                state.selected_persona =
                                    (state.selected_persona + 1).min(state.personas.len() + 1);
                                Self::apply_new_session_persona(state);
                            }
                            KeyCode::Enter if state.selected_persona == state.personas.len() => {
                                let manager = PersonasState {
                                    personas: state.personas.clone(),
                                    selected: state.active_persona.unwrap_or(0),
                                    return_to: ModalTarget::NewSession(state.clone()),
                                };
                                self.open_persona_editor(manager, None, false, true);
                                return Effect::None;
                            }
                            KeyCode::Enter
                                if state.selected_persona == state.personas.len() + 1 =>
                            {
                                let Some(active) = state.active_persona else {
                                    self.show_error("Select a configured persona before editing");
                                    return Effect::None;
                                };
                                let Some(persona) = state.personas.get(active).cloned() else {
                                    self.show_error("Selected persona is unavailable");
                                    return Effect::None;
                                };
                                let manager = PersonasState {
                                    personas: state.personas.clone(),
                                    selected: active,
                                    return_to: ModalTarget::NewSession(state.clone()),
                                };
                                self.open_persona_editor(manager, Some(persona), false, true);
                                return Effect::None;
                            }
                            KeyCode::Enter => {
                                Self::apply_new_session_persona(state);
                                state.focused_field = 4;
                            }
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
                        5 if key.code == KeyCode::Enter => {
                            return self.submit_new_session(state.as_ref().clone());
                        }
                        6 if key.code == KeyCode::Enter => {
                            self.popup = None;
                            return Effect::None;
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
    fn open_provider_popup(&mut self, return_to: ModalTarget) {
        let selected_name = match &return_to {
            ModalTarget::NewSession(state) => state.providers.get(state.selected_provider).cloned(),
            _ => self.history.as_ref().and_then(|history| {
                self.config
                    .core
                    .providers
                    .iter()
                    .find_map(|(name, provider)| {
                        (provider == &history.configuration.configuration.provider)
                            .then(|| name.clone())
                    })
            }),
        };
        self.open_provider_popup_selected(return_to, selected_name, 0);
    }

    fn open_provider_popup_selected(
        &mut self,
        return_to: ModalTarget,
        selected_name: Option<String>,
        fallback: usize,
    ) {
        let names = self
            .config
            .core
            .providers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let selected = selected_name
            .as_ref()
            .and_then(|name| names.iter().position(|candidate| candidate == name))
            .unwrap_or_else(|| fallback.min(names.len()));
        self.popup = Some(Popup::Providers {
            names,
            selected,
            return_to,
        });
    }

    fn provider_modal_target(
        return_to: ModalTarget,
        selected_name: Option<String>,
        selected: usize,
    ) -> ModalTarget {
        ModalTarget::Providers {
            return_to: Box::new(return_to),
            selected_name,
            selected,
        }
    }

    fn restore_modal(&mut self, target: ModalTarget) {
        match target {
            ModalTarget::Sessions | ModalTarget::Chat => self.popup = None,
            ModalTarget::NewSession(state) => self.popup = Some(Popup::NewSession(state)),
            ModalTarget::Providers {
                return_to,
                selected_name,
                selected,
            } => self.open_provider_popup_selected(*return_to, selected_name, selected),
            ModalTarget::Presets(state) => self.popup = Some(Popup::Presets(state)),
        }
    }

    fn open_import_artifact(
        &mut self,
        expected_kind: Option<ArtifactKind>,
        return_to: ModalTarget,
    ) {
        let directory = self
            .import_browser_dir
            .clone()
            .filter(|directory| directory.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        self.popup = Some(Popup::ImportArtifact(ImportArtifactState::new(
            expected_kind,
            return_to,
            directory,
        )));
    }

    fn import_artifact_path(&mut self, mut state: ImportArtifactState) -> Effect {
        let path_str = state.input.trim();
        if path_str.is_empty() {
            self.show_error("Path cannot be empty");
            self.popup = Some(Popup::ImportArtifact(state));
            return Effect::None;
        }
        let expanded = state.resolve_input();
        if expanded.is_dir() {
            state.navigate(expanded);
            state.input.clear();
            state.focus = ImportFocus::DirectoryList;
            self.import_browser_dir = Some(state.browser.directory.clone());
            self.popup = Some(Popup::ImportArtifact(state));
            return Effect::None;
        }
        self.import_artifact_file(state, expanded)
    }

    fn import_artifact_file(&mut self, state: ImportArtifactState, path: PathBuf) -> Effect {
        if !path.exists() {
            self.show_error(format!("File does not exist: {}", path.display()));
            self.popup = Some(Popup::ImportArtifact(state));
            return Effect::None;
        }
        let mut source = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.show_error(format!("Failed to read file: {error}"));
                self.popup = Some(Popup::ImportArtifact(state));
                return Effect::None;
            }
        };
        if let Err(message) = check_artifact_source(&state, &source) {
            self.show_error(message);
            self.popup = Some(Popup::ImportArtifact(state));
            return Effect::None;
        }
        if state.expects_preset() {
            let mut preset: serde_json::Value =
                serde_json::from_slice(&source).expect("validated preset source is JSON");
            let name = (!state.name.trim().is_empty())
                .then(|| state.name.trim().to_owned())
                .or_else(|| {
                    ["preset_name", "name"].into_iter().find_map(|field| {
                        preset
                            .get(field)
                            .and_then(serde_json::Value::as_str)
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                    })
                })
                .or_else(|| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().trim().to_owned())
                        .filter(|stem| !stem.is_empty())
                })
                .unwrap_or_else(|| "Preset".to_owned());
            preset
                .as_object_mut()
                .expect("validated preset source is a JSON object")
                .insert("preset_name".to_owned(), serde_json::Value::String(name));
            source = serde_json::to_vec(&preset).expect("JSON values are serializable");
        }
        self.import_browser_dir = path
            .parent()
            .map(Path::to_path_buf)
            .or(Some(state.browser.directory.clone()));
        self.popup = Some(Popup::ImportArtifact(state));
        Effect::Execute(EngineCommand::ImportArtifact { source })
    }

    fn profile_config_dir(&self) -> PathBuf {
        self.config_dir
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    fn open_personas(&mut self, return_to: ModalTarget) {
        match PersonaStore::load(&self.profile_config_dir()) {
            Ok(store) => {
                self.popup = Some(Popup::Personas(Box::new(PersonasState {
                    personas: store.personas(),
                    selected: 0,
                    return_to,
                })));
            }
            Err(error) => self.show_error(format!("Failed to load personas: {error}")),
        }
    }

    fn open_persona_editor(
        &mut self,
        manager: PersonasState,
        source: Option<Persona>,
        copy: bool,
        resume_new_session: bool,
    ) {
        let (original_key, copy_source_key, name, description) = match source {
            Some(persona) if copy => {
                let base = format!("{}-copy", persona.name);
                let name =
                    if manager.personas.iter().all(|entry| entry.name != base) {
                        base
                    } else {
                        (2..)
                        .map(|suffix| format!("{base}-{suffix}"))
                        .find(|candidate| {
                            manager.personas.iter().all(|entry| entry.name != *candidate)
                        })
                        .expect(
                            "unbounded suffix sequence always contains an available persona name",
                        )
                    };
                (None, Some(persona.key), name, persona.description)
            }
            Some(persona) => (Some(persona.key), None, persona.name, persona.description),
            None => (None, None, String::new(), String::new()),
        };
        self.popup = Some(Popup::PersonaEditor(Box::new(PersonaEditorState {
            original_key,
            copy_source_key,
            name,
            description,
            focused_field: 0,
            manager: Box::new(manager),
            resume_new_session,
        })));
    }

    fn save_persona_editor(&mut self, state: PersonaEditorState) {
        if state.name.trim().is_empty() {
            self.show_error("Persona name cannot be empty");
            self.popup = Some(Popup::PersonaEditor(Box::new(state)));
            return;
        }
        let directory = self.profile_config_dir();
        let mut store = match PersonaStore::load(&directory) {
            Ok(store) => store,
            Err(error) => {
                self.show_error(format!("Failed to load personas: {error}"));
                self.popup = Some(Popup::PersonaEditor(Box::new(state)));
                return;
            }
        };
        let key = if let Some(key) = &state.original_key {
            if let Err(error) = store.update(key, state.name.trim(), state.description.trim()) {
                self.show_error(format!("Failed to update persona: {error}"));
                self.popup = Some(Popup::PersonaEditor(Box::new(state)));
                return;
            }
            key.clone()
        } else if let Some(source_key) = &state.copy_source_key {
            let new_key = match store.duplicate(source_key) {
                Ok(new_key) => new_key,
                Err(error) => {
                    self.show_error(format!("Failed to copy persona: {error}"));
                    self.popup = Some(Popup::PersonaEditor(Box::new(state)));
                    return;
                }
            };
            if let Err(error) = store.update(&new_key, state.name.trim(), state.description.trim())
            {
                self.show_error(format!("Failed to update persona: {error}"));
                self.popup = Some(Popup::PersonaEditor(Box::new(state)));
                return;
            }
            new_key
        } else {
            store.insert(state.name.trim(), state.description.trim())
        };
        if let Err(error) = store.save(&directory) {
            self.show_error(format!("Failed to save personas: {error}"));
            self.popup = Some(Popup::PersonaEditor(Box::new(state)));
            return;
        }
        let mut manager = *state.manager;
        manager.personas = store.personas();
        manager.selected = manager
            .personas
            .iter()
            .position(|persona| persona.key == key)
            .unwrap_or(0);
        self.show_info(format!("Saved persona '{}'", state.name.trim()));
        if state.resume_new_session {
            self.select_persona_from_manager(manager);
        } else {
            self.popup = Some(Popup::Personas(Box::new(manager)));
        }
    }

    fn delete_selected_persona(&mut self, mut manager: PersonasState) {
        let Some(persona) = manager.personas.get(manager.selected).cloned() else {
            self.popup = Some(Popup::Personas(Box::new(manager)));
            return;
        };
        let directory = self.profile_config_dir();
        let mut store = match PersonaStore::load(&directory) {
            Ok(store) => store,
            Err(error) => {
                self.show_error(format!("Failed to load personas: {error}"));
                self.popup = Some(Popup::Personas(Box::new(manager)));
                return;
            }
        };
        store.remove(&persona.key);
        if let Err(error) = store.save(&directory) {
            self.show_error(format!("Failed to save personas: {error}"));
            self.popup = Some(Popup::Personas(Box::new(manager)));
            return;
        }
        manager.personas = store.personas();
        manager.selected = manager
            .selected
            .min(manager.personas.len().saturating_sub(1));
        self.show_info(format!("Deleted persona '{}'", persona.name));
        self.popup = Some(Popup::Personas(Box::new(manager)));
    }

    fn import_persona_backup(&mut self, state: ImportPersonasState) {
        let path = expand_home_path(state.input.trim());
        if state.input.trim().is_empty() {
            self.show_error("Path cannot be empty");
            self.popup = Some(Popup::ImportPersonas(Box::new(state)));
            return;
        }
        let directory = self.profile_config_dir();
        let mut store = match PersonaStore::load(&directory) {
            Ok(store) => store,
            Err(error) => {
                self.show_error(format!("Failed to load personas: {error}"));
                self.popup = Some(Popup::ImportPersonas(Box::new(state)));
                return;
            }
        };
        let previous_keys = store
            .personas()
            .into_iter()
            .map(|persona| persona.key)
            .collect::<std::collections::HashSet<_>>();
        let imported = match store.import_backup(&path) {
            Ok(imported) => imported,
            Err(error) => {
                self.show_error(format!("Failed to import personas: {error}"));
                self.popup = Some(Popup::ImportPersonas(Box::new(state)));
                return;
            }
        };
        if let Err(error) = store.save(&directory) {
            self.show_error(format!("Failed to save personas: {error}"));
            self.popup = Some(Popup::ImportPersonas(Box::new(state)));
            return;
        }
        let mut manager = *state.manager;
        manager.personas = store.personas();
        manager.selected = manager
            .personas
            .iter()
            .position(|persona| !previous_keys.contains(&persona.key))
            .unwrap_or_else(|| {
                manager
                    .selected
                    .min(manager.personas.len().saturating_sub(1))
            });
        self.show_info(format!("Imported {imported} personas"));
        self.popup = Some(Popup::Personas(Box::new(manager)));
    }

    fn select_persona_from_manager(&mut self, manager: PersonasState) {
        let Some(persona) = manager.personas.get(manager.selected).cloned() else {
            self.popup = Some(Popup::Personas(Box::new(manager)));
            return;
        };
        match manager.return_to {
            ModalTarget::NewSession(mut state) => {
                state.personas = manager.personas;
                state.selected_persona = manager.selected;
                state.active_persona = Some(manager.selected);
                state.persona = persona.name;
                state.persona_description = persona.description;
                self.popup = Some(Popup::NewSession(state));
            }
            return_to => {
                self.popup = Some(Popup::Personas(Box::new(PersonasState {
                    return_to,
                    ..manager
                })));
            }
        }
    }

    fn apply_new_session_persona(state: &mut NewSessionState) {
        let Some(persona) = state.personas.get(state.selected_persona) else {
            return;
        };
        state.active_persona = Some(state.selected_persona);
        state.persona = persona.name.clone();
        state.persona_description = persona.description.clone();
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
                let artifact = self
                    .engine
                    .inspect(EngineQuery::ArtifactSource {
                        revision_hash: record.revision_hash.clone(),
                    })
                    .ok()
                    .and_then(|inspection| match inspection {
                        EngineInspection::ArtifactSource(source) => decode_artifact(&source).ok(),
                        _ => None,
                    });
                let label = artifact
                    .as_ref()
                    .and_then(|artifact| {
                        artifact
                            .semantic
                            .get("preset_name")
                            .or_else(|| artifact.semantic.get("name"))
                            .and_then(serde_json::Value::as_str)
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
                let mut summary = artifact
                    .as_ref()
                    .map(|artifact| {
                        PresetSummary::from_semantic(&artifact.semantic, &record.revision_hash)
                    })
                    .unwrap_or_default();
                if let Ok(EngineInspection::PluginArtifactOutput(output)) =
                    self.engine.inspect(EngineQuery::InspectArtifactWithPlugin {
                        plugin_id: DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID.to_owned(),
                        revision_hash: record.revision_hash.clone(),
                    })
                    && let Ok(inspection) = serde_json::from_value::<PresetInspection>(output.value)
                {
                    summary.constraints = inspection.constraints;
                    summary.diagnostics = inspection.diagnostics;
                }
                PresetOption {
                    record,
                    label,
                    summary,
                }
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
        let persona_store = match PersonaStore::load(&self.profile_config_dir()) {
            Ok(store) => store,
            Err(error) => {
                self.show_error(format!("Failed to load personas: {error}"));
                PersonaStore::default()
            }
        };
        let personas = persona_store.personas();
        let selected_persona = persona_store
            .default_persona()
            .and_then(|key| personas.iter().position(|persona| persona.key == key))
            .unwrap_or(0);
        let selected = personas.get(selected_persona).cloned();
        let state = Box::new(NewSessionState {
            selected_character: 0,
            characters,
            providers: self.config.core.providers.keys().cloned().collect(),
            selected_provider: 0,
            presets,
            selected_preset: 0,
            personas,
            selected_persona,
            active_persona: selected.as_ref().map(|_| selected_persona),
            persona: selected
                .as_ref()
                .map(|persona| persona.name.clone())
                .unwrap_or_else(|| "User".to_owned()),
            persona_description: selected
                .map(|persona| persona.description)
                .unwrap_or_default(),
            selected_greeting: 0,
            focused_field: 0,
        });
        if state.characters.is_empty() {
            self.open_import_artifact(None, ModalTarget::NewSession(state));
        } else {
            self.popup = Some(Popup::NewSession(state));
        }
    }

    fn preselect_new_session_preset(&mut self, revision: Option<&ContentHash>) {
        let select = |state: &mut NewSessionState| {
            state.selected_preset = revision
                .and_then(|revision| {
                    state
                        .presets
                        .iter()
                        .position(|preset| &preset.record.revision_hash == revision)
                })
                .map_or(0, |index| index + 1);
        };
        match &mut self.popup {
            Some(Popup::NewSession(state)) => select(state),
            Some(Popup::ImportArtifact(state)) => {
                if let ModalTarget::NewSession(session) = &mut state.return_to {
                    select(session);
                }
            }
            _ => {}
        }
    }

    fn open_provider_profile_copy(&mut self, source_name: String, return_to: ModalTarget) {
        let copy_name = self.available_provider_copy_name(&source_name);
        self.open_provider_profile_popup(Some(source_name.clone()), return_to);
        if let Some(Popup::ProviderProfile(state)) = &mut self.popup {
            state.original_name = None;
            state.copy_source_name = Some(source_name);
            state.name = copy_name;
            Self::focus_profile_field(state, 1);
        }
    }

    fn available_provider_copy_name(&self, source_name: &str) -> String {
        let base = format!("{source_name}-copy");
        if !self.config.core.providers.contains_key(&base) {
            return base;
        }
        (2..)
            .map(|suffix| format!("{base}-{suffix}"))
            .find(|candidate| !self.config.core.providers.contains_key(candidate))
            .expect("unbounded suffix sequence always contains an available profile name")
    }

    pub fn open_provider_profile_popup(
        &mut self,
        original_name: Option<String>,
        return_to: ModalTarget,
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
        let original_settings = original_name
            .as_ref()
            .and_then(|name| self.config.core.providers.get(name))
            .cloned();
        let (name, base_url, model, chat_path, api_key_env, stream, timeout_seconds) =
            if let (Some(name), Some(settings)) = (&original_name, &original_settings) {
                (
                    name.clone(),
                    settings.base_url.clone(),
                    settings.model.clone(),
                    settings.chat_completions_path.clone(),
                    settings.api_key_env.clone().unwrap_or_default(),
                    settings.stream,
                    settings.timeout_seconds.to_string(),
                )
            } else {
                (
                    String::new(),
                    "https://".to_owned(),
                    String::new(),
                    "/v1/chat/completions".to_owned(),
                    String::new(),
                    true,
                    "120".to_owned(),
                )
            };
        self.popup = Some(Popup::ProviderProfile(Box::new(ProviderProfileState {
            templates,
            selected_template: 0,
            original_name,
            copy_source_name: None,
            original_settings,
            name,
            base_url,
            model,
            chat_path,
            api_key_env,
            stream,
            timeout_seconds,
            focused_field: 1,
            cursor_position: 0,
            return_to,
        })));
    }

    fn focus_profile_field(state: &mut ProviderProfileState, focused_field: usize) {
        state.focused_field = focused_field;
        state.cursor_position = match focused_field {
            1 => state.name.chars().count(),
            2 => state.base_url.chars().count(),
            3 => state.model.chars().count(),
            4 => state.chat_path.chars().count(),
            5 => state.api_key_env.chars().count(),
            7 => state.timeout_seconds.chars().count(),
            _ => 0,
        };
    }

    fn handle_profile_text_input(state: &mut ProviderProfileState, key: KeyEvent) {
        let value = match state.focused_field {
            1 => &mut state.name,
            2 => &mut state.base_url,
            3 => &mut state.model,
            4 => &mut state.chat_path,
            5 => &mut state.api_key_env,
            7 => &mut state.timeout_seconds,
            _ => return,
        };
        let mut cursor = state.cursor_position.min(value.chars().count());
        match key.code {
            KeyCode::Left => cursor = cursor.saturating_sub(1),
            KeyCode::Right => cursor = (cursor + 1).min(value.chars().count()),
            KeyCode::Backspace if cursor > 0 => {
                let start = value
                    .char_indices()
                    .nth(cursor - 1)
                    .map_or(value.len(), |(index, _)| index);
                let end = value
                    .char_indices()
                    .nth(cursor)
                    .map_or(value.len(), |(index, _)| index);
                value.replace_range(start..end, "");
                cursor -= 1;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let index = value
                    .char_indices()
                    .nth(cursor)
                    .map_or(value.len(), |(index, _)| index);
                value.insert(index, character);
                cursor += 1;
            }
            _ => {}
        }
        state.cursor_position = cursor;
    }

    fn apply_template_to_profile_state(state: &mut ProviderProfileState) {
        if state.selected_template == 0 {
            return;
        }
        if let Some(template) = state.templates.get(state.selected_template - 1) {
            state.base_url = template.base_url.clone();
            state.chat_path = template.chat_completions_path.clone();
            state.model = template.default_model.clone();
            state.api_key_env = template.api_key_env.clone().unwrap_or_default();
            state.stream = template.stream;
            state.timeout_seconds = template.timeout_seconds.to_string();
            if state.name.is_empty() {
                state.name = template.id.clone();
            }
        }
    }

    fn submit_provider_profile(&mut self, state: ProviderProfileState) -> Effect {
        if state.name.trim().is_empty() {
            self.show_error("Profile name cannot be empty");
            self.popup = Some(Popup::ProviderProfile(Box::new(state)));
            return Effect::None;
        }
        if state.copy_source_name.is_some()
            && self.config.core.providers.contains_key(state.name.trim())
        {
            self.show_error(format!(
                "Provider profile '{}' already exists",
                state.name.trim()
            ));
            self.popup = Some(Popup::ProviderProfile(Box::new(state)));
            return Effect::None;
        }
        let timeout_seconds = match state.timeout_seconds.trim().parse::<u64>() {
            Ok(timeout) => timeout,
            Err(_) => {
                self.show_error("Timeout must be a non-negative whole number");
                self.popup = Some(Popup::ProviderProfile(Box::new(state)));
                return Effect::None;
            }
        };
        let mut settings = state.original_settings.clone().unwrap_or(ProviderSettings {
            id: String::new(),
            base_url: String::new(),
            chat_completions_path: String::new(),
            api_key_env: None,
            static_headers: BTreeMap::new(),
            timeout_seconds,
            ca_certificate_pem: None,
            model: String::new(),
            stream: true,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        });
        if state.original_settings.is_none() {
            settings.id = state.name.trim().to_owned();
        }
        settings.base_url = state.base_url.trim().to_owned();
        settings.chat_completions_path = state.chat_path.trim().to_owned();
        settings.api_key_env =
            (!state.api_key_env.trim().is_empty()).then(|| state.api_key_env.trim().to_owned());
        settings.timeout_seconds = timeout_seconds;
        settings.model = state.model.trim().to_owned();
        settings.stream = state.stream;
        if let Err(error) = validate_provider_settings(&settings) {
            self.show_error(format!("Invalid settings: {error}"));
            self.popup = Some(Popup::ProviderProfile(Box::new(state)));
            return Effect::None;
        }
        let active_profile = state.original_settings.as_ref().is_some_and(|original| {
            self.history
                .as_ref()
                .is_some_and(|history| &history.configuration.configuration.provider == original)
        });
        let config_dir = self.profile_config_dir();
        if let Err(error) = stcli_core::Config::save_provider_profile(
            &config_dir,
            state.original_name.as_deref(),
            &state.name,
            settings,
        ) {
            self.show_error(format!("Failed to save profile: {error}"));
            self.popup = Some(Popup::ProviderProfile(Box::new(state)));
            return Effect::None;
        }
        if let Err(error) = self.reload_config() {
            self.show_error(format!("Failed to reload config: {error}"));
            self.popup = Some(Popup::ProviderProfile(Box::new(state)));
            return Effect::None;
        }
        let profile_name = state.name.clone();
        let action = if state.original_name.is_some() {
            "Updated"
        } else {
            "Created"
        };
        self.show_info(format!("{action} provider profile '{profile_name}'"));
        if active_profile
            && let (Some(history), Some(provider)) =
                (&self.history, self.config.core.providers.get(&profile_name))
        {
            let mut configuration = history.configuration.configuration.clone();
            configuration.provider = provider.clone();
            self.popup = None;
            return Effect::Execute(EngineCommand::UpdateConfiguration {
                session_id: history.session.session_id,
                configuration: Box::new(configuration),
            });
        }
        match state.return_to {
            ModalTarget::NewSession(mut session_state) => {
                session_state.providers = self.config.core.providers.keys().cloned().collect();
                session_state.selected_provider = session_state
                    .providers
                    .iter()
                    .position(|name| name == &profile_name)
                    .unwrap_or(0);
                self.popup = Some(Popup::NewSession(session_state));
            }
            ModalTarget::Providers { return_to, .. } => {
                self.open_provider_popup_selected(*return_to, Some(profile_name), 0);
            }
            target => self.restore_modal(target),
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
            persona_description: (!state.persona_description.trim().is_empty())
                .then(|| state.persona_description.clone()),
            lorebook_revisions: vec![],
            prompt_preset_revision,
            prompt_order_overrides: BTreeMap::new(),
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

    fn open_clone_preset(&mut self, picker: PresetPickerState) {
        let Some(source_revision) = picker.selected_revision() else {
            self.popup = Some(Popup::Presets(Box::new(picker)));
            return;
        };
        let source = match self.engine.inspect(EngineQuery::ArtifactSource {
            revision_hash: source_revision.clone(),
        }) {
            Ok(EngineInspection::ArtifactSource(source)) => source,
            Ok(_) => unreachable!("artifact source query returned another inspection type"),
            Err(error) => {
                self.show_error(error.to_string());
                self.popup = Some(Popup::Presets(Box::new(picker)));
                return;
            }
        };
        let decoded = match decode_artifact(&source) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.show_error(error.to_string());
                self.popup = Some(Popup::Presets(Box::new(picker)));
                return;
            }
        };
        let source_name = decoded
            .semantic
            .get("preset_name")
            .or_else(|| decoded.semantic.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Preset");
        let base = format!("{source_name}-copy");
        let name = if picker.rows.iter().all(|row| row.label != base) {
            base
        } else {
            (2..)
                .map(|suffix| format!("{base}-{suffix}"))
                .find(|candidate| picker.rows.iter().all(|row| row.label != *candidate))
                .expect("unbounded suffix sequence always contains an available preset name")
        };
        let temperature =
            summary_value(decoded.semantic.get("temperature")).unwrap_or_else(|| "1".to_owned());
        let max_context = summary_value(
            decoded
                .semantic
                .get("max_context")
                .or_else(|| decoded.semantic.get("openai_max_context")),
        )
        .unwrap_or_else(|| "8192".to_owned());
        let max_tokens = summary_value(
            decoded
                .semantic
                .get("openai_max_tokens")
                .or_else(|| decoded.semantic.get("max_tokens")),
        )
        .unwrap_or_else(|| "512".to_owned());
        let use_sysprompt = decoded
            .semantic
            .get("use_sysprompt")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        self.popup = Some(Popup::ClonePreset(Box::new(ClonePresetState {
            source_revision,
            name,
            temperature,
            max_context,
            max_tokens,
            use_sysprompt,
            focused_field: 0,
            picker: Box::new(picker),
        })));
    }

    fn submit_cloned_preset(&mut self, state: ClonePresetState) -> Effect {
        if state.name.trim().is_empty() {
            self.show_error("Preset name cannot be empty");
            self.popup = Some(Popup::ClonePreset(Box::new(state)));
            return Effect::None;
        }
        let temperature = match state.temperature.trim().parse::<f64>() {
            Ok(value) => value,
            Err(_) => {
                self.show_error("Temperature must be a number");
                self.popup = Some(Popup::ClonePreset(Box::new(state)));
                return Effect::None;
            }
        };
        let max_context = match state.max_context.trim().parse::<u64>() {
            Ok(value) => value,
            Err(_) => {
                self.show_error("Max context tokens must be a non-negative whole number");
                self.popup = Some(Popup::ClonePreset(Box::new(state)));
                return Effect::None;
            }
        };
        let max_tokens = match state.max_tokens.trim().parse::<u64>() {
            Ok(value) => value,
            Err(_) => {
                self.show_error("Max tokens must be a non-negative whole number");
                self.popup = Some(Popup::ClonePreset(Box::new(state)));
                return Effect::None;
            }
        };
        let source = match self.engine.inspect(EngineQuery::ArtifactSource {
            revision_hash: state.source_revision.clone(),
        }) {
            Ok(EngineInspection::ArtifactSource(source)) => source,
            Ok(_) => unreachable!("artifact source query returned another inspection type"),
            Err(error) => {
                self.show_error(error.to_string());
                self.popup = Some(Popup::ClonePreset(Box::new(state)));
                return Effect::None;
            }
        };
        let clone = match clone_and_patch_preset(
            &source,
            PresetPatch {
                preset_name: state.name.trim().to_owned(),
                temperature,
                max_context,
                max_tokens,
                use_sysprompt: state.use_sysprompt,
            },
        ) {
            Ok(clone) => clone,
            Err(error) => {
                self.show_error(format!("Failed to clone preset: {error}"));
                self.popup = Some(Popup::ClonePreset(Box::new(state)));
                return Effect::None;
            }
        };
        self.popup = Some(Popup::ClonePreset(Box::new(state)));
        Effect::Execute(EngineCommand::ImportArtifact { source: clone })
    }

    fn apply_session_prompt_order_overrides(&self, rows: &mut [PresetOption]) {
        let Some(history) = self.history.as_ref() else {
            return;
        };
        let configuration = &history.configuration.configuration;
        let Some(revision) = configuration.prompt_preset_revision.as_ref() else {
            return;
        };
        let Some(row) = rows
            .iter_mut()
            .find(|row| &row.record.revision_hash == revision)
        else {
            return;
        };
        for entry in &mut row.summary.prompt_order {
            entry.override_enabled = configuration
                .prompt_order_overrides
                .get(&entry.identifier)
                .copied();
            entry.enabled = entry.override_enabled.unwrap_or(entry.preset_enabled);
        }
    }

    fn open_preset_popup(&mut self, return_to: ModalTarget) {
        let mut rows = match self.query_preset_options() {
            Ok(rows) => rows,
            Err(error) => {
                self.show_error(error.to_string());
                return;
            }
        };
        if matches!(return_to, ModalTarget::Chat) {
            self.apply_session_prompt_order_overrides(&mut rows);
        }
        let selected = match &return_to {
            ModalTarget::NewSession(state) => state.selected_preset.min(rows.len()),
            _ => self
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
                .map_or(0, |index| index + 1),
        };
        self.popup = Some(Popup::Presets(Box::new(PresetPickerState {
            rows,
            selected,
            return_to,
            filter: String::new(),
            filtering: false,
            show_details: false,
            details_scroll: 0,
            order_focus: None,
        })));
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

    fn start_duplicate_session(&mut self) {
        let entries = self.session_list_entries();
        let Some(SessionListEntry::Session(i)) = entries.get(self.selected_session) else {
            self.show_error("Can only duplicate sessions, not branches");
            return;
        };
        let filtered = self.filtered_sessions();
        let Some(session) = filtered.get(*i) else {
            return;
        };
        let input = available_duplicated_session_name(
            &session.display_name,
            self.sessions
                .iter()
                .map(|candidate| candidate.display_name.as_str()),
        );
        self.popup = Some(Popup::DuplicateSession {
            session_id: session.session_id,
            input,
        });
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
            Ok(EngineResult::DuplicatedSession(created)) => {
                let session_id = created.session.session_id;
                self.popup = None;
                self.filter.clear();
                self.filtering = false;
                if let Err(error) = self.reload_sessions() {
                    self.show_error(error.to_string());
                    return true;
                }
                let filtered_index = self
                    .filtered_sessions()
                    .iter()
                    .position(|session| session.session_id == session_id);
                if let Some(filtered_index) = filtered_index
                    && let Some(selected) = self.session_list_entries().iter().position(|entry| {
                        matches!(
                            entry,
                            SessionListEntry::Session(index) if *index == filtered_index
                        )
                    })
                {
                    self.selected_session = selected;
                }
                true
            }
            Ok(EngineResult::Branch(branch)) if self.pending_branch_creation => {
                self.pending_branch_creation = false;
                let composer = branch
                    .forked_from_turn_id
                    .and_then(|turn_id| {
                        let parent_branch_id = branch.parent_branch_id?;
                        match self.engine.inspect(EngineQuery::BranchTurns {
                            branch_id: parent_branch_id,
                        }) {
                            Ok(EngineInspection::Turns(turns)) => turns
                                .into_iter()
                                .find(|turn| turn.turn.turn_id == turn_id)
                                .map(|turn| turn.turn.user_content),
                            _ => None,
                        }
                    })
                    .unwrap_or_default();
                if let Err(error) = self.open_branch(branch.session_id, branch.branch_id) {
                    self.show_error(error.to_string());
                    return false;
                }
                self.composer = composer;
                self.chat_focus = ChatFocus::Composer;
                self.show_info("Created Branch");
                true
            }
            Ok(EngineResult::Branch(_)) => {
                if let Err(error) = self.reload_history() {
                    self.show_error(error.to_string());
                }
                true
            }
            Ok(EngineResult::ArtifactBundle {
                primary,
                supplementary_artifacts,
                asset_count,
            }) => {
                let preset_metadata = (primary.kind == ArtifactKind::ChatCompletionPreset)
                    .then(|| {
                        self.engine
                            .inspect(EngineQuery::ArtifactSource {
                                revision_hash: primary.revision_hash.clone(),
                            })
                            .ok()
                            .and_then(|inspection| match inspection {
                                EngineInspection::ArtifactSource(source) => {
                                    decode_artifact(&source).ok()
                                }
                                _ => None,
                            })
                    })
                    .flatten()
                    .map(|artifact| {
                        let name = artifact
                            .semantic
                            .get("preset_name")
                            .or_else(|| artifact.semantic.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Unnamed preset")
                            .to_owned();
                        let scripts = artifact
                            .semantic
                            .pointer("/extensions/regex_scripts")
                            .and_then(serde_json::Value::as_array)
                            .map_or(0, Vec::len);
                        (name, scripts)
                    });
                let status = match preset_metadata {
                    Some((name, scripts)) if scripts > 0 => {
                        format!("Imported preset '{name}' (contains {scripts} untrusted scripts)")
                    }
                    Some((name, _)) => format!("Imported preset '{name}'"),
                    None => format!(
                        "Imported {} ({} supplementary Artifacts, {asset_count} assets)",
                        primary.kind,
                        supplementary_artifacts.len()
                    ),
                };
                let return_to = match self.popup.take() {
                    Some(Popup::ImportArtifact(state)) => Some(state.return_to),
                    Some(Popup::ClonePreset(state)) => Some(ModalTarget::Presets(state.picker)),
                    _ => None,
                };
                if let Some(return_to) = return_to {
                    match return_to {
                        ModalTarget::NewSession(mut session_state) => {
                            if primary.kind == ArtifactKind::ChatCompletionPreset {
                                session_state.presets =
                                    self.query_preset_options().unwrap_or_else(|error| {
                                        self.show_error(error.to_string());
                                        Vec::new()
                                    });
                                session_state.selected_preset = session_state
                                    .presets
                                    .iter()
                                    .position(|preset| {
                                        preset.record.revision_hash == primary.revision_hash
                                    })
                                    .map_or(0, |index| index + 1);
                            } else {
                                session_state.characters = self.query_character_options();
                                if let Some(pos) =
                                    session_state.characters.iter().position(|character| {
                                        character.revision_hash == primary.revision_hash
                                    })
                                {
                                    session_state.selected_character = pos;
                                    session_state.selected_greeting = 0;
                                }
                            }
                            self.show_info(status);
                            self.popup = Some(Popup::NewSession(session_state));
                        }
                        ModalTarget::Presets(mut picker) => {
                            picker.filter.clear();
                            picker.filtering = false;
                            picker.rows = self.query_preset_options().unwrap_or_else(|error| {
                                self.show_error(error.to_string());
                                Vec::new()
                            });
                            let selected = picker
                                .rows
                                .iter()
                                .position(|preset| {
                                    preset.record.revision_hash == primary.revision_hash
                                })
                                .map_or(0, |index| index + 1);
                            picker.select(selected);
                            self.show_info(status);
                            self.popup = Some(Popup::Presets(picker));
                        }
                        target => {
                            self.show_info(status);
                            self.restore_modal(target);
                        }
                    }
                } else {
                    self.show_info(status);
                    self.popup = None;
                }
                true
            }
            Ok(EngineResult::Configuration(_)) if self.pending_override_message.is_some() => {
                let Some(Popup::Presets(mut picker)) = self.popup.take() else {
                    self.pending_override_message = None;
                    return false;
                };
                let selected_revision = picker.selected_revision();
                if let Err(error) = self.reload_history() {
                    self.show_error(error.to_string());
                }
                picker.rows = self.query_preset_options().unwrap_or_else(|error| {
                    self.show_error(error.to_string());
                    Vec::new()
                });
                self.apply_session_prompt_order_overrides(&mut picker.rows);
                picker.select_filtered_revision(selected_revision.as_ref());
                let message = self
                    .pending_override_message
                    .take()
                    .expect("guarded pending override message");
                self.show_info(message);
                self.popup = Some(Popup::Presets(picker));
                true
            }
            Ok(EngineResult::PromptOrderUpdated {
                artifact,
                configuration,
            }) => {
                let Some(Popup::Presets(mut picker)) = self.popup.take() else {
                    if configuration.is_some()
                        && let Err(error) = self.reload_history()
                    {
                        self.show_error(error.to_string());
                    }
                    self.show_info("Updated prompt order");
                    return true;
                };
                picker.filter.clear();
                picker.filtering = false;
                picker.rows = self.query_preset_options().unwrap_or_else(|error| {
                    self.show_error(error.to_string());
                    Vec::new()
                });
                picker.selected = picker
                    .rows
                    .iter()
                    .position(|row| row.record.revision_hash == artifact.revision_hash)
                    .map_or(0, |index| index + 1);
                picker.details_scroll = 0;
                picker.order_focus = None;
                if configuration.is_some()
                    && let Err(error) = self.reload_history()
                {
                    self.show_error(error.to_string());
                }
                let marker_warning = picker
                    .rows
                    .get(picker.selected.saturating_sub(1))
                    .is_some_and(|row| {
                        row.summary
                            .prompt_order
                            .iter()
                            .any(|entry| entry.marker && !entry.enabled)
                    });
                let preset_only = configuration.is_none();
                let mut message = match (preset_only, marker_warning) {
                    (true, true) => {
                        "Updated preset prompt order (warning: a structural marker is disabled)"
                            .to_owned()
                    }
                    (true, false) => "Updated preset prompt order".to_owned(),
                    (false, true) => {
                        "Updated prompt order (warning: a structural marker is disabled)".to_owned()
                    }
                    (false, false) => "Updated prompt order".to_owned(),
                };
                if let Some((identifier, enabled)) = self.pending_preset_toggle.take() {
                    message.push_str(&format!(
                        "; {identifier} {}",
                        if enabled { "enabled" } else { "disabled" }
                    ));
                }
                let auto_disabled = std::mem::take(&mut self.pending_auto_disabled);
                if !auto_disabled.is_empty() {
                    message.push_str(&format!("; auto-disabled {}", auto_disabled.join(", ")));
                }
                let warnings = std::mem::take(&mut self.pending_directive_warnings);
                if !warnings.is_empty() {
                    message.push_str(&format!("; warning: {}", warnings.join(" ")));
                }
                self.show_info(message);
                match picker.return_to.clone() {
                    ModalTarget::NewSession(mut session) => {
                        session.presets = picker.rows;
                        session.selected_preset = session
                            .presets
                            .iter()
                            .position(|row| row.record.revision_hash == artifact.revision_hash)
                            .map_or(0, |index| index + 1);
                        self.popup = Some(Popup::NewSession(session));
                    }
                    _ => self.popup = Some(Popup::Presets(picker)),
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
                self.pending_branch_creation = false;
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

    fn create_branch_from_focus(&mut self) -> Effect {
        if self.running_attempt().is_some() {
            return Effect::None;
        }
        let Some(history) = &self.history else {
            return Effect::None;
        };
        let session_id = history.session.session_id;
        let source_branch_id = history.branch.branch_id;
        let at_turn_id = match resolve_focus(history, self.focused_message) {
            Some(FocusedSlot::UserMessage(index) | FocusedSlot::AssistantMessage(index)) => {
                Some(history.turns[index].turn.turn_id)
            }
            Some(FocusedSlot::Greeting) | None => None,
        };
        self.pending_branch_creation = true;
        Effect::Execute(EngineCommand::CreateBranch {
            session_id,
            source_branch_id: Some(source_branch_id),
            at_turn_id,
        })
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
            Some(Popup::Providers {
                names, selected, ..
            }) => {
                *selected = selected.saturating_add_signed(amount).min(names.len());
                return;
            }
            Some(Popup::Presets(state)) => {
                if state.show_details {
                    state.details_scroll = state.details_scroll.saturating_add_signed(amount);
                } else {
                    state.select(
                        state
                            .selected
                            .saturating_add_signed(amount)
                            .min(state.filtered_rows().len()),
                    );
                }
                return;
            }
            Some(Popup::Personas(state)) => {
                state.selected = state
                    .selected
                    .saturating_add_signed(amount)
                    .min(state.personas.len().saturating_sub(1));
                return;
            }
            Some(
                Popup::Help
                | Popup::ConfirmExit
                | Popup::ConfirmDelete { .. }
                | Popup::ConfirmDeleteProvider { .. }
                | Popup::Rename { .. }
                | Popup::DuplicateSession { .. }
                | Popup::NewSession(_)
                | Popup::ImportArtifact(_)
                | Popup::ProviderProfile(_)
                | Popup::ClonePreset(_)
                | Popup::PersonaEditor(_)
                | Popup::ImportPersonas(_),
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
                match &mut self.popup {
                    Some(Popup::Branches { selected, .. } | Popup::Providers { selected, .. }) => {
                        *selected = index
                    }
                    Some(Popup::Presets(state)) => state.select(index),
                    _ => {}
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

fn check_artifact_source(state: &ImportArtifactState, source: &[u8]) -> Result<(), String> {
    let charx_candidate = state.expected_kind.is_none()
        && matches!(&state.return_to, ModalTarget::NewSession(_))
        && source.starts_with(b"PK\x03\x04");
    if charx_candidate {
        return Ok(());
    }
    let decoded =
        decode_artifact(source).map_err(|error| format!("Failed to decode artifact: {error}"))?;
    let invalid_kind = match state.expected_kind {
        Some(expected) => decoded.kind != expected,
        None if matches!(&state.return_to, ModalTarget::NewSession(_)) => !matches!(
            decoded.kind,
            ArtifactKind::CharacterCardV1
                | ArtifactKind::CharacterCardV2
                | ArtifactKind::CharacterCardV3
        ),
        None => false,
    };
    if invalid_kind {
        let expected = state
            .expected_kind
            .map_or_else(|| "character-card".to_owned(), |kind| kind.to_string());
        return Err(format!("File is a {}, expected a {expected}", decoded.kind));
    }
    Ok(())
}

fn expand_home_path(path: &str) -> PathBuf {
    if path == "~" {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(path))
    } else if let Some(stripped) = path.strip_prefix("~/") {
        std::env::var("HOME")
            .map(|home| Path::new(&home).join(stripped))
            .unwrap_or_else(|_| PathBuf::from(path))
    } else {
        PathBuf::from(path)
    }
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
    fn chat_b_dispatches_branch_at_focused_user_turn() {
        // Regression test for 01-chat-b: b branches at the focused Turn.
        let (mut app, _directory) = app_with_session();
        let turn_id = append_answered_turn(&mut app);
        app.chat_focus = ChatFocus::History;
        app.focused_message = 1;

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(matches!(
            effect,
            Effect::Execute(EngineCommand::CreateBranch {
                session_id,
                source_branch_id: Some(_),
                at_turn_id: Some(actual),
            }) if session_id == app.history.as_ref().unwrap().session.session_id && actual == turn_id
        ));
    }

    #[test]
    fn chat_b_on_greeting_branches_from_start_and_uppercase_b_opens_popup() {
        let (mut app, _directory) = app_with_session();
        app.chat_focus = ChatFocus::History;
        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(matches!(
            effect,
            Effect::Execute(EngineCommand::CreateBranch {
                at_turn_id: None,
                ..
            })
        ));
        execute_command(&mut app, effect);
        assert!(app.history.as_ref().unwrap().turns.is_empty());
        assert!(app.composer.is_empty());

        app.chat_focus = ChatFocus::History;
        assert!(matches!(
            app.handle_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE)),
            Effect::None
        ));
        assert!(matches!(app.popup, Some(Popup::Branches { .. })));
    }

    #[test]
    fn chat_b_is_a_no_op_while_streaming() {
        // Regression test for 01-chat-b AC5: streaming blocks Branch creation.
        let (mut app, _directory) = app_with_session();
        app.chat_focus = ChatFocus::History;
        app.generation = Some(GenerationState {
            partial: String::new(),
            reasoning: String::new(),
            streaming: true,
            pending_input: None,
            continues: false,
        });

        assert!(matches!(
            app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)),
            Effect::None
        ));
        assert!(app.popup.is_none());
    }

    #[test]
    fn greeting_selection_branch_result_keeps_composer_and_focus() {
        let (mut app, _directory) = app_with_session();
        let branch = app.history.as_ref().unwrap().branch.clone();
        app.composer = "draft".to_owned();
        app.chat_focus = ChatFocus::History;

        assert!(app.finish_command(Ok(EngineResult::Branch(branch))));

        assert_eq!(app.composer, "draft");
        assert_eq!(app.chat_focus, ChatFocus::History);
        assert!(app.toast.is_none());
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
    fn duplicate_session_key_uses_collision_safe_name_and_highlights_result() {
        let (mut app, _directory) = app_with_session();
        let source_session_id = app.history.as_ref().unwrap().session.session_id;
        app.screen = Screen::Sessions;
        app.history = None;
        let source_name = app
            .sessions
            .iter()
            .find(|session| session.session_id == source_session_id)
            .unwrap()
            .display_name
            .clone();
        execute_command(
            &mut app,
            Effect::Execute(EngineCommand::DuplicateSession {
                session_id: source_session_id,
                branch_id: None,
                up_to_turn_id: None,
                new_name: Some(format!("{source_name} (copy)")),
            }),
        );
        // Regression: archived Sessions remain available as duplication sources.
        execute_command(
            &mut app,
            Effect::Execute(EngineCommand::ArchiveSession {
                session_id: source_session_id,
            }),
        );
        let source_index = app
            .filtered_sessions()
            .iter()
            .position(|session| session.session_id == source_session_id)
            .unwrap();
        app.selected_session = app
            .session_list_entries()
            .iter()
            .position(
                |entry| matches!(entry, SessionListEntry::Session(index) if *index == source_index),
            )
            .unwrap();

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));

        assert!(matches!(effect, Effect::None));
        assert!(matches!(
            &app.popup,
            Some(Popup::DuplicateSession { session_id, input })
                if *session_id == source_session_id
                    && input == &format!("{source_name} (copy 2)")
        ));

        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            &effect,
            Effect::Execute(EngineCommand::DuplicateSession {
                session_id,
                branch_id: None,
                up_to_turn_id: None,
                new_name: Some(name),
            }) if *session_id == source_session_id && name == &format!("{source_name} (copy 2)")
        ));
        execute_command(&mut app, effect);

        let entries = app.session_list_entries();
        let filtered = app.filtered_sessions();
        let SessionListEntry::Session(selected_index) = entries[app.selected_session] else {
            panic!("duplicated Session must be highlighted");
        };
        let selected = &filtered[selected_index];
        assert_ne!(selected.session_id, source_session_id);
        assert_eq!(selected.display_name, format!("{source_name} (copy 2)"));
        assert!(!selected.archived);
        assert_eq!(app.screen, Screen::Sessions);
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
        assert!(state.persona_description.is_empty());
        assert_eq!(state.selected_greeting, 0);
        assert_eq!(state.focused_field, 0);
    }
    #[test]
    fn persona_manager_handles_add_copy_edit_delete_and_import() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let config_dir = directory.path().join("config");
        let mut personas = stcli_core::PersonaStore::default();
        personas.insert("Alice", "An archivist.");
        personas.save(&config_dir).unwrap();
        let backup = directory.path().join("personas_backup.json");
        fs::write(
            &backup,
            r#"{"personas":{"bob.png":"Bob"},"persona_descriptions":{"bob.png":{"description":"A navigator.","position":0}}}"#,
        )
        .unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.set_config_dir(config_dir.clone());

        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        let Some(Popup::Personas(state)) = &app.popup else {
            panic!("expected persona manager");
        };
        assert_eq!(state.personas[0].name, "Alice");

        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        let Some(Popup::PersonaEditor(state)) = &app.popup else {
            panic!("expected copied persona editor");
        };
        assert_eq!(state.name, "Alice-copy");
        assert_eq!(state.description, "An archivist.");
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let Some(Popup::PersonaEditor(state)) = &mut app.popup else {
            panic!("expected new persona editor");
        };
        state.name = "Carol".to_owned();
        state.description = "A cartographer.".to_owned();
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let Some(Popup::PersonaEditor(state)) = &mut app.popup else {
            panic!("expected persona editor");
        };
        state.name = "Carol Prime".to_owned();
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let Some(Popup::ImportPersonas(state)) = &mut app.popup else {
            panic!("expected persona import dialog");
        };
        state.input = backup.display().to_string();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Popup::Personas(state)) = &app.popup else {
            panic!("expected persona manager after import");
        };
        assert!(state.personas.iter().any(|persona| persona.name == "Bob"));
        assert!(
            state
                .personas
                .iter()
                .any(|persona| persona.name == "Carol Prime")
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let saved = stcli_core::PersonaStore::load(&config_dir).unwrap();
        assert_eq!(saved.personas().len(), 3);
    }

    #[test]
    fn persona_copy_preserves_imported_position_and_metadata_through_tui() {
        // Regression: copying an imported SillyTavern persona must retain position and flattened metadata.
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let config_dir = directory.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("personas.json"),
            r#"{"personas":{"alice.png":"Alice"},"persona_descriptions":{"alice.png":{"description":"A curious archivist.","position":3,"depth":2}}}"#,
        )
        .unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.set_config_dir(config_dir.clone());

        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let saved = stcli_core::PersonaStore::load(&config_dir).unwrap();
        let personas = saved.personas();
        let clone = personas
            .iter()
            .find(|persona| persona.name == "Alice-copy")
            .unwrap();
        assert_eq!(clone.position, 3);
        assert_eq!(clone.description, "A curious archivist.");
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(config_dir.join("personas.json")).unwrap()).unwrap();
        assert_eq!(raw["persona_descriptions"][clone.key.clone()]["depth"], 2);
        assert_eq!(
            raw["persona_descriptions"][clone.key.clone()]["position"],
            3
        );
    }

    #[test]
    fn new_session_persona_selector_applies_profiles_and_inline_actions() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let config_dir = directory.path().join("config");
        let mut store = stcli_core::Store::open(&database).unwrap();
        store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        drop(store);
        let mut personas = stcli_core::PersonaStore::default();
        personas.insert("Alice", "An archivist.");
        personas.insert("Bob", "A navigator.");
        personas.save(&config_dir).unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.set_config_dir(config_dir);
        app.open_new_session_popup();

        let Some(Popup::NewSession(state)) = &mut app.popup else {
            panic!("expected new session popup");
        };
        assert_eq!(state.personas.len(), 2);
        assert_eq!(state.persona, "Alice");
        assert_eq!(state.persona_description, "An archivist.");
        state.focused_field = 3;

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        let Some(Popup::NewSession(state)) = &app.popup else {
            panic!("expected new session popup");
        };
        assert_eq!(state.persona, "Bob");
        assert_eq!(state.persona_description, "A navigator.");

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Popup::PersonaEditor(state)) = &mut app.popup else {
            panic!("expected inline persona editor");
        };
        assert!(state.original_key.is_none());
        state.focused_field = 3;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Popup::NewSession(state)) = &mut app.popup else {
            panic!("expected New Session after inline persona cancellation");
        };
        state.focused_field = 3;
        state.selected_persona = state.personas.len();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Popup::PersonaEditor(state)) = &mut app.popup else {
            panic!("expected inline persona editor");
        };
        state.name = "Carol".to_owned();
        state.description = "A cartographer.".to_owned();
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        let Some(Popup::NewSession(state)) = &mut app.popup else {
            panic!("expected resumed new session popup");
        };
        assert_eq!(state.persona, "Carol");
        assert_eq!(state.persona_description, "A cartographer.");
        state.focused_field = 3;
        state.selected_persona = state.personas.len() + 1;

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Popup::PersonaEditor(state)) = &app.popup else {
            panic!("expected inline persona editor");
        };
        assert!(state.original_key.is_some());
        assert_eq!(state.name, "Carol");
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
        let Some(Popup::ImportArtifact(import_state)) = &mut app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert!(matches!(import_state.return_to, ModalTarget::NewSession(_)));

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

    fn open_import_at(app: &mut App, kind: ArtifactKind, directory: &Path) {
        app.popup = Some(Popup::ImportArtifact(ImportArtifactState::new(
            Some(kind),
            ModalTarget::Sessions,
            directory.to_path_buf(),
        )));
    }

    fn import_entries(app: &App) -> Vec<String> {
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        state
            .browser
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    #[test]
    fn import_browser_scans_filters_and_orders_entries() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let browse = directory.path().join("browse");
        fs::create_dir_all(browse.join("cards")).unwrap();
        fs::create_dir_all(browse.join("alpha")).unwrap();
        fs::create_dir_all(browse.join(".hidden")).unwrap();
        fs::write(
            browse.join("card.json"),
            stcli_testkit::fixtures::minimal_card(),
        )
        .unwrap();
        fs::write(browse.join("image.png"), b"png").unwrap();
        fs::write(browse.join("notes.txt"), b"notes").unwrap();
        fs::write(browse.join(".secret.json"), b"{}").unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();

        open_import_at(&mut app, ArtifactKind::ChatCompletionPreset, &browse);
        assert_eq!(import_entries(&app), ["..", "alpha", "cards", "card.json"]);
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert!(!state.browser.access_denied);
        assert!(state.browser.entries[1].is_dir);
        assert!(!state.browser.entries[3].is_dir);

        open_import_at(&mut app, ArtifactKind::CharacterCardV2, &browse);
        assert_eq!(
            import_entries(&app),
            ["..", "alpha", "cards", "card.json", "image.png"]
        );
    }

    #[test]
    fn import_browser_navigates_directories_and_toggles_dotfiles() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let browse = directory.path().join("browse");
        fs::create_dir_all(browse.join("sub")).unwrap();
        fs::create_dir_all(browse.join(".config")).unwrap();
        fs::write(
            browse.join("sub").join("inner.json"),
            stcli_testkit::fixtures::minimal_card(),
        )
        .unwrap();
        fs::write(browse.join(".secret.json"), b"{}").unwrap();
        let canonical_browse = fs::canonicalize(&browse).unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        open_import_at(&mut app, ArtifactKind::ChatCompletionPreset, &browse);

        assert_eq!(import_entries(&app), ["..", "sub"]);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(
            import_entries(&app),
            ["..", ".config", "sub", ".secret.json"]
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::NONE));
        assert_eq!(import_entries(&app), ["..", "sub"]);

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.browser.directory, canonical_browse.join("sub"));
        assert_eq!(import_entries(&app), ["..", "inner.json"]);

        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.browser.directory, canonical_browse);

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.browser.directory, canonical_browse);

        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.focus, ImportFocus::NameInput);
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.focus, ImportFocus::PathInput);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.focus, ImportFocus::DirectoryList);
    }

    #[test]
    fn import_browser_tab_completes_path_segments() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let browse = directory.path().join("browse");
        fs::create_dir_all(browse.join("downloads")).unwrap();
        fs::write(browse.join("dove.json"), b"{}").unwrap();
        fs::write(browse.join("preset-a.json"), b"{}").unwrap();
        fs::write(browse.join("preset-b.json"), b"{}").unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        open_import_at(&mut app, ArtifactKind::CharacterCardV2, &browse);

        for c in "dow".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.input, "downloads/");
        assert_eq!(state.focus, ImportFocus::PathInput);

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(
            state.browser.directory,
            fs::canonicalize(browse.join("downloads")).unwrap()
        );
        assert!(state.input.is_empty());
        assert_eq!(state.focus, ImportFocus::DirectoryList);

        open_import_at(&mut app, ArtifactKind::CharacterCardV2, &browse);
        for c in "dov".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.input, "dove.json");

        open_import_at(&mut app, ArtifactKind::CharacterCardV2, &browse);
        for c in "preset".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.input, "preset-");
        assert_eq!(state.completion_hint.as_deref(), Some("2 matches"));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.focus, ImportFocus::DirectoryList);
    }

    #[test]
    fn import_browser_selection_validates_dispatches_and_remembers_directory() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let browse = directory.path().join("browse");
        fs::create_dir_all(&browse).unwrap();
        fs::write(
            browse.join("card.json"),
            stcli_testkit::fixtures::minimal_card(),
        )
        .unwrap();
        fs::write(
            browse.join("preset.json"),
            stcli_testkit::fixtures::preset(),
        )
        .unwrap();
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        open_import_at(&mut app, ArtifactKind::ChatCompletionPreset, &browse);

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effect, Effect::None));
        assert!(matches!(app.popup, Some(Popup::ImportArtifact(_))));
        assert_eq!(
            app.toast.as_ref().map(|toast| toast.message.as_str()),
            Some("File is a character-card-v2, expected a chat-completion-preset")
        );

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Effect::Execute(EngineCommand::ImportArtifact { source }) = effect else {
            panic!("expected artifact import command");
        };
        let source: serde_json::Value = serde_json::from_slice(&source).unwrap();
        assert_eq!(source["preset_name"], "Default Roleplay");
        assert_eq!(app.import_browser_dir.as_deref(), Some(browse.as_path()));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.popup.is_none());

        app.open_import_artifact(
            Some(ArtifactKind::ChatCompletionPreset),
            ModalTarget::Sessions,
        );
        let Some(Popup::ImportArtifact(state)) = &app.popup else {
            panic!("expected Popup::ImportArtifact");
        };
        assert_eq!(state.browser.directory, browse);
    }

    #[test]
    fn provider_copy_opens_prefilled_profile_and_saves_without_replacing_source() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let config_dir = directory.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.toml"),
            "# keep\n[providers.source]\nid = \"openai-compatible\"\nbase_url = \"https://example.com\"\nchat_completions_path = \"/v1/chat/completions\"\napi_key_env = \"EXAMPLE_API_KEY\"\ntimeout_seconds = 45\nmodel = \"source-model\"\nstream = false\n",
        )
        .unwrap();
        let config = Config::load(&config_dir).unwrap();
        let source = config.core.providers["source"].clone();
        let mut app = App::load(StcliEngine::new(database), config, None).unwrap();
        app.set_config_dir(config_dir.clone());
        app.open_provider_popup(ModalTarget::Sessions);

        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));

        let Some(Popup::ProviderProfile(state)) = &app.popup else {
            panic!("expected Popup::ProviderProfile");
        };
        assert_eq!(state.name, "source-copy");
        assert!(state.original_name.is_none());
        assert_eq!(state.original_settings.as_ref(), Some(&source));
        assert_eq!(state.model, "source-model");
        assert_eq!(state.api_key_env, "EXAMPLE_API_KEY");
        assert!(!state.stream);
        assert_eq!(state.timeout_seconds, "45");

        let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
            panic!("expected Popup::ProviderProfile");
        };
        state.name = "source".to_owned();
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.toast.as_ref().is_some_and(|toast| toast.error));
        let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
            panic!("expected clone editor after name collision");
        };
        state.name = "source-copy".to_owned();

        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        let saved = stcli_core::Config::load(&config_dir).unwrap();
        assert_eq!(saved.providers["source"].model, "source-model");
        assert_eq!(saved.providers["source-copy"].model, "source-model");
        assert!(
            fs::read_to_string(config_dir.join("config.toml"))
                .unwrap()
                .contains("# keep")
        );
    }

    #[test]
    fn provider_profile_text_can_be_edited_at_the_cursor() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();
        app.open_provider_profile_popup(None, ModalTarget::Sessions);

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        for _ in 0..3 {
            app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let Some(Popup::ProviderProfile(state)) = &app.popup else {
            panic!("expected Popup::ProviderProfile");
        };
        // Regression: provider profile fields must support editing existing text.
        assert_eq!(state.base_url, "httpsx://");

        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("httpsx█://"));
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
        let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
            panic!("expected Popup::ProviderProfile");
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
        app.open_provider_profile_popup(None, ModalTarget::Sessions);
        let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
            panic!("expected Popup::ProviderProfile");
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
        let mut personas = PersonaStore::default();
        personas.insert("Tester", "{{user}} greets {{char}}.");
        personas.save(&config_dir).unwrap();

        let mut app = App::load(
            StcliEngine::new(database),
            Config::load(&config_dir).unwrap(),
            None,
        )
        .unwrap();
        app.set_config_dir(config_dir);

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        let Some(Popup::NewSession(state)) = &mut app.popup else {
            panic!("expected Popup::NewSession");
        };
        assert_eq!(state.persona, "Tester");
        assert_eq!(state.persona_description, "{{user}} greets {{char}}.");

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let Effect::Execute(EngineCommand::CreateSession { configuration, .. }) = &effect else {
            panic!("expected CreateSession effect");
        };
        assert_eq!(
            configuration.persona_description.as_deref(),
            Some("{{user}} greets {{char}}.")
        );
        execute_command(&mut app, effect);

        assert_eq!(app.screen, Screen::Chat);
        assert!(app.history.is_some());
        assert!(app.popup.is_none());
    }
    #[test]
    fn provider_management_routes_from_sessions_chat_and_new_session() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        drop(store);
        let mut app = App::load(StcliEngine::new(database), Config::default(), None).unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(matches!(
            app.popup,
            Some(Popup::Providers {
                return_to: ModalTarget::Sessions,
                ..
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.popup.is_none());
        assert_eq!(app.screen, Screen::Sessions);

        app.open_new_session_popup();
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(matches!(
            app.popup,
            Some(Popup::Providers {
                return_to: ModalTarget::NewSession(_),
                ..
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.popup, Some(Popup::NewSession(_))));

        let (mut app, _directory) = app_with_session();
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert!(matches!(
            app.popup,
            Some(Popup::Providers {
                return_to: ModalTarget::Chat,
                ..
            })
        ));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.popup.is_none());
        assert_eq!(app.screen, Screen::Chat);

        app.chat_focus = ChatFocus::History;
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(matches!(app.popup, Some(Popup::Providers { .. })));
    }

    #[test]
    fn switching_provider_in_chat_pins_a_configuration_revision() {
        let (mut app, _directory) = app_with_session();
        let mut alternate = app
            .history
            .as_ref()
            .unwrap()
            .configuration
            .configuration
            .provider
            .clone();
        alternate.id = "alternate".to_owned();
        alternate.model = "alternate-model".to_owned();
        app.config
            .core
            .providers
            .insert("alternate".to_owned(), alternate);
        let previous_hash = app
            .history
            .as_ref()
            .unwrap()
            .configuration
            .revision_hash
            .clone();

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        let Some(Popup::Providers {
            names, selected, ..
        }) = &mut app.popup
        else {
            panic!("expected Popup::Providers");
        };
        *selected = names.iter().position(|name| name == "alternate").unwrap();
        let effect = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        execute_command(&mut app, effect);

        let history = app.history.as_ref().unwrap();
        assert_eq!(
            history.configuration.configuration.provider.model,
            "alternate-model"
        );
        assert_ne!(history.configuration.revision_hash, previous_hash);
        assert!(app.popup.is_none());
        assert_eq!(app.screen, Screen::Chat);
    }

    #[test]
    fn editing_provider_prefills_fields_and_renames_active_profile() {
        let (mut app, directory) = app_with_session();
        let config_dir = directory.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let mut active = app
            .history
            .as_ref()
            .unwrap()
            .configuration
            .configuration
            .provider
            .clone();
        active.base_url = "https://example.com".to_owned();
        app.history
            .as_mut()
            .unwrap()
            .configuration
            .configuration
            .provider = active.clone();
        stcli_core::Config::add_provider_profile(&config_dir, "active", active.clone()).unwrap();
        app.config = Config::load(&config_dir).unwrap();
        app.set_config_dir(config_dir.clone());
        let previous_hash = app
            .history
            .as_ref()
            .unwrap()
            .configuration
            .revision_hash
            .clone();

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
            panic!("expected Popup::ProviderProfile");
        };
        assert_eq!(state.original_name.as_deref(), Some("active"));
        let unchanged = fs::read_to_string(config_dir.join("config.toml")).unwrap();
        state.base_url = "http://example.com".to_owned();
        let invalid = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(invalid, Effect::None));
        assert!(matches!(app.popup, Some(Popup::ProviderProfile(_))));
        assert!(app.toast.as_ref().is_some_and(|toast| toast.error));
        assert_eq!(
            fs::read_to_string(config_dir.join("config.toml")).unwrap(),
            unchanged
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.popup, Some(Popup::Providers { .. })));
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let Some(Popup::ProviderProfile(state)) = &mut app.popup else {
            panic!("expected Popup::ProviderProfile");
        };
        assert_eq!(state.base_url, active.base_url);
        assert_eq!(state.model, active.model);
        assert_eq!(state.chat_path, active.chat_completions_path);
        assert_eq!(state.api_key_env, active.api_key_env.unwrap_or_default());
        assert_eq!(state.stream, active.stream);
        assert_eq!(state.timeout_seconds, active.timeout_seconds.to_string());
        state.name = "renamed".to_owned();
        state.model = "updated-model".to_owned();

        let effect = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(
            effect,
            Effect::Execute(EngineCommand::UpdateConfiguration { .. })
        ));
        let persisted = stcli_core::Config::load(&config_dir).unwrap();
        assert!(!persisted.providers.contains_key("active"));
        assert_eq!(persisted.providers["renamed"].model, "updated-model");
        execute_command(&mut app, effect);
        assert_ne!(
            app.history.as_ref().unwrap().configuration.revision_hash,
            previous_hash
        );
        assert_eq!(
            app.history
                .as_ref()
                .unwrap()
                .configuration
                .configuration
                .provider
                .model,
            "updated-model"
        );
    }

    #[tokio::test]
    async fn deleting_provider_requires_confirmation_and_clamps_selection() {
        let provider = stcli_testkit::MockProvider::spawn(["Persisted response"])
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = stcli_core::Store::open(&database).unwrap();
        let character = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let mut session_configuration = stcli_testkit::configuration(character.revision_hash);
        session_configuration.provider = provider.provider_settings();
        let created = store
            .create_session(session_configuration.clone(), 0)
            .unwrap();
        store
            .send_message(
                created.session.session_id,
                created.branch.branch_id,
                "Keep this turn".to_owned(),
                |_| {},
            )
            .await
            .unwrap();
        let turn = store
            .turns_for_branch(created.branch.branch_id)
            .unwrap()
            .pop()
            .unwrap();
        let attempt = store
            .attempts_for_turn(turn.turn_id)
            .unwrap()
            .pop()
            .unwrap();
        let capsule = store
            .export_turn_capsule(attempt.attempt_id, stcli_core::CapsuleKind::Portable, false)
            .unwrap();
        let mut active = session_configuration.provider.clone();
        active.base_url = "https://example.com".to_owned();
        session_configuration.provider = active.clone();
        store
            .update_session_configuration(created.session.session_id, session_configuration)
            .unwrap();
        drop(store);

        let config_dir = directory.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        stcli_core::Config::add_provider_profile(&config_dir, "active", active.clone()).unwrap();
        let mut spare = active;
        spare.id = "spare".to_owned();
        stcli_core::Config::add_provider_profile(&config_dir, "spare", spare).unwrap();
        let mut app = App::load(
            StcliEngine::new(database.clone()),
            Config::load(&config_dir).unwrap(),
            Some(created.session.session_id),
        )
        .unwrap();
        app.set_config_dir(config_dir.clone());

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        let Some(Popup::Providers {
            names, selected, ..
        }) = &mut app.popup
        else {
            panic!("expected Popup::Providers");
        };
        *selected = names.iter().position(|name| name == "spare").unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(matches!(
            app.popup,
            Some(Popup::ConfirmDeleteProvider { ref name, .. }) if name == "spare"
        ));
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| crate::ui::render(frame, &mut app))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Delete provider profile 'spare'? [y/N]"));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            stcli_core::Config::load(&config_dir)
                .unwrap()
                .providers
                .contains_key("spare")
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(
            !stcli_core::Config::load(&config_dir)
                .unwrap()
                .providers
                .contains_key("spare")
        );
        let Some(Popup::Providers {
            names, selected, ..
        }) = &app.popup
        else {
            panic!("expected Popup::Providers");
        };
        assert_eq!(names, &vec!["active".to_owned()]);
        assert_eq!(*selected, 0);
        app.reload_history().unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(
            stcli_core::Config::load(&config_dir)
                .unwrap()
                .providers
                .is_empty()
        );
        app.reload_history().unwrap();
        let history = app.history.as_ref().unwrap();
        assert_eq!(history.turns.len(), 1);
        assert_eq!(
            selected_candidate(&history.turns[0]).unwrap().content,
            "Persisted response"
        );
        let store = stcli_core::Store::open(database).unwrap();
        let replay = store.replay_turn_capsule(&capsule).unwrap();
        assert_eq!(replay.provider_calls, 0);
        assert_eq!(
            app.toast.as_ref().unwrap().message,
            "Deleted provider profile 'active'"
        );
    }
}
