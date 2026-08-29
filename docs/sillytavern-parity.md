# SillyTavern Parity Matrix

This document tracks STcli's implementation status against [SillyTavern](https://github.com/SillyTavern/SillyTavern) features, specifically focusing on the pinned [`sillytavern-1.18-core`](../compat/profiles/sillytavern-1.18-core.json) profile and post-MVP roadmap progression.

**Compatibility Progress**: `[█████████████░░░░░░░]` **65%** (46 / 71 tracked features implemented)

## Status Legend

| Status | Meaning |
| :--- | :--- |
| ✅ **Implemented** | Full functional parity with passing fixtures and tests. |
| ⚠️ **Partial / Fallback** | Preserved metadata, non-blocking warning, or bounded fallback. |
| ❌ **Planned / Gap** | Out of scope for MVP; planned in roadmap (version indicated). |
| 🛑 **Hard Unsupported** | Excluded by design (violates deterministic replay, local-first, or security). |

---

## 1. Content & File Formats

| Feature / Format | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Character Card V1 JSON** | ✅ | Full import & export parity in [`crates/stcli-core/src/artifact.rs`](file:///home/thomas/src/STcli/crates/stcli-core/src/artifact.rs). |
| **Character Card V2 JSON** | ✅ | Exact data model support; byte-accurate re-export of untouched content. |
| **Character Book V2 (Embedded)** | ✅ | Embedded lorebook extracted and activated via lore engine. |
| **Standalone Lorebook JSON** | ✅ | SillyTavern 1.18 format imported as versioned immutable artifact revisions. |
| **Chat Completion Preset JSON** | ✅ | Parses prompt list, order profiles (`100001`), and generation parameters. |
| **Character Card V3 (CCv3) JSON** | ❌ | Target for **v0.2** as external `artifact-codec` Wasm proof candidate ([#20](https://github.com/its-a-unixsystem/STcli/issues/20)). |
| **PNG / APNG Image Cards** | ❌ | Target for **v0.3** (`tEXt` / `iTXt` chunk parsing and asset extraction). |
| **WebP Image Cards** | ❌ | Target for **v0.3** (subject to explicit embedding container contract). |
| **CHARX Archive Containers** | ❌ | Target for **v0.3** (multi-file card archives with bundled assets). |
| **Duplicate JSON Keys** | 🛑 | Hard unsupported; rejected on import with path-aware validation error. |

---

## 2. Prompt Compilation & Presets

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Sequential Prompt Assembly** | ✅ | Implemented in [`Store::prepare_turn`](file:///home/thomas/src/STcli/crates/stcli-core/src/turn.rs#L127-L135). Order profile `100001` selected. |
| **In-Chat Prompt Injection** | ✅ | Supports dynamic history depth slicing (both relative and absolute depths). |
| **System Message Squashing** | ✅ | Consecutive system messages combined when `squash_system_messages` is active. |
| **System Prompt Gating** | ✅ | Respects `use_sysprompt` toggle. |
| **Assistant & Continuation Prefill** | ✅ | Prefills appended to trailing assistant turn for supported models. |
| **Exact Tokenization & Budgeting** | ✅ | Exact counts via [`TokenizerRegistry`](file:///home/thomas/src/STcli/crates/stcli-core/src/tokenizer.rs) (`tiktoken-rs` & HF tokenizers). |
| **Context Pruning** | ✅ | Prunes earliest messages to respect `openai_max_context` and response limits. |
| **Embedded Regex Scripts (Presets & Characters)** | ✅ | Extracted from both Prompt Presets (`/extensions/regex_scripts`) and Character Cards (`extensions.regex_scripts`). Applied via isolated worker with ReDoS protection. Supports `$0`..`$n`, `{{match}}`, `trimStrings`, `minDepth`/`maxDepth`, global flag, macros in `replaceString`, and `substituteRegex` modes. **Grant-gated**: requires explicit SHA-256 digest in `script_grants` (`--grant-script`). Stored content strictly raw; presentation scripts produce `rendered_content` on projections ([#19](https://github.com/its-a-unixsystem/STcli/issues/19)). |
| **Regex Placements (Display, World Info, Reasoning)** | ✅ | Full placement parity: **Placement 1 (`UserInput`)** & **2 (`AiOutput`)** in prompt assembly; **Placement 5 (`WorldInfo`)** applied to activated lorebook entries before token budgeting; **Placement 6 (`Reasoning`)** cleanses finalized reasoning buffers; `markdownOnly` / display scripts precomputed on candidate projections for TUI/CLI rendering. Placement 3 (`SlashCommand`) deferred to **v0.6**. |
| **Prompt Itemization & Segment Detail** | ✅ | Granular inspection of raw vs. rendered content for individual prompt segments via `stcli prompt inspect <attempt> --segment <slot_or_index>` with correlated macro, regex, and state metadata ([#61](https://github.com/its-a-unixsystem/STcli/issues/61)). |
| **Generation Prompt Diffing (`diffPrevPrompt`)** | ✅ | Structural segment, textual line/word diffing, and kept/pruned token delta accounting between generation attempts or predecessor turns via `stcli prompt diff` and `stcli prompt inspect --diff-prev` ([#62](https://github.com/its-a-unixsystem/STcli/issues/62)). |
| **Flat Text Completion Prompts** | ❌ | Story strings, instruct templates, and separators planned for **v0.4**. |
| **Extension Directive Comments** | ⚠️ | Directives (e.g. `NemoPresetExt` comments) preserved intact as text with warnings. |

---

## 3. World Info / Lorebook Engine

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Keyword Matching (Primary & Secondary)** | ✅ | Implemented in [`crates/stcli-core/src/lore.rs`](file:///home/thomas/src/STcli/crates/stcli-core/src/lore.rs). Selective and logic filters. |
| **Recursive Activation** | ✅ | Multi-pass scanning with cycle detection and depth boundaries. |
| **Insertion Order & Position** | ✅ | Inset at character, before/after chat history, or specific depth. |
| **Activation Limits & Probabilities** | ✅ | Deterministic evaluation using attempt-seeded PRNG. |
| **Vector / Semantic Lore Retrieval** | ❌ | Smart Context / embeddings planned for **v0.7** via `lore-retriever` ([#24](https://github.com/its-a-unixsystem/STcli/issues/24)). |

---

## 4. Macros & Dynamic State

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Context Macros (50+ Core)** | ✅ | Exact parity in [`crates/stcli-core/src/macros.rs`](file:///home/thomas/src/STcli/crates/stcli-core/src/macros.rs) (`{{user}}`, `{{char}}`, `{{scenario}}`, etc.). |
| **Deterministic PRNG Macros** | ✅ | `{{random}}`, `{{pick}}`, `{{roll}}` resolve deterministically from attempt seed. |
| **Date & Time Macros** | ✅ | `{{time}}`, `{{date}}`, `{{isodate}}`, `{{datetimeformat}}` evaluated at turn prep. |
| **Local & Global Variables** | ✅ | Full support for `.var` (local) and `$var` (global) in [`state_cells`](file:///home/thomas/src/STcli/crates/stcli-core/src/state.rs). |
| **Variable Shorthand Operators** | ✅ | `=`, `+=`, `-=`, `++`, `--`, `??=`, `||=`, `==`, `!=`, `>`, `<`, `>=`, `<=`. |
| **Block Conditionals** | ✅ | `{{if condition}}...{{else}}...{{/if}}` with side-effect isolation for skipped blocks. |
| **Whitespace Control** | ✅ | `{{trim}}`, `#` whitespace preservation (`{{#tag}}`), `{{reverse}}`, `{{space}}`. |
| **Solo Group / Memory Fallbacks** | ✅ | Solo `{{group}}` resolves empty; `{{summary}}` and memories use core fallbacks. |
| **Unknown Macro Fallback** | ⚠️ | Preserved literally in prompt text; logs non-blocking `MacroWarning`. |
| **Interactive UI Macros** | 🛑 | `{{input}}`, `{{ismobile}}`, `{{banned}}`, `{{systemprompt}}` hard-unsupported. |
| **STscript Execution** | ❌ | Command piping, closures, and loops planned for **v0.6**. |

---

## 5. Turn Operations & Roleplaying Loop

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Normal Send Turn** | ✅ | Atomic turn orchestration ([`crates/stcli-core/src/turn.rs`](file:///home/thomas/src/STcli/crates/stcli-core/src/turn.rs)). |
| **Continue Generation** | ✅ | Trailing assistant message continuation via model prefill. |
| **Regenerate Turn** | ✅ | Re-executes generation attempt with identical context settings. |
| **Swipe / Candidate Variants** | ✅ | Preserves multiple candidate responses per turn; switchable active selection. |
| **Alternate Greetings** | ✅ | Choose between author-provided first messages at branch creation. |
| **Branching History Tree** | ✅ | Create new branches from arbitrary points in history without mutating original branch. |
| **Dry Run Preview** | ✅ | Full turn prep preview without calling provider, committing state, or writing trace. |
| **Offline Deterministic Replay** | ✅ | Replays turns from turn trace without calling live APIs or clocks. |
| **Turn Capsules (Portability)** | ✅ | Export/import self-contained `.capsule` files embedding full replay history. |
| **Group Chat (Multi-Character)** | ❌ | Multi-character rooms, speaker selection, and nudges planned for **v0.5**. |
| **Quiet / Background Generation** | ❌ | Nested generation attempts (e.g. summarization) planned for **v0.7** ([#25](https://github.com/its-a-unixsystem/STcli/issues/25)). |
| **In-Place Message Deletion** | ✅ | Event-sourced tombstones (`turn delete`, `candidate delete`, `branch delete`) and session compaction (`session compact`). Hiding (`turn hide`, `candidate hide`) keeps entities in projection but excludes from prompt. |

---

## 6. Provider & AI Backends

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **OpenAI-Compatible HTTPS** | ✅ | Full streaming SSE adapter in [`crates/stcli-core/src/provider.rs`](file:///home/thomas/src/STcli/crates/stcli-core/src/provider.rs). |
| **Request Parameters** | ✅ | Temperature, top-p, max tokens, stop sequences, seed, static headers. |
| **Proprietary Direct Adapters** | ❌ | Native Anthropic, Gemini, KoboldCpp APIs require OpenAI-compatible proxy endpoints. |
| **Connection Profile Switching** | ✅ | Loaded from `config.toml` via `[providers.<name>]`, selectable at session create/update (`--provider-profile`), and switchable interactively in the TUI picker ([#22](https://github.com/its-a-unixsystem/STcli/issues/22)). |
| **Reasoning Delta Streaming** | ✅ | Extracts `/choices/0/delta/reasoning` and `reasoning_content` as `ProviderEvent::ReasoningDelta`, streaming thinking tokens in real time in CLI JSONL and TUI live thinking view ([#60](https://github.com/its-a-unixsystem/STcli/issues/60)). |
| **Automatic Provider Retries** | 🛑 | Excluded by design; failures preserve status and response body for auditability. |

---

## 7. Frontend, Media & UI

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Headless / Scriptable CLI** | ✅ | Full CLI with human and JSON output modes ([`docs/cli.md`](cli.md)). |
| **Interactive Terminal UI (TUI)** | ✅ | Full-screen terminal roleplay with chat, prompt, and lore views, session browser, and candidate swiping ([`PRD-TUI.md`](../PRD-TUI.md)). |
| **Themes, Mouse, Nerd Fonts** | ✅ | Dark/light themes, plain-glyph fallback, full keyboard parity, and assisted mouse navigation. |
| **Browser / Web Frontend** | ❌ | Target for **v1.x** with DOM slot integration. |
| **Asset Storage & Avatars** | ❌ | Content-addressed asset store planned for **v0.3** ([#20](https://github.com/its-a-unixsystem/STcli/issues/20)). |
| **Character Expressions** | ❌ | Dynamic emotion classification and sprites planned for **v0.3 / v1.x** ([#27](https://github.com/its-a-unixsystem/STcli/issues/27)). |
| **Quick Replies Palette** | ❌ | Action shortcuts in TUI planned for **v0.2** ([#23](https://github.com/its-a-unixsystem/STcli/issues/23)). |
| **Media Gallery** | ❌ | Media viewer planned for **v1.x** ([#30](https://github.com/its-a-unixsystem/STcli/issues/30)). |
| **Text-to-Speech (TTS)** | ❌ | Audio CLI pipe / sidecar planned for **v1.x** ([#29](https://github.com/its-a-unixsystem/STcli/issues/29)). |
| **Image Generation / Captioning**| ❌ | Sidecar multimodal integrations planned for **v1.x** ([#31](https://github.com/its-a-unixsystem/STcli/issues/31), [#32](https://github.com/its-a-unixsystem/STcli/issues/32)). |

---

## 8. Extensibility & Plugins

| Feature | Status | Remarks & Implementation Seam |
| :--- | :---: | :--- |
| **Pure Wasm Plugins** | ✅ | Component Model host with declarative effects ([`ADR 0003`](adr/0003-pure-wasm-plugins.md)). |
| **External Wasm Codecs** | ❌ | Extensible artifact parsing interface planned for **v0.2**. |
| **SillyTavern JS UI Extensions** | 🛑 | MVP non-goal. Sandboxed JS subset bridge planned for **v1.0**. |
| **Server Plugins** | 🛑 | MVP non-goal. Trusted sidecars planned for **v1.x**. |
