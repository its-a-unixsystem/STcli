# 01: Walking skeleton — a minimal Extension loads and changes a prompt

**What to build:** A user can drop a minimal SillyTavern Extension directory (`manifest.json` + `js`) into the local store, and it takes effect on a turn. The Extension's module code runs once in a persistent `st-bridge` context; it registers a single `CHAT_COMPLETION_PROMPT_READY` handler that reads a value from a frozen `getContext()` and injects prompt text; a **Dry Run** shows the modified prompt. This is the thinnest complete path through load → runtime → one event → getContext → verification, proving the integration end to end. Later tickets widen each dimension; here everything is minimal.

**Blocked by:** None (can start immediately).

**Status:** done

- [x] A minimal local Extension directory loads into a persistent, per-session `st-bridge` context, running its module init exactly once.
- [x] The context survives across turns so a registered handler remains callable on a later turn.
- [x] A single `CHAT_COMPLETION_PROMPT_READY` handler fires during prompt construction and can inject prompt text via read-back.
- [x] A minimal frozen `getContext()` exposes at least the active character name and chat turns for the handler to read.
- [x] A Dry Run reflects the Extension's prompt modification.

**Design:** [PRD](../spec.md), [ADR 0009](../../../docs/adr/0009-sillytavern-extension-bridge-packaging-and-trust.md).
**Tests:** Seam A (Dry Run) for the prompt change; Seam B for context persistence across invocations.
**Docs:** note the new `st-bridge` runtime path in `stcli-core` module docs.
