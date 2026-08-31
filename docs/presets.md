# Chat Completion Presets

> Part of the [STcli documentation](README.md). For everyday tasks, see the [usage guide](guide.md). For command syntax, see the [CLI reference](cli.md).

STcli imports, resolves, and executes SillyTavern Chat Completion prompt presets while preserving exact source bytes and enforcing bounded compatibility under the [`sillytavern-1.18-core`](../compat/profiles/sillytavern-1.18-core.json) profile.

This document describes how STcli processes complex presets, resolves generation settings, executes prompt assembly, reports compatibility diagnostics, and verifies oracle parity against real-world presets such as the Nemo Engine.

---

## 1. Preset Lifecycle in STcli

1. **Import**: Presets are imported via [`stcli artifact import <file>`](cli.md#artifact-commands) as immutable Artifact Revisions. Unknown JSON members, order profiles, embedded regex scripts, and directive comments are preserved byte-for-byte.
2. **Duplicate**: In the TUI preset picker, `c` opens a focused clone form for the name, temperature, maximum context tokens, maximum response tokens, and `use_sysprompt`. Saving imports a new immutable Artifact Revision and keeps the source revision unchanged.
3. **Selection**: A Session selects a preset revision via `--preset <revision>` in [`session create`](cli.md#session-commands) or [`session update`](cli.md#session-commands), creating an immutable Session Configuration Revision.
4. **Turn Preparation**: Before generation or preview, [`Store::prepare_turn`](../crates/stcli-core/src/turn.rs) resolves generation settings, assembles the Prompt Plan, processes macros sequentially, injects in-chat depth prompts, applies assembly behaviors, prunes tokens, and computes the canonical provider request hash.
5. **Dry Run / Execution**: Both Dry Run (`--dry-run`) and live Generation Attempts share the exact same preparation output. Dry Run emits the prepared request, effective settings, and diagnostics without performing provider I/O or state mutations; a live attempt submits the prepared request and commits the effect receipt to the authoritative Turn Trace.

Preset duplication patches only `preset_name`, `temperature`, the `max_context`/`openai_max_context` compatibility pair, `openai_max_tokens`, and `use_sysprompt`. Prompt slots, order profiles such as `character_id: 100001`, extension metadata, and embedded regex script values are copied unchanged. Unchanged script content therefore retains the same canonical script digest and does not require a new Preset Script Grant.
Prompt Order Entry toggles are preset-level edits. From the Chat picker, focusing an order entry and pressing `Space` creates one new immutable, content-addressed Artifact Revision containing the changed `enabled` flag(s), preserving all other preset content. The current session is automatically re-pinned through a new Session Configuration Revision; completed turns and prior Generation Attempts retain their original pinned configuration, and other sessions are unaffected until they explicitly re-select the preset. Reapplying an identical change set deduplicates to the existing revision. Disabled entries are excluded from subsequent prompt assembly and Dry Runs. Structural markers such as `chatHistory` may be disabled permissively, with a non-blocking warning in the TUI.

This intentionally diverges from live SillyTavern, where editing a preset file affects chats using that file immediately: STcli revisions make changes auditable and forward-only. Prompt reordering and prompt content authoring remain out of scope.

---

## 2. Effective Generation Settings and Precedence

Generation settings are resolved field-by-field into [`EffectiveGenerationSettings`](../crates/stcli-core/src/turn.rs), with three-tier precedence:

1. **Session Configuration**: Explicit values supplied via [`--generation-settings`](cli.md#generation-settings-json-fields) or CLI flags take top priority.
2. **Prompt Preset**: Settings declared inside the selected preset JSON apply when not overridden by the Session.
3. **Compatibility Profile Defaults**: Fallback defaults defined by the active profile apply last (e.g. `max_tokens: 512`, `max_context: 8192`, `squash_system_messages: false`, `use_sysprompt: true`, `continue_prefill: false`).

Each resolved setting retains an immutable provenance tag ([`GenerationSettingSource`](../crates/stcli-core/src/turn.rs): `session`, `preset`, or `profile`) inspectable in Dry Run and recorded in Attempt effect receipts.

### Provider Settings vs. Assembly-Only Settings

Settings can also be set or overridden outside of presets at the Session Configuration level via [`--generation-settings`](cli.md#generation-settings-json-fields). Settings are divided into provider parameters and engine assembly parameters:

- **Provider parameters** (`temperature`, `top_p`, `top_k`, `min_p`, `frequency_penalty`, `presence_penalty`, `repetition_penalty`, `reasoning_effort`, `seed`, `n`, `max_tokens` mapped from `openai_max_tokens`): Passed to the model endpoint in the prepared request payload.
  - Normalization: `seed: -1`, `min_p: 0.0`, and `n: 1` are stripped to omit provider defaults.
  - Model and streaming ownership: Provider model (`--model`) and streaming (`--provider-stream`) remain strictly owned by Session/Provider Settings, never by preset values.
- **Assembly-only parameters**: Withheld from the provider request and consumed entirely during prompt preparation:
  - `openai_max_context` / `max_context`: Total context token ceiling for pruning.
  - `squash_system_messages`: Consecutive system message squashing.
  - `use_sysprompt`: Inclusion of the `main` system prompt slot.
  - `assistant_prefill`: API assistant prefill text appended at the end of the message payload.
  - `continue_prefill`: Continuation prefill toggle for `message continue`.
  - `continue_nudge_prompt`, `max_context_unlocked`, `names_behavior`: Preserved for assembly behavior without blind passthrough.

---

## 3. Prompt Ordering and Slot Rules

Presets define prompt ordering in the `prompt_order` array.

### Order Profile Selection (Character ID 100001)

SillyTavern Chat Completion presets maintain two order configurations:
- `character_id: 100000`: Text Completion / Instruct order.
- `character_id: 100001`: Chat Completion order.

STcli explicitly selects `character_id: 100001`. If absent, STcli falls back to the first available order profile and emits a `prompt-order-profile-fallback` Compatibility Warning.

### Native Slot Fallback Suppression

Native SillyTavern slots include:
`main`, `charDescription`, `charPersonality`, `scenario`, `personaDescription`, `dialogueExamples`, `nsfw`, `jailbreak`, `worldInfoBefore`, `worldInfoAfter`, `chatHistory`, `userInput`, `enhanceDefinitions`.

If a native slot is present in `prompt_order` but marked `enabled: false`, STcli suppresses it completely; it will never re-insert it as an unordered fallback.

### Live User Input and Chat History Semantics

- **Active `chatHistory` with disabled/omitted `userInput`**: The live user message is appended to the end of the `chatHistory` message sequence. This ensures current user input reaches the provider exactly once without duplicating or losing it.
- **Active `userInput`**: The current user input is placed in the dedicated `userInput` slot.
- **Generation Types and History**:
  - `normal`: Assembles prior Branch turns followed by the live user message.
  - `continue`: Assembles prior Branch turns and uses the selected parent candidate as the continuation prefix (no new user message).
  - `regenerate` and `swipe`: Excludes the replaced Turn from history so the turn's previous response does not bleed into the new attempt.

---

## 4. In-Chat Depth Injections

Custom prompts configured with `injection_position: 1` (`in_chat`) are placed at relative depths from the conversation end:

- **Depth Indexing**: Injected at `top_level.len().saturating_sub(injection_depth)` from the bottom of assembled messages.
- **Multi-Segment Splicing**: Multiple injections targeting the same depth are sorted by `injection_order` (and identifier tiebreaker) and spliced into the message stream together.
- **Empty-Rendering Prompts**: If an in-chat or top-level custom prompt renders to empty or whitespace (for example, a prompt whose purpose is executing `{{setvar::...}}` macros), no empty message is sent to the provider. Crucially, its state mutations and macro evaluations are fully preserved.

---

## 5. Assembly Behaviors

### System Message Squashing (`squash_system_messages`)

When `squash_system_messages: true`:
- Runs of consecutive `system` messages are merged with a single newline `\n`.
- Boundaries between `system`, `user`, and `assistant` are never crossed.
- Token counts are updated for squashed blocks, ensuring that token budgeting accurately reflects the submitted wire payload.

### System Prompt Control (`use_sysprompt`)

When `use_sysprompt: false`, the `main` system prompt segment is removed from the assembled prompt segments before token budgeting.

### Assistant and Continuation Prefill

- **`assistant_prefill`**: If non-empty, an `assistant` message containing the configured prefill is appended at the very end of the request message list for `normal`, `regenerate`, and `swipe` generations.
- **`continue_prefill`**: When `continue_prefill: true`, `message continue` appends the parent candidate text as the trailing `assistant` message.

### Token Budgeting

Prompt pruning calculates available space as `prompt_limit = max_context - max_tokens`.
Pruning evaluates squashed segments using the selected [`TokenizerId`](../crates/stcli-core/src/tokenizer.rs) (`tiktoken:cl100k_base` or `tiktoken:o200k_base`), protecting required system blocks while discarding examples and older turns when limits are reached.

---

## 6. Macros and Sequential Dataflow

STcli executes macros in prompt presets sequentially in the order prompts are evaluated:

- **Core Aliases**: Expanded to include `{{description}}`, `{{personality}}`, `{{scenario}}`, `{{summary}}`, `{{short_term_memory}}`, `{{long_term_memory}}`, and `{{lastChatMessage}}`.
- **Solo Group Macros**: `{{group}}` and `{{groupNotMuted}}` resolve safely to an empty string in single-character chat rather than returning errors.
- **Sequential Prompt-Side Dataflow**: Variables set via `{{setvar::key::val}}` in earlier prompts (such as variable initialization banks) are immediately observable by `{{getvar::key}}` in subsequent prompts within the same turn.
- **Unknown and Extension Macros**: Unknown macros (e.g. extension-provided `{{getglobalvar::...}}`) are preserved literally and emit `unknown-macro-preserved` diagnostics without aborting prompt preparation.
- **Tag Comment Safety**: Comments formatted as `{{// @directive}}` are not treated as closing tags.

---

## 7. Safety, Embedded Scripts, and Third-Party Directives

Complex presets frequently bundle JavaScript regex scripts and third-party extension directives. STcli enforces explicit security boundaries under [`sillytavern-1.18-core`](../compat/profiles/sillytavern-1.18-core.json) per [ADR 0004](adr/0004-preset-settings-and-transformations.md):

### Embedded Regex Scripts (`/extensions/regex_scripts`)

- Scripts are scanned and indexed by canonical digest ([`PresetScriptMetadata`](../crates/stcli-core/src/turn.rs)).
- They are **never executed implicitly**. A script runs only when its exact digest is listed in the session's `preset_script_grants` (set at session creation with `--grant-preset-script <digest>`).
- Granted, enabled scripts are applied at prompt assembly by placement — **user input** (1) transforms user messages, **AI output** (2) transforms assistant messages — through the isolated ECMAScript worker. Stored artifacts and candidates remain raw; the transform is transient, matching SillyTavern's consume-time model. Supported: `$0`..`$n` and `{{match}}` substitution, `trimStrings`, `minDepth`/`maxDepth`, and the global flag.
- Diagnostics:
  - `preset-scripts-not-authorized`: Emitted when enabled scripts lack an explicit Preset Script Grant (they are not executed).
  - `preset-scripts-placement-unsupported`: Emitted when a granted script targets only placements the engine does not yet apply (display-only `markdownOnly`, slash command, world info, or reasoning).

### Third-Party Directives (for example, NemoPresetExt)

Core preserves unknown directive comments but does not parse vendor-specific vocabularies. The default `org.stcli.nemo-directives` Plugin evaluates these directives through the read-only Artifact-inspection surface:

- Exclusivity: `@mutual-exclusive-group`, legacy `@exclusive-with-category`, `@exclusive-with`, and `@max-one-per-category` with `@category`.
- Warnings: `@conflicts-with`, `@warning`, `@deprecated`, and unresolved references.
- Matching: exact identifier or normalized display name.

When a user enables an exclusive Prompt Order Entry, the preset picker disables its enabled siblings in the same Artifact Revision and names them in the toast. Soft findings remain non-blocking Compatibility Warnings. Unknown directives are ignored, and the Plugin never modifies Artifact content.

### Disabled structural markers

If effective Session state disables a `marker: true` Prompt Order Entry, turn preparation and Dry Run emit `structural-prompt-marker-disabled`. The warning names each disabled marker and never blocks generation or changes prompt assembly.

---

## 8. Real-World Oracle Parity: Nanobear

The compatibility test suite verifies provider-request assembly against the redistributable **Nanobear v2.1 Chat Completion** preset. Its 26 prompts, macro-bearing custom content, native prompt slots, provider settings, and system-message squashing exercise the complex-preset path. The preset and recorded transcript are committed under [`compat/external/`](../compat/external/) and hash-pinned by [`phase4-preset-parity.json`](../compat/fixtures/phase4-preset-parity.json).

### Pinned Sources

| Source | SHA-256 Digest | Pinned Revision | Override Environment Variable |
|---|---|---|---|
| Nanobear v2.1 Chat Completion preset | `d7a7dfcbc8349d6813171b1a1edbab40cc229e37550be90426ddd7cbfdd78c7f` | `a3aa566983e96f1f0f29718622390fca4baa1bd6` | `STCLI_NANOBEAR_PRESET` |
| Recorded provider-request oracle | `c71002ece73343ce2d4be1ad52a4ae5e4b255325822abafd4777b65d3829a863` | `a3aa566983e96f1f0f29718622390fca4baa1bd6` | `STCLI_NANOBEAR_ORACLE` |

The preset is redistributed unmodified under CC BY 4.0 with attribution in [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md). Environment variables are optional overrides for re-recording. Without overrides, verification reads the committed files.

### Running Oracle Parity Tests

```bash
cargo run --bin stcli -- compat verify
cargo test -p stcli-cli provider_test::tests::checked_in_oracle_matches_all_dry_run_generation_types
```

The oracle test proves exact provider-request match across four generation types:

1. **Normal**: includes greeting, selected history, and the current user action.
2. **Continue**: preserves the full selected history and appends the continuation nudge.
3. **Regenerate**: excludes the replaced Turn and submits its user action again.
4. **Swipe**: matches regenerate history boundaries for a new Candidate.

---

## 9. Preset Field Classification Reference Table

Every SillyTavern 1.18 Chat Completion preset field has an explicit classification in [`compat/profiles/sillytavern-1.18-core.json`](../compat/profiles/sillytavern-1.18-core.json):

| Field | Classification | STcli Handling |
|---|---|---|
| `temperature` | `provider-behavior` | Forwarded in provider request |
| `top_p` | `provider-behavior` | Forwarded in provider request |
| `top_k` | `provider-behavior` | Forwarded in provider request |
| `min_p` | `provider-behavior` | Forwarded in provider request (omitted if `0.0`) |
| `frequency_penalty` | `provider-behavior` | Forwarded in provider request |
| `presence_penalty` | `provider-behavior` | Forwarded in provider request |
| `repetition_penalty` | `provider-behavior` | Forwarded in provider request |
| `reasoning_effort` | `provider-behavior` | Forwarded in provider request |
| `seed` | `provider-behavior` | Forwarded in provider request (omitted if `-1`) |
| `n` | `provider-behavior` | Forwarded in provider request (omitted if `1`) |
| `openai_max_tokens` | `provider-behavior` | Maps to `max_tokens` in provider request |
| `squash_system_messages` | `assembly-behavior` | Merges consecutive system messages with newline |
| `use_sysprompt` | `assembly-behavior` | Controls inclusion of `main` prompt slot |
| `assistant_prefill` | `assembly-behavior` | Injected as trailing assistant message |
| `continue_prefill` | `assembly-behavior` | Continuation prefix injected for `continue` |
| `continue_nudge_prompt` | `assembly-behavior` | Preserved for continuation assembly |
| `openai_max_context` | `assembly-behavior` | Maps to `max_context` for prompt budgeting |
| `max_context_unlocked` | `assembly-behavior` | Retained for context window calculation |
| `names_behavior` | `assembly-behavior` | Retained for role/name formatting |
| `prompt_order` | `assembly-behavior` | Used to order native and custom prompt slots |
| `prompts` | `assembly-behavior` | Segment definitions rendered into prompt plan |
| `preset_name` | `preserved-metadata` | Retained without engine effect |
| `extensions` | `preserved-metadata` | Retained; embedded scripts and directives warned |
| `nemo_merge_note` | `preserved-metadata` | Retained without engine effect |
| `chat_completion_source` | `preserved-metadata` | Retained without engine effect |
| `bypass_status_check` | `preserved-metadata` | Retained without engine effect |
| `bias_preset_selected` | `preserved-metadata` | Retained without engine effect |
| `show_thoughts` | `preserved-metadata` | Retained without engine effect |
| `stream_openai` | `preserved-metadata` | Retained; streaming is owned by Session Provider Settings |
| `top_a` | `preserved-metadata` | Retained without engine effect |
| `verbosity` | `preserved-metadata` | Retained without engine effect |
| `inline_image_quality` | `preserved-metadata` | Retained without engine effect |
| `continue_postfix` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `custom_prompt_post_processing` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `new_chat_prompt` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `new_example_chat_prompt` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `personality_format` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `scenario_format` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `send_if_empty` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `wi_format` | `documented-fallback` | Emits `preset-field-documented-fallback` warning |
| `assistant_impersonation` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `enable_web_search` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `function_calling` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `group_nudge_prompt` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `impersonation_prompt` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `image_inlining` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `media_inlining` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `new_group_chat_prompt` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `request_image_aspect_ratio` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `request_image_resolution` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `request_images` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `tool_call_recurse_limit` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
| `tool_reasoning_mode` | `hard-unsupported` | Emits `preset-field-hard-unsupported` warning |
