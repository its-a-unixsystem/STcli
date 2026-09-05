# STcli Layered Plugin System PRD

**Status:** Detailed design companion to [`PRD.md`](../PRD.md)  
**Document version:** 0.2.0  
**Target milestones:** v0.6 (Scripting Runtime), v0.7 (Brokered Live Effects), v1.0 (Extension Bridge), v1.x (Webviews & Sidecars)  
**Parent document:** [`PRD.md`](../PRD.md)  
**Architecture references:** [`ARCHITECTURE.md`](../ARCHITECTURE.md), [ADR 0006](adr/0006-layered-plugins-and-brokered-effects.md)  

---

## 1. Executive Summary & Problem Statement

### 1.1 Context
STcli's MVP provides a capability-limited, pure WebAssembly Component Model plugin host ([`crates/stcli-core/src/plugin.rs`](../crates/stcli-core/src/plugin.rs)). This design guarantees 100% deterministic offline replay for prompt construction and state transitions ([ADR 0003](adr/0003-pure-wasm-plugins.md)), but on its own it presents limitations for the broader ecosystem:

1. **High Authoring Friction**: Requiring authors to write Rust or C and compile to Wasm Component Model bytecode is disproportionate for casual gameplay scripts (e.g. dice rolling, in-game clocks, simple RPG stat trackers).
2. **Missing UI Seams**: Real-world roleplay behavior (e.g. SillyTavern Roadway, Stepped Thinking, Megumin Suite, Character Expressions) requires configuration dialogs, top-bar buttons, multi-step setup wizards, and custom visual elements.
3. **Multi-Frontend Reality**: SillyTavern extensions assume a single Chromium browser with unrestricted jQuery DOM access. STcli supports headless CLI, terminal TUI, and future Web/Desktop interfaces. Terminal environments cannot execute arbitrary browser DOM scripts.
4. **Heavy Engine Customization**: Protocol-level expansions (e.g. non-OpenAI provider transports, external tokenizers) need high-performance engine hooks with controlled capabilities.

### 1.2 Proposed Solution
A **Plugin** is the native extensibility unit. It evolves from a single pure Wasm sandbox into a native package that may declare several **capability layers**, superseding ADR 0003 for the post-MVP roadmap ([ADR 0006](adr/0006-layered-plugins-and-brokered-effects.md)):

- **Engine Hook (Wasm Component Model)** — high-performance, compiled modules for engine-level protocols, custom provider transports, and codecs.
- **Plugin Script (QuickJS)** — zero-compile, interpreted scripting embedded in `stcli-core` for turn lifecycle hooks, prompts, macros, and state management.
- **UI Contribution (Dual-Mode)** — universal declarative form schemas (`settings.schema.json`) for CLI and TUI, paired with sandboxed Webviews (`<iframe>`) for Web and Desktop frontends.
- **Unified Package (`.stplugin` / `.zip`)** — a single distribution format containing any combination of layers, installable by non-technical users.
- **Explicit Capability-Gated Security** — installation-time permission grants, isolated namespaces, sandboxed runtimes, and brokered secret proxying.

The distinction between a native **Plugin** and a SillyTavern **Extension** is kept: an Extension is a SillyTavern JavaScript UI extension or server plugin, reproduced by the v1.0 compatibility bridge on the same QuickJS runtime.

---

## 2. System Architecture

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                          Plugin Bundle (.stplugin)                          │
│ ┌──────────────────────┐ ┌───────────────────────┐ ┌──────────────────────┐ │
│ │     Engine Hook      │ │     Plugin Script     │ │    UI Contribution   │ │
│ │   (Wasm Component)   │ │       (QuickJS)       │ │ (Declarative/Webview)│ │
│ └──────────┬───────────┘ └───────────┬───────────┘ └──────────┬───────────┘ │
└────────────┼─────────────────────────┼────────────────────────┼─────────────┘
             │                         │                        │
             ▼                         ▼                        ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                             stcli Host Core                                 │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                     Host Capability Broker                            │  │
