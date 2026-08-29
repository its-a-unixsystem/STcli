# STcli examples

This folder holds starter examples for each primary artifact type that STcli supports:

- [`character.json`](character.json): Character Card V2 JSON artifact
- [`lorebook.json`](lorebook.json): Standalone SillyTavern World Info / Lorebook JSON artifact
- [`preset.json`](preset.json): Chat Completion prompt preset JSON artifact

New to STcli? Start with the [root README](../README.md) and the [usage guide](../docs/guide.md). For the documentation map, see [`docs/README.md`](../docs/README.md).

---

## 1. Character Card (`character.json`)

Implements the **Character Card V2** specification (`spec: "chara_card_v2"`).

- **Character**: *Elspeth*, Master Archivist of the Grand Archive of Oakhaven.
- **Fields demonstrated**:
  - Full character attributes: `name`, `description`, `personality`, `scenario`.
  - Opening message: `first_mes`.
  - Formatted dialogue examples: `mes_example` using `<START>` tags and `{{user}}` / `{{char}}` macros.
  - Character system prompt and post-history instructions: `system_prompt` and `post_history_instructions`.
  - Multiple alternate greetings: `alternate_greetings` (selectable when starting or branching a session).

### Import command

```bash
cargo run --quiet --bin stcli -- --output json \
  artifact import ./examples/character.json
```

---

## 2. Lorebook (`lorebook.json`)

Implements the **SillyTavern 1.18 World Info** specification.

- **World**: *The Grand Archive of Oakhaven*.
- **Features demonstrated**:
  - `grand_archive`: Primary location entry activated by keywords (`"archive"`, `"library"`, etc.).
  - `aether_engine`: Secondary machine entry designed for recursive activation when the Archive is discussed.
  - `restricted_stacks`: High-security vault with selective logic (`selective: true`), requiring secondary keys (`"permit"`, `"key"`, `"forbidden"`, etc.) before triggering.
  - `automaton`: Auxiliary constructs entry demonstrating insertion order and priority.

### Import command

```bash
cargo run --quiet --bin stcli -- --output json \
  artifact import ./examples/lorebook.json
```

---

## 3. Chat Completion Preset (`preset.json`)

Implements the **SillyTavern Chat Completion Prompt Manager** preset specification.

- **Features demonstrated**:
  - `main`: Configures overarching roleplay guidelines and prose formatting.
  - Native slot ordering: `worldInfoBefore`, `charDescription`, `worldInfoAfter`, `dialogueExamples`, `chatHistory`, and `userInput`.
  - In-chat depth injection: `narrativeStyle` injected at depth 4 to guide ongoing conversation quality.

For complex presets (including settings resolution, system message squashing, macro dataflow, and script safety), see [`docs/presets.md`](../docs/presets.md).

### Import command

```bash
cargo run --quiet --bin stcli -- --output json \
  artifact import ./examples/preset.json
```

---

## 4. End-to-End Walkthrough

Once imported, list your revisions to retrieve their content hashes:

```bash
cargo run --quiet --bin stcli -- artifact list
```

Create a session using all three artifacts:

```bash
cargo run --quiet --bin stcli -- --output json session create \
  --character <character-hash> \
  --lorebook <lorebook-hash> \
  --preset <preset-hash> \
  --persona "Scholar" \
  --provider-base-url "https://api.openai.com" \
  --provider-chat-path "/v1/chat/completions" \
  --provider-api-key-env OPENAI_API_KEY \
  --model "gpt-4o-mini" \
  --tokenizer "tiktoken:o200k_base" \
  --generation-settings '{"temperature":0.8,"max_tokens":512}'
```

Preview prompt composition with a dry run (no API call, no tokens consumed):

```bash
cargo run --quiet --bin stcli -- --output json message send \
  --session <session-id> \
  --branch <branch-id> \
  --dry-run \
  "Could you show me where the Aether Engines are located?"
```
