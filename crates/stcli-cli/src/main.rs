mod provider_test;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};
use stcli_core::{
    AppPaths, ArtifactBundle, CapsuleKind, CliEnvelope, CliError, Config, ContentHash,
    EngineCommand, EngineInspection, EngineQuery, EngineResult, EntityId, InstalledPlugin,
    PluginCapability, PromptDiff, PromptSegmentChange, PromptSegmentInspection,
    PromptTextChangeKind, ProviderEvent, ProviderSettings, RegexReplaceRequest, RegexRequest,
    SessionConfiguration, StcliEngine, TurnCapsule, run_replace_worker, run_worker,
    verify_fixture_suite,
};

#[derive(Debug, Parser)]
#[command(
    name = "stcli",
    version,
    about = "Local SillyTavern-compatible roleplaying engine",
    after_help = "Argument conventions:
  Primary resource targets and command payloads are positional.
  Context, optional selectors, modifiers, and configuration use named --options.
  Run `stcli <command> <subcommand> --help` for the exact command syntax."
)]
struct Cli {
    #[arg(long, value_enum, default_value_t = OutputFormat::Human, global = true)]
    output: OutputFormat,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Message {
        #[command(subcommand)]
        command: MessageCommand,
    },
    Turn {
        #[command(subcommand)]
        command: TurnCommand,
    },
    Branch {
        #[command(subcommand)]
        command: BranchCommand,
    },
    Candidate {
        #[command(subcommand)]
        command: CandidateCommand,
    },
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    Prompt {
        #[command(subcommand)]
        command: PromptCommand,
    },
    Compat {
        #[command(subcommand)]
        command: CompatCommand,
    },
    Tui {
        session: Option<EntityId>,
    },
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    ProviderTest {
        #[command(subcommand)]
        command: ProviderTestCommand,
    },
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Self::Artifact { command } => command.name(),
            Self::Session { command } => command.name(),
            Self::Message { command } => command.name(),
            Self::Turn { command } => command.name(),
            Self::Branch { command } => command.name(),
            Self::Candidate { command } => command.name(),
            Self::Plugin { command } => command.name(),
            Self::Prompt { command } => command.name(),
            Self::Profile { command } => command.name(),
            Self::Compat { .. } => "compat.verify",
            Self::Tui { .. } => "tui",
            Self::ProviderTest { .. } => "provider-test.serve",
            Self::Internal { .. } => "internal.regex-worker",
        }
    }
}

#[derive(Debug, Subcommand)]
enum ArtifactCommand {
    Import {
        path: PathBuf,
    },
    List,
    Show {
        revision: ContentHash,
    },
    Export {
        revision: ContentHash,
        destination: PathBuf,
    },
}

impl ArtifactCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Import { .. } => "artifact.import",
            Self::List => "artifact.list",
            Self::Show { .. } => "artifact.show",
            Self::Export { .. } => "artifact.export",
        }
    }
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    Create(Box<CreateSessionArgs>),
    Update {
        session: EntityId,
        #[command(flatten)]
        configuration: Box<CreateSessionArgs>,
    },
    Duplicate {
        session: EntityId,
        #[arg(long)]
        branch: Option<EntityId>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        up_to: Option<EntityId>,
    },
    Greeting {
        #[arg(long)]
        session: EntityId,
        branch: EntityId,
        greeting: usize,
    },
    List,
    Archive {
        session: EntityId,
    },
    Purge {
        session: EntityId,
    },
    Compact {
        session: EntityId,
    },
    Recover,
    Show {
        session: EntityId,
    },
    Branches {
        session: EntityId,
    },
    Rebuild,
}

impl SessionCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Archive { .. } => "session.archive",
            Self::Purge { .. } => "session.purge",
            Self::Recover => "session.recover",
            Self::Compact { .. } => "session.compact",
            Self::Create(_) => "session.create",
            Self::Update { .. } => "session.update",
            Self::Duplicate { .. } => "session.duplicate",
            Self::Greeting { .. } => "session.greeting",
            Self::List => "session.list",
            Self::Show { .. } => "session.show",
            Self::Branches { .. } => "session.branches",
            Self::Rebuild => "session.rebuild",
        }
    }
}

#[derive(Debug, Subcommand)]
enum MessageCommand {
    Send {
        #[arg(long)]
        session: EntityId,
        #[arg(long)]
        branch: Option<EntityId>,
        #[arg(long)]
        dry_run: bool,
        text: String,
    },
    Retry {
        #[arg(long)]
        turn: EntityId,
        attempt: EntityId,
    },
    Continue {
        turn: EntityId,
        #[arg(long)]
        dry_run: bool,
    },
    Regenerate {
        turn: EntityId,
        #[arg(long)]
        dry_run: bool,
    },
    Swipe {
        turn: EntityId,
        #[arg(long, conflicts_with = "dry_run")]
        candidate: Option<EntityId>,
        #[arg(long)]
        dry_run: bool,
    },
    EditUser {
        turn: EntityId,
        text: String,
    },
    EditCandidate {
        candidate: EntityId,
        text: String,
    },
    Cancel {
        attempt: EntityId,
    },
    Turns {
        branch: EntityId,
    },
}