│  │  - Secondary Inference Proxy (Named Provider Profiles)                │  │
│  │  - Brokered HTTPS Egress (wasi:http / fetch with domain whitelist)    │  │
│  │  - Out-of-Band Secret Injection (No raw keys in plugin memory)        │  │
│  │  - Isolated Namespaced Storage (local:<plugin_id>.*)                  │  │
│  │  - Monotonic Clock & Seeded Deterministic PRNG                        │  │
│  └───────────────────────────────────┬───────────────────────────────────┘  │
│                                      │                                      │
│                                      ▼                                      │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │                  Authoritative SQLite Turn Trace                      │  │
│  │   Records all secondary attempts, prompt mutations, and HTTP receipts │  │
│  │   Enables 100% offline, bit-for-bit replay without network access     │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Capability Layers

### Engine Hook (Wasm Component Model)
Targeted at core developers and systems integrators requiring wire-speed processing, protocol transformation, or custom codecs.

- **Technology**: Wasm Component Model compiled from Rust, C/C++, or Go.
- **Sandboxing**: Wasmtime engine with explicit capabilities via WIT definitions (`wit/plugin.wit`).
- **Use Cases**:
  - Custom provider transports (e.g. streaming Anthropic Messages or Google Vertex protocols natively).
  - External artifact codecs (e.g. Character Card V3, CHARX decompression).
  - High-throughput tokenizer approximations and embeddings.

### Plugin Script (Interpreted QuickJS)
Designed for gameplay mechanics, lore manipulation, and narrative scripting without requiring a compiler.

- **Technology**: A single sandboxed QuickJS runtime embedded in `stcli-core` (via `rquickjs`), capitalizing on existing SillyTavern community familiarity with JavaScript. The same runtime later hosts STscript as a command set (v0.6) and the sandboxed SillyTavern Extension bridge (v1.0).
- **Developer Workflow**: Authors write plain script files (e.g. `script.js`) with zero build tools or toolchain installations.
- **Execution Lifecycle**:
  - Subscribes to turn lifecycle hooks: `preLore`, `prePrompt`, `preRequest`, `postCommit`, and custom user commands.
  - Injects content into engine-defined closed slots (`PromptSlot`).
  - Manages attempt-local and persistent state isolated to `local:<plugin_id>.*`.
- **Runtime Sandboxing**:
  - Standard OS APIs (`node:fs`, `node:child_process`, raw sockets) are absent from the runtime environment.
  - Deterministic RNG and monotonic timestamps are brokered through the engine.

### UI Contribution (Dual-Mode)
Reconciles the fundamental difference between terminal character matrices and graphical browser DOMs.

#### A. Universal Declarative UI (CLI & TUI) — primary
- **Settings & Config Dialogs**: Driven by standard JSON Schema (`settings.schema.json`).
  - **TUI (Ratatui)**: Automatically renders interactive modals with checkboxes, sliders, input fields, and dropdowns.
  - **CLI**: Automatically validates inputs for `stcli config set --plugin <id> <key>=<value>`.
- **Contribution Actions**: Declared in `manifest.json` under `ui.actions` (e.g. buttons with hotkeys or icons).
  - Rendered in TUI bottom bars, command palettes, or popup menus.

#### B. Sandboxed Webviews (Web & Desktop) — graphical fallback
- **Arbitrary Rich UI**: The package bundles `ui/index.html`, CSS, and client-side JavaScript.
- **Execution Isolation**: Runs in a sandboxed `<iframe>` with restricted permissions (e.g. no top-level navigation). Webview code is frontend presentation only; it never participates in prompt construction or replay.
- **Engine RPC Bridge**: Communicates with `stcli-core` over a typed JSON messaging channel (`window.stcli.invokeCommand(...)`, `window.stcli.onEvent(...)`).
- **TUI Graceful Degradation**: If a Plugin defines only graphical Webviews without declarative fallbacks, the TUI displays a badge indicating graphical-mode availability.

---

## 4. Package Format & Distribution

