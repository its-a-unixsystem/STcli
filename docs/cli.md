# CLI argument conventions

This document is the canonical syntax reference for `stcli` arguments. Runtime behavior and domain terminology are defined by [`CONTEXT.md`](../CONTEXT.md); each command also exposes its current parser-generated syntax through `stcli <command> <subcommand> --help`.

## Syntax rules

- A command's primary resource target and required payloads are positional arguments.
- Resource identifiers used only as context or scope are named options. For example, `turn inspect` targets an Attempt positionally and takes its containing Session through `--session`.
- Optional selectors, modifiers, output destinations, and configuration values are named options unless a command syntax below explicitly defines them as positional.
- Boolean switches such as `--dry-run`, `--thin`, and `--redact-content` are false when absent and true when present.
- `--provider-stream` is a boolean value option, not a switch: use `--provider-stream true` or `--provider-stream false`.
- Repeatable options are supplied once per value, for example `--lorebook <revision> --lorebook <revision>`.
- STcli does not provide positional/named aliases for the same argument. Scripts should use the canonical form shown here.
- Named options may be interleaved with positional arguments, but scripts should keep required context options before positionals for readability.

Notation used below:

- `<VALUE>`: required positional argument.
- `--name <VALUE>`: required named option.
- `[--name <VALUE>]`: optional named option.
- `[--switch]`: optional boolean switch.
- `...`: the preceding option is repeatable.

## Value formats

| Placeholder | Expected value |
| --- | --- |
| `<session>`, `<branch>`, `<turn>`, `<attempt>`, `<candidate>` | A 26-character ULID emitted by STcli. |
| `<revision>`, `<digest>` | `sha256:` followed by exactly 64 hexadecimal digits. |
| `<path>`, `<directory>`, `<destination>`, `<file>`, `<capsule>` | A filesystem path interpreted by the selected command. |
| `<json>` | One shell argument containing JSON. Quote it to prevent shell expansion. |
| `<text>` | One shell argument. Quote text containing spaces or shell metacharacters. |
| `<url>` | An absolute HTTPS provider base URL. Plain HTTP provider URLs are rejected. |
| `<tokenizer>` | Exactly `tiktoken:cl100k_base` or `tiktoken:o200k_base`. |

## Global option

| Option | Values | Default | Behavior |
| --- | --- | --- | --- |
| `--output <FORMAT>` | `human`, `json` | `human` | Selects command output formatting. This option is global and may appear before or after subcommands. Streaming JSON output is JSONL followed by the final command envelope. |

Clap also generates `-h`/`--help` at each command level. The top-level command provides `-V`/`--version`.

## Terminal UI

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `tui` | `tui [session]` | Opens the interactive Session browser, or opens the supplied Session directly. TUI configuration and keybindings are documented in the root README. |

