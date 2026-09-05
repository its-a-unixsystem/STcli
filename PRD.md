# STcli Roleplaying Engine Product Requirements Document

**Status:** Accepted design baseline
**Working name:** STcli
**Compatibility profile:** `sillytavern-1.18-core`
**License:** AGPL-3.0-or-later
**Implementation:** Rust workspace with internal library crates and a local CLI binary
**MVP frontend:** Scriptable debug CLI
**Post-MVP frontend:** Rich terminal UI
**MVP platforms:** Linux x86-64 and Windows x86-64
**Data boundary:** Strictly local except configured model requests
**Release model:** Release-gate-driven; no fixed date

STcli is an independent, unofficial SillyTavern-compatible engine. It is not affiliated with or endorsed by the SillyTavern project.

## 1. Executive Summary

### Problem Statement

SillyTavern power users have reusable character cards, lorebooks, prompt presets, macros, and variables, but the behavior is coupled to a browser-oriented JavaScript application. Existing content cannot be moved into a scriptable Rust engine merely by parsing its JSON: correct behavior also depends on prompt ordering, lore activation, macro evaluation, variable coercion, tokenization, candidate selection, plugin effects, and generation type.

Generation failures are difficult to reproduce because the final provider request rarely explains which content activated, which state changed, which version was used, or why context was pruned. Mutable files and UI-owned state also make historical replay unreliable.

### Proposed Solution

STcli will implement a local-first, branchable roleplaying engine with a versioned `sillytavern-1.18-core` compatibility profile. The MVP will support Character Card V1/V2 JSON, embedded V2 character lore, standalone lorebook JSON, Chat Completion prompt presets, a frozen macro manifest, compatible local/global variables, deterministic World Info behavior, explicit tokenizers, one generic OpenAI-compatible HTTPS provider, and SillyTavern-like continue/regenerate/swipe behavior.

The authoritative record is an append-only Turn Trace stored in SQLite. Session state is a rebuildable projection. Every generation attempt records the content and effect outcomes needed for inspection and deterministic replay. Users can export either a self-contained Portable Capsule or a Thin Capsule that references local content.

The MVP includes one pure, deterministic, capability-limited WebAssembly Plugin interface. External artifact codecs, JavaScript compatibility, networked plugin effects, additional containers, and the rich terminal interface are post-MVP work.

### Product Principles

1. **Observable parity, not byte theater.** Parity covers provider messages, lore decisions, macros, variables, token counts, pruning, and turn behavior. It excludes irrelevant JSON member order and HTTP byte identity.
2. **Compatibility is explicit and bounded.** The product claims `sillytavern-1.18-core`, never unqualified SillyTavern 1.18 compatibility.
3. **Safety and data integrity outrank compatibility.** Unsafe code execution, secret leakage, ambiguous destructive data, and trace corruption are never required for parity.
4. **History is immutable.** Artifact revisions, candidates, configuration revisions, branches, and authoritative trace outcomes are not rewritten in place.
5. **Every turn is explainable.** Prompt segments, lore decisions, macro evaluations, token pruning, variable changes, plugin effects, and provider receipts are inspectable.
6. **Replay is not rerun.** Replay uses recorded effects without network or plugin execution. Rerun creates a new attempt by submitting a recorded provider request again.
7. **Frontends do not own state.** The debug CLI and future rich TUI use the same command/event interface.
8. **Installed code receives explicit authority.** MVP Plugins are pure Wasm components returning declarative effects.
9. **Local means local.** There is no telemetry, cloud account, synchronization, or hosted persistence.
10. **The engine is content-neutral.** Narrative-content moderation is delegated to the user and configured provider; structural and execution safety remain enforced.

### Compatibility Outcomes

Every relevant artifact field, macro, lore behavior, prompt feature, and turn operation is classified as one of:

- **Exact:** included in the profile manifest and covered by passing fixtures.
- **Preserved metadata:** retained losslessly but not used in prompt behavior; parity is unaffected.
- **Documented fallback:** a defined fallback exists. Execution requires explicit non-parity permission and is marked non-parity.
- **Hard unsupported:** no safe interpretation exists; execution is blocked even when non-parity mode is allowed.

An unresolved literal macro is a documented fallback. Duplicate JSON keys, unsupported executable plugin behavior, and invalid capsule effects are hard unsupported.

### Success Criteria

1. **Usable roleplay loop:** A user can import supported JSON artifacts, configure an OpenAI-compatible provider and tokenizer, create a session, select a greeting, send, continue, regenerate, swipe, branch, archive, purge, persist, resume, inspect, dry-run, export a capsule, replay, and explicitly rerun through the debug CLI.
2. **Profile parity:** One hundred percent of fixtures in the checked-in `sillytavern-1.18-core` manifest pass. Each manifest feature has positive, negative, boundary, ordering, and failure fixtures.
3. **Immutable artifact history:** Untouched imported JSON exports byte-for-byte identically. Re-importing changed or reformatted content creates a new Artifact Revision without changing prior sessions or attempts.
4. **Deterministic replay:** Every MVP test capsule replays with zero provider calls and zero Plugin execution and produces the original Session Projection hash.
5. **Pure plugin proof:** One out-of-tree Plugin installs without recompilation, receives only declared pure capabilities, registers a macro or prompt contribution, writes only its namespace, and participates in live execution and recorded replay.
6. **Secret exclusion:** Automated scans find no resolved API keys or authorization headers in logs, SQLite, CLI JSON, trace entries, or capsules.
7. **Crash consistency:** Interruption cannot leave a selected Candidate referencing an absent response or uncommitted state revision.
8. **Public protocol stability:** CLI JSON, CLI JSONL events, capsule schemas, profile manifests, and MVP WIT interfaces pass schema compatibility fixtures.

