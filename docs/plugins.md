# Writing plugins

A plugin adds behavior to the engine without changing engine code. STcli runs plugins in a sandbox. A plugin never gets network, filesystem, provider, or secret access. It returns declarative effects that the engine records and can replay.

STcli supports two plugin runtimes:

- **Wasm**: A WebAssembly Component Model binary. This is the default runtime.
- **Script**: A JavaScript file that runs in a sandboxed QuickJS engine.

This document explains both, with a focus on the Script runtime. For the install and adopt steps, see the [usage guide](guide.md#install-and-adopt-a-plugin). For the design and the capability model, see [`ARCHITECTURE.md`](../ARCHITECTURE.md#plugin-system) and [ADR 0006](adr/0006-layered-plugins-and-brokered-effects.md).

## Contents

- [Pick a runtime](#pick-a-runtime)
- [The manifest](#the-manifest)
- [Write a script plugin](#write-a-script-plugin)
- [Hooks](#hooks)
- [The script API](#the-script-api)
- [The sandbox and its limits](#the-sandbox-and-its-limits)
- [Capabilities](#capabilities)

## Pick a runtime

Use the Wasm runtime for full effects and strong isolation. A Wasm plugin can register macros and commands, contribute prompt segments, write its own state, observe lifecycle events, and abort before a request.

Use the Script runtime for small prompt and state logic that is quick to write. A Script plugin can contribute prompt segments, write its own state, and log messages. It does not register macros or commands and does not abort a request.

The Script runtime needs the `scripting` build feature. This feature is on by default. When STcli is built without it, a script plugin returns an error.

## The manifest

Every plugin is a directory with a `manifest.json` file and its component file. A Wasm plugin ships a `.wasm` file. A Script plugin ships a `.js` file.

The manifest declares the runtime and the component. The [manifest schema](../schemas/plugin-manifest.schema.json) defines the full format.

| Field | Behavior |
| --- | --- |
| `runtime` | `wasm` (default) or `script`. Omit it for a Wasm plugin. |
| `component` | The component filename. A `.wasm` file for Wasm, or a `.js` file for Script. |
| `component_sha256` | The SHA-256 digest of the component file, as `sha256:<64 hex digits>`. |

The engine checks the digest before it runs the component. When the file and the digest do not match, the engine rejects the plugin.

## Write a script plugin

This example is a script plugin that adds one line to the prompt and counts how often it runs.

Follow these steps:

1. Create a plugin directory.
2. Write the script file `plugin.js`:

```js
function prePrompt(input) {
  const count = (stcli.state.get("runs") || 0) + 1;
  stcli.state.set("runs", count);
  stcli.log("info", "run number " + count);
  stcli.prompt.inject("in-chat", "Stay in character at all times.");
}
```

3. Get the digest of the script file:

```bash
sha256sum plugin.js
```

4. Write `manifest.json`. Set `runtime` to `script` and put the digest in `component_sha256`:

```json
{
  "schema": "stcli.plugin-manifest/v1",
  "id": "org.example.reminder",
  "version": "0.1.0",
  "engine": ">=0.1.0",
  "runtime": "script",
  "component": "plugin.js",
  "component_sha256": "sha256:REPLACE_WITH_DIGEST",
  "dependencies": [],
  "license": "MIT",
  "subscriptions": ["pre-prompt"],
  "prompt_slots": ["in-chat"],
  "commands": [],
  "macros": [],
  "settings_schema": null,
  "requested_capabilities": ["contribute-prompt", "write-own-state"],
  "before": [],
  "after": []
}
```

5. Validate the plugin without installing it:

```bash
cargo run --quiet --bin stcli -- --output json plugin doctor ./my-plugin
```

6. Install and adopt the plugin. See the [usage guide](guide.md#install-and-adopt-a-plugin) for the adopt command.

The engine runs a hook only when the plugin subscribes to its event. It applies a prompt injection only when the plugin declares the slot and holds the `contribute-prompt` capability.

## Hooks

The engine calls one exported function per event. Name the function for the event you subscribe to.

| Event (manifest) | Hook function | When it runs |
| --- | --- | --- |
| `pre-lore` | `preLore` | Before the lore engine runs. |
| `pre-prompt` | `prePrompt` | Before the prompt is assembled. |
| `pre-request` | `preRequest` | Before the provider request is built. |
| `post-commit` | `postCommit` | After the turn is committed. Read-only. |
| (command) | `command` | When a user invokes a plugin command. |

Each hook takes one argument: the plugin input. The input is an object with these keys:

- `event`: The event name.
- `plugin_id`: The plugin ID.
- `settings`: The plugin settings for this session.
- `context`: Context data for the event.
- `state`: The plugin's own stored state.
- `session`: The permitted session data.

When a plugin subscribes to an event but does not export its hook, the engine returns an error.

## The script API

The sandbox gives the script one global object named `stcli`. The script has no other host access.

### State

`stcli.state.get(name)` returns the stored value for `name`, or `undefined` when it is not set. The value is parsed from JSON.

`stcli.state.set(name, value)` stores `value` under `name`. The value must be JSON-serializable. This writes to the plugin's own namespace and needs the `write-own-state` capability.

A state name must not be empty. It can hold letters, digits, `_`, and `-`. It can use `.` to separate namespace parts, for example `counters.runs`.

### Prompt

`stcli.prompt.inject(slot, content)` adds `content` to a prompt slot as a system message. The slot must be one the manifest declares in `prompt_slots`. The call needs the `contribute-prompt` capability.

The `slot` value is one of the slot names in the [manifest schema](../schemas/plugin-manifest.schema.json), for example `in-chat` or `before-character-definitions`.

### Log

`stcli.log(level, message)` records a log line in the plugin receipt. The `level` must be `error`, `warn`, `info`, or `debug`. The logs help you debug a plugin. They do not change the prompt or the state.

## The sandbox and its limits

The QuickJS sandbox removes unsafe globals. It deletes `eval`. It deletes `Math.random`. It gives no timers, no network, and no filesystem.

The sandbox keeps safe built-ins. The script can use `JSON`, `RegExp`, `Map`, and `Set`.

The engine caps each run with these default limits:

| Limit | Default |
| --- | --- |
| Memory | 16 MiB |
| Stack | 256 KiB |
| Execution steps | 200 interrupt ticks |
| Log entries | 64 |
| Log message size | 2048 bytes |

When a script runs past its step budget, the engine stops it and returns a step-limit error. When a script throws, the engine returns a trap error with the message.

## Capabilities

A Script plugin uses the same capability model as a Wasm plugin. The manifest requests capabilities. The session grant allows a subset. The engine rejects an effect that the grant does not allow.

A Script plugin uses these capabilities:

- `contribute-prompt`: Required for `stcli.prompt.inject`.
- `write-own-state`: Required for `stcli.state.set`.

The engine records every effect in the authoritative trace. During replay, the engine reads the recorded effects and does not run the script again.