impl MessageCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Send { dry_run: true, .. } => "message.send.dry-run",
            Self::Send { .. } => "message.send",
            Self::Retry { .. } => "message.retry",
            Self::Continue { dry_run: true, .. } => "message.continue.dry-run",
            Self::Continue { .. } => "message.continue",
            Self::Regenerate { dry_run: true, .. } => "message.regenerate.dry-run",
            Self::Regenerate { .. } => "message.regenerate",
            Self::Swipe {
                candidate: Some(_), ..
            } => "message.swipe.select",
            Self::Swipe { dry_run: true, .. } => "message.swipe.dry-run",
            Self::Swipe { .. } => "message.swipe",
            Self::EditUser { .. } => "message.edit-user",
            Self::EditCandidate { .. } => "message.edit-candidate",
            Self::Cancel { .. } => "message.cancel",
            Self::Turns { .. } => "message.turns",
        }
    }
}
#[derive(Debug, Subcommand)]
enum TurnCommand {
    Inspect {
        #[arg(long)]
        session: EntityId,
        attempt: EntityId,
    },
    Export {
        #[arg(long)]
        session: EntityId,
        attempt: EntityId,
        #[arg(long)]
        thin: bool,
        #[arg(long)]
        redact_content: bool,
        #[arg(long)]
        file: PathBuf,
    },
    Replay {
        capsule: PathBuf,
    },
    Import {
        capsule: PathBuf,
    },
    Rerun {
        #[arg(long)]
        session: EntityId,
        attempt: EntityId,
        #[arg(long)]
        dry_run: bool,
    },
    Hide {
        turn: EntityId,
    },
    Delete {
        turn: EntityId,
    },
}

impl TurnCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Inspect { .. } => "turn.inspect",
            Self::Export { .. } => "turn.export",
            Self::Replay { .. } => "turn.replay",
            Self::Import { .. } => "turn.import",
            Self::Rerun { dry_run: true, .. } => "turn.rerun.dry-run",
            Self::Rerun { .. } => "turn.rerun",
            Self::Hide { .. } => "turn.hide",
            Self::Delete { .. } => "turn.delete",
        }
    }
}

#[derive(Debug, Subcommand)]
enum BranchCommand {
    Delete { branch: EntityId },
}

impl BranchCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Delete { .. } => "branch.delete",
        }
    }
}

#[derive(Debug, Subcommand)]
enum CandidateCommand {
    Hide { candidate: EntityId },
    Delete { candidate: EntityId },
}

impl CandidateCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Hide { .. } => "candidate.hide",
            Self::Delete { .. } => "candidate.delete",
        }
    }
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    Doctor {
        directory: PathBuf,
    },
    Install {
        directory: PathBuf,
    },
    List,
    Inspect {
        id: String,
    },
    Adopt {
        #[arg(long)]
        session: EntityId,
        id: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        digest: ContentHash,
        #[arg(long)]
        capability: Vec<String>,
        #[arg(long, default_value = "{}")]
        settings: String,
    },
    Upgrade {
        #[arg(long)]
        session: EntityId,
        id: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        digest: ContentHash,
    },
    Invoke {
        #[arg(long)]
        session: EntityId,
        id: String,
        command: String,
        #[arg(long, default_value = "null")]
        arguments: String,
    },
    Enable {
        #[arg(long)]
        session: EntityId,
        id: String,
    },
    Disable {
        #[arg(long)]
        session: EntityId,
        id: String,
    },
    Remove {
        id: String,
    },
}

impl PluginCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Doctor { .. } => "plugin.doctor",
            Self::Install { .. } => "plugin.install",
            Self::List => "plugin.list",
            Self::Inspect { .. } => "plugin.inspect",
            Self::Adopt { .. } => "plugin.adopt",
            Self::Upgrade { .. } => "plugin.upgrade",
            Self::Invoke { .. } => "plugin.invoke",
            Self::Enable { .. } => "plugin.enable",
            Self::Disable { .. } => "plugin.disable",
            Self::Remove { .. } => "plugin.remove",
        }
    }
}

#[derive(Debug, Subcommand)]
enum PromptCommand {
    Inspect {
        attempt: EntityId,
        #[arg(long)]
        diff_prev: bool,
        #[arg(long, conflicts_with = "diff_prev")]
        segment: Option<String>,
    },
    Diff {
        baseline_attempt: EntityId,
        target_attempt: EntityId,
    },
}

impl PromptCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::Inspect { .. } => "prompt.inspect",
            Self::Diff { .. } => "prompt.diff",
        }
    }
}

#[derive(Debug, Args)]
struct CreateSessionArgs {
    #[arg(long)]
    character: ContentHash,
    #[arg(long, default_value = "User")]
    persona: String,
    #[arg(long)]
    persona_description: Option<String>,
    #[arg(long)]
    lorebook: Vec<ContentHash>,
    #[arg(long)]
    preset: Option<ContentHash>,
    /// Digest of a regex script to authorize (repeatable).
    #[arg(long = "grant-script", alias = "grant-preset-script")]
    grant_script: Vec<ContentHash>,
    /// Named provider connection profile from config.toml.
    #[arg(long)]
    provider_profile: Option<String>,
    #[arg(long, default_value = "default")]
    provider: String,
    #[arg(long)]
    provider_base_url: Option<String>,
    #[arg(long)]
    provider_chat_path: Option<String>,
    #[arg(long)]
    provider_api_key_env: Option<String>,
    #[arg(long)]
    provider_ca_certificate: Option<PathBuf>,
    #[arg(long)]
    provider_timeout: Option<u64>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, action = clap::ArgAction::Set)]
    provider_stream: Option<bool>,
    #[arg(long, default_value = "tiktoken:o200k_base")]
    tokenizer: String,
    #[arg(long, default_value = "{}")]
    generation_settings: String,
    #[arg(long, default_value_t = 0)]
    greeting: usize,
    #[arg(long, default_value = "sillytavern-1.18-core")]
    compatibility_profile: String,
}

#[derive(Serialize)]
struct CliStreamEvent<'a> {
    schema: &'static str,
    event_id: EntityId,
    event_type: &'static str,
    data: &'a ProviderEvent,
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    List,
    Show {
        name: String,
    },
    Add {
        name: String,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    Remove {
        name: String,
    },
}

impl ProfileCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::List => "profile.list",
            Self::Show { .. } => "profile.show",
            Self::Add { .. } => "profile.add",
            Self::Remove { .. } => "profile.remove",
        }
    }
}