## 2. User Experience & Functionality

### User Personas

#### Primary: SillyTavern Power User

A technical roleplay user with existing JSON cards, lorebooks, and prompt presets. They prioritize local ownership, exact prompt behavior, swipes, branches, scripting, and diagnostics over initial visual polish.

#### Secondary: Content and Compatibility Author

A card, lorebook, or prompt author who needs to understand activation, ordering, token use, macros, variables, and differences from SillyTavern 1.18 core behavior.

#### Secondary: Plugin Author

A developer who adds deterministic macros, commands, prompt segments, and namespaced state without modifying or recompiling STcli.

### Domain Model

The canonical vocabulary lives in `CONTEXT.md`. The structural relationships are visualized in [`docs/architecture/domain-model.html`](docs/architecture/domain-model.html) — a UML class diagram showing composition, cardinality, and pinning.

**Containment.** A **Session** owns a tree of **Branches** and a set of **Session Configuration Revisions**. Each **Branch** opens with one **Greeting Selection** and owns a linear sequence of **Turns**. A **Turn** owns multiple primary **Generation Attempts** and **Candidates** and may have zero or one **Selection** (the currently active Candidate). A background Generation Attempt belongs to the Session and Branch, links to an initiating Attempt and caller, and cannot create a Turn, Candidate, or Selection. A **Candidate** is an assistant response variant with origin `generated`, `continued`, `manual`, or accepted partial output.

**Pinning.** A **Session Configuration Revision** pins every behavior-affecting selection — character, persona, lorebooks, preset, provider, model, tokenizer, profile, and plugins — and references zero or more **Artifact Revisions**. Every primary or background **Generation Attempt** pins exactly one configuration revision.

**Derivation** (not structural, not shown in the class diagram):

- The **Turn Trace** is the authoritative record; a **Session Projection** is a rebuildable view derived from it.
- A **Turn Capsule** is derived from a trace slice. A **Portable Capsule** is self-contained; a **Thin Capsule** references local content.
- **Replay** reconstructs from recorded outcomes without live effects. **Rerun** submits a recorded provider request again as a new attempt.

### Core User Flow

1. Configure an OpenAI-compatible HTTPS provider and explicit tokenizer.
2. Import a Character Card V1/V2 JSON Artifact Revision and optional lorebook and prompt-preset revisions.
3. Create a Session Configuration Revision selecting the character, persona, lorebooks, prompt preset, provider, model, tokenizer, compatibility profile, generation settings, and Plugin digests.
4. Create a Session and choose the default or alternate Greeting.
5. Run a Dry Run to inspect lore, macros, state overlays, token pruning, plugin contributions, and the canonical redacted provider request.
6. Send a user action. STcli records the Turn and immediately starts its first Generation Attempt.
7. Observe streamed output and the final Candidate or inspect a failed, cancelled, or incomplete Turn with no Selection.
8. Continue, regenerate, swipe, manually edit through a Branch, or change future configuration explicitly.
9. Inspect the prompt, lore, variables, plugins, provider receipt, and Turn Trace.
10. Export, redact, import, replay, or explicitly rerun a capsule.
11. Archive or purge the Session.

### User Stories and Acceptance Criteria

#### US-1: Import immutable JSON artifacts

**Story:** As a power user, I want to import existing JSON roleplay content so I can reuse it without mutating my historical source.

**Acceptance Criteria:**

- Built-in codecs accept Character Card V1 JSON, Character Card V2 JSON, embedded V2 `character_book`, standalone SillyTavern 1.18 lorebook JSON, and supported Chat Completion prompt-preset JSON.
- Import reports artifact kind, source format, specification version, exact source SHA-256, semantic SHA-256, and compatibility outcomes.
- Artifact Revision identity is the domain-separated SHA-256 of artifact kind, source format, and exact imported bytes.
- Duplicate object keys are rejected with a path-aware hard-unsupported error.
- Unknown JSON members and namespaced plugin data are preserved.
- Artifacts are immutable after import. The MVP provides no general card, lorebook, or preset editor and no live-file synchronization.
- Re-importing changed or reformatted bytes creates a new Artifact Revision even when the semantic hash matches.
- Existing Sessions, Branches, Turns, Attempts, and capsules remain pinned to their original revisions.
- Untouched export returns the original bytes.
- Unsupported formats may be retained as inert original content but cannot participate in execution without a built-in compatible codec.

#### US-2: Configure one generic provider

**Story:** As a user, I want one configurable OpenAI-compatible HTTPS provider so I can use a compatible hosted or self-managed endpoint.

**Acceptance Criteria:**

- Configuration includes base URL, Chat Completions path, model, API-key environment-variable name, timeout, streaming toggle, tokenizer, and optional static headers.
- Requests support system, user, and assistant text messages, temperature, top-p, response-token limit, stop sequences, optional seed, and streaming.
- Streaming emits typed start, text-delta, usage, completion, cancellation, and failure events.
- API keys resolve only at request time and are removed before trace creation.
- Non-success responses preserve status and a redacted raw response body for inspection.
- Tool calls, multimodal input, reasoning controls, vendor-specific sampling, model discovery, and automatic retries are unsupported.
- A transport, HTTP, stream, or decode failure ends the Generation Attempt. An explicit user retry creates a linked new Attempt.
- Cancellation records partial text in the trace but does not create a Candidate in the MVP.

