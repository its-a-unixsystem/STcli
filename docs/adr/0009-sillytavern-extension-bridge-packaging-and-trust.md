# SillyTavern Extension bridge: packaging, mutation, and Replay model

**Status:** accepted

A SillyTavern **Extension** (third-party JavaScript) runs headless as a **Plugin whose runtime is `st-bridge`** on the QuickJS `ScriptHost`, reusing STcli's existing manifest, digest-pinning, capability-grant, and Turn-Trace machinery rather than a parallel loader. An importer ingests SillyTavern's native `manifest.json` layout at install so users install unmodified extensions (the ST way — point a frontend at a git repo), but everything below the import normalizes into one trust-and-Replay core. We chose this over a separate first-class extension subsystem because forking the loader would fork the one boundary [ADR 0006](0006-layered-plugins-and-brokered-effects.md) insists on keeping single.

## Considered Options

- **Separate first-class Extension loader** consuming ST's native layout end-to-end with its own pin/grant path. Rejected: two trust models and two determinism paths for one product guarantee.
- **Re-execute extension JavaScript during Replay.** Rejected: arbitrary community JS (async, `fetch`, timers, randomness) cannot be made bit-reproducible; re-running it would desync Replay.
- **Mutable `getContext()` proxy with captured diffs** for maximum source-compatibility. Rejected: complex, failure-prone, and it contradicts the read-only-snapshot guarantee; the common extensions mutate through `generate_interceptor` / `setExtensionPrompt` / the prompt-ready payload anyway.

## Consequences

- **Record effects, do not re-run JS.** Extension callbacks run live during the original attempt; their observable effects (prompt diffs, `extension.command` results, state/settings writes, brokered receipts) are recorded to the Turn Trace, and Replay re-applies them without re-executing the JavaScript.
- **Read-only context, sanctioned mutation surfaces.** `getContext()` is a frozen snapshot; the only host-read-back mutation surfaces are the `generate_interceptor` `chat` argument, the `CHAT_COMPLETION_PROMPT_READY` payload, and `setExtensionPrompt`. Writes to the frozen snapshot emit a non-fatal Compatibility Warning.
- **Persistent runtime.** The bridge holds a long-lived, per-session, per-extension QuickJS context (module init runs once; registered listeners/commands fire later), replacing the stateless `script::execute` model on this path. It adds Promises + a bounded microtask drain, and restores `Math.random` only through a seeded PRNG whose seed is recorded in the trace.
- **Bounded nondeterminism.** Async that has not settled at the microtask bound is abandoned with a warning; no dangling effects are recorded.
- **Trust.** Install-time consent grants the `st-bridge` capability tier (namespaced state, command registration, brokered egress, secondary inference), pinned per session like any `PluginGrant`; egress domains default to an empty allow-list. `auto_update` is off.
- **Lifecycle.** Extensions install globally and are adopted + digest-pinned per session; mid-session enable/disable is a recorded Session Configuration Revision and is never retroactive. Mid-session adoption begins observation at the adoption turn (history is reachable via `getContext().chat`; past lifecycle events are not re-emitted).
- **Degradation.** Missing browser/DOM globals return control-flow-safe values and emit one-time, deduplicated, non-blocking Compatibility Warnings (diagnostics channel only). Reduced source-compatibility for context-mutating or visual extensions is accepted.