#[derive(Debug, Subcommand)]
enum CompatCommand {
    Verify(VerifyArgs),
}

#[derive(Debug, Args)]
struct VerifyArgs {
    #[arg(long, default_value = "compat/profiles/sillytavern-1.18-core.json")]
    profile: PathBuf,
    #[arg(long, default_value = "compat/fixtures")]
    fixtures: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Subcommand)]
enum ProviderTestCommand {
    Serve {
        #[arg(long, default_value = "127.0.0.1:3443")]
        bind: SocketAddr,
        #[arg(long)]
        certificate_output: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum InternalCommand {
    RegexWorker,
    RegexReplaceWorker,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let command_name = cli.command.name();
    match run(cli.output, cli.command).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            emit_error(cli.output, command_name, &error);
            ExitCode::FAILURE
        }
    }
}

async fn run(output: OutputFormat, command: Command) -> Result<()> {
    match command {
        Command::Artifact { command } => artifact(output, command).await,
        Command::Session { command } => session(output, command).await,
        Command::Message { command } => message(output, command).await,
        Command::Turn { command } => turn(output, command).await,
        Command::Branch { command } => branch(output, command).await,
        Command::Candidate { command } => candidate(output, command).await,
        Command::Plugin { command } => plugin(output, command).await,
        Command::Prompt { command } => prompt(output, command),
        Command::Profile { command } => profile(output, command).await,
        Command::Compat {
            command: CompatCommand::Verify(args),
        } => verify(output, args).await,
        Command::Tui { session } => {
            let paths = AppPaths::discover()?;
            stcli_tui::run(&paths, session)
        }
        Command::ProviderTest {
            command:
                ProviderTestCommand::Serve {
                    bind,
                    certificate_output,
                },
        } => provider_test::serve(bind, certificate_output.as_deref()).await,
        Command::Internal {
            command: InternalCommand::RegexWorker,
        } => regex_worker(),
        Command::Internal {
            command: InternalCommand::RegexReplaceWorker,
        } => regex_replace_worker(),
    }
}

fn regex_worker() -> Result<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let request = serde_json::from_slice::<RegexRequest>(&input)?;
    serde_json::to_writer(io::stdout(), &run_worker(request))?;
    Ok(())
}

fn regex_replace_worker() -> Result<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let request = serde_json::from_slice::<RegexReplaceRequest>(&input)?;
    serde_json::to_writer(io::stdout(), &run_replace_worker(request))?;
    Ok(())
}

async fn artifact(output: OutputFormat, command: ArtifactCommand) -> Result<()> {
    let command_name = command.name();
    let engine = open_engine()?;
    match command {
        ArtifactCommand::Import { path } => {
            let source = fs::read(&path)
                .with_context(|| format!("failed to read artifact '{}'", path.display()))?;
            let EngineResult::ArtifactBundle {
                primary,
                supplementary_artifacts,
                asset_count,
            } = engine
                .execute(EngineCommand::ImportArtifact { source }, |_| {})
                .await?
            else {
                unreachable!()
            };
            emit(
                output,
                command_name,
                &ArtifactBundle {
                    primary,
                    supplementary_artifacts,
                    asset_count,
                },
            )
        }
        ArtifactCommand::List => {
            let EngineInspection::Artifacts(records) =
                engine.inspect(EngineQuery::Artifacts { kind: None })?
            else {
                unreachable!()
            };
            emit(output, command_name, &records)
        }
        ArtifactCommand::Show { revision } => {
            let EngineInspection::Artifact(record) = engine.inspect(EngineQuery::Artifact {
                revision_hash: revision,
            })?
            else {
                unreachable!()
            };
            emit(output, command_name, &record)
        }
        ArtifactCommand::Export {
            revision,
            destination,
        } => {
            let EngineInspection::ArtifactSource(source) =
                engine.inspect(EngineQuery::ArtifactSource {
                    revision_hash: revision.clone(),
                })?
            else {
                unreachable!()
            };
            fs::write(&destination, source).with_context(|| {
                format!("failed to export artifact to '{}'", destination.display())
            })?;
            emit(
                output,
                command_name,
                &json!({"revision_hash": revision, "destination": destination}),
            )
        }
    }
}

fn configuration_from_args(
    args: CreateSessionArgs,
    config: &Config,
) -> Result<SessionConfiguration> {
    let provider = if let Some(profile_name) = &args.provider_profile {
        let profile = config
            .resolve_provider_profile(profile_name)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let mut provider = profile.clone();
        if let Some(model) = args.model {
            provider.model = model;
        }
        if let Some(stream) = args.provider_stream {
            provider.stream = stream;
        }
        if let Some(base_url) = args.provider_base_url {
            provider.base_url = base_url;
        }
        if let Some(chat_path) = args.provider_chat_path {
            provider.chat_completions_path = chat_path;
        }
        if args.provider_api_key_env.is_some() {
            provider.api_key_env = args.provider_api_key_env;
        }
        if let Some(timeout) = args.provider_timeout {
            provider.timeout_seconds = timeout;
        }
        if let Some(ca_path) = &args.provider_ca_certificate {
            provider.ca_certificate_pem = Some(
                fs::read_to_string(ca_path).context("failed to read provider CA certificate")?,
            );
        }
        provider
    } else {
        let ca_certificate_pem = args
            .provider_ca_certificate
            .as_ref()
            .map(fs::read_to_string)
            .transpose()
            .context("failed to read provider CA certificate")?;
        ProviderSettings {
            id: args.provider,
            base_url: args
                .provider_base_url
                .unwrap_or_else(|| "https://127.0.0.1:3443".to_owned()),
            chat_completions_path: args
                .provider_chat_path
                .unwrap_or_else(|| "/v1/chat/completions".to_owned()),
            api_key_env: args.provider_api_key_env,
            static_headers: BTreeMap::new(),
            timeout_seconds: args.provider_timeout.unwrap_or(120),
            ca_certificate_pem,
            model: args.model.unwrap_or_else(|| "fixture-model".to_owned()),
            stream: args.provider_stream.unwrap_or(true),
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        }
    };
    let generation_settings = serde_json::from_str::<Value>(&args.generation_settings)
        .context("generation settings must be valid JSON")?;
    let persona_description = args
        .persona_description
        .map(|description| {
            let Some(path) = description.strip_prefix('@') else {
                return Ok(description);
            };
            fs::read_to_string(path)
                .with_context(|| format!("failed to read persona description file '{path}'"))
        })
        .transpose()?;
    Ok(SessionConfiguration {
        compatibility_profile: args.compatibility_profile,
        character_revision: args.character,
        persona_name: args.persona,
        persona_description,
        lorebook_revisions: args.lorebook,
        prompt_preset_revision: args.preset,
        provider,
        tokenizer: args.tokenizer,
        generation_settings,
        plugins: vec![],
        script_grants: args.grant_script,
    })
}