## Artifact commands

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `artifact import` | `artifact import <path>` | Imports the path as a new immutable Artifact Revision. The command reads the format from the file content. It accepts JSON cards (V1, V2, V3), PNG cards, WebP cards (V2 or V3), and CHARX archives. It returns a bundle with the primary revision, any supplementary revisions (such as bundled lorebooks), and the count of stored media assets. See [Import character cards](guide.md#import-character-cards-from-images-and-archives). |
| `artifact list` | `artifact list` | No command arguments. |
| `artifact show` | `artifact show <revision>` | Targets one Artifact Revision. |
| `artifact export` | `artifact export <revision> <destination>` | Targets one Artifact Revision and writes its exact imported bytes to the required destination path. |

## Provider profile commands

Provider connection profiles are stored in `config.toml` under `[providers.<name>]`. They define HTTPS endpoints, model names, and Credential References or authentication environment variable names.

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `profile list` | `profile list` | Lists all configured provider profiles from `config.toml`. |
| `profile show` | `profile show <name>` | Displays configuration details for the named provider profile. |
| `profile add` | `profile add <name> [--file <path>]` | Adds or updates a provider profile named `<name>`. Reads a JSON or TOML definition from `<path>`, or from standard input if `--file` is omitted or `-`. |
| `profile remove` | `profile remove <name>` | Removes the named provider profile from `config.toml`. |

### Provider profile file format

Provider profiles can be provided in JSON or TOML. Example in JSON:

```json
{
  "id": "openrouter",
  "base_url": "https://openrouter.ai",
  "chat_completions_path": "/api/v1/chat/completions",
  "model": "anthropic/claude-3.5-sonnet",
  "stream": true,
  "timeout_seconds": 120,
  "credential_key": "openrouter"
}
```

Example in TOML:

```toml
id = "openrouter"
base_url = "https://openrouter.ai"
chat_completions_path = "/api/v1/chat/completions"
model = "anthropic/claude-3.5-sonnet"
stream = true
timeout_seconds = 120
credential_key = "openrouter"
```

> **Security rule**: Provider profiles must specify `https://` URLs and may not contain literal passwords or secret API keys in `config.toml`. Use `credential_key` for the platform Credential Store or `api_key_env` for an environment variable.

A provider profile can also select Text Completion mode. Set `format_mode` to `text-completion` and add the `completions_path`, `instruct_template`, and `context_formatting` fields. For the full field list, see [Text Completion prompts](text-completion.md).

## Credential Store commands

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `credentials set` | `credentials set <alias>` | Reads a secret from the terminal without echo and stores it under service `stcli`. |
| `credentials list` | `credentials list` | Audits provider-profile Credential References and reports `configured`, `missing`, or `unavailable`. |
| `credentials delete` | `credentials delete <alias>` | Deletes the named entry from the platform Credential Store. |

See [Configuration](configuration.md#provider-credentials) for precedence and headless-environment guidance.

## Session commands

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `session create` | `session create <configuration-options>` | Creates a Session from the required `--character` revision and the configuration options below. |
| `session update` | `session update <session> <configuration-options>` | Targets the Session positionally. The supplied configuration creates a new Session Configuration Revision. |
| `session duplicate` | `session duplicate <session> [--branch <branch>] [--name <name>] [--up-to <turn>]` | Creates an independent Duplicated Session from one Branch lineage. `--up-to` is inclusive. |
| `session greeting` | `session greeting --session <session> <branch> <greeting>` | Targets the Branch positionally, uses the Session as validation context, and selects the zero-based Greeting index. |
| `session list` | `session list` | No command arguments. |
| `session archive` | `session archive <session>` | Targets one Session. |
| `session purge` | `session purge <session>` | Targets one Session. |
| `session compact` | `session compact <session>` | Physically removes logically deleted Turns, Candidates, and Branches without active descendants. |
| `session recover` | `session recover` | No command arguments. |
| `session show` | `session show <session>` | Targets one Session. |
| `session branches` | `session branches <session>` | Targets one Session. |
| `session rebuild` | `session rebuild` | No command arguments. |

### Duplicate a session

Duplicate the root Branch lineage with an automatically generated name:

```bash
stcli --output json session duplicate <session-id>
```

Use `--branch <branch-id>` to select another lineage, `--up-to <turn-id>` to stop after that Turn, and `--name <name>` to set the new Session name:

```bash
stcli --output json session duplicate <session-id> \
  --branch <branch-id> \
  --up-to <turn-id> \
  --name "Alternate route"
```

The JSON response uses the standard envelope and returns the new Session, root Branch, and reused Session Configuration Revision. This example abbreviates each record to its identifying fields:

```json
{
  "schema": "stcli.cli/v1",
  "command": "session.duplicate",
  "ok": true,
  "data": {
    "session": { "session_id": "<new-session-id>", "custom_name": "Alternate route" },
    "branch": { "branch_id": "<new-root-branch-id>", "session_id": "<new-session-id>" },
    "configuration": { "revision_hash": "<configuration-revision-hash>" }
  }
}
```

### Session configuration options

`session create` and `session update` share these options. `--character` is always required; omitted options use the listed defaults, including during an update.

| Option | Required/repeatable | Default | Behavior |
| --- | --- | --- | --- |
| `--character <revision>` | Required | None | Character-card Artifact Revision. |
| `--persona <text>` | Optional | `User` | Persona name used by future Turns. |
| `--persona-description <text>` | Optional | None | Persona description used by future Turns. Prefix with `@` to read the description from a file relative to the current working directory. |
| `--lorebook <revision>` | Optional, repeatable | Empty | Adds a lorebook Artifact Revision for each occurrence. |
| `--preset <revision>` | Optional | None | Chat Completion preset Artifact Revision. |
| `--provider-profile <name>` | Optional | None | Named provider connection profile from `config.toml`. Explicit CLI flags override profile fields. |
| `--provider <name>` | Optional | `default` | Provider configuration name. |
| `--provider-base-url <url>` | Optional | `https://127.0.0.1:3443` | HTTPS provider base URL. |
| `--provider-chat-path <path>` | Optional | `/v1/chat/completions` | Chat Completions path joined to the provider base URL. |
| `--provider-api-key-env <name>` | Optional | None | Name of the environment variable containing the API key; the key itself must not be passed here. |
| `--provider-ca-certificate <path>` | Optional | None | PEM certificate path for a private provider endpoint. |
| `--provider-timeout <seconds>` | Optional | `120` | Provider request timeout in seconds. |
| `--model <name>` | Optional | `fixture-model` | Provider model name. |
| `--provider-stream <bool>` | Optional | `true` | Enables or disables provider streaming; requires an explicit `true` or `false` value. |
| `--tokenizer <tokenizer>` | Optional | `tiktoken:o200k_base` | Tokenizer identifier used for prompt budgeting. |
| `--grant-script <digest>` | Optional, repeatable | Empty | Authorizes execution of a regex transformation script by its SHA-256 digest (alias: `--grant-preset-script`). Ungranted scripts remain inert. |
| `--generation-settings <json>` | Optional | `{}` | Provider and engine generation settings as a JSON object. See [Generation settings JSON fields](#generation-settings-json-fields). |
| `--greeting <index>` | Optional | `0` | Initial zero-based Greeting index. |
| `--compatibility-profile <name>` | Optional | `sillytavern-1.18-core` | Versioned Compatibility Profile. |

#### Generation settings JSON fields

`--generation-settings` accepts a JSON object that configures generation parameters at the Session Configuration level. Settings defined here take top precedence, overriding any values from the selected `--preset` and Compatibility Profile defaults.

##### Provider parameters

These fields are forwarded directly in the JSON request payload to the model provider:

| Parameter | Type | Default | Behavior |
| --- | --- | --- | --- |
| `temperature` | number | None | Sampling temperature (e.g. `0.7`). Forwarded to provider when set. |
| `top_p` | number | None | Nucleus sampling probability threshold (e.g. `0.9`). Forwarded when set. |
| `top_k` | integer | None | Top-k sampling limit (e.g. `40`). Forwarded when set. |
| `min_p` | number | None | Minimum probability threshold (e.g. `0.05`). Stripped if `0.0`. |
| `frequency_penalty` | number | None | Frequency penalty factor (e.g. `0.5`). Forwarded when set. |
| `presence_penalty` | number | None | Presence penalty factor (e.g. `0.5`). Forwarded when set. |
| `repetition_penalty` | number | None | Repetition penalty factor (e.g. `1.1`). Forwarded when set. |
| `reasoning_effort` | string | None | Provider reasoning effort level (e.g. `"low"`, `"medium"`, `"high"`). |
| `seed` | integer | None | Random seed for deterministic generation. Stripped if `-1`. |
| `n` | integer | None | Number of completions to request. Stripped if `1`. |
| `max_tokens` | integer | `512` | Maximum generation tokens. Overrides preset `openai_max_tokens` and profile default (`512`). |

Additional unreserved keys in the JSON object are passed through to the provider request payload.

> Note: Provider model and streaming are not set in `--generation-settings`; they are strictly owned by [`--model`](#session-configuration-options) and [`--provider-stream`](#session-configuration-options).

##### Assembly-only parameters

These fields are consumed by the STcli engine during prompt preparation and budgeting, and are withheld from the provider payload:

| Parameter | Type | Default | Behavior |
| --- | --- | --- | --- |
| `max_context` | integer | `8192` | Total context window limit used for token budgeting (`prompt_limit = max_context - max_tokens`). Overrides preset `openai_max_context` and profile default (`8192`). |
| `squash_system_messages` | boolean | `false` | When `true`, merges consecutive system messages with newlines. |
| `use_sysprompt` | boolean | `true` | When `false`, suppresses the `main` system prompt slot. |
| `assistant_prefill` | string | None | Prefill text appended as a trailing assistant message. |
| `continue_prefill` | boolean | `false` | When `true`, appends parent candidate content as continuation prefill for `message continue`. |
| `continue_nudge_prompt` | string | None | Preserved for continuation assembly. |
| `max_context_unlocked` | boolean | None | Retained for context window calculation. |
| `names_behavior` | integer | None | Retained for role/name formatting behavior. |

For resolution details, provenance tracking, and full preset field classifications, see [Chat Completion Presets](presets.md#2-effective-generation-settings-and-precedence).

## Message commands

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `message send` | `message send --session <session> [--branch <branch>] [--dry-run] <text>` | The text is the payload. Session and optional Branch are context. `--dry-run` prepares the Turn without creating an Attempt, calling the provider, or committing state. |
| `message retry` | `message retry --turn <turn> <attempt>` | Targets the Attempt positionally and uses its Turn as context. |
| `message continue` | `message continue <turn> [--dry-run]` | Targets the Turn. |
| `message regenerate` | `message regenerate <turn> [--dry-run]` | Targets the Turn. |
| `message swipe` | `message swipe <turn> [--dry-run | --candidate <candidate>]` | Targets the Turn. Omit both options to generate and select a new Candidate; provide `--dry-run` to prepare the swipe without calling the provider or committing state; provide `--candidate` to select an existing Candidate. |
| `message edit-user` | `message edit-user <turn> <text>` | Targets the Turn and supplies replacement user text. |
| `message edit-candidate` | `message edit-candidate <candidate> <text>` | Targets the Candidate and supplies replacement assistant text. |
| `message cancel` | `message cancel <attempt>` | Targets the Generation Attempt. |
| `message turns` | `message turns <branch>` | Targets the Branch whose Turns are listed. |

## Turn commands

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `turn inspect` | `turn inspect --session <session> <attempt>` | Targets the Attempt positionally and uses its Session as context. |
| `turn export` | `turn export --session <session> <attempt> --file <file> [--thin] [--redact-content]` | Targets the Attempt, uses its Session as context, and requires an output file. `--thin` references stored content; `--redact-content` removes narrative and provider content and disables unsupported Replay/Rerun capabilities. |
| `turn replay` | `turn replay <capsule>` | Replays the capsule offline without provider calls or Plugin execution. |
| `turn import` | `turn import <capsule>` | Validates and imports the capsule as an isolated Imported Session. |
| `turn rerun` | `turn rerun --session <session> <attempt> [--dry-run]` | Targets the recorded Attempt and uses its Session as context. A live Rerun is a new Generation Attempt; `--dry-run` only prepares it. |
| `turn hide` | `turn hide <turn>` | Toggles the hidden state of a Turn, excluding it from future prompt assembly while preserving it in session projections. |
| `turn delete` | `turn delete <turn>` | Appends a Logical Deletion tombstone for the Turn. Compaction permanently removes it if unreferenced. |

## Branch command

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `branch delete` | `branch delete <branch>` | Appends a Logical Deletion tombstone for the Branch. Root branches cannot be deleted. |

## Candidate commands

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `candidate hide` | `candidate hide <candidate>` | Toggles the hidden state of a Candidate. |
| `candidate delete` | `candidate delete <candidate>` | Appends a Logical Deletion tombstone for the Candidate. Automatically advances active selection to an adjacent Candidate if available. |

## Plugin commands

These commands work the same for Wasm plugins and for script plugins. The manifest `runtime` field selects the runtime. For the manifest and the script API, see [Writing plugins](plugins.md).

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `plugin doctor` | `plugin doctor <directory>` | Validates a Plugin package directory without installing it. |
| `plugin install` | `plugin install <directory>` | Installs a Plugin package directory. |
| `plugin list` | `plugin list` | No command arguments. |
| `plugin restore-defaults` | `plugin restore-defaults` | Clears default-Plugin opt-outs and restores the embedded offline defaults. |
| `plugin inspect` | `plugin inspect <id>` | Targets a Plugin ID. |
| `plugin adopt` | `plugin adopt --session <session> <id> --version <version> --digest <digest> [--capability <name>]... [--settings <json>] [--egress-domain <host>]...` | Targets a Plugin ID, uses the Session as context, pins version and digest, grants each repeated capability, and adds each repeated domain to the egress allow-list (requires the `brokered-egress` capability to take effect). Settings default to `{}`. |
| `plugin upgrade` | `plugin upgrade --session <session> <id> --version <version> --digest <digest>` | Targets a Plugin ID and pins its replacement version and digest for the Session. |
| `plugin invoke` | `plugin invoke --session <session> <id> <command> [--arguments <json>]` | Targets a Plugin ID, invokes its positional command name, and passes JSON arguments. Arguments default to `null`. |
| `plugin enable` | `plugin enable --session <session> <id>` | Targets a Plugin ID in the Session context. |
| `plugin disable` | `plugin disable --session <session> <id>` | Targets a Plugin ID in the Session context. |
| `plugin remove` | `plugin remove <id>` | Removes an installed Plugin ID. Removing a default Plugin persists an opt-out until `plugin restore-defaults`. |

## Extension commands

These commands import SillyTavern-native Extension directories into the normalized `st-bridge`
Plugin runtime. Git clone and fetch remain frontend responsibilities.

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `extension import` | `extension import <directory>` | Imports one local native Extension directory, installs its content-addressed normalized package, and returns the installed Plugin plus non-blocking import warnings. |
| `extension adopt` | `extension adopt --session <session> <id> --version <version> --digest <digest> [--settings <json>] [--egress-domain <host>]...` | Pins the Extension to a new Session Configuration Revision and grants the fixed bridge capability tier. Settings default to `{}`. The egress allow-list defaults empty and contains only explicitly repeated domains. |

## Prompt command

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `prompt inspect` | `prompt inspect <attempt> [--diff-prev \| --segment <selector>]` | Targets the Generation Attempt whose recorded Prompt Plan is inspected. `--diff-prev` compares it with the preceding Turn's selected Candidate's Generation Attempt (fails on an initial Turn). `--segment <selector>` filters inspection to matching segment(s) by exact slot identifier (e.g. `charDescription`, `personaDescription`, `main`) or 0-based numeric index, reporting raw and rendered content with correlated transformation metadata. `--diff-prev` and `--segment` are mutually exclusive. |
| `prompt diff` | `prompt diff <baseline-attempt> <target-attempt>` | Compares two Generation Attempts. Human output itemizes structural changes, line and word text changes, and token deltas. `--output json` returns the structured diff envelope. |

## Compatibility command

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `compat verify` | `compat verify [--profile <file>] [--fixtures <directory>]` | `--profile` defaults to `compat/profiles/sillytavern-1.18-core.json`; `--fixtures` defaults to `compat/fixtures`. Oracle inputs default to the digest-pinned files under `compat/external/`. `STCLI_NANOBEAR_PRESET` and `STCLI_NANOBEAR_ORACLE` optionally override them for re-recording. Missing files, unresolved sources, and digest mismatches fail verification. |

## Provider test command

| Command | Canonical syntax | Argument behavior |
| --- | --- | --- |
| `provider-test serve` | `provider-test serve [--bind <address>] [--certificate-output <file>]` | `--bind` accepts a socket address and defaults to `127.0.0.1:3443`. `--certificate-output` optionally writes the generated test certificate. |

`internal regex-worker` is a hidden implementation command with no user-facing arguments. It is not a stable scripting interface.
