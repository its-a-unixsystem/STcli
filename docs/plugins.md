# Writing plugins

A plugin adds behavior to the engine without a change to engine code. You can add a line to the prompt, register a macro or a command, or keep a small piece of your own state across a session.

STcli runs every plugin in a sandbox. A plugin gets no network, no filesystem, no provider, and no secret access. It does not receive a mutable reference to the engine. Instead, it reads a copy of permitted data and returns a list of **declarative effects**. The engine validates each effect, applies the allowed ones, and records them. This is why the engine can replay a whole session offline: it reads the recorded effects and does not run the plugin again.

This page is the complete guide. It has an introduction, two tutorials, a packaging how-to, and a reference. New authors can read it top to bottom. Experienced authors can jump to the [reference](#reference).

## Contents

- [What a plugin is](#what-a-plugin-is)
- [How a plugin runs](#how-a-plugin-runs)
- [Choose a runtime](#choose-a-runtime)
- [The manifest](#the-manifest)
- [Tutorial: a script plugin](#tutorial-a-script-plugin)
- [Tutorial: a Wasm plugin](#tutorial-a-wasm-plugin)
- [Combine several features in one plugin](#combine-several-features-in-one-plugin)
- [Package and distribute a plugin](#package-and-distribute-a-plugin)
- [Reference](#reference)

## What a plugin is

A plugin is a directory. The directory holds a `manifest.json` file and one component file. The component file is the code the engine runs.

STcli supports two runtimes for the component:

- **Wasm**: A WebAssembly Component Model binary (`.wasm`). This is the default runtime. It has access to every effect type.
- **Script**: A JavaScript file (`.js`) that runs in a sandboxed QuickJS engine. It is quick to write and needs no build step. It can contribute to the prompt, write its own state, and log messages.

Both runtimes use the same manifest, the same capability model, and the same effect types. The runtime only changes how you write the code.

A plugin can do these things when its manifest requests them and the session grants them:

- Observe supported lifecycle events.
- Register macros and commands (Wasm only).
- Contribute prompt segments to closed slots.
- Read permitted session data.
- Write to its own state namespace.
- Abort a turn before the provider request (Wasm only).

A plugin can never open a socket, read a file, call a model, or read a secret. These limits come from [ADR 0003](adr/0003-pure-wasm-plugins.md). The later [ADR 0006](adr/0006-layered-plugins-and-brokered-effects.md) plans live effects through one brokered boundary, but that is roadmap work and is not in the engine today.

## How a plugin runs

During a turn, the engine calls each subscribed plugin one time for each event the plugin subscribes to. The call is a pure function: the engine passes canonical JSON in, and the plugin returns canonical JSON out.

![Plugin lifecycle: an author writes a bundle, validates it with plugin doctor, installs it to the local store, and adopts it into a session with a pinned version, digest, and capabilities. On each turn the engine runs the plugin in a sandbox as a pure function, validates the returned effects against the grant, applies the allowed effects to state and the prompt, and records them in the SQLite Turn Trace. Replay reads the recorded effects and does not run the plugin again.](diagrams/plugin-lifecycle.png)

<!-- Editable source: docs/diagrams/plugin-lifecycle.html — re-export the PNG with headless Chromium after edits. -->

The steps are:

1. The engine builds the input. The input holds the event name, the plugin settings, permitted session data, and the plugin's own stored state.
2. The engine runs the component with that input.
3. The plugin returns a list of effects.
4. The engine validates each effect against the manifest and the session grant. It rejects any effect that the grant does not allow.
5. The engine applies the allowed effects and records them in the Turn Trace.

Replay reads the recorded effects from the trace. It does not run the plugin again. As a result, a plugin cannot make a session non-deterministic.

### Inspect an Artifact outside a Session

An Artifact inspector runs without a Session. The engine loads one decoded Artifact Revision, passes its value in `input.artifact`, and returns one typed `output` value to the caller. The plugin must subscribe to `inspect-artifact`, request the `inspect-artifact` capability, and have an exact id, version, digest, and capability set registered in the Store.

This path is read-only. The host rejects prompt contributions, state writes, aborts, and every effect except `output`. It does not create a Turn Trace receipt because no Turn exists; the result is ephemeral. The WIT interface remains the same JSON-string boundary used by Session events.

## Choose a runtime

Use this table to pick a runtime.

| Question | Script | Wasm |
|---|---|---|
| Contribute prompt segments? | Yes | Yes |
| Write own state? | Yes | Yes |
| Log messages? | Yes | Yes |
| Register macros and commands? | No | Yes |
| Abort a turn before the request? | No | Yes |
| Needs a build toolchain? | No | Yes (Rust and `wasm-tools`) |
| Best for | Small prompt and state logic | Full effects and heavy logic |

Start with a script plugin for narrative logic, such as a counter, a clock, or an ambient prompt line. Move to a Wasm plugin when you need a macro, a command, an abort, or heavy computation.

The Script runtime needs the `scripting` build feature. This feature is on by default. When STcli is built without it, a script plugin returns an error.

## The manifest

Every plugin has a `manifest.json` file. The [manifest schema](../schemas/plugin-manifest.schema.json) defines the full format. The engine rejects a manifest that does not match the schema.

| Field | Behavior |
|---|---|
| `schema` | Always `stcli.plugin-manifest/v1`. |
| `id` | The plugin identifier. Lowercase letters, digits, and `-`, with `.` between parts. For example `org.example.my-plugin`. |
| `version` | A semantic version, such as `1.0.0`. |
| `engine` | A semantic version requirement for the engine, such as `>=0.1.0, <0.2.0`. |
| `runtime` | `wasm` (default) or `script`. Omit it for a Wasm plugin. |
| `component` | The component filename. A `.wasm` file for Wasm, or a `.js` file for Script. |
| `component_sha256` | The SHA-256 digest of the component file, as `sha256:<64 hex digits>`. |
| `dependencies` | Other plugins this one needs. An empty array when there are none. |
| `license` | An SPDX license expression, such as `MIT`. |
| `subscriptions` | The lifecycle events the plugin subscribes to. See [Hooks and events](#hooks-and-events). |
| `prompt_slots` | The prompt slots the plugin can write to. See [Prompt slots](#prompt-slots). |
| `commands` | The command names the plugin can register (Wasm only). |
| `macros` | The macro names the plugin can register (Wasm only). |
| `settings_schema` | The filename of a JSON settings schema, or `null`. |
| `requested_capabilities` | The capabilities the plugin asks for. See [Capabilities](#capabilities). |
| `before` | Plugin identifiers this plugin must run before. |
| `after` | Plugin identifiers this plugin must run after. |

The engine validates the component digest before it runs the component. When the file and the digest do not match, the engine rejects the plugin.

## Tutorial: a script plugin

This tutorial builds a small script plugin named **turn counter**. On each turn, it counts the turn and adds one line to the prompt, such as `[Turn 12]`. It shows the three things a script plugin does: read a setting, write its own state, and contribute to the prompt.

The finished plugin is in the repository at [`plugins/turn-counter`](../plugins/turn-counter). You can follow the steps to build your own, or read the finished files.

### Step 1: Create the directory

Make a new directory for the plugin:

```bash
mkdir turn-counter
cd turn-counter
```

### Step 2: Write the script

Create `script.js`. The engine calls one function for each event. For the `pre-prompt` event, the function name is `prePrompt`:

```js
function prePrompt(input) {
  const settings = input.settings || {};
  const start = settings.start || 0;
  const turn = (stcli.state.get("turns") || start) + 1;
  stcli.state.set("turns", turn);
  stcli.log("info", "turn " + turn);
  stcli.prompt.inject("after-character-definitions", "[Turn " + turn + "]");
}
```

The function reads the turn count from its own state. When there is no stored count, it starts from the `start` setting. It adds one, stores the new count, and injects one line into the prompt.

The sandbox gives the script one global object named `stcli`. See [The script API](#the-script-api) for every method.

### Step 3: Add a settings schema

A settings schema lets a user set a value at adopt time. Create `settings.schema.json` with one setting:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "start": {
      "type": "integer",
      "minimum": 0,
      "default": 0,
      "description": "The turn count before the first message."
    }
  }
}
```

The `settings_schema` field in the manifest points to this file. The schema is optional. Set the field to `null` when the plugin has no settings.

### Step 4: Get the component digest

The manifest pins the SHA-256 digest of the script file. Get the digest:

```bash
sha256sum script.js
```

Copy the 64-character digest. You will paste it into the manifest in the next step.

### Step 5: Write the manifest

Create `manifest.json`. Set `runtime` to `script`, set `component` to the script filename, and paste the digest into `component_sha256`:

```json
{
  "schema": "stcli.plugin-manifest/v1",
  "id": "org.example.turn-counter",
  "version": "1.0.0",
  "engine": ">=0.1.0, <0.2.0",
  "runtime": "script",
  "component": "script.js",
  "component_sha256": "sha256:REPLACE_WITH_DIGEST",
  "dependencies": [],
  "license": "MIT",
  "subscriptions": ["pre-prompt"],
  "prompt_slots": ["after-character-definitions"],
  "commands": [],
  "macros": [],
  "settings_schema": "settings.schema.json",
  "requested_capabilities": ["contribute-prompt", "write-own-state"],
  "before": [],
  "after": []
}
```

The plugin subscribes to `pre-prompt`, declares the one slot it writes to, and asks for the two capabilities it needs. The engine denies any effect outside these declarations.

### Step 6: Validate the plugin

Run `plugin doctor` to validate the bundle before you install it:

```bash
cargo run --quiet --bin stcli -- --output json plugin doctor ./turn-counter
```

The command returns `"ok":true` when the manifest, the digest, and the settings schema are all correct. When the digest does not match, redo [Step 4](#step-4-get-the-component-digest).

### Step 7: Install and adopt

Install and adopt the plugin. For the full lifecycle, see [Package and distribute a plugin](#package-and-distribute-a-plugin):

```bash
cargo run --quiet --bin stcli -- --output json plugin install ./turn-counter
cargo run --quiet --bin stcli -- --output json plugin adopt \
  --session <session-id> \
  --version 1.0.0 \
  --digest sha256:<component-digest> \
  --capability contribute-prompt \
  --capability write-own-state \
  org.example.turn-counter
```

Now each turn in that session adds a `[Turn N]` line after the character definitions.

## Tutorial: a Wasm plugin

A Wasm plugin can do everything a script plugin can do, plus register macros and commands and abort a turn. You write it in a language that compiles to a WebAssembly Component, such as Rust.

The in-tree [`plugins/proof`](../plugins/proof) plugin is the reference Wasm plugin. This tutorial explains its shape. Read [`plugins/proof/src/lib.rs`](../plugins/proof/src/lib.rs) beside this text.

### The interface

A Wasm plugin implements the `plugin` world from [`wit/plugin.wit`](../wit/plugin.wit). The world has one export:

```wit
export run: func(input: string) -> result<string, string>;
```

The host calls `run` with canonical JSON input. The plugin returns canonical JSON output, or an error string. The output holds the list of effects:

```json
{ "effects": [ { "effect": "register-macro", "name": "proof-greeting", "value": "Hello from Wasm" } ] }
```

### The Rust source

The Rust source generates bindings from the WIT world and implements the `run` function:

```rust
wit_bindgen::generate!({
    path: "../../wit",
    world: "plugin",
});

struct MyPlugin;

impl Guest for MyPlugin {
    fn run(input: String) -> Result<String, String> {
        Ok(r#"{"effects":[{"effect":"prompt","contribution":{"slot":"after-character-definitions","name":"note","role":"system","content":"Stay in character.","depth":null,"order":10,"outlet":null}}]}"#.to_owned())
    }
}

export!(MyPlugin);
```

The `run` function reads the input JSON to find the event and the context. It returns the effects for that event. For every effect type and its fields, see [Effect types](#effect-types).

### The build

Build the core module, then wrap it into a Component with `wasm-tools`:

```bash
cargo build --release --target wasm32-unknown-unknown --locked
wasm-tools component new \
  target/wasm32-unknown-unknown/release/my_plugin.wasm \
  -o component.wasm
```

The result is `component.wasm`. This is the component file the manifest points to. Get its digest with `sha256sum component.wasm`, then write the manifest as in the [script tutorial](#step-5-write-the-manifest). Set `runtime` to `wasm`, or omit the `runtime` field, because `wasm` is the default.

## Combine several features in one plugin

One plugin can return several effects in one call. The [`plugins/proof`](../plugins/proof) plugin does this: on the `pre-prompt` event it registers a macro, registers a command, contributes a prompt line, and writes its own state, all in one output.

To combine features, list every declaration in the manifest and request every capability:

```json
{
  "subscriptions": ["pre-prompt"],
  "prompt_slots": ["after-character-definitions"],
  "commands": ["proof-set"],
  "macros": ["proof-greeting"],
  "requested_capabilities": [
    "register-macro",
    "register-command",
    "contribute-prompt",
    "write-own-state"
  ]
}
```

Then return the matching effects from `run`. The engine validates each effect against these declarations.

One thing you cannot do today is combine a Wasm component and a script in a single plugin. A manifest has exactly one `component` and one `runtime`. [ADR 0006](adr/0006-layered-plugins-and-brokered-effects.md) plans layered plugins that hold several code layers in one package, but that is roadmap work. For now, ship one runtime per plugin.

## Package and distribute a plugin

A plugin ships as a directory. The directory holds the manifest, the component file, and the optional settings schema:

```text
my-plugin/
├── manifest.json
├── component.wasm         (Wasm runtime)
└── settings.schema.json   (optional)
```

A script plugin ships the same way, with a `.js` file instead of a `.wasm` file.

### The lifecycle commands

The `stcli plugin` commands manage a plugin from validation to removal.

| Command | Purpose |
|---|---|
| `plugin doctor <dir>` | Validate a bundle without installing it. |
| `plugin install <dir>` | Validate and store a bundle in the local plugin store. |
| `plugin list` | List installed plugins. |
| `plugin inspect <id>` | Show details of an installed plugin. |
| `plugin adopt <id>` | Grant a plugin to a session, with an explicit version, digest, and capabilities. |
| `plugin enable <id>` / `plugin disable <id>` | Turn a session grant on or off. |
| `plugin upgrade <id>` | Replace a session's plugin with a new pinned version and digest. It keeps the grants and settings. |
| `plugin invoke <id> <command>` | Run a plugin command. Its effects enter the Turn Trace. |
| `plugin remove <id>` | Remove an installed plugin. It refuses a plugin that a stored session revision references. |

For the exact arguments of each command, see the [CLI reference](cli.md) and the [usage guide](guide.md#install-and-adopt-a-plugin).

### Why adoption is explicit

Adoption creates a new immutable Session Configuration Revision. The version, the digest, and each granted capability are all explicit in the command. This makes a session reproducible: the session pins the exact code it ran.

A grant can only allow capabilities that the manifest requests. The engine rejects a grant that asks for more. It also rejects, at run time, any effect that the grant does not allow.

An upgrade keeps the session's grants and settings, and pins the new version and digest in a new revision. This is why an upgrade does not lose your configuration.

## Reference

### Hooks and events

The engine runs a hook only when the plugin subscribes to its event in the manifest. A script plugin exports one function per event, named for the event. A Wasm plugin reads the event name from the input JSON.

| Event (manifest) | Script function | When it runs |
|---|---|---|
| `pre-lore` | `preLore` | Before the lore engine runs. |
| `pre-prompt` | `prePrompt` | Before the prompt is assembled. |
| `pre-request` | `preRequest` | Before the provider request is built. Only here can a Wasm plugin abort. |
| `post-commit` | `postCommit` | After the turn is committed. Read-only: only `observe` effects are allowed. |
| `inspect-artifact` | `inspectArtifact` | Outside a Session, with a decoded Artifact Revision in `input.artifact`. Only one `output` effect is allowed. |
| (command) | `command` | When a user runs a plugin command. Only `observe` and `state-write` effects are allowed. |

When a script plugin subscribes to an event but does not export its function, the engine returns an error.

### The plugin input

Each hook receives one input object with these keys:

- `event`: The event name.
- `plugin_id`: The plugin identifier.
- `settings`: The plugin settings for this session.
- `context`: Context data for the event.
- `state`: The plugin's own stored state.
- `artifact`: The decoded Artifact value for `inspect-artifact`; omitted from other events.
- `session`: The permitted session data.

A script reads these from its function argument, for example `input.settings.start`. A Wasm plugin parses them from the input JSON string.

### Prompt slots

A prompt contribution goes into one closed slot. The slot must be one the manifest declares in `prompt_slots`. The slot names are:

- `before-character-definitions`
- `after-character-definitions`
- `before-example-messages`
- `after-example-messages`
- `named-lore-outlet`
- `in-chat`
- `before-history`
- `after-history`
- `post-history-instructions`

### Effect types

A plugin returns a list of effects. Each effect has an `effect` field that names its type. The engine validates every effect before it applies it.

| Effect | Fields | Capability | Notes |
|---|---|---|---|
| `observe` | `value` | `observe-lifecycle` | Records a value in the receipt. Changes nothing. |
| `output` | `value` | `inspect-artifact` | Returns the typed inspection result. Allowed only during Artifact inspection. |
| `register-macro` | `name`, `value` | `register-macro` | The name must be in `macros`. Wasm only. |
| `register-command` | `name`, `description` | `register-command` | The name must be in `commands`. Wasm only. |
| `prompt` | `contribution` | `contribute-prompt` | The slot must be in `prompt_slots`. |
| `state-write` | `key`, `value` | `write-own-state` | The key scope must be `local` and the name must start with `<plugin_id>.`. |
| `abort` | `code`, `message` | `abort-pre-request` | Allowed only on the `pre-request` event. |

A prompt `contribution` has these fields: `slot`, `name`, `role`, `content`, `depth`, `order`, and `outlet`.

### The script API

The sandbox gives a script one global object named `stcli`. The script has no other host access.

#### State

`stcli.state.get(name)` returns the stored value for `name`, or `undefined` when it is not set. The value is parsed from JSON.

`stcli.state.set(name, value)` stores `value` under `name`. The value must be JSON-serializable. This writes to the plugin's own namespace and needs the `write-own-state` capability.

A state name must not be empty. It can hold letters, digits, `_`, and `-`. It can use `.` to separate namespace parts, for example `counters.runs`.

#### Prompt

`stcli.prompt.inject(slot, content)` adds `content` to a prompt slot as a system message. The slot must be one the manifest declares in `prompt_slots`. The call needs the `contribute-prompt` capability.

#### Inspection output

`stcli.output(value)` returns one JSON-serializable value from `inspectArtifact`. Artifact inspection rejects every mutation effect and rejects missing or multiple output values.

#### Log

`stcli.log(level, message)` records a log line in the plugin receipt. The `level` must be `error`, `warn`, `info`, or `debug`. The logs help you debug a plugin. They do not change the prompt or the state.

### The sandbox and its limits

The QuickJS sandbox removes unsafe globals. It deletes `eval`. It deletes `Math.random`. It gives no timers, no network, and no filesystem.

The sandbox keeps safe built-ins. The script can use `JSON`, `RegExp`, `Map`, and `Set`.

The engine caps each run with these default limits:

| Limit | Default |
|---|---|
| Memory | 16 MiB |
| Stack | 256 KiB |
| Execution steps | 200 interrupt ticks |
| Log entries | 64 |
| Log message size | 2048 bytes |

When a script runs past its step budget, the engine stops it and returns a step-limit error. When a script throws, the engine returns a trap error with the message.

### Capabilities

The manifest requests capabilities. The session grant allows a subset. The engine rejects an effect that the grant does not allow.

| Capability | Allows | Runtime |
|---|---|---|
| `observe-lifecycle` | An `observe` effect. | Wasm |
| `inspect-artifact` | One `output` effect from a registered Artifact inspector. | Wasm, Script |
| `register-macro` | A `register-macro` effect. | Wasm |
| `register-command` | A `register-command` effect. | Wasm |
| `contribute-prompt` | A `prompt` effect, and `stcli.prompt.inject`. | Wasm, Script |
| `read-session` | A read of permitted session data. | Wasm, Script |
| `write-own-state` | A `state-write` effect, and `stcli.state.set`. | Wasm, Script |
| `abort-pre-request` | An `abort` effect on the `pre-request` event. | Wasm |

A script plugin uses `contribute-prompt` and `write-own-state`. It cannot register a macro or a command, and it cannot abort a turn. For those effects, write a Wasm plugin.

The engine records every applied effect in the authoritative Turn Trace. During replay, the engine reads the recorded effects and does not run the plugin again.

## See also

- [Usage guide: install and adopt a plugin](guide.md#install-and-adopt-a-plugin)
- [CLI reference](cli.md)
- [Plugins directory](../plugins/README.md)
- [Architecture: plugin system](../ARCHITECTURE.md#plugin-system)
- [ADR 0003: pure Wasm plugins](adr/0003-pure-wasm-plugins.md)
- [ADR 0006: layered plugins](adr/0006-layered-plugins-and-brokered-effects.md)
- [Manifest schema](../schemas/plugin-manifest.schema.json)
- [Plugin WIT world](../wit/plugin.wit)
