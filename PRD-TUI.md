# STcli Rich Terminal UI Product Requirements Document

**Status:** Draft
**Parent PRD:** [`PRD.md`](PRD.md) (STcli Roleplaying Engine)
**Milestone:** v0.2 — Rich TUI (replaces v0.2 roadmap entry in parent PRD)
**Prerequisite:** v0.1 MVP (debug CLI) shipped
**Framework:** ratatui
**Binary:** `stcli tui` subcommand of the existing `stcli` binary
**License:** AGPL-3.0-or-later (inherits from parent)

This document owns the presentation layer, distribution, and TUI-specific configuration. Engine-side changes discovered during TUI work (new events, new inspection queries) feed back into the parent PRD.

## 1. Executive Summary

### Problem Statement

The debug CLI is designed for scripting and automation. Its JSON-oriented output and subcommand surface are powerful for diagnostics but poorly suited for interactive roleplay. Console-comfortable power users — content creators, prompt tuners, and developers who prefer visual exploration — have no way to use the engine without composing CLI commands by hand or writing scripts.

### Proposed Solution

A rich terminal UI launched via `stcli tui` that provides an interactive chat experience over the existing engine command/event interface. The TUI is a presentation layer: it calls `execute()` and `inspect()`, renders events, and owns no authoritative state. It starts as an interactive chat client with session browsing, streaming responses, and candidate navigation (swipes), with inspection views and layout customization deferred to later versions.

### Product Principles

1. **The TUI owns no state.** All authoritative state lives in the engine. The TUI renders Session Projections and dispatches commands through the existing frontend seam. (Inherits parent Principle 7.)
2. **Chat first.** The primary interaction is reading and writing roleplay messages. Every other feature is secondary to the chat flow.
3. **Non-intrusive errors.** Errors and diagnostics appear as transient notifications, never interrupting the conversation flow.
4. **Discoverable without a manual.** Context-sensitive keybinding hints are always visible. Popups use a consistent interaction pattern.
5. **Two input vocabularies.** Arrow keys and vim-style keys both work. Mouse assists but is never required.

### Success Criteria

1. **Interactive roleplay loop:** A user can browse sessions, open one, read the conversation, send a message, receive a streamed response, swipe through candidates, regenerate, continue, stop mid-stream, switch branches, and change provider/preset — all without touching the debug CLI.
2. **Visual clarity:** Messages are rendered with markdown formatting, role-tagged blocks, and graphic separators. Light and dark terminals produce readable output without user configuration.
3. **Consistent interaction model:** Provider/preset switching, branch navigation, and help all use the same popup overlay pattern.
4. **Graceful degradation:** Plain Unicode glyphs render correctly on any modern terminal. No Nerd Font dependency.

## 2. User Experience & Functionality

### User Personas

#### Primary: Console Power User

A roleplay user comfortable with the terminal who prefers a dedicated TUI over a browser interface. They write character cards and prompt presets, tune generation settings, and want to see how their content assembles. They know the domain (characters, lorebooks, presets, swipes) but do not necessarily read Rust code or write shell scripts.

#### Secondary: Developer

A developer who uses the debug CLI for automation but wants a visual interface for interactive testing and exploratory debugging. The TUI supplements the CLI rather than replacing it.

### UI Terminology

The TUI uses user-facing labels that may differ from engine terminology defined in `CONTEXT.md`. This is intentional: UI labels optimize for recognition, engine terms optimize for precision.

| TUI Label | Engine Term | Notes |
|---|---|---|
| Swipe | Candidate navigation | `← 2/5 →` indicator on messages |
| Chat | Session (on a Branch) | The conversation as displayed |
| Message | Turn (user) / Candidate (assistant) | Individual chat entries |
| Greeting | Greeting Selection | First assistant message, card-authored |

The TUI never invents domain concepts. Every UI action maps to an engine command.

### Core User Flow

1. Launch `stcli tui` or `stcli tui <session-id>` to jump directly to a session.
2. Browse sessions in a sortable, filterable table showing name, dates, turn count, character, and token count.
3. Select a session to open the chat view.
4. Read the conversation with markdown-rendered, role-tagged messages and graphic separators.
5. Type a message in the entry field and press Enter to send.
6. Watch the streamed response arrive token-by-token (or wait for completion if streaming is disabled in the preset).
7. Swipe through candidates on the last message with `← 2/5 →` navigation. Swiping past the last candidate generates a new one.
8. Stop generation mid-stream (partial output is kept) or continue a completed response.
9. Switch branches via the branch picker popup.
10. Switch provider or preset via the provider/preset picker popup.
11. Copy a message to the clipboard via keybinding.
12. Press `q` to quit (with confirmation if generation is active).