#### US-3: Create and configure a Session

**Story:** As a user, I want a durable Session whose historical behavior remains reproducible when content or settings change.

**Acceptance Criteria:**

- Session creation selects one character revision, one persona, zero or more lorebook revisions, one prompt-preset revision, one provider profile, one model, one tokenizer, one compatibility profile, generation settings, and enabled Plugin digests and grants.
- These selections form an immutable Session Configuration Revision.
- Every Generation Attempt pins exactly one Session Configuration Revision.
- Changing character, persona, lorebooks, preset, provider, model, tokenizer, generation settings, or plugins creates a new configuration revision for future Turns.
- Configuration changes do not rewrite history and do not automatically fork a Branch.
- The user explicitly creates a Branch when comparing configurations.
- Effective configuration is inspectable with secrets redacted.

#### US-4: Select a Greeting

**Story:** As a user, I want to choose the default or alternate card Greeting without pretending it was provider-generated.

**Acceptance Criteria:**

- A Branch begins with one card-authored Greeting Selection before its first Turn.
- Default creation selects `first_mes` unless the user explicitly selects an alternate Greeting.
- Greetings are not Turns, Candidates, or Generation Attempts.
- Greeting Selection may change freely before the first Turn.
- Changing Greeting Selection after Turns exist creates a new Branch from the Session root.
- Existing Branches remain unchanged.

#### US-5: Send and receive a roleplay response

**Story:** As a user, I want one command to record my action and generate a response while preserving failures for diagnosis.

**Acceptance Criteria:**

- `message send` records the user action, creates the Turn, and immediately creates the first Generation Attempt.
- A failed, cancelled, aborted, or incomplete Attempt leaves a valid Turn with no Selection.
- The engine evaluates lore, macros, variables, pure Plugins, prompt ordering, token budgeting, and provider execution through an attempt-local overlay.
- A successful provider result creates an immutable generated Candidate and selects it atomically with compatible state effects.
- Provider failure never fabricates an assistant response.
- The MVP has no standalone command for appending a user action without generation.
- Explicit retry creates a linked new Generation Attempt for the same Turn.

#### US-6: Continue, regenerate, swipe, edit, and branch

**Story:** As a user, I want alternative histories without destructive mutation.

**Acceptance Criteria:**

- Regenerate creates a new Attempt and potentially a new Candidate for the same Turn.
- Swipe selects an existing Candidate or requests a new sibling Candidate.
- Continue creates a new Attempt and a new combined Candidate whose content contains the prior selected Candidate plus the continuation. The prior Candidate remains unchanged.
- Editing a historical user action creates a new Branch from immediately before that Turn.
- Editing an assistant Candidate creates a manually authored Candidate on a new Branch and records its source Candidate.
- Original Branches and Candidates remain unchanged.
- Candidate origins and parent relationships are inspectable.
- No automatic provider retry or hidden branch creation occurs.

#### US-7: Preview with Dry Run

**Story:** As a content author, I want to preview exactly what generation would do without spending tokens or changing durable state.

**Acceptance Criteria:**

- Dry Run is available for send, continue, regenerate, and rerun preparation.
- It loads the exact baseline, evaluates lore, evaluates macros in a disposable overlay, executes pure Plugins, builds and prunes the Prompt Plan, and builds the redacted canonical provider request.
- It never creates a Turn or Generation Attempt, calls the provider, or commits state.
- It accepts optional fixed clock and RNG seed inputs.
- Output uses the public versioned CLI JSON envelope and identifies all non-parity outcomes.

#### US-8: Inspect prompt, lore, state, and provider behavior

**Story:** As a content author, I want to understand exactly why a response occurred.

**Acceptance Criteria:**

- Prompt inspection shows ordered messages, roles, slots, in-chat depth/order, source Artifact Revision and field, macro inputs/results, plugin source, token counts, response reserve, and pruning decisions.
- Lore trace reports source precedence, keys, ECMAScript regex result, optional filters, recursion, probability draw, groups, timed effects, generation triggers, insertion, and budget result.
- Variable inspection shows typed and raw values, scope, owner, origin, revision, coercion profile, mutation Attempt, and commit/discard result.
- Plugin inspection shows pinned digest, grants, event inputs, declarative effects, ordering, failure, and limit usage.
- Provider inspection shows primary and background Attempts, their parent/caller ownership, a redacted canonical request, request and response hashes, status, safe response receipt, available usage, and error.
- Segment-level provenance is required; token-span provenance is not.
- Human and versioned JSON output are available.

#### US-9: Use compatible variables and macros

**Story:** As a user, I want imported prompts and lore to preserve SillyTavern core state behavior.

**Acceptance Criteria:**

- The native State Store uses typed cells with scope, owner, origin, revision, optional raw legacy representation, and coercion-profile ID.
- The compatibility adapter supports local/global get, set, add, increment, decrement, flush, indexed JSON access, and shorthand operations listed in the profile manifest.
- Local-before-global precedence and missing/blank/string/number/boolean/array/object behavior pass mandatory fixtures.
- Macro side effects execute in an attempt-local overlay with read-your-writes behavior.
- The pinned profile determines commit behavior for success, failure, regeneration, swipe, and cancellation.
- Unknown macros remain literal and produce a documented-fallback warning. Execution requires explicit non-parity permission unless a pinned Plugin resolves them.

