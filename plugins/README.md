# STcli plugins

This directory contains WebAssembly Component Model plugins maintained in the STcli repository.

STcli plugins are pure, capability-gated Wasm modules executed inside a sandboxed Wasmtime runtime with **no WASI imports** (no network, filesystem, process, or secret access). Plugins interact with the engine exclusively through canonical JSON input and declarative effect receipts defined by [`wit/plugin.wit`](../wit/plugin.wit).

STcli also runs plugins written in JavaScript through a sandboxed QuickJS runtime (manifest `runtime: "script"`). Script plugins share the same capability model and effect types as Wasm plugins. To write either kind, see [Writing plugins](../docs/plugins.md).

## Directory layout

| Plugin | Identifier | Purpose |
|---|---|---|
| [`proof/`](proof/) | `org.stcli.proof` | Reference implementation and test harness proving the pure Wasm plugin architecture. |
| [`turn-counter/`](turn-counter/) | `org.stcli.turn-counter` | Reference script plugin for the [Writing plugins](../docs/plugins.md#tutorial-a-script-plugin) tutorial. Counts turns and injects one prompt line. |

## The `proof` plugin

[`plugins/proof`](proof/) is an in-tree **reference implementation and verification artifact** fulfilling [PRD Success Criterion 5 ("Pure plugin proof")](../PRD.md) and [ADR 0003](../docs/adr/0003-pure-wasm-plugins.md).

- **Not a production plugin**: It is neither bundled into releases nor enabled by default in user sessions.
- **Architectural role**: Proves that an out-of-tree plugin builds against the public WIT world, installs without host recompilation, and runs within strict sandbox boundaries.
- **Test harness**: Implements declarative effects (macros, commands, prompt contributions, namespaced state) and controllable test modes in [`proof/src/lib.rs`](proof/src/lib.rs) to exercise host limits, error handling, timeouts, and unauthorized state rejection in [`crates/stcli-core/tests/plugins.rs`](../crates/stcli-core/tests/plugins.rs) and [`crates/stcli-cli/tests/plugins.rs`](../crates/stcli-cli/tests/plugins.rs).

## The `turn-counter` plugin

[`plugins/turn-counter`](turn-counter/) is the reference **script plugin**. It is the worked example in the [Writing plugins](../docs/plugins.md#tutorial-a-script-plugin) tutorial.

- **Runtime**: QuickJS script (`runtime: "script"`), so it needs no build toolchain.
- **Behavior**: On each `pre-prompt` event it counts the turn, writes the count to its own state, and injects one line (`[Turn N]`) into the `after-character-definitions` slot.
- **Settings**: A one-property `settings.schema.json` (`start`) shows how a plugin declares settings.
- **Validation**: `stcli plugin doctor plugins/turn-counter` passes, and `stcli plugin install plugins/turn-counter` installs it.

## Further documentation

- **How-to**: [Install and adopt a plugin](../docs/guide.md#install-and-adopt-a-plugin) in the usage guide.
- **Architecture**: [Plugin system design](../ARCHITECTURE.md#plugin-system) and [ADR 0003 (Pure Wasm Plugins)](../docs/adr/0003-pure-wasm-plugins.md).
- **Interface & Schema**: Public interface in [`wit/plugin.wit`](../wit/plugin.wit) and manifest format in [`schemas/plugin-manifest.schema.json`](../schemas/plugin-manifest.schema.json).
- **Build & CI**: Reproducibility verification recipe in [`.github/workflows/plugin.yml`](../.github/workflows/plugin.yml) and [`docs/testing.md`](../docs/testing.md).
