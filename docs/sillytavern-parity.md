# SillyTavern Parity Matrix

This document tracks how much of [SillyTavern](https://github.com/SillyTavern/SillyTavern) STcli can run. It targets the pinned [`sillytavern-1.18-core`](../compat/profiles/sillytavern-1.18-core.json) profile and the post-MVP roadmap.

SillyTavern has two kinds of features:

- **Core features.** These are built into the SillyTavern app. Examples are character cards, World Info, macros, and the chat loop.
- **Built-in extensions.** SillyTavern ships a fixed set of bundled extensions. Examples are Summarize, Vector Storage, Image Generation, and Text To Speech.

This document keeps the same split, so a SillyTavern user recognizes the structure.

## How to read this document

There are two matrices:

- **Part 1 — SillyTavern parity.** Only real SillyTavern features. Each row is a feature that SillyTavern has. The status says whether STcli runs it.
- **Part 2 — Beyond SillyTavern.** STcli features that SillyTavern does not have. These do not count toward parity, because there is nothing in SillyTavern to match.

A feature is in Part 2, not Part 1, when SillyTavern has no equivalent. A command-line interface is one example. SillyTavern is a web app and has no command-line interface.

## Compatibility progress

`[█████████████░░░░░░░]` **67%** (44 / 66 SillyTavern features fully implemented)

The percentage counts fully implemented features (✅) against all tracked SillyTavern features. Partial features (⚠️), planned gaps (❌), and by-design exclusions (🛑) are not counted as implemented. STcli-only features (Part 2) are excluded from the total.

## Status legend

| Status | Meaning |
| :--- | :--- |
| ✅ **Implemented** | Full parity with passing fixtures and tests. |
| ⚠️ **Partial / Fallback** | Preserved metadata, a non-blocking warning, or a bounded fallback. |
| ❌ **Planned / Gap** | Not built yet. Planned in the roadmap (version shown). |
| 🛑 **Excluded by design** | Left out on purpose. It breaks deterministic replay, the local-first model, or the security model. |

---

# Part 1 — SillyTavern parity

## 1. Character cards and content formats

| Feature / Format | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Character Card V1 JSON** | ✅ | Full import and export in [`crates/stcli-core/src/artifact.rs`](../crates/stcli-core/src/artifact.rs). |
| **Character Card V2 JSON** | ✅ | Exact data model. Untouched content is re-exported byte for byte. |
| **Character Card V3 (CCv3) JSON** | ✅ | Full import and export in [`crates/stcli-core/src/artifact.rs`](../crates/stcli-core/src/artifact.rs). |
| **Character Book V2 (embedded)** | ✅ | The embedded lorebook is extracted and activated by the lore engine. |
| **Standalone Lorebook JSON** | ✅ | The SillyTavern 1.18 format is imported as a versioned, immutable revision. |
| **PNG / APNG image cards** | ✅ | `tEXt` and `iTXt` chunks are parsed. The image is stored as the avatar in the asset store. |
| **WebP image cards** | ✅ | EXIF and XMP chunks are parsed for V2 and V3 cards. The image is stored as the avatar. |
| **CHARX archive containers** | ✅ | Multi-file V3 archives with bundled assets and lorebooks. Asset references are validated on import. |
| **Duplicate JSON keys** | 🛑 | Rejected on import with a path-aware validation error. |

## 2. Prompt building and presets

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Chat Completion preset JSON** | ✅ | Parses the prompt list, order profiles (`100001`), and generation parameters. |
| **Sequential prompt assembly** | ✅ | Built in [`Store::prepare_turn`](../crates/stcli-core/src/turn.rs). Order profile `100001` is selected. |
| **In-chat prompt injection** | ✅ | Supports relative and absolute history depth. |
| **System message squashing** | ✅ | Consecutive system messages are combined when `squash_system_messages` is active. |
| **System prompt gating** | ✅ | Respects the `use_sysprompt` toggle. |
| **Assistant and continuation prefill** | ✅ | The prefill is added to the trailing assistant turn for models that support it. |
| **Exact tokenization and budgeting** | ✅ | Exact counts from the [`TokenizerRegistry`](../crates/stcli-core/src/tokenizer.rs) (`tiktoken-rs` and HF tokenizers). |
| **Context pruning** | ✅ | Prunes the earliest messages to respect `openai_max_context` and the response limit. |
| **Prompt itemization** | ✅ | Inspect raw and rendered content per segment with `stcli prompt inspect <attempt> --segment <slot_or_index>`. Shows correlated macro, regex, and state metadata. |
| **Generation prompt diffing (`diffPrevPrompt`)** | ✅ | Segment, line, word, and token-delta diffing between attempts or predecessor turns. Use `stcli prompt diff` or `stcli prompt inspect --diff-prev`. |
| **Flat text completion prompts** | ⚠️ | Story strings, instruct templates, and separators in [`crates/stcli-core/src/text_completion.rs`](../crates/stcli-core/src/text_completion.rs). Selected per provider profile (`format_mode: text-completion`). Not yet tested against a live provider. See [`docs/text-completion.md`](text-completion.md). Planned for **v0.4**. |
| **Extension directive comments** | ⚠️ | Directives such as `NemoPresetExt` comments are kept intact as text, with a warning. |

## 3. World Info / lorebooks

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Keyword matching (primary and secondary)** | ✅ | Built in [`crates/stcli-core/src/lore.rs`](../crates/stcli-core/src/lore.rs). Selective and logic filters. |
| **Recursive activation** | ✅ | Multi-pass scanning with cycle detection and depth limits. |
| **Insertion order and position** | ✅ | Inserts at the character, before or after the chat history, or at a set depth. |
| **Activation limits and probabilities** | ✅ | Deterministic evaluation with an attempt-seeded PRNG. |

## 4. Macros and dynamic state

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Context macros (50+ core)** | ✅ | Exact parity in [`crates/stcli-core/src/macros.rs`](../crates/stcli-core/src/macros.rs) (`{{user}}`, `{{char}}`, `{{scenario}}`, and more). |
| **PRNG macros** | ✅ | `{{random}}`, `{{pick}}`, and `{{roll}}` resolve from the attempt seed. See Part 2 for why this is deterministic. |
| **Date and time macros** | ✅ | `{{time}}`, `{{date}}`, `{{isodate}}`, and `{{datetimeformat}}` are evaluated at turn prep. |
| **Local and global variables** | ✅ | Full support for `.var` (local) and `$var` (global) in [`state_cells`](../crates/stcli-core/src/state.rs). |
| **Variable shorthand operators** | ✅ | `=`, `+=`, `-=`, `++`, `--`, `??=`, `\|\|=`, `==`, `!=`, `>`, `<`, `>=`, `<=`. |
| **Block conditionals** | ✅ | `{{if condition}}...{{else}}...{{/if}}`. Skipped blocks have no side effects. |
| **Whitespace control** | ✅ | `{{trim}}`, `#` whitespace preservation (`{{#tag}}`), `{{reverse}}`, and `{{space}}`. |
| **Solo group and memory fallbacks** | ✅ | A solo `{{group}}` resolves empty. `{{summary}}` and memories use core fallbacks. |
| **Unknown macro fallback** | ⚠️ | The macro is kept as literal text. A non-blocking `MacroWarning` is logged. |
| **Interactive UI macros** | 🛑 | `{{input}}`, `{{ismobile}}`, `{{banned}}`, and `{{systemprompt}}` need a live GUI. Not supported. |
| **STscript execution** | ⚠️ | Core parser/evaluator supports quoted arguments, command pipes, closures, `/if`, `/else`, `/while`, `/delay`, `/echo`, `/abort`, `/eval`, persistent local/global variables, and attempt-local `/let`. Execution is bounded and outcomes commit atomically to the Turn Trace. Browser-only commands and the wider SillyTavern slash-command registry remain unsupported. |

## 5. Chat loop and roleplay

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Normal send turn** | ✅ | Atomic turn orchestration in [`crates/stcli-core/src/turn.rs`](../crates/stcli-core/src/turn.rs). |
| **Continue generation** | ✅ | Continues the trailing assistant message with model prefill. |
| **Regenerate turn** | ✅ | Re-runs the attempt with the same context settings. |
| **Swipe / candidate variants** | ✅ | Keeps several candidate responses per turn. You can switch the active one. |
| **Alternate greetings** | ✅ | Choose between author-provided first messages when you create a branch. |
| **Branching history tree** | ✅ | Create a branch from any point in the history. The original branch does not change. |
| **In-place message deletion** | ✅ | Event-sourced tombstones (`turn delete`, `candidate delete`, `branch delete`) and `session compact`. Hiding (`turn hide`, `candidate hide`) keeps the entity but drops it from the prompt. |
| **Group chat (multi-character)** | ❌ | Multi-character rooms, speaker selection, and nudges planned for **v0.5**. |
| **Quiet / background generation** | ❌ | Nested generation attempts, such as summarization, planned for **v0.7**. |

## 6. Personas and notes

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **User persona name** | ✅ | `session create --persona <text>` sets the name used by future turns. |
| **User persona description** | ✅ | `session create` and `session update` accept inline or `@path` descriptions. The engine expands contextual macros and injects the result through the preset-controlled `personaDescription` slot. |
| **Author's Note** | ✅ | Author's Note positions (`AuthorNoteTop`, `AuthorNoteBottom`) are honored for insertion. |

## 7. Providers and connections

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **OpenAI-compatible HTTPS** | ✅ | Full streaming SSE adapter in [`crates/stcli-core/src/provider.rs`](../crates/stcli-core/src/provider.rs). |
| **Request parameters** | ✅ | Temperature, top-p, max tokens, stop sequences, seed, and static headers. |
| **Reasoning delta streaming** | ✅ | Streams `reasoning` and `reasoning_content` deltas as `ProviderEvent::ReasoningDelta`. Shown live in CLI JSONL and the TUI thinking view. |
| **Proprietary direct adapters** | ❌ | Native Anthropic, Gemini, and KoboldCpp APIs need an OpenAI-compatible proxy. |
| **Automatic provider retries** | 🛑 | Excluded by design. A failure keeps the status and body for audit. See Part 2 on determinism. |

## 8. Frontend

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Browser / web frontend** | ❌ | SillyTavern's frontend is a browser app. STcli uses a CLI and a TUI instead (see Part 2). A web frontend is a target for **v1.x**. |
| **Character expressions** | ❌ | See the Expressions built-in extension in section 10. |
| **Media gallery** | ❌ | See the Gallery built-in extension in section 10. |

## 9. Built-in extensions

SillyTavern ships these bundled extensions. Each row is one of them.

| Extension | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Regex** | ✅ | Regex scripts from presets (`/extensions/regex_scripts`) and cards (`extensions.regex_scripts`). Runs in an isolated worker with ReDoS protection. Supports `$0`..`$n`, `{{match}}`, `trimStrings`, `minDepth`/`maxDepth`, the global flag, macros in `replaceString`, and `substituteRegex` modes. Placements 1, 2, 5, and 6 are covered; placement 3 (`SlashCommand`) planned for **v0.6**. See Part 2 for the grant model. |
| **Connection Manager** | ✅ | Connection profiles loaded from `config.toml` (`[providers.<name>]`). Selectable at session create or update (`--provider-profile`) and in the TUI picker. |
| **Persona backups** | ✅ | Loads, saves, and imports SillyTavern-compatible `personas.json` data with `personas`, `persona_descriptions`, and optional `default_persona` fields. The TUI persona manager supports add, copy, edit, delete, and backup import; New Session selects saved names and descriptions. |
| **Token Counter** | ✅ | Exact token counts per segment through the tokenizer registry. See "Exact tokenization" in section 2. |
| **Assets** | ✅ | Content-addressed asset store at `data/assets/sha256/` with SQLite reference tracking ([ADR 0007](adr/0007-external-content-addressed-asset-storage.md)). Stores card avatars and CHARX assets. See Part 2. |
| **Expressions** | ❌ | Emotion classification and character sprites planned for **v0.3 / v1.x**. |
| **Image Captioning** | ❌ | Multimodal captioning adapter planned for **v1.x**. |
| **Gallery** | ❌ | Media gallery viewer planned for **v1.x**. |
| **Summarize (Memory)** | ❌ | Chat summarization needs background generation. Planned for **v0.7**. |
| **Quick Reply** | ❌ | Shortcut action palette in the TUI planned for **v0.2**. |
| **Image Generation** | ❌ | Stable Diffusion, FLUX, and DALL-E integration planned for **v1.x**. |
| **Chat Translation** | ❌ | Provider-backed message translation planned for the roadmap. |
| **Text To Speech (TTS)** | ❌ | Audio pipe or sidecar planned for **v1.x**. |
| **Vector Storage** | ❌ | Smart Context and embeddings planned for **v0.7** through `lore-retriever`. |
| **Data Bank (Attachments)** | ❌ | Chat attachments and document ingestion planned for the roadmap. |

## 10. Extension and plugin compatibility

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **SillyTavern JS UI extensions** | 🛑 | An MVP non-goal. A sandboxed JS subset bridge is planned for **v1.0**. STcli runs its own sandboxed plugins instead (see Part 2). |
| **Server plugins** | 🛑 | An MVP non-goal. Trusted sidecars planned for **v1.x**. |
| **External Wasm codecs** | ❌ | An extensible artifact-parsing interface planned for **v0.2**. |

---

# Part 2 — Beyond SillyTavern

These are STcli features. SillyTavern has no equivalent, or STcli does the job in a safer way. They do not count toward the parity percentage.

| STcli feature | Why it matters |
| :--- | :--- |
| **Scriptable CLI with JSON output** | Every command has a human mode and a JSON mode. You can drive STcli from scripts and pipelines. SillyTavern is a web app and has no command-line interface. See [`docs/cli.md`](cli.md). |
| **Terminal UI (TUI)** | A full roleplay UI in the terminal, with chat, prompt, and lore views, a session browser, and candidate swiping. No browser and no server are needed. |
| **Deterministic engine** | Seeded PRNG macros, seeded lore probabilities, and seeded attempts. The same inputs give the same output. SillyTavern's randomness is not reproducible. |
| **Offline turn replay** | Rebuild any past turn from its turn trace, with no live API call and no clock read. See [ADR 0001](adr/0001-authoritative-turn-trace.md). |
| **Turn capsules** | Export a turn as one self-contained `.capsule` file with its full replay history. Import it elsewhere and replay it. SillyTavern has no such portable unit. |
| **Dry-run preview** | See the exact prompt for a turn without calling the provider, committing state, or writing a trace. |
| **Immutable versioned artifacts** | Every import is a fixed revision. Edits create a new revision and never change history. See [ADR 0002](adr/0002-versioned-compatibility-and-revisions.md). |
| **Content-addressed asset store** | Avatars and assets are stored by SHA-256 hash and deduplicated. This is STcli's take on the Assets extension. |
| **Grant-gated regex scripts** | A regex script runs only after you approve its SHA-256 digest (`--grant-script`). SillyTavern runs card regex with no such gate. This is STcli's take on the Regex extension. |
| **Sandboxed plugins (Wasm + QuickJS)** | Plugins run in isolation with brokered effects ([ADR 0003](adr/0003-pure-wasm-plugins.md), [ADR 0006](adr/0006-layered-plugins-and-brokered-effects.md)). SillyTavern third-party extensions run as unsandboxed browser JavaScript. See [`docs/plugins.md`](plugins.md). |
| **Prompt itemization with diff** | Inspect raw and rendered content per segment, and diff segments across attempts. This goes past SillyTavern's prompt itemizer. See section 2. |
| **Local-first, no telemetry** | The only network call is to the model provider you choose. There is no cloud account and no analytics. |