#### US-10: Export, replay, import, and rerun capsules

**Story:** As a user, I want a portable explanation of one Attempt that I can replay or explicitly rerun.

**Acceptance Criteria:**

- `turn export` creates a self-contained Portable Capsule by default.
- `turn export --thin` creates a Thin Capsule with local content references.
- Both include a redaction manifest and explicit `inspect`, `replay`, and `rerun` capability flags.
- Redaction recalculates capabilities; a non-replayable capsule never claims replay support.
- Replay uses recorded provider, plugin, RNG, and clock outcomes and performs no live effects.
- Rerun creates a new Attempt by submitting the recorded provider request; it is not deterministic replay.
- Portable Capsule import validates schema, hashes, ordering, identities, historical effect schemas, and recorded grants before creating data.
- Import creates an isolated Imported Session linked to the capsule hash. It never merges into an existing Session.
- Import does not install plugins, contact providers, or create a partial Session after validation failure.

#### US-11: Install a pure Plugin

**Story:** As an plugin author, I want deterministic Wasm plugins that cannot escape their declared role.

**Acceptance Criteria:**

- An plugin is an unpacked directory containing `manifest.json`, a Wasm component, and optional settings schema.
- The manifest declares ID, semantic version, engine range, component path, component SHA-256, dependencies, SPDX license, subscriptions, prompt slots, commands/macros, settings, and requested capabilities.
- MVP capabilities are limited to supported lifecycle observation, macro registration, plugin-command registration, closed-slot prompt contribution, permitted session reads, own-namespace state writes, and structured pre-request abort.
- Network, direct model, filesystem, secret, subprocess, native-library, arbitrary provider-request mutation, arbitrary segment mutation, and post-commit state mutation are unavailable.
- Plugins return declarative, serializable effects and receive no mutable engine references.
- Prompt contributions target only engine-defined closed slots.
- Dependencies are ordered topologically; explicit `before`/`after` relationships apply within a slot; remaining ties use plugin ID; cycles fail before an Attempt.
- Session Configuration Revisions pin exact component digests and grants.
- Installing an update never changes existing Sessions. Explicit adoption creates a new configuration revision.
- Replay uses recorded effects and does not require component execution.
- The proof plugin passes grant denial, state isolation, deterministic ordering, limits, failure, upgrade, replay, disable, and removal fixtures.

#### US-12: Automate through stable CLI protocols

**Story:** As a power user, I want stable machine-readable commands and events.

**Acceptance Criteria:**

- Debug CLI subcommands never own authoritative state.
- Human-readable text is the default.
- `--output json` uses a versioned envelope with schema, success, command, data, error, and warnings fields.
- Streaming JSON uses a separately versioned JSONL event schema.
- Within protocol `v1`, additions may be compatible; removals, renames, and semantic changes require `v2`.
- Full ULIDs appear in JSON and storage; the human CLI accepts only unambiguous ULID prefixes.
- Exit codes distinguish invalid input, hard unsupported, non-parity permission required, provider failure, plugin failure, replay failure, and storage failure.

#### US-13: Archive, recover, and purge local data

**Story:** As a local user, I want crash recovery and actual control over retained Sessions.

**Acceptance Criteria:**

- Archive hides a Session without deleting authoritative data.
- Purge physically deletes the Session Turn Trace, projections, unshared plugin state, and content objects with no remaining referrers.
- Shared Artifact Revisions and blobs survive while any Session or capsule object still references them.
- Interrupted Attempts are classified incomplete and can be inspected, discarded, or retried.
- There is no selective message-content erasure in the MVP.
- There is no semantic trace compaction in the MVP.
- Rebuildable projections, indexes, and snapshots may be deleted and regenerated.

### Debug CLI Surface

The exact spelling may change before protocol freeze, but the MVP must expose equivalent operations:

```text
stcli artifact import <file.json>
stcli artifact list
stcli artifact show <id>
stcli artifact export <revision> --output <file.json>
stcli artifact compatibility <revision>

stcli provider check <profile>
stcli config effective --session <id>
stcli config update --session <id> ...

stcli session create --character <revision> [options]
stcli session list
stcli session show <id>
stcli session archive <id>
stcli session purge <id>

stcli greeting list --session <id>
stcli greeting select --session <id> <greeting>

stcli message send --session <id> <text>
stcli message continue --session <id>
stcli message regenerate --session <id>
stcli message retry --session <id> --turn <id>
stcli message swipe --session <id> [--candidate <id>]
stcli message edit --session <id> --message <id> <text>
stcli branch create --session <id> --turn <id>

stcli prompt inspect --session <id> --attempt <id>
stcli lore trace --session <id> --attempt <id>
stcli vars list --session <id>
stcli vars diff --session <id> --from <attempt> --to <attempt>
stcli plugin trace --session <id> --attempt <id>
stcli provider inspect --session <id> --attempt <id>

stcli message send --session <id> <text> --dry-run
stcli message continue --session <id> --dry-run
stcli message regenerate --session <id> --dry-run

stcli turn inspect --session <id> --attempt <id>
stcli turn export --session <id> --attempt <id> --output <capsule.json>
stcli turn export --session <id> --attempt <id> --thin --output <capsule.json>
stcli turn replay <capsule.json>
stcli turn import <capsule.json>
stcli turn rerun --session <id> --attempt <id>

stcli plugin install <directory>
stcli plugin list
stcli plugin inspect <id>
stcli plugin enable <id>
stcli plugin disable <id>
stcli plugin adopt --session <id> <id>@<digest>
stcli plugin remove <id>
stcli plugin doctor <directory>
```