async fn profile(output: OutputFormat, command: ProfileCommand) -> Result<()> {
    let command_name = command.name();
    let paths = AppPaths::discover()?;
    paths.ensure_exists()?;
    match command {
        ProfileCommand::List => {
            let config = Config::load(&paths.config)?;
            emit(output, command_name, &config.providers)
        }
        ProfileCommand::Show { name } => {
            let config = Config::load(&paths.config)?;
            let profile = config
                .resolve_provider_profile(&name)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            emit(output, command_name, profile)
        }
        ProfileCommand::Add { name, file } => {
            let content = if let Some(file_path) = file {
                if file_path == Path::new("-") {
                    let mut buffer = String::new();
                    io::stdin().read_to_string(&mut buffer)?;
                    buffer
                } else {
                    fs::read_to_string(&file_path).with_context(|| {
                        format!("failed to read profile file '{}'", file_path.display())
                    })?
                }
            } else {
                let mut buffer = String::new();
                io::stdin().read_to_string(&mut buffer)?;
                buffer
            };
            let settings: ProviderSettings = if content.trim_start().starts_with('{') {
                serde_json::from_str(&content).context("failed to parse profile JSON")?
            } else {
                toml::from_str(&content).context("failed to parse profile TOML")?
            };
            Config::add_provider_profile(&paths.config, &name, settings.clone())
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            emit(
                output,
                command_name,
                &json!({ "name": name, "profile": settings }),
            )
        }
        ProfileCommand::Remove { name } => {
            let removed = Config::remove_provider_profile(&paths.config, &name)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            if !removed {
                anyhow::bail!("provider profile '{name}' not found");
            }
            emit(
                output,
                command_name,
                &json!({ "name": name, "removed": true }),
            )
        }
    }
}

