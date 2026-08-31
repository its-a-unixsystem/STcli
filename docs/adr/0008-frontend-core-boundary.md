# Frontend–Core boundary and four-tier taxonomy

STcli classifies every crate and external consumer into one of four tiers: **Core**, **Gateway**, **Interactive Frontend**, and **Headless Consumer**. The engine command/query seam (`StcliEngine`) is the recommended integration point for all tiers, but `Store` remains accessible for specialized tooling. Core returns raw strings after semantic transformations; frontends own all presentation rendering.

## Tiers

| Tier | Crate / consumer | Owns | Does not own |
|---|---|---|---|
| **Core** (`stcli-core`) | Engine library | Domain logic, storage, prompt construction, provider communication, display script execution, Turn Trace, configuration parsing (providers, credentials, limits, compatibility) | Rendering, UI, user interaction, terminal/browser/mobile concerns |
| **Gateway** (`stcli-cli`) | CLI binary | Scriptable command interface, `CliEnvelope` JSON output, fixture verification, provider-test server, debugging/inspection commands, hidden `internal` plumbing commands for tooling authors | Interactive sessions, real-time streaming display, visual layouts |
| **Interactive Frontend** (`stcli-tui`, future GUI/browser/mobile) | TUI, browser app, mobile app | Interactive UX, real-time streaming display, candidate presentation (markdown→ANSI, HTML→DOM, etc.), session navigation, keybindings/gestures, frontend-specific config (theme, layout) | Domain logic, prompt construction, provider communication |
| **Headless Consumer** | CI scripts, backup tools, export pipelines | Consuming `CliEnvelope` JSON, reading `Store` for bulk operations (backups, migrations) | Interactive display, generation orchestration |

## Engine seam

`StcliEngine::execute` and `StcliEngine::inspect` are the recommended integration boundary. All mutations that affect the Turn Trace must go through engine commands.

`Store` and its methods remain `pub` because specialized tooling (database backups, migrations, third-party plugins, community utilities) has legitimate reasons to bypass the engine. Module-level documentation warns that direct `Store` use bypasses Turn Trace guarantees. This is a convention, not an enforcement boundary.

## Content contract

Core applies semantic transformations (display script regex, macro expansion) and returns raw strings. Core is format-agnostic: it does not parse markdown, strip HTML, or produce terminal escape sequences.

Each frontend converts the raw string to its native presentation format: ratatui widgets for TUI, DOM nodes for browser, native views for mobile, plain text for headless. If AI-produced content contains HTML and the frontend is a terminal, the frontend calls its own `html_to_text` conversion. If the frontend is a browser, it renders the HTML natively.

## Configuration split

Core owns any configuration that affects prompt generation, Turn Trace reproducibility, provider networking, artifact storage, or credential management. Frontends own display-only preferences: themes, keybindings, toast timeouts, layout panels, font sizes.

Both may live in the same `config.toml` file, parsed independently: core parses `[providers]` and domain sections; each frontend parses its own section (e.g., `[tui]`).

## CLI role

The CLI is a gateway and plumbing layer, not a roleplay frontend. It exposes the full `EngineCommand`/`EngineQuery` surface as subcommands plus hidden `internal` plumbing commands for tooling authors. It does not implement interactive roleplay workflows (real-time swiping, visual streaming, session navigation UX). The TUI is the initial interactive frontend for power users; future GUI/browser/mobile frontends target non-technical users.

## Deferred decisions

- **Shared frontend crate**: No `stcli-frontend-common` until a second interactive frontend materializes and the actual shared surface is visible. Premature extraction would guess at the interface.
- **WASM compilation target for core**: Core stays native-only. A browser frontend communicates with a running daemon or server. If browser-native becomes a priority, the long-term path is extracting pure-computation modules (prompt construction, macro engine, lore engine, tokenizer) behind trait seams with pluggable I/O, but that abstraction is not justified today.
