# Layered Plugins with a single brokered live-effect boundary

**Status:** accepted (supersedes ADR 0003)

ADR 0003 limited Plugins to pure Wasm returning declarative effects and rejected arbitrary JavaScript and any network, model, filesystem, or secret access. That kept the MVP deterministic but blocked casual authoring, real roleplay extensions, and custom provider transports. We now extend the model: a **Plugin** is a native package that may declare several capability layers — a compiled **Engine Hook** (Wasm), an interpreted **Plugin Script** (one QuickJS runtime, also the substrate for STscript at v0.6 and the SillyTavern Extension bridge at v1.0), and a **UI Contribution** (declarative first, sandboxed Webview as a graphical fallback). Live effects are permitted but flow through **one host-brokered boundary**, never raw sockets, and every effect is recorded so Replay stays offline and deterministic.

## Considered Options

- **Keep ADR 0003 as-is.** Rejected: it permanently excludes scripting, secondary inference, and provider transports the roadmap needs.
- **Two trust tiers with raw `wasi:sockets` for Engine Hooks.** Rejected: a socket-holding hook speaks TLS itself and would see resolved secrets, breaking the single secret boundary.
- **Relax determinism for live-effect Plugins.** Rejected: offline replay of every attempt is the product's core guarantee.

## Consequences

- **Zero raw sockets, everywhere.** All external interaction maps to two brokered primitives: **Secondary Inference** (a completion through a named provider profile) and **Brokered HTTPS Egress** (`wasi:http` / host `fetch` with manifest domain-whitelisting and out-of-band secret injection). Secrets never enter Plugin memory.
- **Determinism preserved.** Each brokered call and script effect records a content-addressed receipt in the Turn Trace; Replay yields recorded results without re-executing code or contacting the network.
- **Native binaries are out-of-process.** `llama.cpp`, TTS engines, and GPU work run as **Trusted Sidecars** on loopback, reached through the same brokered egress — not as in-process Wasm or JS.
- **Pinning generalizes.** Sessions pin the exact digest of every layer, not a single component hash.
- **MVP is unchanged.** The MVP still ships only the pure Engine Hook layer. Scripting lands at v0.6, brokered live effects at v0.7, the SillyTavern bridge at v1.0, and Webviews and sidecars at v1.x.