### MVP Non-Goals

The following are explicitly excluded from the MVP and retained in the roadmap:

- Artifact editing or live-file synchronization
- External artifact-codec plugins
- Character Card V3 as a built-in format
- PNG, APNG, WebP, CHARX, and embedded assets
- Text Completion and Advanced Formatting
- Group chats
- Full STscript
- Quiet/background model generation
- Plugin network, model, filesystem, secret, subprocess, or native-code access
- SillyTavern JavaScript UI extensions and server plugins
- Vector/embedding lore retrieval
- Tool/function calling
- Multimodal, speech, audio, expressions, and image generation
- Rich terminal menus, mouse interaction, themes, and Nerd Font presentation
- Local daemon, multi-writer, collaborative, distributed, or cloud Sessions
- Telemetry and analytics
- Selective message erasure
- Trace compaction
- Built-in storage encryption
- Automatic provider retry
- crates.io publication of unstable internal crates
- Official macOS binaries

## 3. AI System Requirements

### Provider Requirements

The MVP contains one OpenAI-compatible Chat Completions HTTPS adapter.

Required configuration:

```toml
[provider.default]
kind = "openai-compatible"
base_url = "https://example.invalid"
chat_completions_path = "/v1/chat/completions"
model = "model-name"
api_key_env = "STCLI_API_KEY"
timeout_seconds = 120
stream = true
tokenizer = "tiktoken:o200k_base"
```

Optional static headers are allowed in configuration, but resolved secret values are not persistable. HTTPS is required in the MVP.

Supported request semantics:

- system, user, and assistant text messages;
- model;
- temperature;
- top-p;
- response-token limit;
- stop sequences;
- optional seed;
- Server-Sent Event streaming.

Unsupported request semantics include tools, multimodal parts, reasoning controls, log probabilities, vendor-specific samplers, model discovery, and automatic retries.

### Prompt Plan

Prompt construction produces a structured Prompt Plan containing:

- ordered segments;
- provider role;
- closed prompt slot;
- in-chat depth and order;
- generation trigger;
- source Artifact Revision and field;
- raw macro input and evaluated result;
- lore source and activation path;
- Plugin digest and contribution;
- truncation priority and decision;
- tokenizer ID/version and token count.

The MVP implements only Chat Completion Prompt Manager behavior in the core profile. Flat Text Completion rendering is post-MVP.

### Core Macro Manifest

The pinned compatibility source commit must produce a checked-in, versioned macro manifest. The MVP manifest covers exact names and argument behavior for:

- identity and card content: user, character, description, personality, scenario, persona, and example-message values;
- composition: original replacement, outlets, no-output/comment behavior, trimming, and supported conditionals;
- chat context values and IDs required by profile fixtures;
- local variable get/set/add/increment/decrement/flush and indexed access;
- global variable equivalents;
- local/global shorthand operators, lazy fallback, and assignment forms;
- deterministic random selection, stable pick, and dice rolls;
- recorded time, date, weekday, ISO time, and ISO date.

A parser accepting a spelling does not make the macro supported. Exact support requires a manifest entry and fixtures.

Unknown macro text remains literal. If a pinned Plugin registers that macro, it may resolve it. Otherwise execution requires explicit non-parity permission.

### Lore Engine

The core profile covers:

- global, character, persona, and chat lore sources;
- source precedence and insertion strategy;
- keyword matching;
- resource-limited ECMAScript-compatible regex semantics;
- case sensitivity and whole-word behavior;
- optional filters and selective logic;
- constant entries;
- scan depth and profile-listed additional sources;
- recursion and recursion limits;
- insertion order, closed prompt slot, role, and depth;
- outlets;
- probability and recorded RNG;
- inclusion groups, weight, priority, and scoring;
- generation triggers supported by the core profile;
- token budget and overflow;
- sticky, cooldown, and delay behavior.

The engine must never reinterpret JavaScript regex using Rust regex semantics. Unsupported regex or resource-limit failure produces a structured lore error.

Vector matching and quiet/background generation are unsupported.

### Tokenizer Registry

Token counting is part of compatibility because it controls lore budgets and pruning.

MVP requirements:

- explicit tokenizer selection;
- versioned tokenizer IDs;
- initial tiktoken support required by fixtures, including `cl100k_base` and `o200k_base` where applicable;
- one tokenizer used consistently for segment counts, lore budgets, examples, history pruning, context limit, and response reserve;
- unknown tokenizer blocks generation by default;
- explicit approximate-tokenizer permission marks the Dry Run, Attempt, trace, inspection, and capsule non-parity;
- approximate mode is prohibited in parity fixtures.

Remote and model-family tokenizer adapters are post-MVP.

### Evaluation Strategy

The engine is evaluated on request construction, state behavior, and replay, not subjective model prose quality.

#### Pinned Compatibility Corpus

The exact SillyTavern 1.18 source commit is recorded in the profile manifest and third-party notices. Synthetic fixtures cover every manifest feature with:

- positive cases;
- negative cases;
- boundary cases;
- ordering interactions;
- failure cases.

Fixture outputs include structured decisions, canonical provider request, token counts, state diff, compatibility outcomes, and expected trace facts. Release requires one hundred percent pass rate.

#### Provider Contract Server

A deterministic local HTTPS test server verifies request shape, headers, secret redaction, normal JSON, split SSE frames, usage, cancellation, non-2xx bodies, malformed response, malformed stream, timeout, and connection failure. Live-provider checks are optional diagnostics.

