# Usage guide

This guide shows how to do the common tasks with STcli. It uses the development binary through Cargo and the sample artifacts in [`examples/`](../examples/). For the [`stcli`](cli.md) binary, replace `cargo run --quiet --bin stcli --` with the path to the built binary.

Replace placeholders such as `<character-revision>`, `<session-id>`, and `<branch-id>` with the IDs that earlier commands return. For the term behind each capitalized word (Turn, Candidate, Branch, Capsule), see [`CONTEXT.md`](../CONTEXT.md). For the exact syntax of every command, see [`docs/cli.md`](cli.md).

## Contents

- [Import content](#import-content)
- [Configure a provider and create a session](#configure-a-provider-and-create-a-session)
- [Preview and send a turn](#preview-and-send-a-turn)
- [Inspect a prompt](#inspect-a-prompt)
- [Generate alternatives](#generate-alternatives)
- [Edit history](#edit-history)
- [Update a session](#update-a-session)
- [Select a greeting](#select-a-greeting)
- [Export and replay capsules](#export-and-replay-capsules)
- [Archive and purge](#archive-and-purge)
- [Install and adopt a plugin](#install-and-adopt-a-plugin)
- [Output formats](#output-formats)
- [Security and privacy](#security-and-privacy)

## Import content

Import a JSON character card as an immutable Artifact Revision:

```bash
cargo run --quiet --bin stcli -- --output json artifact import ./examples/character.json
```

Lorebooks and Chat Completion presets use the same command:

```bash
cargo run --quiet --bin stcli -- --output json artifact import ./examples/lorebook.json
cargo run --quiet --bin stcli -- --output json artifact import ./examples/preset.json
```

List the imported revisions to get their content hashes:

```bash
cargo run --quiet --bin stcli -- artifact list
```

For a field-by-field breakdown of each sample artifact, see [`examples/README.md`](../examples/README.md).

## Configure a provider and create a session

Keep API keys in environment variables. Pass the variable name, not the key:

```bash
export OPENAI_API_KEY="your-provider-key"
```

Create a session using individual provider flags:

```bash
cargo run --quiet --bin stcli -- --output json session create \
  --character <character-revision> \
  --persona "User" \
  --provider-base-url "https://api.example.com" \
  --provider-chat-path "/v1/chat/completions" \
  --provider-api-key-env OPENAI_API_KEY \
  --model "model-name" \
  --tokenizer "tiktoken:o200k_base" \
  --generation-settings '{"temperature":0.8,"max_tokens":512}'
```

Alternatively, use a named profile defined in `config.toml` (under `[providers.<name>]`):

```bash
cargo run --quiet --bin stcli -- --output json session create \
  --character <character-revision> \
  --provider-profile openrouter
```

Any explicit CLI flags provided alongside `--provider-profile` (e.g. `--model "different-model"`) override individual fields of the profile.

If the imported character card or preset bundles regex transformation scripts, they are discovered during session inspection and turn preparation. Authorize execution by passing `--grant-script <digest>` (repeatable). Ungranted scripts emit non-blocking warnings and remain inert.

Add repeatable `--lorebook <revision>` options or a `--preset <revision>` option when needed. Use `--provider-ca-certificate <pem-file>` for a private HTTPS test endpoint. The result includes the session ID and root branch ID.

## Preview and send a turn

A dry run builds the same Prompt Plan and provider request as a live send. It does not create a turn, call the provider, or commit variable changes:

```bash
cargo run --quiet --bin stcli -- --output json message send \
  --session <session-id> \
  --branch <branch-id> \
  --dry-run \
  "Open the library door."
```

Send the turn for real by removing `--dry-run`:

```bash
cargo run --quiet --bin stcli -- --output json message send \
  --session <session-id> \
  --branch <branch-id> \
  "Open the library door."
```

With streaming enabled, JSON output emits typed JSONL provider events before the final command envelope.

## Inspect a prompt

Show the ordered segments, macro evaluations, lore decisions, token counts, and pruning results of a recorded Prompt Plan:

```bash
cargo run --quiet --bin stcli -- --output json prompt inspect <attempt-id>
```

Filter inspection down to a single prompt segment to inspect its raw authored content versus final rendered text alongside correlated macro, regex, and state mutations:

```bash
# Filter by exact slot identifier:
cargo run --quiet --bin stcli -- prompt inspect <attempt-id> --segment char_persona

# Filter by 0-based prompt segment index:
cargo run --quiet --bin stcli -- prompt inspect <attempt-id> --segment 3
```

Compare a generation attempt against the previous Turn's attempt to inspect additions, evictions, text edits, and token changes:

```bash
cargo run --quiet --bin stcli -- prompt inspect <attempt-id> --diff-prev
```

Or compute a structural and textual diff between any two arbitrary attempts:

```bash
cargo run --quiet --bin stcli -- prompt diff <baseline-attempt-id> <target-attempt-id>
```

List the turns on a branch:

```bash
cargo run --quiet --bin stcli -- --output json message turns <branch-id>
```

## Generate alternatives

Generate another response for a turn:

```bash
cargo run --quiet --bin stcli -- message regenerate <turn-id>
cargo run --quiet --bin stcli -- message swipe <turn-id>
cargo run --quiet --bin stcli -- message continue <turn-id>
```

Add `--dry-run` to preview any alternative with no provider call and no committed state:

```bash
cargo run --quiet --bin stcli -- message regenerate <turn-id> --dry-run
cargo run --quiet --bin stcli -- message swipe <turn-id> --dry-run
cargo run --quiet --bin stcli -- message continue <turn-id> --dry-run
```

Select an existing Candidate as the active swipe:

```bash
cargo run --quiet --bin stcli -- message swipe <turn-id> --candidate <candidate-id>
```

## Edit history

Editing creates a new branch and leaves the original unchanged:

```bash
cargo run --quiet --bin stcli -- message edit-user <turn-id> "Rewritten user action"
cargo run --quiet --bin stcli -- message edit-candidate <candidate-id> "Manually authored assistant response"
```

## Update a session

A session update creates a new immutable Session Configuration Revision for future attempts. It does not rewrite prior attempts or create a branch:

```bash
cargo run --quiet --bin stcli -- session update <session-id> \
  --character <character-revision> \
  --persona "Updated persona" \
  --provider-base-url "https://api.example.com" \
  --provider-api-key-env OPENAI_API_KEY \
  --model "model-name" \
  --tokenizer "tiktoken:o200k_base" \
  --generation-settings '{"temperature":0.7,"max_tokens":512}'
```

You can also switch provider connection profiles or grant additional regex transformation scripts on an existing session:

```bash
cargo run --quiet --bin stcli -- session update <session-id> \
  --character <character-revision> \
  --provider-profile local-ollama \
  --grant-script <script-digest>
```

## Select a greeting

```bash
cargo run --quiet --bin stcli -- session greeting \
  --session <session-id> \
  <branch-id> \
  1
```

Before the first turn, this updates the branch. After a turn exists, the engine creates a new branch from the session root and leaves the original branch unchanged.

## Export and replay capsules

Export a self-contained Portable Capsule:

```bash
cargo run --quiet --bin stcli -- turn export \
  --session <session-id> \
  <attempt-id> \
  --file turn-capsule.json
```

Add `--thin` to reference content already in the local store. Add `--redact-content` to remove narrative and provider content. The engine then disables Replay and Rerun capabilities rather than make a false claim.

Replay does no provider call and runs no plugin:

```bash
cargo run --quiet --bin stcli -- turn replay turn-capsule.json
```

Import validates the complete Portable Capsule before it creates an isolated Imported Session:

```bash
cargo run --quiet --bin stcli -- turn import turn-capsule.json
```

Rerun is a new live Generation Attempt, not deterministic Replay:

```bash
cargo run --quiet --bin stcli -- turn rerun \
  --session <session-id> \
  <attempt-id>
```

## Archive and purge

Archive retains the authoritative Turn Trace. Purge physically removes the session and collects only content that no session or capsule still references:

```bash
cargo run --quiet --bin stcli -- session archive <session-id>
cargo run --quiet --bin stcli -- session purge <session-id>
cargo run --quiet --bin stcli -- session recover
```

## Install and adopt a plugin

Plugins are pure, capability-limited Wasm modules. They contribute declarative behavior to the engine without direct access to engine state. For the plugin design and the capability model, see [`ARCHITECTURE.md`](../ARCHITECTURE.md#plugin-system) and [ADR 0003](adr/0003-pure-wasm-plugins.md).

Validate and install a local Component Model package that contains `manifest.json` and `component.wasm`:

```bash
cargo run --quiet --bin stcli -- --output json plugin doctor ./plugins/proof
cargo run --quiet --bin stcli -- --output json plugin install ./plugins/proof
cargo run --quiet --bin stcli -- --output json plugin list
```

Adoption creates a new immutable Session Configuration Revision. The digest and each granted capability are explicit:

```bash
cargo run --quiet --bin stcli -- --output json plugin adopt \
  --session <session-id> \
  --version <plugin-version> \
  --digest sha256:<component-digest> \
  --capability register-macro \
  --capability register-command \
  --capability contribute-prompt \
  --capability write-own-state \
  org.stcli.proof
```

Use `plugin inspect`, `plugin upgrade`, `plugin enable`, and `plugin disable` for lifecycle management. An upgrade keeps the session's grants and settings. It pins the explicit replacement version and digest in a new Session Configuration Revision. Invoke a registered command with `plugin invoke`. Its declarative effects and namespaced state changes enter the authoritative trace. `plugin remove` refuses a plugin that any stored Session Configuration Revision references.

Plugins receive canonical JSON input and return declarative effects through [`wit/plugin.wit`](../wit/plugin.wit). The host links no WASI or other imports. A plugin gets no network, filesystem, provider, secret, subprocess, or native-library access. [`schemas/plugin-manifest.schema.json`](../schemas/plugin-manifest.schema.json) defines the public manifest format.

## Output formats

Human-readable output is the default. Use `--output json` for the stable, versioned command envelope:

```json
{
  "schema": "stcli.cli/v1",
  "ok": true,
  "command": "artifact.list",
  "data": [],
  "error": null,
  "warnings": []
}
```

Streaming provider events use the `stcli.cli-event/v1` JSONL schema. All schemas are in [`schemas/`](../schemas/).

## Security and privacy

- The engine sends network requests only to the HTTPS provider you configure.
- Refer to API keys and secret-valued headers through environment variables.
- Resolved secrets do not enter SQLite, the Turn Trace, receipts, or CLI output.
- Provider error bodies and streamed data pass through request-local secret redaction before storage or display.
- The engine rejects URL userinfo.
- On Unix, the engine creates its directories with mode `0700` and the SQLite database with mode `0600`.
- ECMAScript lore regex runs in a subprocess with input limits and a timeout.

CAUTION: Do not put secrets directly in `--generation-settings` or literal configuration fields. Use environment variable references instead.