### Screen Inventory

#### Session Browser (Home Screen)

A full-width table listing all sessions. Columns:

- Session name / ID
- Created / last modified
- Turn count
- Character / persona name
- Token count

Interaction:

- Arrow keys or `j`/`k` to navigate rows.
- Enter to open the selected session in chat view.
- Column sort: cycle sort key with a keybinding.
- Fuzzy text filter: type to narrow the list.
- `P` opens the Prompt Preset picker. `Enter` or `Esc` closes it; `n` starts a new Session with the highlighted preset.

#### Chat View (Primary View)

The main interaction screen. Layout:

- **Top bar:** "STcli" app name in the top left corner. Current session name and character.
- **Chat area:** Scrollable conversation history. Messages are role-tagged blocks (`[Character Name]` / `[You]`) separated by graphic separators (exact separator determined during implementation). Rich markdown rendering (bold, italic, code blocks). Greeting is visually distinguished from generated messages.
- **Entry field:** Single-line text input at the bottom of the chat area. Enter sends, Shift+Enter inserts a newline.
- **Bottom bar (line 1):** Context information — session name, character, provider/model, branch indicator.
- **Bottom bar (line 2):** Context-sensitive keybinding hints.

Scrollback: smart scroll — auto-scrolls to bottom when already at the bottom, freezes when the user has scrolled up. Full session history loads.

#### Swipe Indicator

When the current message has multiple candidates, an inline indicator appears: `← 2/5 →`. The message content swaps in place. Navigation past the last candidate triggers a new generation attempt (swipe-to-regenerate).

Greeting swipe uses the same `← 1/3 →` mechanism.

#### Popup Overlays

A consistent interaction pattern for secondary actions:

- **New Session Modal:** Keystroke `n` on the home screen opens a single-page modal for configuring a session: Character selection, Provider profile, Prompt preset, Persona name (default `"User"`), Persona description (optional), and Initial Greeting. The Prompt preset field includes `<Import preset...>` and resumes with a successfully imported preset selected. Submitting creates the session and immediately transitions to the Chat view.
- **Provider Profile Creator:** Form modal for creating and persisting new provider connection profiles into `config.toml`. Includes template selector steered by `provider-templates.toml`, fields for URL, Model, Chat completions path, API key environment variable (with live env detection), and stream toggle.
- **Artifact Import Dialog:** Reusable form with `~`-expanding Path input, expected Artifact kind, and an optional Name input for Prompt Presets. `Tab`, `Down`, and `Up` move among the Path, Name, and directory-list controls. A supplied preset name is written to `preset_name`; otherwise import resolves the label from embedded `preset_name`, embedded `name`, or the filename stem. A kind mismatch preserves the form, writes nothing, and reports the detected and expected kinds before returning to New Session or the Prompt Preset picker.
- **Provider/Preset Picker:** The Prompt Preset picker opens with `P` from Sessions or Chat. It supports `i` import, `/` substring filtering, and a `d`/`Tab` side-by-side inspector for prompt order, Compatibility Warnings, generation parameters, and inert embedded scripts. When the inspector is open, `Right` enters the Prompt Order Entry list, `Left` returns to preset selection, and `Space` immediately edits the preset-level entry. In Chat, `Ctrl+Space` creates a Session Prompt Order Override, entries label their effective source as `preset` or `override`, and `r` resets an override to the preset default. Nemo exclusivity constraints auto-disable enabled siblings in the same immutable revision and name them in the toast. `PgDn`/`PgUp` scroll the inspector, `Shift+Down`/`Shift+Up` scroll line by line, and the mouse wheel scrolls it; the scroll position clamps to content bounds and resets when the selected preset changes.
- **Branch Picker:** Keystroke opens a modal overlay listing branches in the current session. Select one to switch.
- **Help Overlay:** `?` opens a modal overlay showing all keybindings for the current context. Escape to dismiss.

All popups follow the same visual pattern: modal overlay, list/form navigation, Enter to select/submit, Escape to dismiss.

### Interaction Model

**Keyboard:** Hybrid scheme. Arrow keys, Enter, Escape, and Ctrl+key combos work out of the box. Vim-style keys (`j`/`k`, `g`/`G`, `/` for search) are available in parallel. No modal switching.

**Mouse:** Assisted. Clicking items, panes, and buttons works. Mouse is never required for any action. Scroll wheel works in scrollable areas.