#### Replay Evaluation

Tests block network, disable Plugin execution, replace clock and RNG with recorded sources, apply validated recorded effects, rebuild the Session Projection, and compare the original projection hash. Missing effects or schema support fail explicitly.

#### Plugin Evaluation

The proof Plugin covers install, manifest validation, SPDX license presence, engine incompatibility, grants, denied operations, state isolation, deterministic order, cycles, resource limits, abort, failure phase, digest pinning, explicit upgrade, replay without execution, disable, and removal.

#### Protocol Evaluation

Schema fixtures cover CLI JSON, JSONL events, profile manifests, Portable and Thin Capsules, canonical JSON, content hashes, storage migrations, and WIT interfaces marked public.

## 4. Technical Specifications

### Architecture

![STcli engine architecture: a debug CLI and post-MVP TUI call a Command/Event seam into the SessionEngine, whose Turn Transaction atomically orchestrates the PromptCompiler, LoreEngine, StateStore, Wasm PluginHost, and an OpenAI-compatible Provider, backed by shared Codec and Tokenizer registries and a SQLite WAL store.](docs/diagrams/architecture.png)

<!-- Editable source: docs/diagrams/architecture.html — re-export the PNG with headless Chromium after edits. -->

### Frontend Seam

The engine exposes conceptually:

```text
execute(session_id, command) -> stream<Event>
inspect(session_id, query) -> InspectionResult
```

Concrete Rust types may use async streams. Frontends never receive mutable storage records, provider clients, plugin memory, or authoritative state references.

### Authority and Persistence

SQLite in WAL mode stores:

- authoritative Turn Trace entries;
- commands and recorded outcomes;
- Artifact Revisions and exact source BLOBs;
- Session Configuration Revisions;
- Branch, Turn, Attempt, Candidate, Selection, and Greeting facts;
- variable and plugin-state revisions;
- provider and Plugin effect receipts;
- content references;
- rebuildable Session Projections, indexes, and snapshots.

The Turn Trace is authoritative. Projections and snapshots are disposable. There is no separate NDJSON authority and no public storage-plugin interface in the MVP.

JSON source and capsule objects are content-addressed SQLite BLOBs. Purge uses transactional reference tracking and deletes only unreferenced objects. Physical SQLite maintenance may rewrite pages but cannot remove logical trace entries outside explicit purge.

Released pre-1.0 schemas receive forward migrations. Development snapshots may be explicitly reset. Migrations may change envelopes and indexes but cannot reinterpret recorded domain outcomes.

### Identity and Canonicalization

- Sessions, Branches, Turns, Generation Attempts, Candidates, Selections, commands, and events use monotonic ULIDs.
- Full ULIDs are stored and emitted; only human CLI input accepts unambiguous prefixes.
- Content objects use domain-separated SHA-256 strings in `sha256:<hex>` form.
- Artifact Revision hashing uses artifact kind, source format, and exact imported bytes.
- Semantic object, canonical provider request, and capsule hashing use RFC 8785 JSON Canonicalization Scheme.
- Persisted hashes record the domain prefix and canonicalization version.

### Turn Transaction

A live send executes:

1. validate command and selected compatibility profile;
2. record user action and create Turn;
3. create Generation Attempt ULID;
4. pin Session Configuration Revision;
5. load immutable baseline and attempt-local state overlays;
6. resolve Artifact Revisions, Greeting Selection, provider, tokenizer, and Plugin digests;
7. execute permitted pure pre-lore/pre-prompt plugin behavior;
8. evaluate lore with recorded RNG and a complete decision trace;
9. evaluate macros through attempt-local state;
10. build and prune the Prompt Plan with the configured tokenizer;
11. validate compatibility outcomes and non-parity permission;
12. apply closed-slot plugin contributions;
13. build RFC-8785 canonical provider request and SHA-256;
14. execute provider and record streaming or failure outcomes;
15. record permitted observational plugin outcomes without post-commit mutation;
16. apply pinned profile commit semantics;
17. atomically commit Candidate, Selection, state effects, provider receipt, and Attempt status;
18. update rebuildable projections.

Dry Run executes preparation through canonical request construction in disposable state but creates no Turn or Attempt and performs no live provider call or commit.

Replay substitutes validated recorded outcomes for provider, plugin, clock, and RNG effects.

### Plugin Host

The MVP uses the WebAssembly Component Model, with Wasmtime as the intended host unless an implementation spike finds a blocker.

Initial behavior is pure and deterministic:

- lifecycle observation;
- macro registration;
- plugin command registration;
- prompt-segment contribution to closed slots;
- permitted session reads;
- own-namespace state writes through the attempt overlay;
- structured pre-request abort.

Closed prompt slots:

- before character definitions;
- after character definitions;
- before example messages;
- after example messages;
- named lore outlet;
- in-chat with explicit role/depth/order;
- before history;
- after history;
- post-history instructions.

Plugins cannot mutate or delete existing segments. No network, model, filesystem, secret, subprocess, native code, arbitrary provider mutation, or post-commit state capability exists in the MVP.

Ordering is dependency topology, explicit `before`/`after` within a slot, then plugin ID. Cycles fail before Attempt creation.

Installed versions are identified by manifest ID, semantic version, and component SHA-256. Sessions pin exact digests. Upgrades are explicit configuration changes.

### Capsule Model

Capsule schema is versioned independently from storage. Required logical sections:

- schema and engine version;
- compatibility profile and feature manifest digest;
- identity and provenance;
- Artifact Revision hashes and codec versions;
- Session Configuration Revision;
- Branch, Greeting, Turn, Attempt, Candidate, and generation type;
- lore decisions;
- macro evaluations;
- variable reads/writes/coercions;
- RNG and clock outcomes;
- Plugin digests, grants, inputs, and recorded effects;
- Prompt Plan and token counts;
- redacted canonical provider request and hash;
- safe provider receipt and response;
- commit result and Session Projection hash;
- embedded objects or Thin Capsule references;
- redaction manifest;
- inspect/replay/rerun capabilities.

Portable import validates every hash, event ordering, ULID relationship, schema, effect, scope, and historical grant before creating an Imported Session.

### CLI Protocol

Non-streaming JSON envelope:

```json
{
  "schema": "stcli.cli/v1",
  "ok": true,
  "command": "prompt.inspect",
  "data": {},
  "error": null,
  "warnings": []
}
```

Streaming uses a versioned JSONL event schema. Public schema versions are stable within a major version.

### Data Paths

Linux:

```text
$XDG_CONFIG_HOME/stcli/   configuration and capability grants
$XDG_DATA_HOME/stcli/     SQLite, installed plugins, durable data
$XDG_CACHE_HOME/stcli/    rebuildable caches
```

Windows uses standard application-data locations. `STCLI_HOME` overrides all paths beneath one root for portable installations and tests.

Private directories and files use restrictive permissions where supported. There is no built-in database encryption in the MVP.

### Security and Privacy

- No telemetry, cloud sync, remote account, or automatic sharing.
- Only configured provider requests leave the process in the MVP.
- Narrative content is not moderated by STcli.
- Structural and execution validation remains mandatory.
- Secrets are resolved from environment-variable names and removed before trace creation.
- Imported JSON is size- and depth-limited.
- ECMAScript regex execution is resource-limited.
- Plugins have memory, fuel, timeout, and closed capabilities.
- Capsule imports never install code or perform live effects.
- Capsules are classified sensitive and support explicit redaction.
- Archive is not deletion. Purge is explicit and reference-aware.

### Packaging, Platforms, and Licensing

- Rust Cargo workspace with internal library crates and `stcli` binary.
- Official MVP CI and binaries: Linux x86-64 and Windows x86-64.
- macOS portability is desired but unsupported until v0.2.
- MVP source and binaries are distributed through GitHub Releases.
- Internal crates are not published to crates.io before interface stabilization.
- License: AGPL-3.0-or-later.
- Compatibility work pins the referenced SillyTavern commit, records provenance, retains required notices, and uses synthetic fixtures.
- Plugin manifests require SPDX identifiers; STcli does not make legal determinations about plugin derivation.
- STcli remains a working name. A naming review is required before the first public release.

## 5. Risks & Roadmap

### Roadmap

#### v0.2: Rich TUI and External JSON Codecs

- Rich terminal chat, candidate, prompt, lore, state, plugin, provider, and capsule views.
- Keyboard, mouse, themes, optional Nerd Font icons, and plain-glyph fallback.
- Same command/event interface; TUI owns no state.
- Expose versioned external `artifact-codec` Wasm interface.
- Use CCv3 JSON as the first serious external codec candidate.
- Add official macOS CI/binaries when platform behavior passes.

#### v0.3: Character Containers and Assets

- PNG/APNG metadata cards.
- WebP only after an explicit embedding contract.
- CHARX archives and CCv3 promotion.
- External content-addressed asset store, limits, deduplication, and safe conversion reports.

#### v0.4: Text Completion

- Flat prompt projection.
- Advanced Formatting, instruct/context templates, story strings, separators, examples, and flat-context pruning fixtures.

#### v0.5: Group Roleplay

- Multiple active characters, group greetings, reply-order strategies, group nudge, group variables/lore/plugins, and group capsules.

#### v0.6: STscript

- Parser, commands, pipes, closures, scoped variables, conditionals, limits, cancellation, and trace integration.
- UI-dependent commands remain unsupported until an appropriate frontend capability exists.

#### v0.7: Retrieval and Live Plugin Effects

- `lore-retriever` interface and embedding/vector retrieval.
- Separate threat model for plugin HTTP, model, filesystem, and secret capabilities.
- Nested/background Generation Attempts with explicit accounting and cancellation.

#### v1.0: SillyTavern JavaScript Compatibility Bridge

- Manifest compatibility reports.
- Sandboxed JavaScript subset for documented context, events, macro/command registration, settings, metadata, and prompt interceptors.
- No compatibility claim for direct mutable identity, undocumented internal imports, or unrestricted browser globals.

#### v1.x: Broader Ecosystem

- Browser frontend and best-effort DOM slots.
- Trusted sidecars for server-plugin-like behavior.
- Tool/function calling.
- Multimedia, expression, speech, audio, and image features.
- Local daemon and multiple frontends.
- Collaborative/distributed Sessions only under a separate architecture and privacy PRD.
- Cloud sync only through an explicit reversal of the local-only principle.
- Encrypted local storage only through a dedicated key-management and recovery PRD.

### Technical Risks