### 4.1 Archive Structure (`.stplugin` / `.zip`)
A Plugin is distributed as an unpacked directory or a standard zip archive (optionally using the extension `.stplugin`) that `stcli plugin install` unpacks and verifies:

```text
package.stplugin (zip)
├── manifest.json            # Required: Metadata, permissions, entrypoints
├── settings.schema.json     # Optional: Form schema for universal settings
├── script.js                # Optional: Plugin Script (QuickJS)
├── hook.wasm                # Optional: Engine Hook (Wasm Component Model)
├── ui/                      # Optional: Sandboxed Webview resources
│   ├── index.html
│   ├── style.css
│   └── app.js
└── assets/                  # Optional: Static icons, sound effects, sprites
```

### 4.2 Manifest Schema (`stcli.plugin-manifest/v1`)
The manifest explicitly declares layers, permissions, entrypoints, and actions:

```json
{
  "$schema": "https://stcli.org/schemas/plugin-manifest/v1.json",
  "id": "org.stcli.dnd-dice",
  "name": "D&D Dice Roller",
  "version": "1.0.0",
  "author": "Community",
  "description": "Parses /roll commands and injects narrative dice outcomes into context",
  "entrypoints": {
    "script": "script.js",
    "hook": "hook.wasm",
    "ui_webview": "ui/index.html",
    "settings_schema": "settings.schema.json"
  },
  "permissions": [
    "state:write",
    "prompt:inject"
  ],
  "ui": {
    "actions": [
      { "id": "roll", "label": "Roll d20", "command": "/roll 1d20", "shortcut": "Ctrl+R" }
    ]
  }
}
```

---

## 5. Unified Live-Effect & Networking Capability Model

A core architectural principle of STcli is that **all live effects and networking flow through one coherent, host-brokered security model**. To prevent split-brain trust boundaries, raw TCP/UDP OS sockets are completely banned across all layers.

### 5.1 The Two Brokered Egress Primitives

Every external interaction by a Plugin must map to one of two engine-brokered capability primitives:

```text
                                 ┌────────────────────────────────────────────────┐
                                 │                Plugin Code                     │
                                 │      (Engine Hook or Plugin Script)            │
                                 └───────────────┬────────────────┬───────────────┘
                                                 │                │
                        1. Secondary Inference   │                │  2. Brokered HTTP
                           (Managed LLM call)    │                │     (wasi:http / fetch)
                                                 ▼                ▼
                                 ┌────────────────────────────────────────────────┐
                                 │                  stcli-core                    │
                                 │  ┌────────────────────────┐ ┌────────────────┐ │
                                 │  │ Turn / Attempt Engine  │ │ Outbound Proxy │ │
                                 │  │ - Provider Profiles    │ │ - Domain Allow │ │
                                 │  │ - Token Accounting     │ │ - Secret Inject│ │
                                 │  └───────────┬────────────┘ └────────┬───────┘ │
                                 └──────────────┼───────────────────────┼─────────┘
                                                │                       │
                                                ▼                       ▼
                                       Configured Model            External API
                                      (e.g. OpenAI / Claude)      (e.g. ElevenLabs)
                                                │                       │
                                                └───────────┬───────────┘
                                                            ▼
                                                Recorded Effect Receipt
                                                 in Authoritative Trace
                                                (100% Offline Replay!)
```

#### Primitive 1: Brokered Secondary Inference (`Capability::Inference`)
- **What it is**: Requesting an AI completion through a named Session Provider Profile.
- **Enables**: next-action suggestions (Roadway), pre-generation internal monologue (Stepped Thinking), memory-core summarization (Megumin).
- **Guarantees**:
  - The Plugin never crafts raw HTTP headers, manages SSE sockets, or handles API keys.
  - Each live call is a background Generation Attempt linked to its Session, Branch, initiating Attempt, and caller; it cannot append dialogue or select a Candidate.
  - The engine pins the provider profile and effective settings and records request/response hashes, terminal status, provider receipt, and available usage.
  - Background cancellation is independent and compare-and-set; it neither cancels nor mutates the initiating Attempt.
  - **Replay**: During offline capsule replay, the engine yields the recorded text and metadata without making live provider calls or re-running the caller.

