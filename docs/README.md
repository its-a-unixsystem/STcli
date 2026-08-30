# STcli documentation

STcli is a [SillyTavern](https://github.com/SillyTavern/SillyTavern)-compatible roleplay engine (see the [SillyTavern Parity Matrix](sillytavern-parity.md)). The engine is the `stcli-core` Rust library. `stcli-cli` is the command-line interface that drives it. For the project overview, start at the [root README](../README.md).

This index maps every document to its audience and purpose.

## For users

| Document | Purpose |
|---|---|
| [Root README](../README.md) | What STcli is, quick start, and the documentation map |
| [Usage guide](guide.md) | Task guide: import, sessions, generation, editing, capsules, plugins |
| [CLI reference](cli.md) | Every command, argument, and value format |
| [Chat Completion presets](presets.md) | Preset semantics, settings precedence, and field classification |
| [Text Completion prompts](text-completion.md) | Text Completion provider profiles, instruct templates, and story strings |
| [Writing plugins](plugins.md) | Wasm and QuickJS script plugins: manifest, script API, and limits |
| [Examples](../examples/README.md) | Sample character, lorebook, and preset artifacts |
| [Plugins directory](../plugins/README.md) | In-tree plugins and the reference proof component |

## For contributors

| Document | Purpose |
|---|---|
| [Architecture](../ARCHITECTURE.md) | System design, C4 diagrams, module layout, and key patterns |
| [Context](../CONTEXT.md) | Domain terminology dictionary: the canonical term for each concept |
| [PRD](../PRD.md) | Accepted scope, requirements, product principles, and roadmap |
| [SillyTavern parity matrix](sillytavern-parity.md) | Feature implementation status, roadmap targets, and gaps vs upstream |
| [Test strategy](testing.md) | Test layers, placement rules, gap-closing workstreams, and CI plan |
| [Agent workflow](agents/) | How coding agents work in this repository |

## Architecture Decision Records

Each ADR records one design decision and its consequences.

| ADR | Decision |
|---|---|
| [0001](adr/0001-authoritative-turn-trace.md) | The Turn Trace is the single source of truth |
| [0002](adr/0002-versioned-compatibility-and-revisions.md) | Bounded compatibility profile and immutable revisions |
| [0003](adr/0003-pure-wasm-plugins.md) | Plugins limited to pure Wasm declarative effects |
| [0004](adr/0004-preset-settings-and-transformations.md) | Resolve preset settings without trusting embedded transformations |
| [0005](adr/0005-granular-deletion-tombstones.md) | Granular deletion as tombstones plus session compaction |
| [0006](adr/0006-layered-plugins-and-brokered-effects.md) | Layered plugins with a single brokered live-effect boundary |
| [0007](adr/0007-external-content-addressed-asset-storage.md) | External content-addressed filesystem storage for media assets |

## Diagrams

The `docs/diagrams/` and `docs/architecture/` directories hold the source and exported images for the diagrams in [`ARCHITECTURE.md`](../ARCHITECTURE.md). Each HTML file is the editable source. Re-export the PNG with headless Chromium after an edit.
