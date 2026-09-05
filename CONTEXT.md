# STcli Roleplaying

> The canonical terminology dictionary. Part of the [STcli documentation](docs/README.md). For system design, see [`ARCHITECTURE.md`](ARCHITECTURE.md).

STcli runs local, branchable roleplay sessions from versioned content while preserving and explaining SillyTavern-compatible behavior.

## Language

**Artifact**:
Imported roleplay content, such as a character card, lorebook, or prompt preset.
_Avoid_: Asset, document, file

**Artifact Revision**:
An immutable snapshot identified by its artifact kind, source format, and exact imported bytes. Re-importing changed or reformatted content creates a new revision.
_Avoid_: Live file, current file, semantic version

**Session**:
The durable container for one roleplay, including all of its branches.
_Avoid_: Chat, conversation

**Session Configuration Revision**:
An immutable set of behavior-affecting selections used by future turns in a session. Every generation attempt pins one revision.
_Avoid_: Global settings, current configuration

**Effective Generation Settings**:
The immutable generation settings used by a Generation Attempt, resolved from explicit Session configuration over the selected prompt preset over Compatibility Profile defaults.
_Avoid_: Preset settings, provider defaults, current settings

**Duplicated Session**:
An independent session created by copying the recorded history of one branch from a source session, with no ongoing link to it.
_Avoid_: fork, clone, copy, child session, alternate session.

**Imported Session**:
An isolated session created by replaying a portable capsule.
_Avoid_: Merged session, restored original session

**Branch**:
One linear message history within a session.
_Avoid_: Child session, alternate session

**Greeting**:
A card-authored assistant message that opens a branch before its first turn.
_Avoid_: First turn, generated candidate

**Greeting Selection**:
The greeting currently active on a branch.
_Avoid_: Greeting candidate, opening turn

**Turn**:
A recorded user action with zero or one selected candidate. Failed, cancelled, or incomplete turns may have no selection.
_Avoid_: Message, generation

**Generation Attempt**:
One provider execution with pinned configuration, effective settings, request and response hashes, status, usage, and receipts. A **Primary Attempt** belongs to a Turn and may create/select a Candidate. A **Background Attempt** belongs to a Session and Branch, links to its initiating Attempt and caller, and never creates a Turn, Candidate, or Selection.
_Avoid_: Turn, request, background turn

**Dry Run**:
A pure preview of turn preparation that builds compatibility decisions and a provider request without creating a generation attempt, calling the provider, or committing state.
_Avoid_: Failed attempt, replay, test turn

**Candidate**:
An assistant response variant for a turn whose content preserves the accepted raw text before presentation-only transformations. Its origin is generated, continued, manual, or explicitly accepted from partial output.
_Avoid_: Swipe, rendered response, attempt, greeting

**Candidate Rendering**:
A rebuildable presentation of a Candidate produced by pinned transformation rules without changing the Candidate content.
_Avoid_: Candidate, provider response, rewritten candidate

**Selection**:
The candidate currently active for a turn on a branch.
_Avoid_: Latest response, current swipe

**Turn Trace**:
The authoritative history of commands and recorded outcomes that occurred during a session.
_Avoid_: Log, session state

**Session Projection**:
A rebuildable view of a session derived from its turn trace.
_Avoid_: Source of truth, mutable session state

**Turn Capsule**:
An immutable export derived from a turn-trace slice and the content required to explain or replay it.
_Avoid_: Session save, live trace

**Portable Capsule**:
A self-contained turn capsule that embeds all non-redacted content required for replay.
_Avoid_: Session archive, thin capsule

**Thin Capsule**:
A turn capsule that references content already present in the local store.
_Avoid_: Portable capsule

**Replay**:
Reconstruction from recorded outcomes without calling providers, plugins, clocks, or other live effects.
_Avoid_: Retry, rerun, regenerate

**Rerun**:
A new generation attempt that submits a previously recorded provider request again.
_Avoid_: Replay, retry

**Compatibility Profile**:
A named, versioned set of observable behavior that STcli reproduces for an external system, initially `sillytavern-1.18-core`.
_Avoid_: Compatibility mode, latest behavior, unqualified SillyTavern compatibility

**Parity**:
Agreement with the observable behavior required by a compatibility profile. Serialization details without semantic effect are excluded.
_Avoid_: Byte identity, approximate compatibility

**Compatibility Warning**:
A non-blocking diagnostic for behavior that the selected Compatibility Profile permits but that is contradictory, risky, or likely accidental. It remains visible to CLI and future UI consumers without changing the behavior.
_Avoid_: Validation error, automatic repair, blocked configuration

**Prompt Order Entry**:
An entry in a prompt preset's prompt order that binds a prompt identifier to an enabled flag. Turn preparation assembles the prompt from enabled entries in order; disabled entries are excluded.
_Avoid_: Prompt slot, toggle, checkbox

**Prompt Order Override**:
A Session Configuration Revision entry that replaces one Prompt Order Entry's preset-level enabled flag for that Session. An absent override inherits the pinned preset value.
_Avoid_: Preset edit, global toggle, Branch override

**Preset Script Grant**:
Explicit authorization for the exact digest of transformation scripts embedded in a prompt preset. Importing or selecting the preset does not grant execution.
_Avoid_: Plugin grant, trusted preset, implicit authorization

**Plugin**:
A capability-limited, sandboxed Wasm module that contributes declarative behavior to the engine without directly mutating engine state. Sessions pin exact component digests.
_Avoid_: Extension, add-on, native extension

**Extension**:
A SillyTavern JavaScript extension run headless by STcli through a compatibility bridge. It observes a read-only view of session state and influences a turn only through sanctioned surfaces. Distinct from a Plugin. Out of scope for the MVP; targeted by the v1.0 compatibility bridge.
_Avoid_: Plugin, add-on, native extension, Runtime Extension

**Logical Deletion**:
A tombstone event appended to the Turn Trace that hides a Turn, Candidate, or Branch from the Session Projection without physically removing it from the database.
_Avoid_: Archive, physical deletion

**Compaction**:
A physical, session-wide garbage collection that permanently removes logically deleted entities from the database, provided they have no active descendants.
_Avoid_: Purge, partial deletion

**Hidden State**:
A flag on an active Turn or Candidate indicating it should be skipped by the Prompt Builder during generation, while remaining visible in the Session Projection (and UI). Hidden entities survive compaction.
_Avoid_: Deleted turn, tombstoned turn

**Credential Store**:
The platform-native secure storage facility (such as the OS keyring, Secret Service, or Keychain) used to store and retrieve API secrets without persisting plaintext tokens in configuration or database tables.
_Avoid_: Vault, password manager, keystore

**Credential Reference**:
An alias string (`credential_key`) configured on a provider profile that points to a secret stored in the platform Credential Store.
_Avoid_: API key, vault key, secret name