#### Primitive 2: Brokered HTTPS Egress (`Capability::HttpEgress`)
- **What it is**: Outbound REST / HTTP requests via `wasi:http/outgoing-handler` (Engine Hook) or host `fetch()` (Plugin Script).
- **Enables**: custom provider transports (Anthropic, Gemini, Mistral via Engine Hooks), media generation helpers (ElevenLabs, Stable Diffusion via Plugin Scripts).
- **Guarantees**:
  - **Zero Raw Sockets**: No direct DNS, TCP, or UDP socket access.
  - **Explicit Domain Whitelisting**: Manifest must declare exact destination hosts (e.g. `["api.elevenlabs.io", "api.anthropic.com"]`). Wildcard `*` is forbidden.
  - **Out-of-Band Secret Injection**: The engine proxy matches configured secret names and attaches authorization headers (`Authorization: Bearer ...`) out-of-band. Plugin code never has read access to raw API keys.
  - **Deterministic Turn Receipts**: If an HTTP call occurs within turn preparation (e.g. translation or lore retrieval), the host records a content-addressed `HttpReceipt` in the Turn Trace so subsequent replay executes offline.

### 5.2 Handling Non-HTTP Tooling (The Sidecar Boundary)
Technologies that require raw native binaries, shared libraries, or GPU drivers (such as local C++ `llama.cpp` bindings or custom TTS engines):
- Are **explicitly excluded** from in-process Wasm/JS execution.
- Must run as **Trusted Sidecars** (external background processes on loopback `localhost`).
- The engine communicates with sidecars strictly through the same Brokered HTTPS/HTTP Egress primitive, preserving domain isolation and auditable network boundaries.

### 5.3 Permission & Trust Lifecycle
1. **Declare**: Manifest specifies permissions (`inference`, `http:api.elevenlabs.io`, `state:write`).
2. **Review**: Interactive user prompt on install/adopt with clear human-readable permission descriptions.
3. **Enforce**: Engine sandbox drops any network call or state mutation outside the granted set.
4. **Audit**: All external egress receipts and attempt records are inspectable via `stcli plugin trace` and `stcli provider inspect`.

---

## 6. Implementation Roadmap

The roadmap aligns with the canonical milestone numbering in [`PRD.md`](../PRD.md#roadmap):

### v0.2: Declarative UI Contributions & Codecs (Shipped)
- Interactive TUI (Ratatui): navigation, forms, profile creation, and session management.
- The external `artifact-codec` Wasm interface.

### v0.6: Scripting Runtime
- Embed a single QuickJS runtime (`rquickjs`) into `stcli-core` as the host for all interpreted code.
- Implement `ScriptHost` supporting turn lifecycle hooks (`preLore`, `prePrompt`, `preRequest`, `postCommit`) and native Plugin Scripts.
- Deliver STscript as a command set on the same runtime.
- Support `.stplugin` packaging and port the proof plugin to a JavaScript reference Plugin.

### v0.7: Brokered Live Effects & Retrieval
- Implement the unified live-effect and networking model: Secondary Inference and Brokered HTTPS Egress, zero raw sockets, manifest domain-whitelisting, out-of-band secret injection.
- Record secondary attempts and content-addressed HTTP receipts into the authoritative Turn Trace.
- Networked Engine Hook provider transports over brokered `wasi:http`; nested/background Generation Attempts with explicit accounting and cancellation.

### v1.0: SillyTavern Extension Bridge
- Sandboxed JavaScript subset for documented SillyTavern extension APIs, running on the QuickJS runtime from v0.6.
- Manifest compatibility reports; no compatibility claim for unrestricted browser globals or undocumented internals.

### v1.x: Webviews & Sidecars
- Implement the Webview container and RPC bridge for browser and desktop frontends.
- Trusted Sidecars on loopback, reached through Brokered HTTPS Egress, for native binaries and server-plugin-like behavior.