| Risk | Impact | Mitigation |
|---|---|---|
| SillyTavern behavior exceeds public documentation | False compatibility claims | Pin source commit, explicit core manifest, synthetic black-box fixtures, bounded claim |
| Tokenizer mismatch | Different lore and pruning | Explicit tokenizer, versioned registry, non-parity approximation only by opt-in |
| JavaScript coercion mismatch | Wrong macros and variables | Raw compatibility values, dedicated adapter, boundary fixtures |
| Regex semantic or resource mismatch | Wrong lore or denial of service | ECMAScript engine, limits, hard errors, no Rust-regex reinterpretation |
| Mutable source or settings | Historical drift | Immutable Artifact and Session Configuration Revisions |
| Split authority | Corrupt replay | SQLite Turn Trace authoritative; projections derived |
| False replay determinism | Misleading capsules | Record every live effect and digest; block live effects during replay |
| Capsule data leakage | Privacy loss | Local-only, secret exclusion, redaction manifest, capability flags, purge |
| Malicious capsule effects | State corruption | Full schema/hash/grant/order validation in isolated import |
| Wasm authority creep | Security and schedule risk | Pure MVP host; live effects deferred to separate threat model |
| Trace growth | Disk use | Reference deduplication, rebuildable snapshot deletion, explicit purge; no semantic compaction |
| Generic provider divergence | Runtime failures | Narrow contract, deterministic HTTPS server, inspectable safe errors, no hidden retries |
| Public protocol churn | Broken scripts/plugins | Independent schema versions and compatibility fixtures |
| Cross-platform differences | Release failures | Linux/Windows CI and platform path/permission tests |
| Name implies affiliation | User confusion | Unofficial notice and required pre-release naming review |
| Scope expands into a SillyTavern clone | MVP never ships | Core profile manifest, explicit non-goals, release gates, phased roadmap |

### Resolved Implementation Facts

- SillyTavern tag `1.18.0` is pinned at commit `51ad27fb86d39a3daca3adaa970375c9670c12df`.
- The core profile records 64 exact macros derived from seven pinned upstream macro-definition files.
- Rust uses edition 2024 with toolchain and pre-release MSRV `1.89.0`.
- RFC 8785 canonicalization, domain-separated SHA-256, ULID identity, CLI envelope, capsule, profile, and fixture schemas are checked in.
- The Phase 0 fixture runner distinguishes exact, preserved-metadata, documented-fallback, and hard-unsupported outcomes.
- The deterministic OpenAI-compatible HTTPS fixture server supports fixed non-streaming and SSE responses.
- SQLite WAL stores the authoritative trace, exact JSON BLOBs, immutable Artifact Revisions, Session Configuration Revisions, and rebuildable projections.
- Phase 1 built-in codecs cover Character Card V1/V2 JSON, embedded V2 character lore, standalone lorebooks, and Chat Completion prompt presets with duplicate-key rejection.
- The debug CLI imports/lists/shows/exports artifacts and creates/lists/shows/rebuilds Sessions using versioned JSON envelopes.
- Phase 2 pins provider settings and explicit tiktoken IDs in Session Configuration Revisions, builds inspectable token-counted Prompt Plans, and persists Turn/Attempt/Candidate projections.
- The OpenAI-compatible client supports HTTPS without URL userinfo, custom CA certificates, non-streaming JSON, split SSE, environment-referenced secrets, redacted error bodies and receipts, no automatic retries, and cooperative external cancellation with partial-text receipts.
- The debug CLI provides send, Dry Run, retry, cancel, Turn inspection, Prompt Plan inspection, and versioned provider event JSONL.
- Automated fixtures verify that raw or provider-echoed secrets never enter SQLite, authoritative trace JSON, or CLI serialization, and that delayed split-SSE cancellation records partial text before aborting the request.
- Phase 4 records complete attempt effect receipts, exports versioned Portable and Thin Capsules, recalculates capabilities after redaction, validates and replays capsules without live effects, imports isolated Sessions linked to capsule hashes, and supports explicit rerun.
- Session archive, reference-aware purge and content GC, and interrupted-Attempt recovery are available through the debug CLI and covered by fixtures.
- Phase 5 provides the public `plugin` WIT world and manifest schema, a no-WASI Wasmtime Component Model host, immutable version-and-digest-pinned Session grants, plugin install/inspect/adopt/upgrade/enable/disable/invoke/remove CLI commands, recorded receipt validation during capsule Replay, and an independently built proof component.
- Chat Completion preset parity unifies Turn preparation across Dry Run and live Attempts, resolves Effective Generation Settings with provenance, applies assembly-only behavior (system message squashing, system prompt gating, assistant and continuation prefill), handles in-chat depth injections and sequential prompt macro dataflow, reports machine-readable warnings for ungranted scripts and third-party directives without execution, and proves 27-case oracle parity using manifest-driven external fixtures (Nemo Engine 11.5.1 and NemoPresetExt).
- Reference benchmarks use Criterion in `crates/stcli-core/benches/engine.rs` with the example character card, lorebook, and preset as the realistic corpus, plus a synthetic 200-entry lorebook and 40-message chat history for scaled evaluation. Reference hardware is AMD Ryzen 9 7900X (12C/24T), 64 GiB DDR5, Linux 7.1.7-zen, Rust 1.89, `bench` profile. Baseline medians: artifact decode 4–17 µs, lore evaluation 3.5 µs (4 entries) / 332 µs (200 entries), macro rendering 0.8–3.4 µs, prompt assembly 48 µs (10 turns) / 299 µs (100 turns), pruning 11 µs (100 turns), dry run 309 µs (empty session with character + lorebook + preset).

Changes to an accepted principle, compatibility claim, authority model, security boundary, or release gate require a PRD revision and, where applicable, an ADR superseding the original decision.
