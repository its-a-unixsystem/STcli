# Shared Brokered HTTPS Egress and Secondary Inference subsystem

**Status:** accepted

The two live-effect primitives [ADR 0006](0006-layered-plugins-and-brokered-effects.md) names — **Brokered HTTPS Egress** (`wasi:http` / host `fetch` with domain-whitelisting and out-of-band secret injection) and **Secondary Inference** (a completion through a named provider profile) — are built as **one general host subsystem** consumed by any layer that needs live effects, not as bridge-local plumbing. The SillyTavern Extension bridge is its first client (`fetch`, `generateQuietPrompt`), but the subsystem is provider- and caller-agnostic so native Plugins reuse it unchanged. We build it now because answering "extensions may reach the network" (PRD `sillytavern-extension-bridge`) forces the primitive to exist, and building it bridge-locally would fork the single brokered boundary the product depends on.

## Considered Options

- **Bridge-local `fetch`/inference plumbing.** Rejected: forks ADR 0006's single boundary; every future live-effect caller would reinvent receipts, whitelisting, and secret handling.
- **Defer the broker; stub `fetch` indefinitely.** Rejected: it permanently excludes network extensions and the summarize smoke target, and the primitive is needed engine-wide regardless.

## Consequences

- **Zero raw sockets.** All external interaction maps to the two brokered primitives; secrets are injected out-of-band via the Credential Store and never enter Extension/Plugin memory.
- **Determinism preserved.** Every brokered call records a content-addressed receipt in the Turn Trace; Replay yields recorded results without contacting the network or re-running the caller.
- **Whitelisting + consent.** Egress is denied by default. Callers that declare no domains (imported ST extensions declare none) start with an empty allow-list; the user extends it, and un-allowed egress is a non-fatal, warned rejection.
- **Secondary Inference is a first-class background Generation Attempt.** `generateQuietPrompt`/`generateRaw` resolve a named provider profile with pinned effective settings, request/response hashes, status, available usage, and provider receipt. The Attempt links to its Session, Branch, initiating Attempt, and caller but has no Turn or Candidate.
- **Cancellation is independent.** Cancelling a background Attempt does not cancel or mutate its initiating Attempt. Cancelling an initiating Attempt does not cascade to children. Completion and cancellation use compare-and-set terminal transitions.
- **Sequencing.** ADR 0006 placed brokered egress at v0.7 as a general primitive; this ADR is that primitive. The bridge's issue 07 depends on it; issues 01–06 do not.
