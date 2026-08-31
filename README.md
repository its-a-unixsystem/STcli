# STcli

[![CI](https://github.com/its-a-unixsystem/STcli/actions/workflows/ci.yml/badge.svg)](https://github.com/its-a-unixsystem/STcli/actions/workflows/ci.yml)
[![License: AGPL-3.0-or-later](https://img.shields.io/badge/License-AGPL--3.0--or--later-blue.svg)](LICENSE)
![Rust 1.89+](https://img.shields.io/badge/Rust-1.89%2B-orange.svg?logo=rust)
![Linux](https://img.shields.io/badge/Linux-supported-success?logo=linux&logoColor=white)
![Windows](https://img.shields.io/badge/Windows-supported-success?logo=windows&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-planned-lightgrey?logo=apple&logoColor=white)
![Android](https://img.shields.io/badge/Android-planned-lightgrey?logo=android&logoColor=white)

A roleplay engine written in Rust that runs your [SillyTavern](https://github.com/SillyTavern/SillyTavern) content — character cards, lorebooks, and Chat Completion presets — locally from the terminal.

[**SillyTavern Compatibility**: `[█████████████░░░░░░░]` **67%**](docs/sillytavern-parity.md)

STcli is not a SillyTavern rewrite or fork. It is an independent engine that understands SillyTavern's content formats and prompt behavior, so you can bring your existing cards and presets without starting over. The project is unofficial and unaffiliated with the SillyTavern team.

## Why STcli

- **Your content, your machine.** Import the character cards, lorebooks, and presets you already have. The only network call is to the model provider you choose — no telemetry, no cloud account.
- **Branch freely.** Retries, edits, and greeting changes each create a new branch. The original stays intact, so you can explore without losing anything.
- **See what the model sees.** Inspect prompt order, lore activation, macro expansion, token counts, and pruning for every turn. Export a turn as a self-contained capsule and replay it offline.
- **Explicit compatibility.** Every SillyTavern feature is classified — supported, preserved, fallback, or unsupported — in the [parity matrix](docs/sillytavern-parity.md). No silent behavior differences.
- **Modular and extensible.** The engine is a Rust library (`stcli-core`) with a CLI and a terminal UI built on top. Sandboxed WebAssembly and JavaScript plugins can extend prompt behavior without touching engine internals.

## Getting started

### Requirements

- Rust 1.89+ (pinned in `rust-toolchain.toml`)
- Linux x86-64 or Windows x86-64
- An OpenAI-compatible Chat Completions endpoint (HTTPS only)

### Build

```bash
cargo build --release --bin stcli
```

The binary lands at `target/release/stcli`. During development you can run it through Cargo:

```bash
cargo run --bin stcli -- --help
```

### Quick start

Import a character card from the included examples:

```bash
cargo run --quiet --bin stcli -- --output json artifact import ./examples/character.json
```

Point the engine at your model provider and create a session:

```bash
export OPENAI_API_KEY="your-provider-key"

cargo run --quiet --bin stcli -- --output json session create \
  --character <character-revision> \
  --persona "User" \
  --provider-base-url "https://api.example.com" \
  --provider-api-key-env OPENAI_API_KEY \
  --model "model-name" \
  --tokenizer "tiktoken:o200k_base" \
  --generation-settings '{"temperature":0.8,"max_tokens":512}'
```

Preview what will be sent to the provider with `--dry-run`:

```bash
cargo run --quiet --bin stcli -- --output json message send \
  --session <session-id> \
  --branch <branch-id> \
  --dry-run \
  "Open the library door."
```

Remove `--dry-run` to send for real. Replace the `<placeholders>` with the IDs returned by earlier commands.

For the full walkthrough — lorebooks, presets, plugins, regenerate, swipe, edit, capsules, archive — see the [usage guide](docs/guide.md). For every command and flag, see the [CLI reference](docs/cli.md).

## Terminal UI

Launch the interactive client:

```bash
stcli tui                # session browser
stcli tui <session-id>   # jump straight into a session
```

The TUI gives you a session browser, a chat view with streaming generation, branch and greeting navigation, candidate cycling (swipes), and provider/preset switching — all keyboard-driven with mouse support.

Configure providers and theme in `config.toml` (e.g. `~/.config/stcli/config.toml` on Linux):

```toml
[tui]
theme = "auto"   # "auto", "light", or "dark"

[providers.local]
id = "local"
base_url = "https://api.example.com"
chat_completions_path = "/v1/chat/completions"
api_key_env = "OPENAI_API_KEY"
model = "model-name"
stream = true
```

Provider credentials must reference environment variables — literal secrets are rejected. Set `STCLI_HOME` to keep all config and data under one directory.

## Privacy

- Network traffic goes only to the HTTPS provider you configure.
- API keys are referenced by environment variable name; resolved secrets never touch the database, logs, or CLI output.
- On Unix, directories are created with mode `0700` and the database with `0600`.
- No telemetry. No hosted account. No sync.

See the [security model](docs/guide.md#security-and-privacy) for details.

## SillyTavern compatibility

STcli targets a bounded compatibility profile (`sillytavern-1.18-core`) pinned to SillyTavern 1.18.0. Each preset field and engine behavior is explicitly classified:

| Classification | Meaning |
|---|---|
| **Exact** | Matches SillyTavern's observable behavior; covered by fixtures |
| **Preserved metadata** | Kept losslessly on import, not used in prompt construction |
| **Documented fallback** | A defined alternative exists; marked non-parity |
| **Hard unsupported** | No safe interpretation; blocked outright |

Run the compatibility test suite:

```bash
cargo run --quiet --bin stcli -- --output json compat verify
```

Group chat, STscript, UI extensions, and vector lore are outside the current profile. See the [parity matrix](docs/sillytavern-parity.md) for the full breakdown and the [preset reference](docs/presets.md) for field-level details.

## Documentation

| Document | Audience | Contents |
|---|---|---|
| [Usage guide](docs/guide.md) | Users | Import, sessions, generation, editing, capsules, plugins |
| [CLI reference](docs/cli.md) | Users | Every command, subcommand, and flag |
| [Preset reference](docs/presets.md) | Users | Chat Completion preset semantics and field classification |
| [Text Completion](docs/text-completion.md) | Users | Flat-prompt provider profiles, instruct templates, story strings |
| [Examples](examples/README.md) | Users | Sample character, lorebook, and preset files |
| [Writing plugins](docs/plugins.md) | Users/Devs | Wasm and QuickJS script plugins, manifest, and script API |
| [Plugins directory](plugins/README.md) | Users/Devs | In-tree plugins and the reference proof component |
| [Parity matrix](docs/sillytavern-parity.md) | Everyone | SillyTavern feature coverage and gap analysis |
| [Domain glossary](CONTEXT.md) | Everyone | Terminology used across the code and docs |
| [Architecture](ARCHITECTURE.md) | Contributors | System design, C4 diagrams, module layout |
| [ADRs](docs/adr/) | Contributors | Architecture Decision Records |
| [PRD](PRD.md) | Contributors | Scope, requirements, and roadmap |

## Roadmap

| Version | Theme | Highlights |
|---|---|---|
| **v0.2** ⚠️ | Terminal UI & codecs | Rich chat interface, themes, keyboard/mouse (shipped); external artifact codecs and macOS support (pending) |
| **v0.3** ✅ | Character containers | PNG/APNG/WebP card import, CHARX archives, asset store |
| **v0.4** ✅ | Text Completion | Instruct/context templates, story strings, flat-prompt mode |
| **v0.5** | Group roleplay | Multiple characters, reply-order strategies, group lore and variables |
| **v0.6** ✅ | STscript | Parser, commands, pipes, closures, scoped variables |
| **v0.7** | Retrieval & live plugins | Embedding/vector lore, plugin HTTP/filesystem capabilities |
| **v1.0** | JS compatibility bridge | Sandboxed subset of SillyTavern's extension APIs |
| **v1.x** | Broader ecosystem | Browser frontend, tool calling, multimedia, local daemon |

## Development

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run --quiet --bin stcli -- --output json compat verify
git diff --check
```

For module layout and contribution guidance, see [ARCHITECTURE.md](ARCHITECTURE.md).

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
