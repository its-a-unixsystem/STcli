pub mod artifact;
pub mod capsule;
pub mod config;
pub mod credential;
pub mod ecma_regex;
pub mod engine;
pub mod fixture;
pub mod identity;
pub mod limits;
pub mod lore;
pub mod macros;
pub mod paths;
pub mod persona;
pub mod plugin;
pub mod profile;
pub mod prompt;
pub mod prompt_diff;
pub mod prompt_inspect;
pub mod protocol;
pub mod provider;
pub mod regex_script;
#[cfg(feature = "scripting")]
pub mod script;
pub mod session;
pub mod state;
pub mod storage;
pub mod stscript;
pub mod text_completion;
pub mod tokenizer;
pub mod turn;

pub use artifact::{
    ArtifactBundle, ArtifactError, ArtifactKind, ArtifactRecord, DecodedArtifact, PresetPatch,
    artifact_semantic_hash, clone_and_patch_preset, content_blob_hash, decode_artifact,
    decode_unique_json,
};
pub use capsule::{
    CapsuleArtifact, CapsuleArtifactSource, CapsuleBaseline, CapsuleCapabilities,
    CapsuleCompatibility, CapsuleEngine, CapsuleError, CapsuleIdentity, CapsuleKind,
    CapsuleProvider, CapsuleReference, CapsuleResult, ImportedCapsule, RedactionEntry,
    ReplayReport, SessionProjectionSnapshot, TurnCapsule,
};
pub use config::{Config, ConfigError, ProviderTemplate};
pub use credential::{
    CredentialError, CredentialResolver, SystemCredentialStore, delete_credential, get_credential,
    set_credential,
};
pub use ecma_regex::{
    EcmaRegexError, EcmaRegexWorker, RegexMatch, RegexReplaceRequest, RegexReplaceResponse,
    RegexRequest, RegexResponse, run_replace_worker, run_worker,
};
pub use engine::{
    BranchHistory, DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID, DeletionReceipt, EngineCommand, EngineError,
    EngineInspection, EngineQuery, EngineResult, EngineTurn, GreetingProjection,
    PluginArtifactOutput, PluginRemovalReceipt, PurgeReport, RebuildReport, SessionDetails,
    SessionSummary, StcliEngine, TurnDetails,
};
pub use fixture::{
    ExternalFixtureSource, FixtureCase, FixtureCaseReport, FixtureHistoryTurn, FixtureReport,
    FixtureSuite, MacroGroup, MacroGroupCase, MacroGroupDefaults, ProviderRequestParityCase,
    collect_case_ids, verify_fixture_suite,
};
pub use identity::{
    ContentHash, EntityId, artifact_revision_hash, canonical_json, canonical_json_hash,
    session_projection_hash,
};
pub use lore::{
    ActivatedLore, LoreDecision, LoreDecisionOutcome, LoreEngine, LoreEntry, LoreError,
    LorePosition, LoreResult, LoreSettings, SelectiveLogic, parse_lore_entries,
};
pub use macros::{
    MacroContext, MacroEngine, MacroError, MacroEvaluation, MacroRender, MacroWarning,
};
pub use paths::AppPaths;
pub use persona::{Persona, PersonaStore, PersonaStoreError};
pub use plugin::{
    ArtifactInspectorRegistration, InstalledPlugin, PluginCapability, PluginDependency,
    PluginEffect, PluginError, PluginEvent, PluginGrant, PluginHost, PluginInput, PluginLimits,
    PluginManifest, PluginOutput, PluginReceipt, PluginRegistry, PluginRuntime, PromptContribution,
    PromptSlot, ScriptLimits, ScriptLog, order_plugins, plugin_digest, validate_recorded_receipt,
};
pub use profile::{
    CompatibilityOutcome, CompatibilityProfile, CompatibilitySubject, CompatibilitySubjectKind,
};
pub use prompt::{
    CHAT_COMPLETION_CHARACTER_ID, PresetOrder, PresetPrompt, PromptError, PromptPreset,
    PromptPruning, PromptSegment, RenderedPromptContent, apply_prompt_preset,
    insert_in_chat_segments, prune_segments,
};
pub use prompt_diff::{
    PromptDiff, PromptSegmentChange, PromptSegmentDiff, PromptSegmentSnapshot, PromptTextChange,
    PromptTextChangeKind, PromptTextDiff, PromptTokenDelta, diff_prompt_plans,
};
pub use prompt_inspect::{
    PromptSegmentDetail, PromptSegmentInspection, PromptSegmentTransformations,
};
pub use protocol::{CliEnvelope, CliError, CliWarning};
pub use provider::{
    ChatMessage, ChatRole, OpenAiProvider, ProviderError, ProviderEvent, ProviderResult,
    provider_request, provider_request_hash, validate_provider_settings,
};
pub use regex_script::{
    RegexPlacement, RegexScript, RegexScriptApplication, SubstituteMode, apply_display_scripts,
};
#[cfg(feature = "scripting")]
pub use script::{ScriptOutcome, execute};
pub use session::{
    BranchProjection, CompactionCounts, CompactionReport, CreatedSession, HeaderSetting, PluginPin,
    ProviderSettings, SessionConfiguration, SessionConfigurationRecord, SessionError,
    SessionProjection, available_duplicated_session_name,
};
pub use state::{StateCell, StateError, StateKey, StateMutation, StateTransaction, VariableScope};
pub use storage::{
    AssetRecord, AssetReference, RecoveryReport, StorageError, Store, TraceEventRecord,
};
pub use stscript::{
    StscriptCommand, StscriptError, StscriptLimits, StscriptProgram, StscriptReplayOutcome,
    StscriptResult, parse_stscript,
};
pub use text_completion::{ContextFormatting, FormatMode, InstructTemplate, NamesBehavior};
pub use tokenizer::{TokenizerError, TokenizerId};
pub use turn::{
    AttemptEffectReceipt, AttemptProjection, AttemptStatus, CandidateOrigin, CandidateProjection,
    CompatibilityWarning, CompletedTurn, DryRunResult, EditedCandidate,
    EffectiveGenerationSettings, FailedTurn, GenerationSettingSource, GenerationType,
    PluginCommandResult, PresetScriptMetadata, PresetTransformationResult, PromptPlan,
    ScriptSource, TurnError, TurnPreparation, TurnProjection, extract_character_scripts,
    transform_preset_content,
};