async fn session(output: OutputFormat, command: SessionCommand) -> Result<()> {
    let command_name = command.name();
    let paths = AppPaths::discover()?;
    paths.ensure_exists()?;
    let config = Config::load(&paths.config)?;
    let engine = StcliEngine::new(paths.database());
    match command {
        SessionCommand::Create(args) => {
            let greeting_index = args.greeting;
            let configuration = configuration_from_args(*args, &config)?;
            let result = engine
                .execute(
                    EngineCommand::CreateSession {
                        configuration: Box::new(configuration),
                        greeting_index,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::Update {
            session,
            configuration,
        } => {
            let configuration = configuration_from_args(*configuration, &config)?;
            let result = engine
                .execute(
                    EngineCommand::UpdateConfiguration {
                        session_id: session,
                        configuration: Box::new(configuration),
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::Duplicate {
            session,
            branch,
            name,
            up_to,
        } => {
            let result = engine
                .execute(
                    EngineCommand::DuplicateSession {
                        session_id: session,
                        branch_id: branch,
                        up_to_turn_id: up_to,
                        new_name: name,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::Greeting {
            session,
            branch,
            greeting,
        } => {
            let result = engine
                .execute(
                    EngineCommand::SelectGreeting {
                        session_id: session,
                        branch_id: branch,
                        greeting_index: greeting,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::List => {
            let EngineInspection::SessionProjections(sessions) =
                engine.inspect(EngineQuery::SessionProjections)?
            else {
                unreachable!()
            };
            emit(output, command_name, &sessions)
        }
        SessionCommand::Archive { session } => {
            let result = engine
                .execute(
                    EngineCommand::ArchiveSession {
                        session_id: session,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::Purge { session } => {
            let result = engine
                .execute(
                    EngineCommand::PurgeSession {
                        session_id: session,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::Compact { session } => {
            let result = engine
                .execute(
                    EngineCommand::CompactSession {
                        session_id: session,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::Recover => {
            let result = engine.execute(EngineCommand::Recover, |_| {}).await?;
            emit_engine_result(output, command_name, &result)
        }
        SessionCommand::Show { session } => {
            let EngineInspection::SessionDetails(details) =
                engine.inspect(EngineQuery::SessionDetails {
                    session_id: session,
                })?
            else {
                unreachable!()
            };
            emit(output, command_name, &details)
        }
        SessionCommand::Branches { session } => {
            let EngineInspection::Branches(branches) = engine.inspect(EngineQuery::Branches {
                session_id: session,
            })?
            else {
                unreachable!()
            };
            emit(output, command_name, &branches)
        }
        SessionCommand::Rebuild => {
            let result = engine
                .execute(EngineCommand::RebuildSessionProjections, |_| {})
                .await?;
            emit_engine_result(output, command_name, &result)
        }
    }
}

async fn branch(output: OutputFormat, command: BranchCommand) -> Result<()> {
    let command_name = command.name();
    let engine = open_engine()?;
    match command {
        BranchCommand::Delete { branch } => {
            engine
                .execute(EngineCommand::DeleteBranch { branch_id: branch }, |_| {})
                .await?;
            emit(
                output,
                command_name,
                &json!({"branch_id": branch, "deleted": true}),
            )
        }
    }
}

async fn candidate(output: OutputFormat, command: CandidateCommand) -> Result<()> {
    let command_name = command.name();
    let engine = open_engine()?;
    match command {
        CandidateCommand::Hide { candidate } => {
            let result = engine
                .execute(
                    EngineCommand::HideCandidate {
                        candidate_id: candidate,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        CandidateCommand::Delete { candidate } => {
            engine
                .execute(
                    EngineCommand::DeleteCandidate {
                        candidate_id: candidate,
                    },
                    |_| {},
                )
                .await?;
            emit(
                output,
                command_name,
                &json!({"candidate_id": candidate, "deleted": true}),
            )
        }
    }
}

async fn message(output: OutputFormat, command: MessageCommand) -> Result<()> {
    let command_name = command.name();
    let paths = AppPaths::discover()?;
    paths.ensure_exists()?;
    let engine = StcliEngine::new(paths.database());
    let result = match command {
        MessageCommand::Send {
            session,
            branch,
            dry_run,
            text,
        } => {
            let branch_id = match branch {
                Some(branch) => branch,
                None => match engine.inspect(EngineQuery::Session {
                    session_id: session,
                })? {
                    EngineInspection::Session(projection) => projection.root_branch_id,
                    _ => unreachable!(),
                },
            };
            engine
                .execute(
                    if dry_run {
                        EngineCommand::DryRunSend {
                            session_id: session,
                            branch_id,
                            content: text,
                        }
                    } else {
                        EngineCommand::Send {
                            session_id: session,
                            branch_id,
                            content: text,
                        }
                    },
                    |event| emit_provider_event(output, event),
                )
                .await?
        }
        MessageCommand::Retry { turn, attempt } => {
            engine
                .execute(
                    EngineCommand::Retry {
                        turn_id: turn,
                        attempt_id: attempt,
                    },
                    |event| emit_provider_event(output, event),
                )
                .await?
        }
        MessageCommand::Continue { turn, dry_run } => {
            engine
                .execute(
                    if dry_run {
                        EngineCommand::DryRunContinue { turn_id: turn }
                    } else {
                        EngineCommand::Continue { turn_id: turn }
                    },
                    |event| emit_provider_event(output, event),
                )
                .await?
        }
        MessageCommand::Regenerate { turn, dry_run } => {
            engine
                .execute(
                    if dry_run {
                        EngineCommand::DryRunRegenerate { turn_id: turn }
                    } else {
                        EngineCommand::Regenerate { turn_id: turn }
                    },
                    |event| emit_provider_event(output, event),
                )
                .await?
        }
        MessageCommand::Swipe {
            turn,
            candidate: Some(candidate),
            ..
        } => {
            engine
                .execute(
                    EngineCommand::SelectCandidate {
                        turn_id: turn,
                        candidate_id: candidate,
                    },
                    |_| {},
                )
                .await?
        }
        MessageCommand::Swipe {
            turn,
            candidate: None,
            dry_run,
        } => {
            engine
                .execute(
                    if dry_run {
                        EngineCommand::DryRunSwipe { turn_id: turn }
                    } else {
                        EngineCommand::GenerateSwipe { turn_id: turn }
                    },
                    |event| emit_provider_event(output, event),
                )
                .await?
        }
        MessageCommand::EditUser { turn, text } => {
            engine
                .execute(
                    EngineCommand::EditUser {
                        turn_id: turn,
                        content: text,
                    },
                    |event| emit_provider_event(output, event),
                )
                .await?
        }
        MessageCommand::EditCandidate { candidate, text } => {
            engine
                .execute(
                    EngineCommand::EditCandidate {
                        candidate_id: candidate,
                        content: text,
                    },
                    |_| {},
                )
                .await?
        }
        MessageCommand::Cancel { attempt } => {
            engine
                .execute(
                    EngineCommand::Cancel {
                        attempt_id: attempt,
                    },
                    |_| {},
                )
                .await?
        }
        MessageCommand::Turns { branch } => {
            let EngineInspection::Turns(turns) =
                engine.inspect(EngineQuery::BranchTurns { branch_id: branch })?
            else {
                unreachable!()
            };
            return emit(output, command_name, &turns);
        }
    };
    emit_engine_result(output, command_name, &result)
}

async fn turn(output: OutputFormat, command: TurnCommand) -> Result<()> {
    let command_name = command.name();
    let engine = open_engine()?;
    match command {
        TurnCommand::Inspect { session, attempt } => {
            let EngineInspection::TurnDetails(details) =
                engine.inspect(EngineQuery::TurnDetails {
                    session_id: session,
                    attempt_id: attempt,
                })?
            else {
                unreachable!()
            };
            emit(output, command_name, &details)
        }
        TurnCommand::Export {
            session,
            attempt,
            thin,
            redact_content,
            file,
        } => {
            let kind = if thin {
                CapsuleKind::Thin
            } else {
                CapsuleKind::Portable
            };
            let EngineInspection::Capsule(capsule) =
                engine.inspect(EngineQuery::ExportCapsule {
                    session_id: session,
                    attempt_id: attempt,
                    kind,
                    redact_content,
                })?
            else {
                unreachable!()
            };
            fs::write(&file, serde_json::to_vec_pretty(&capsule)?)
                .with_context(|| format!("failed to write {}", file.display()))?;
            emit(
                output,
                command_name,
                &json!({
                    "path": file,
                    "capsule_hash": capsule.hash()?,
                    "kind": kind,
                    "capabilities": capsule.capabilities,
                }),
            )
        }
        TurnCommand::Replay { capsule } => {
            let source = read_bounded_file(&capsule, stcli_core::limits::MAX_CAPSULE_BYTES)?;
            let capsule = serde_json::from_slice::<TurnCapsule>(&source)?;
            let EngineInspection::ReplayReport(report) =
                engine.inspect(EngineQuery::ReplayCapsule {
                    capsule: Box::new(capsule),
                })?
            else {
                unreachable!()
            };
            emit(output, command_name, &report)
        }
        TurnCommand::Import { capsule } => {
            let source = read_bounded_file(&capsule, stcli_core::limits::MAX_CAPSULE_BYTES)?;
            let capsule = serde_json::from_slice::<TurnCapsule>(&source)?;
            let result = engine
                .execute(
                    EngineCommand::ImportCapsule {
                        capsule: Box::new(capsule),
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        TurnCommand::Rerun {
            session,
            attempt,
            dry_run: true,
        } => {
            let EngineInspection::DryRun(preview) = engine.inspect(EngineQuery::DryRunRerun {
                session_id: session,
                attempt_id: attempt,
            })?
            else {
                unreachable!()
            };
            emit(output, command_name, &preview)
        }
        TurnCommand::Rerun {
            session,
            attempt,
            dry_run: false,
        } => {
            let result = engine
                .execute(
                    EngineCommand::Rerun {
                        session_id: session,
                        attempt_id: attempt,
                    },
                    |event| emit_provider_event(output, event),
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        TurnCommand::Hide { turn } => {
            let result = engine
                .execute(EngineCommand::HideTurn { turn_id: turn }, |_| {})
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        TurnCommand::Delete { turn } => {
            engine
                .execute(EngineCommand::DeleteTurn { turn_id: turn }, |_| {})
                .await?;
            emit(
                output,
                command_name,
                &json!({"turn_id": turn, "deleted": true}),
            )
        }
    }
}

async fn plugin(output: OutputFormat, command: PluginCommand) -> Result<()> {
    let command_name = command.name();
    let paths = AppPaths::discover()?;
    paths.ensure_exists()?;
    let engine = StcliEngine::new(paths.database());
    match command {
        PluginCommand::Doctor { directory } => {
            let EngineInspection::InstalledPlugin(plugin) =
                engine.inspect(EngineQuery::DoctorPlugin { directory })?
            else {
                unreachable!()
            };
            emit(output, command_name, &plugin)
        }
        PluginCommand::Install { directory } => {
            let result = engine
                .execute(EngineCommand::InstallPlugin { directory }, |_| {})
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        PluginCommand::List => emit(output, command_name, &installed_plugins(&engine, None)?),
        PluginCommand::Inspect { id } => {
            let plugins = installed_plugins(&engine, Some(&id))?;
            anyhow::ensure!(!plugins.is_empty(), "Plugin '{id}' was not found");
            emit(output, command_name, &plugins)
        }
        PluginCommand::Adopt {
            session,
            version,
            id,
            digest,
            capability,
            settings,
        } => {
            let capabilities = capability
                .into_iter()
                .map(|value| value.parse::<PluginCapability>())
                .collect::<Result<BTreeSet<_>, _>>()?;
            let settings =
                serde_json::from_str(&settings).context("plugin settings must be valid JSON")?;
            let result = engine
                .execute(
                    EngineCommand::AdoptPlugin {
                        session_id: session,
                        id,
                        version,
                        digest,
                        capabilities,
                        settings,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        PluginCommand::Upgrade {
            session,
            id,
            version,
            digest,
        } => {
            let result = engine
                .execute(
                    EngineCommand::UpgradePlugin {
                        session_id: session,
                        id,
                        version,
                        digest,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        PluginCommand::Invoke {
            session,
            id,
            command,
            arguments,
        } => {
            let arguments =
                serde_json::from_str(&arguments).context("command arguments must be valid JSON")?;
            let result = engine
                .execute(
                    EngineCommand::InvokePlugin {
                        session_id: session,
                        plugin_id: id,
                        command,
                        arguments,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        PluginCommand::Enable { session, id } => {
            let result = engine
                .execute(
                    EngineCommand::SetPluginEnabled {
                        session_id: session,
                        id,
                        enabled: true,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        PluginCommand::Disable { session, id } => {
            let result = engine
                .execute(
                    EngineCommand::SetPluginEnabled {
                        session_id: session,
                        id,
                        enabled: false,
                    },
                    |_| {},
                )
                .await?;
            emit_engine_result(output, command_name, &result)
        }
        PluginCommand::Remove { id } => {
            let result = engine
                .execute(EngineCommand::RemovePlugin { plugin_id: id }, |_| {})
                .await?;
            emit_engine_result(output, command_name, &result)
        }
    }
}

fn installed_plugins(
    engine: &StcliEngine,
    plugin_id: Option<&str>,
) -> Result<Vec<InstalledPlugin>> {
    let EngineInspection::Plugins(plugins) = engine.inspect(EngineQuery::Plugins {
        plugin_id: plugin_id.map(str::to_owned),
    })?
    else {
        unreachable!()
    };
    Ok(plugins)
}

fn prompt(output: OutputFormat, command: PromptCommand) -> Result<()> {
    let command_name = command.name();
    let engine = open_engine()?;
    match command {
        PromptCommand::Inspect {
            attempt,
            diff_prev: false,
            segment: None,
        } => {
            let EngineInspection::PromptPlan(plan) = engine.inspect(EngineQuery::PromptPlan {
                attempt_id: attempt,
            })?
            else {
                unreachable!()
            };
            emit(output, command_name, &plan)
        }
        PromptCommand::Inspect {
            attempt,
            diff_prev: false,
            segment: Some(selector),
        } => {
            let EngineInspection::PromptSegments(inspection) =
                engine.inspect(EngineQuery::PromptSegments {
                    attempt_id: attempt,
                    selector,
                })?
            else {
                unreachable!()
            };
            emit_prompt_segment_inspection(output, command_name, &inspection)
        }
        PromptCommand::Inspect {
            attempt,
            diff_prev: true,
            segment: _,
        } => {
            let EngineInspection::PromptDiff(diff) =
                engine.inspect(EngineQuery::PreviousPromptDiff {
                    attempt_id: attempt,
                })?
            else {
                unreachable!()
            };
            emit_prompt_diff(output, command_name, &diff)
        }
        PromptCommand::Diff {
            baseline_attempt,
            target_attempt,
        } => {
            let EngineInspection::PromptDiff(diff) = engine.inspect(EngineQuery::PromptDiff {
                baseline_attempt_id: baseline_attempt,
                target_attempt_id: target_attempt,
            })?
            else {
                unreachable!()
            };
            emit_prompt_diff(output, command_name, &diff)
        }
    }
}

fn emit_prompt_segment_inspection(
    output: OutputFormat,
    command: &str,
    inspection: &PromptSegmentInspection,
) -> Result<()> {
    if matches!(output, OutputFormat::Json) {
        return emit(output, command, inspection);
    }
    for detail in &inspection.segments {
        let segment = &detail.segment;
        println!(
            "Segment {}: {} ({:?})",
            detail.index, segment.slot, segment.role
        );
        println!("source: {}", segment.source);
        println!(
            "source artifact revision: {}",
            segment
                .source_revision
                .as_ref()
                .map(ToString::to_string)
                .as_deref()
                .unwrap_or("none")
        );
        println!(
            "tokens: {}  in-chat depth: {}  order: {}",
            segment.token_count,
            segment
                .in_chat_depth
                .map(|depth| depth.to_string())
                .as_deref()
                .unwrap_or("none"),
            segment.in_chat_order
        );
        println!(
            "truncation priority: {}  pruned: {}",
            segment.truncation_priority, segment.pruned
        );
        println!("\nRaw authored content:\n{}", segment.raw_content);
        println!("\nRendered content:\n{}", segment.content);
        println!(
            "\nMacro evaluations:\n{}",
            serde_json::to_string_pretty(&detail.transformations.macro_evaluations)?
        );
        println!(
            "Regex applications:\n{}",
            serde_json::to_string_pretty(&detail.transformations.regex_applications)?
        );
        println!(
            "State mutations:\n{}",
            serde_json::to_string_pretty(&detail.transformations.state_mutations)?
        );
    }
    Ok(())
}

fn emit_prompt_diff(output: OutputFormat, command: &str, diff: &PromptDiff) -> Result<()> {
    if matches!(output, OutputFormat::Json) {
        return emit(output, command, diff);
    }
    println!(
        "Prompt diff {} -> {}",
        diff.baseline_attempt_id, diff.target_attempt_id
    );
    println!(
        "tokens  kept {:+}  pruned {:+}  total {:+}",
        diff.token_delta.kept_tokens, diff.token_delta.pruned_tokens, diff.token_delta.total_tokens
    );
    for segment in &diff.segments {
        let labels = segment
            .changes
            .iter()
            .map(|change| match change {
                PromptSegmentChange::Added => "added",
                PromptSegmentChange::Removed => "removed",
                PromptSegmentChange::Reordered => "reordered",
                PromptSegmentChange::PruningStatusChanged => "pruning-status-changed",
                PromptSegmentChange::TextModified => "text-modified",
                PromptSegmentChange::TokenCountChanged => "token-count-changed",
                PromptSegmentChange::MetadataModified => "metadata-modified",
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "@@ {} [{:?} -> {:?}] ({labels}) @@",
            segment.source, segment.baseline_index, segment.target_index
        );
        if segment.changes.contains(&PromptSegmentChange::Added) {
            println!("\x1b[32m+ segment added\x1b[0m");
        } else if segment.changes.contains(&PromptSegmentChange::Removed) {
            println!("\x1b[31m- segment removed\x1b[0m");
        }
        let Some(text_diff) = &segment.text_diff else {
            continue;
        };
        for change in &text_diff.line {
            match change.kind {
                PromptTextChangeKind::Equal => print_prefixed_lines(" ", &change.value, None),
                PromptTextChangeKind::Insert => {
                    print_prefixed_lines("+", &change.value, Some("\x1b[32m"))
                }
                PromptTextChangeKind::Delete => {
                    print_prefixed_lines("-", &change.value, Some("\x1b[31m"))
                }
            }
        }
        print!("  words: ");
        for change in &text_diff.word {
            match change.kind {
                PromptTextChangeKind::Equal => print!("{}", change.value),
                PromptTextChangeKind::Insert => {
                    print!("\x1b[32m{{+{}+}}\x1b[0m", change.value)
                }
                PromptTextChangeKind::Delete => {
                    print!("\x1b[31m[-{}-]\x1b[0m", change.value)
                }
            }
        }
        println!();
    }
    Ok(())
}

fn print_prefixed_lines(prefix: &str, value: &str, color: Option<&str>) {
    for line in value.split_inclusive('\n') {
        if let Some(color) = color {
            print!("{color}{prefix}{line}\x1b[0m");
        } else {
            print!("{prefix}{line}");
        }
        if !line.ends_with('\n') {
            println!();
        }
    }
    if value.is_empty() {
        println!("{prefix}");
    }
}

fn emit_provider_event(output: OutputFormat, event: &ProviderEvent) {
    match output {
        OutputFormat::Json => {
            let event_type = match event {
                ProviderEvent::Started => "provider.started",
                ProviderEvent::TextDelta { .. } => "provider.text-delta",
                ProviderEvent::ReasoningDelta { .. } => "provider.reasoning-delta",
                ProviderEvent::Usage { .. } => "provider.usage",
                ProviderEvent::Completed => "provider.completed",
            };
            let envelope = CliStreamEvent {
                schema: "stcli.cli-event/v1",
                event_id: EntityId::new(),
                event_type,
                data: event,
            };
            println!(
                "{}",
                serde_json::to_string(&envelope).expect("provider event is serializable")
            );
            io::stdout().flush().expect("stdout flush succeeds");
        }
        OutputFormat::Human => match event {
            ProviderEvent::TextDelta { text } => {
                print!("{text}");
                io::stdout().flush().expect("stdout flush succeeds");
            }
            ProviderEvent::Completed => println!(),
            ProviderEvent::Started
            | ProviderEvent::ReasoningDelta { .. }
            | ProviderEvent::Usage { .. } => {}
        },
    }
}

async fn verify(output: OutputFormat, args: VerifyArgs) -> Result<()> {
    let report = verify_fixture_suite(&args.profile, &args.fixtures)?;
    let report = provider_test::verify_provider_request_parity(&args.fixtures, report).await?;
    emit(output, "compat.verify", &report)?;
    if report.is_success() {
        Ok(())
    } else {
        anyhow::bail!("compatibility fixtures failed")
    }
}

fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to read {}", path.display()))?;
    anyhow::ensure!(
        metadata.len() <= limit as u64,
        "{} exceeds {} byte limit ({} bytes)",
        path.display(),
        limit,
        metadata.len(),
    );
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn open_engine() -> Result<StcliEngine> {
    let paths = AppPaths::discover()?;
    paths.ensure_exists()?;
    Ok(StcliEngine::new(paths.database()))
}

fn emit_engine_result(output: OutputFormat, command: &str, result: &EngineResult) -> Result<()> {
    match result {
        EngineResult::InstalledPlugin(data) => emit(output, command, data),
        EngineResult::PluginRemoval(data) => emit(output, command, data),
        EngineResult::ArtifactBundle {
            primary,
            supplementary_artifacts,
            asset_count,
        } => emit(
            output,
            command,
            &ArtifactBundle {
                primary: primary.clone(),
                supplementary_artifacts: supplementary_artifacts.clone(),
                asset_count: *asset_count,
            },
        ),
        EngineResult::CreatedSession(data) | EngineResult::DuplicatedSession(data) => {
            emit(output, command, data)
        }
        EngineResult::Session(data) => emit(output, command, data),
        EngineResult::Purge(data) => emit(output, command, data),
        EngineResult::Compaction(data) => emit(output, command, data),
        EngineResult::Recovery(data) => emit(output, command, data),
        EngineResult::Rebuild(data) => emit(output, command, data),
        EngineResult::DeletedBranch(data) => emit(output, command, data),
        EngineResult::Candidate(data) => emit(output, command, data),
        EngineResult::DeletedCandidate(data) => emit(output, command, data),
        EngineResult::DeletedTurn(data) => emit(output, command, data),
        EngineResult::ImportedCapsule(data) => emit(output, command, data),
        EngineResult::PluginCommand(data) => emit(output, command, data),
        EngineResult::CompletedTurn(data) => emit(output, command, data),
        EngineResult::Turn(data) => emit(output, command, data),
        EngineResult::Attempt(data) => emit(output, command, data),
        EngineResult::Branch(data) => emit(output, command, data),
        EngineResult::Configuration(data) => emit(output, command, data),
        EngineResult::EditedCandidate(data) => emit(output, command, data),
        EngineResult::DryRun(data) => emit(output, command, data),
        EngineResult::Stscript(data) => emit(output, command, data),
    }
}

fn emit(output: OutputFormat, command: &str, data: &impl Serialize) -> Result<()> {
    match output {
        OutputFormat::Human => println!("{}", serde_json::to_string_pretty(data)?),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string(&CliEnvelope::success(command, data))?
        ),
    }
    Ok(())
}

fn emit_error(output: OutputFormat, command: &str, error: &anyhow::Error) {
    match output {
        OutputFormat::Human => eprintln!("error: {error:#}"),
        OutputFormat::Json => {
            let envelope = CliEnvelope::<Value>::failure(
                command,
                CliError {
                    code: "command_failed".to_owned(),
                    message: error.to_string(),
                    details: None,
                },
            );
            eprintln!(
                "{}",
                serde_json::to_string(&envelope).expect("error envelope is serializable")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01M0ZVXKJ3GN413FMVXVAGGT37";
    const HASH: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn primary_resource_targets_are_positional() {
        let cases: &[&[&str]] = &[
            &["stcli", "session", "update", ID, "--character", HASH],
            &["stcli", "session", "greeting", "--session", ID, ID, "1"],
            &["stcli", "message", "retry", "--turn", ID, ID],
            &["stcli", "message", "continue", ID],
            &["stcli", "message", "regenerate", ID],
            &["stcli", "message", "swipe", ID],
            &["stcli", "message", "edit-user", ID, "replacement"],
            &["stcli", "message", "edit-candidate", ID, "replacement"],
            &["stcli", "message", "cancel", ID],
            &["stcli", "message", "turns", ID],
            &["stcli", "turn", "inspect", "--session", ID, ID],
            &[
                "stcli",
                "turn",
                "export",
                "--session",
                ID,
                ID,
                "--file",
                "capsule.json",
            ],
            &["stcli", "turn", "rerun", "--session", ID, ID],
            &["stcli", "turn", "hide", ID],
            &["stcli", "turn", "delete", ID],
            &["stcli", "branch", "delete", ID],
            &["stcli", "candidate", "hide", ID],
            &["stcli", "candidate", "delete", ID],
            &["stcli", "session", "compact", ID],
            &["stcli", "prompt", "inspect", ID],
            &["stcli", "prompt", "inspect", ID, "--diff-prev"],
            &["stcli", "prompt", "diff", ID, ID],
        ];

        for args in cases {
            assert!(
                Cli::try_parse_from(*args).is_ok(),
                "failed to parse {}",
                args.join(" ")
            );
        }
    }
}
