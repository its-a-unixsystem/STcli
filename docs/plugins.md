# Writing plugins

A plugin adds behavior to the engine without a change to engine code. You can add a line to the prompt, register a macro or a command, or keep a small piece of your own state across a session.

STcli runs every plugin in a sandbox. A plugin gets no network, no filesystem, no provider, and no secret access. It does not receive a mutable reference to the engine. Instead, it reads a copy of permitted data and returns a list of **declarative effects**. The engine validates each effect, applies the allowed ones, and records them. This is why the engine can replay a whole session offline: it reads the recorded effects and does not run the plugin again.

This page is the complete guide. It has an introduction, two tutorials, a packaging how-to, and a reference. New authors can read it top to bottom. Experienced authors can jump to the [reference](#reference).

## Contents

- [What a plugin is](#what-a-plugin-is)
- [How a plugin runs](#how-a-plugin-runs)
- [Choose a runtime](#choose-a-runtime)
- [st-bridge deterministic globals](#st-bridge-deterministic-globals)
- [Pinned real-world Extension fixtures](#pinned-real-world-extension-fixtures)
- [Brokered HTTPS egress](#brokered-https-egress)
- [SillyTavern.getContext() read-only surface](#sillytaverngetcontext-read-only-surface)
- [The manifest](#the-manifest)
- [Tutorial: a script plugin](#tutorial-a-script-plugin)
- [Tutorial: a Wasm plugin](#tutorial-a-wasm-plugin)
- [Combine several features in one plugin](#combine-several-features-in-one-plugin)
- [Package and distribute a plugin](#package-and-distribute-a-plugin)
- [Reference](#reference)

## What a plugin is

A plugin is a directory. The directory holds a `manifest.json` file and one component file. The component file is the code the engine runs.

STcli supports three runtimes for the component:

- **Wasm**: A WebAssembly Component Model binary (`.wasm`). This is the default runtime. It has access to every effect type.
- **Script**: A JavaScript file (`.js`) that runs in a sandboxed QuickJS engine. It is quick to write and needs no build step. It can contribute to the prompt, write its own state, and log messages.
- **st-bridge Extension**: A SillyTavern-compatible JavaScript Extension (`.js`) run headlessly in a persistent QuickJS context. It is an **Extension**, not a Plugin: it uses the normalized Plugin manifest and grants, but its sanctioned mutable surfaces are the ST-shaped `generate_interceptor` chat array, `CHAT_COMPLETION_PROMPT_READY` payload, and `SillyTavern.setExtensionPrompt`. `getContext()` remains a frozen snapshot.

All three runtimes use the same manifest, capability model, and effect types. The `st-bridge` runtime is an Extension compatibility surface with recorded prompt rewrites and contributions; Replay and Rerun reuse those recorded effects without executing JavaScript.

A plugin can do these things when its manifest requests them and the session grants them:

- Observe supported lifecycle events.
- Register macros and commands (Wasm only).
- Contribute prompt segments to closed slots.
- Read permitted session data.
- Write to its own state namespace.
- Abort a turn before the provider request (Wasm only).

Plugin code never receives raw socket, filesystem, provider, or secret access. Wasm and Script components remain declarative and offline. An `st-bridge` Extension may request Brokered HTTPS Egress or Secondary Inference through the host-controlled boundaries defined by [ADR 0006](adr/0006-layered-plugins-and-brokered-effects.md) and [ADR 0010](adr/0010-brokered-egress-and-secondary-inference.md); the host enforces grants, injects secrets out of band, and records receipts.

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

Use this table to pick a runtime. `st-bridge` is for importing SillyTavern Extensions; use Script or Wasm when authoring a native STcli Plugin.

| Question | Script | Wasm | st-bridge Extension |
|---|---|---|---|
| Contribute prompt segments? | Yes | Yes | Yes, through supported SillyTavern mutation surfaces |
| Write own state? | Yes | Yes | Yes, in its namespaced settings and `localStorage` |
| Log messages? | Yes | Yes | Yes |
| Register macros and commands? | No | Yes | Slash commands only |
| Abort a turn before the request? | No | Yes | No |
| Brokered HTTPS or Secondary Inference? | No | Through declared effects | Yes, with the fixed bridge grant and configured policy |
| Needs a build toolchain? | No | Yes (Rust and `wasm-tools`) | No |
| Best for | Small prompt and state logic | Full effects and heavy logic | Headless-compatible SillyTavern Extensions |

Start with a Script Plugin for native narrative logic, such as a counter, a clock, or an ambient prompt line. Move to Wasm when you need a macro, an abort, or heavy computation. Use `st-bridge` only when importing or adapting a SillyTavern Extension.

The Script runtime needs the `scripting` build feature. This feature is on by default. When STcli is built without it, a script plugin returns an error.

### st-bridge deterministic globals

The `st-bridge` runtime keeps one QuickJS context for each Session, Extension, and component
digest. It provides these deterministic browser-compatible globals:

- `Math.random()` uses a seeded Xoshiro128++ generator. Each bridge invocation starts a fresh
  sequence and records its seed in the Plugin Receipt as `prng_seed`. Reinitializing the generator
  from that receipt reproduces the random sequence for that invocation, including later calls in
  the same persistent Extension context.
- `setTimeout(callback, 0)` and `setInterval(callback, 0)` enqueue the callback as a microtask.
  The returned numeric handle is informational. `clearTimeout` and `clearInterval` are no-ops.
- A positive timer delay is unsupported. The call throws and records one Compatibility Warning
  per Extension context. The callback is not scheduled.

Promise settlement is bounded to 64 microtasks by default. If a handler remains pending after the
bound, STcli abandons the Extension context, discards its effects, and records a Compatibility
Warning. Later calls to the abandoned context produce no effects.

Browser and UI surfaces the headless runtime does not implement (`document`, `window`, `$`/`jQuery`
outside `$.ajax`, `toastr`, `callPopup`) are control-flow-safe stubs. Each unsupported API warns once
per API name for the lifetime of the Extension's persistent context: the warning appears in the
Plugin Receipt's `script_logs` and is not re-emitted on later invocations of that context. A stub
warning never fails the Turn. Warnings reappear only after the context resets, which happens on
Session adoption and on an enable toggle.

### Pinned real-world Extension fixtures

The repository keeps two modified/derived MIT-licensed Extension fixtures under
`crates/stcli-core/tests/fixtures/real_extensions/`. `metamorph-lifecycle` preserves real
lifecycle, prompt, settings, Secondary Inference, and headless-degradation call shapes.
`request-monitor-wire` preserves fetch, settings, localStorage, and response-handling call shapes
and adds supported `$.ajax` and slash-command usage.

These are test inputs, not bundled Extensions. Their imports, panels, styles, and unrelated browser
behavior are intentionally removed. Each directory has a `provenance.json` sidecar with the
upstream repository, full commit, source paths, license, derivation notes, update procedure, and
digests for every committed fixture file. Tests use only these pinned local bytes. See
[Pinned real-world Extension fixtures](testing.md#pinned-real-world-extension-fixtures) for the
reviewed update procedure.

The deterministic public-engine workflow in
`real_extension_complete_session_workflow_replays_offline` combines both fixtures: lifecycle
observation and Secondary Inference from `metamorph-lifecycle`, and brokered HTTPS plus slash
commands from `request-monitor-wire`. Live Turns record prompt contributions, lifecycle receipts,
egress, and inference. After the TLS server stops and installed JavaScript is removed,
`DryRunRerun` reconstructs the recorded provider request and `ReplayCapsule` validates the recorded
projection hash without executing Extension code or issuing new requests. Secrets injected by the
host never enter Extension memory, receipts, capsules, or projections.

The opt-in `extension_real_provider_smoke` uses a test-local, `max_tokens: 64` derivation of the
pinned lifecycle fixture. Its primary Turn and both Secondary Inference APIs share the same three
provider-agnostic environment variables, require no Extension egress, and assert only structural
completion plus secret exclusion. It does not launch Chromium or Electron, render a graphical pane
or HTML/CSS, or make a visual compatibility claim. See the
[live-provider smoke operator policy](testing.md#live-provider-smoke-test-opt-in).

### Extension slash commands

An `st-bridge` Extension may register a command in either SillyTavern-compatible form:

```js
SillyTavern.registerSlashCommand('/greet', (namedArgs, unnamedArg) =>
  `${namedArgs.who}: ${unnamedArg}`
);
SillyTavern.registerSlashCommand({
  name: 'summarize',
  callback: (namedArgs, unnamedArg) => `${namedArgs.mode}: ${unnamedArg}`,
  description: 'Example command'
});
```

The leading slash is optional during registration and is normalized away. The callback receives
the STscript named arguments as a JSON object first, followed by the single unnamed argument
string. Its string-compatible return value becomes the STscript pipe output; `undefined` produces
an empty output. The latest registration for a name wins within that persistent Session/Extension
context.

Before every callback, STcli hydrates the Extension's persisted local state from
`extension.<id>.*`. `extension.<id>.settings` becomes `extension_settings[id]`, and
`extension.<id>.ls.<key>` becomes `localStorage[key]`. Writes remain limited to that namespace.

Slash commands use the existing STscript unknown-command fallback, so `/greet` is not a second
command language. A missing registration remains the normal `StscriptError::UnknownCommand`.
Each attempted Extension invocation is recorded as exactly one `extension.command` Turn-Trace
event, including arguments, output, callback logs/effects, and proposed state mutations. Extension
command writes commit only when the whole STscript evaluation succeeds. If a later pipeline
command fails, STcli records the failed evaluation and command trace but commits none of its state
writes.

Replay consumes recorded output and state mutations from a successful evaluation. It never
resolves the component, starts QuickJS, executes JavaScript, invokes timers, or contacts the
network. Replay does not initialize the PRNG or run timer callbacks.

### Brokered HTTPS egress

An `st-bridge` Extension can call `fetch(url, options)` and `$.ajax(settings)`. Both route through
the host-controlled egress broker: the Extension never opens a socket, and every exchange that
reaches the transport records a receipt in the Turn Trace. `$.ajax` supports the common
SillyTavern call shape (`url`, `method`/`type`, `headers`, `data`, `dataType`, `success`,
`error`, `complete`); other jQuery surface is not provided.

Egress is denied by default. A call succeeds only when all of the following hold:

1. The Session's plugin pin grants the `brokered-egress` capability.
2. The URL uses `https`.
3. The URL's host exactly matches (case-insensitive) a domain in the pin's egress allow-list.

A denied call does not fail the turn. `fetch` resolves with `ok: false`, `status: 0`,
`statusText: "egress denied"`, and an empty body, and the plugin receipt's `script_logs`
records one warn-level script log stating the denial reason. The same non-fatal shape
applies when the transport itself fails, with `statusText: "transport error"`.

The allow-list lives on the plugin pin, next to the capabilities. `plugin adopt` adds domain
entries with a repeatable `--egress-domain <host>` flag:

```shell
stcli plugin adopt --session <session> <id> --version <version> --digest <digest> \
  --capability brokered-egress --egress-domain api.example.com
```

An allowance may carry a secret injection: the broker resolves a Credential Reference from the
Credential Store, replaces `{secret}` in a value template, and injects the header after the
Extension hands over the request. Secret values never enter Extension memory, receipts, or
hashes. Secret-carrying allowances are configured programmatically through
`EngineCommand::AdoptPlugin`; an interactive consent surface is planned with the Extension import
UX.

Every exchange that reaches the transport records an `EgressReceipt` on the plugin receipt's
`egress` list:

| Field | Content |
|---|---|
| `url` / `method` | The brokered URL and upper-cased HTTP method. |
| `request_hash` | Content hash over `stcli:egress-request:v1` of `{method, url, body, injected_headers}`. Only injected header *names* are hashed; secret values are excluded. |
| `status` | The HTTP status, or `0` when the transport failed before any response. |
| `response_hash` | Content-blob hash of the response body. |
| `body` | The response body, or the transport error description when `status` is `0`. |

Replay never re-executes the Extension. A rerun reuses the recorded receipts and effects, so a
recorded turn replays offline even when the component file is gone.

Dry Runs exercise egress without touching the network: a live broker answers a canned empty `200`
response, and a broker configured with a stub transport forwards to the stub. Native Plugin hosts
can reuse the same boundary through `EgressBroker`, `EgressTransport`, and `StubTransport` in
`stcli-core`.

The deterministic wire-path test uses `stcli-testkit::BrokerTestServer`, which binds an ephemeral
loopback port and serves HTTPS with a generated certificate. A test client explicitly trusts the
exposed certificate PEM before it is passed through `ReqwestTransport::with_client` and
`EgressBroker::with_transport`. This exercises production request serialization, response
parsing, and out-of-band secret injection over a real TLS socket without weakening the production
HTTPS policy or contacting an external service. See
[Local TLS broker boundary](testing.md#local-tls-broker-boundary).

### `SillyTavern.setExtensionPrompt`

`SillyTavern.setExtensionPrompt(key, value, position, depth, scan, role)` records a prompt
contribution during the Extension's prompt-phase invocation. The supported SillyTavern positions
are `IN_PROMPT` (`0`, after the story/character definitions), `IN_CHAT` (`1`, at the requested
depth), and `BEFORE_PROMPT` (`2`, before the story/character definitions). Roles `0`, `1`, and `2`
map to system, user, and assistant. Reusing a key replaces that contribution for the invocation.
The contribution is applied after interceptor read-back and before context pruning and provider
request serialization. Prompt observation and mutation are bridge-inherent surfaces; they do not
add a fifth capability to the fixed consent grant.

### `SillyTavern.getContext()` read-only surface

`SillyTavern.getContext()` returns a frozen, deep-copied snapshot of the active session. The
snapshot is rebuilt for every call; writes to it warn without effect. The frozen fields are:

| Field | Content |
|---|---|
| `name1` | The active persona name (the "user" name). |
| `name2` | The active character name. |
| `chatId` | The active branch ID. |
| `sessionId` | The active session ID. |
| `chat` | The full chat history as `[{role, content}]` turns. |
| `characters` | One-element array with the active character's name and revision hash. |
| `characterId` | The active character's content-addressed revision hash. |
| `groups` | Empty array (group chat is not yet supported). |
| `chatMetadata` | Empty object placeholder. |
| `worldInfo` | Empty array (lorebook exposure is not yet supported). |
| `generationSettings` | The effective generation settings (session + preset + profile defaults). |

The following methods are present as control-flow-safe host APIs. Generation calls return a
Promise and route through the brokered Secondary Inference boundary; they do not mutate the
primary Session Configuration Revision.

| Method | Return value | Requirements |
|---|---|---|
| `setExtensionPrompt` | `undefined` | Prompt slot injection |
| `registerSlashCommand` | `undefined` | Ticket 04 (slash commands) |
| `executeSlashCommands` | `undefined` | Ticket 04 |
| `substituteParams(text)` | `text` unchanged | Macro expansion |
| `getTokenCount(text)` | `0` | Token counting |
| `saveSettingsDebounced` | `undefined` | Ticket 05 (settings) |
| `saveMetadata` | `undefined` | Ticket 05 |
| `updateChatMetadata` | `undefined` | Ticket 05 |
| `generateQuietPrompt(prompt, options)` | `Promise<string>` | `secondary-inference` grant |
| `generateRaw(prompt, options)` | `Promise<string>` | `secondary-inference` grant |

`options.provider` or `options.providerProfile` selects a named `[providers.<name>]` profile;
when omitted, the session's provider profile is used. Other option fields are independent,
per-call Effective Generation Settings and never update session configuration. Each successful
or transport-failed exchange records the profile, canonical request hash, response content hash,
effective settings, returned text, and status in the Plugin Receipt's `inference` array.

Dry Runs use a configured stub transport, or return a deterministic empty completion without
network access. Replay applies the recorded completion without resolving credentials, contacting
the network, or executing the Extension. Denials and malformed calls warn and return an empty
control-flow-safe completion.

Any other `SillyTavern.X` member access returns a no-op function that warns once per property
name. Writes to `SillyTavern` properties warn once and are ignored.

`eventSource` supports `on(event, listener)`, `off(event, listener)`, and `emit(event)` (no-op).
`off` removes a previously registered listener; `emit` is a control-flow-safe no-op.

## The manifest

Every plugin has a `manifest.json` file. The [manifest schema](../schemas/plugin-manifest.schema.json) defines the full format. The engine rejects a manifest that does not match the schema.

| Field | Behavior |
|---|---|
| `schema` | Always `stcli.plugin-manifest/v1`. |
| `id` | The plugin identifier. Lowercase letters, digits, and `-`, with `.` between parts. For example `org.example.my-plugin`. |
| `version` | A semantic version, such as `1.0.0`. |
| `runtime` | `wasm` (default), `script`, or `st-bridge`. The latter runs a SillyTavern Extension headlessly. |
| `component` | The component filename. A `.wasm` file for Wasm, or a `.js` file for Script/st-bridge. |
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
| `generate_interceptor` | Optional JavaScript global function name for an `st-bridge` Extension. It requires the `generate-interceptor` subscription. Prompt read-back is inherent to the bridge runtime. |

The engine validates the component digest before it runs the component. When the file and the digest do not match, the engine rejects the plugin.

### Import a SillyTavern Extension

`extension import <directory>` accepts an unmodified local SillyTavern Extension directory. Core
does not clone or update git repositories. It reads the native `manifest.json`, copies the declared
JavaScript component into a normalized internal `st-bridge` package, and installs it under the
exact component digest. The source directory basename becomes a lowercase kebab-case Extension
identifier.

To persist a global Extension selection, use `extension enable <id> --version <version>
--digest <digest>`. New Sessions auto-adopt that exact digest-pinned revision. Existing Sessions
are changed only by `extension enable <id> --session <session>` or
`extension disable <id> --session <session>`, which append a Session Configuration Revision;
global disable removes the default without changing existing Sessions. `extension list` reports
installed bridge Extensions and global selections while omitting secret values.

| Native field | Normalized internal field or behavior |
|---|---|
| `js` | The single JavaScript component. A string or one-element array is accepted. |
| `version` | `version`; it must be semantic. |
| `display_name`, `author` | Optional display metadata. Neither field determines identity. |
| `generate_interceptor` | `generate_interceptor` plus the corresponding event subscription. |
| `dependencies`, `requires` | Required Plugin dependencies with an unrestricted version range. |
| `optional` | Optional Plugin dependencies with an unrestricted version range. |
| `loading_order` | Numeric ordering tie-breaker after dependency edges. Among Extensions with no ordering edge between them, a lower value runs first, so its prompt changes precede later Extensions' changes in the provider request. |
| `css`, `html`, `i18n` | Ignored with non-blocking Compatibility Warnings. |
| `auto_update` | Ignored. The normalized value is always `false`. |

Other native fields are not persisted. Import does not execute JavaScript and rejects component
paths that are absolute, escape the source directory, or resolve through a symlink outside it.

Adopt the installed Extension into a Session with:

```bash
stcli extension adopt --session <session-id> \
  --version <version> \
  --digest sha256:<component-digest> \
  <extension-id>
```

This creates a Session Configuration Revision that pins the exact version and digest. Read-only
Session context, lifecycle observation, and prompt read-back are inherent to the bridge runtime.
The fixed consent grant covers namespaced state writes, command registration, Brokered HTTPS
Egress, and Secondary Inference. The egress allow-list is empty unless
`--egress-domain <host>` is repeated explicitly.

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
| `chat-completion-prompt-ready` | `chatCompletionPromptReady` | `st-bridge` only: receives mutable `{chat, dryRun}` and its read-back becomes the prompt. |
| `generate-interceptor` | `generateInterceptor` | `st-bridge` only: the manifest-named global receives the ST-shaped mutable chat array. |
| `st-bridge-lifecycle` | `stBridgeLifecycle` | `st-bridge` only: observes ordered lifecycle batches. |
| (command) | `command` | When a user runs a plugin command. Only `observe` and `state-write` effects are allowed. |

The bridge exposes the verified SillyTavern event literals through `event_types`: `APP_READY`, `CHAT_CHANGED`, `GENERATION_STARTED`, `MESSAGE_SENT`, `MESSAGE_RECEIVED`, `GENERATION_ENDED`, and `CHAT_COMPLETION_PROMPT_READY`. Render events (`USER_MESSAGE_RENDERED`, `CHARACTER_MESSAGE_RENDERED`, and `TOOL_CALLS_RENDERED`) accept registrations but are headless no-ops. Bridge callbacks run sequentially; Promise callbacks are drained for at most 64 QuickJS microtask jobs. A still-pending callback is abandoned and its effects discarded.

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
| `brokered-egress` | Brokered `fetch`/`$.ajax` calls through the egress allow-list on the pin. | st-bridge |

A script plugin uses `contribute-prompt` and `write-own-state`. It cannot register a macro or a command, and it cannot abort a turn. For those effects, write a Wasm plugin.

The engine records every applied effect in the authoritative Turn Trace. During replay, the engine reads the recorded effects and does not run the plugin again.

The deterministic public-engine workflow test (`real_extension_complete_session_workflow_replays_offline`) combines two pinned native Extension fixtures. It imports copies, verifies metadata, digest, and ignored visual fields, adopts the exact pin and four-capability grant, and checks session configuration before exercising turns. Its final phase removes the component and stops the local services; dry-run rerun and capsule replay must preserve recorded hashes without resolving or executing the Extension. This is the supported boundary for offline reproducibility.

## See also

- [Usage guide: install and adopt a plugin](guide.md#install-and-adopt-a-plugin)
- [CLI reference](cli.md)
- [Plugins directory](../plugins/README.md)
- [Architecture: plugin system](../ARCHITECTURE.md#plugin-system)
- [ADR 0003: pure Wasm plugins](adr/0003-pure-wasm-plugins.md)
- [ADR 0006: layered plugins](adr/0006-layered-plugins-and-brokered-effects.md)
- [Manifest schema](../schemas/plugin-manifest.schema.json)
- [Plugin WIT world](../wit/plugin.wit)