**Streaming:** Controlled by the `stream` setting in the provider/preset configuration. When enabled, tokens render incrementally. When disabled, a loading indicator shows until the full response arrives.

**Stop/Continue:**

- During active generation, a keybinding stops the stream. Partial output is kept as a candidate with `accepted-partial` origin.
- After a completed response, a keybinding sends a continue command for the same turn.

**Clipboard:** A keybinding copies the currently focused message content to the system clipboard.

**Exit:** `q` or Ctrl+C quits immediately unless generation is actively streaming, in which case a confirmation prompt appears.

### Notifications

Errors (provider failures, network timeouts, rate limits) appear as transient toast notifications in a dedicated notification area. Toasts auto-dismiss after a configurable interval. They do not interrupt the chat flow or insert into the conversation history.

## 3. Technical Specifications

### Architecture

The TUI is a presentation layer consuming the engine's frontend seam:

```text
stcli tui
  └── TUI Application (ratatui)
        ├── Session Browser ──► inspect(query: list_sessions)
        ├── Chat View ──────► inspect(session_id, query: projection)
        ├── Send ────────────► execute(session_id, message_send) -> stream<Event>
        ├── Regenerate ──────► execute(session_id, regenerate) -> stream<Event>
        ├── Continue ────────► execute(session_id, continue) -> stream<Event>
        ├── Stop ────────────► execute(session_id, cancel)
        ├── Swipe ───────────► execute(session_id, swipe)
        ├── Branch Switch ───► execute(session_id, switch_branch)
        └── Config Change ──► execute(session_id, config_update)
```

The TUI owns rendering, input handling, and layout. It holds no authoritative session data. All state queries go through `inspect()`; all mutations go through `execute()`.

### Crate Structure

The TUI lives in a new workspace crate:

```text
stcli-tui/
  src/
    main.rs          (or integrated into stcli-cli behind a subcommand)
    app.rs           (application state, event loop)
    views/
      session_browser.rs
      chat.rs
    widgets/
      message.rs     (markdown rendering, role tags, separators)
      swipe.rs       (candidate indicator and navigation)
      entry.rs       (text input field)
      popup.rs       (modal overlay framework)
      toast.rs       (notification area)
    input.rs         (key/mouse mapping)
    theme.rs         (light/dark detection, color tokens)
    clipboard.rs     (system clipboard integration)
```

Whether this is a separate crate or a module behind a feature flag in `stcli-cli` is an implementation decision. The `stcli tui` subcommand is the public interface regardless.

### Dependencies

- **ratatui** — terminal UI framework (immediate-mode rendering)
- **crossterm** — terminal backend (cross-platform input/output)
- **tui-textarea** or equivalent — text input widget (if not hand-rolled)
- **pulldown-cmark** or equivalent — markdown parsing for message rendering
- Clipboard crate TBD during implementation

### Configuration

TUI-specific settings live in a `[tui]` section of the existing STcli configuration file:

```toml
[tui]
# theme = "auto"        # "auto" | "light" | "dark"
# nerd_font = false      # opt-in Nerd Font icons (deferred)
# toast_timeout = 5      # seconds before toast auto-dismiss
```

The `[tui]` section starts minimal and grows as features are added. Settings that affect engine behavior (provider, preset, streaming) remain in their existing configuration sections.

### Theming

Two built-in themes: light and dark. Auto-detection from terminal background is the default. No user-defined theme files in the initial release.

Color tokens are defined as named constants in the theme module, making future theme customization a matter of loading alternate token sets.

### Rendering

**Markdown:** Messages are parsed and rendered with terminal-appropriate styling:

- Bold → terminal bold
- Italic → terminal italic (or dim on terminals without italic support)
- Code spans → highlighted inline
- Code blocks → bordered region with syntax indication
- Headers, lists, blockquotes → indented/styled appropriately
- HTML content → stripped or rendered as plain text (full HTML rendering deferred)

**Message blocks:** Each message is a tagged block:

```
[Character Name]
─────────────────────────────
Message content with **markdown** rendering.

[You]
─────────────────────────────
User message content.
```

Exact separator character and styling determined during implementation based on visual testing.

**Greeting tag:** Greetings render with a visual distinction (e.g., a `[Greeting]` sub-tag or different separator style) to signal they are card-authored, not generated.

### Performance

- Full session history loads on open. Lazy widget rendering handles long sessions without rendering all messages to the terminal buffer simultaneously.
- Streaming tokens render incrementally without full-screen redraws (ratatui's immediate-mode diffing handles this).
- Session browser table handles hundreds of sessions without perceptible lag.

## 4. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Markdown rendering edge cases | Garbled display | Strip unsupported constructs gracefully; degrade to plain text |
| Terminal compatibility | Broken rendering on some terminals | Test on major terminals (kitty, alacritty, wezterm, Windows Terminal, iTerm2); crossterm abstracts most differences |
| Streaming + TUI event loop contention | Dropped input during generation | Separate async tasks for input and event processing; ratatui's event loop handles this pattern |
| Clipboard access varies by platform | Silent failure | Graceful fallback with toast notification; document platform requirements |
| Large session rendering | Slow scroll, high memory | Virtual/lazy rendering — only visible messages are fully rendered |
| Theme auto-detection unreliable | Wrong colors | Explicit `theme` config override; two themes are a small surface to test |

## 5. Deferred Features

The following are explicitly excluded from the initial TUI release and tracked for future versions:

### Layout & Navigation

- **Dynamic panel hiding / fullscreen toggle.** Allow hiding panels and expanding a single view (e.g., chat) to fill the screen.
- **Session tabs.** Multiple sessions open simultaneously with keybinding-based tab switching.
- **Inline branch markers.** Visual markers at branch divergence points in the chat, allowing inline branch switching.
- **TUI state persistence.** Remember last session, sort order, and layout between launches.

### Inspection Views

- **Prompt view.** Full prompt as sent to the provider — system prompt, context, assembled messages, token counts.
- **Lore view.** Lorebook entries, activation decisions, and which entries contributed to the prompt.
- **State view.** Session state — variables, settings, overrides at a point in time.
- **Plugin view.** Plugin execution trace — which plugins ran, what effects they produced.
- **Provider view.** API call details — model, parameters, token usage, timing, raw request/response.
- **Capsule view.** Capsule export/import interface — preview Turn Capsules, export Portable or Thin Capsules, and inspect capsule contents and capability flags.

### Content & Rendering

- **HTML rendering in chat.** Parse and render HTML content in messages beyond plain-text stripping.
- **Nerd Font icon support.** Opt-in Nerd Font icons alongside plain Unicode glyphs, toggled via config.
- **User-defined themes.** Custom theme files with named color tokens.

### Interaction

- **Message editing.** Edit previous messages (user or assistant) in the chat view, with branch implications.
- **Interactive file tree browser.** Graphical directory tree browser for character cards and assets.

## 6. Resolved Design Decisions

These decisions were made during the design phase and are recorded here for context. Architectural Decision Records (ADRs) are created only when a decision is hard to reverse, surprising without context, and the result of a genuine trade-off.

| Decision | Choice | Rationale |
|---|---|---|
| Framework | ratatui | Community standard; immediate-mode pairs with command/event architecture |
| Binary packaging | `stcli tui` subcommand | One binary to distribute; discoverable |
| Layout | Hybrid (fixed scaffold + popup overlays) | Natural for list→detail domain; start simple |
| Home screen | Full-width session table | Information-dense; familiar pattern |
| Primary view | Chat with entry field | Chat-first principle; this IS the interactive experience |
| Entry field | Single-line, Enter sends | Simple; roleplay messages are usually short-to-medium |
| Message rendering | Rich markdown | RP content uses formatting heavily |
| Message structure | Tagged blocks with separators | Terminal-space efficient; clean |
| Swipe UX | Inline replacement with indicator | Matches SillyTavern mental model |
| Swipe-to-regenerate | Past last candidate generates new | ST-compatible; natural extension of browsing |
| Streaming | Default on, preset-controlled toggle | User expectation in 2026; preset setting for control |
| Error display | Toast notifications | Non-intrusive; keeps chat flow unbroken |
| Keyboard scheme | Hybrid (vim + conventional) | Broad audience; no modal switching |
| Theme | Light/dark auto-detect | Covers 90% of users; minimal config |
| Icons | Plain Unicode first | Universal; no font dependency |
| Branch navigation | Popup picker | Consistent with other popups; simple |
| Sessions open | One at a time | Avoids state management complexity |
| Config location | `[tui]` section in existing config | One config file; consistent |
| State persistence | None | Fresh launch is simple and predictable |
| Exit behavior | Confirm only during active generation | Don't nag; protect against accidental data loss |
| Clipboard | Keybinding to copy message | Terminal selection is clumsy with TUI rendering |
| UI terminology | Differs from engine terms where appropriate | "Swipe" is the right UX word; "Candidate" is the right engine word |
| Engine changes | Feed back to parent PRD | TUI PRD owns presentation; engine PRD owns engine |
