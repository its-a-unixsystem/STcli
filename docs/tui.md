# Terminal user interface

Run the terminal user interface with:

```bash
stcli tui
```

## Manage provider profiles

Press `p` to open the provider profile picker. Press `c` to copy the highlighted profile. The profile editor opens with the source settings and a non-conflicting name such as `<name>-copy`. Change the model or other fields, then press `Ctrl+S`. Saving creates a new `[providers.<name>]` entry in `config.toml`; it does not replace the source profile.

The picker also supports `a` to add, `e` to edit, and `x` to delete a profile.

## Manage prompt presets

Press `P` from the Sessions screen or Chat view to open the prompt preset picker. The picker lists `No preset` and every imported Chat Completion preset.

| Key | Action |
|---|---|
| `Up` / `Down`, `k` / `j` | Move the selection |
| `Enter` | Select the highlighted preset in Chat; close the picker on Sessions |
| `i` | Import a preset from a file |
| `c` | Copy the highlighted preset and open the tuning form |
| `d` or `Tab` | Show or hide the detail inspector |
| `/` | Filter preset names by a case-insensitive substring |
| `Esc` | Clear an active filter, or close the picker |
| `n` | From Sessions, open New Session with the highlighted preset selected |

The detail inspector shows the prompt count, selected order profile, system-prompt state, prompt order, `temperature`, `top_p`, `max_tokens`, and embedded regex scripts. Script entries are marked `[inert — requires grant]`.

To duplicate a preset, highlight it and press `c`. Set the new name, temperature, maximum context tokens, maximum response tokens, and system-prompt state. Press `Ctrl+S` to import the clone as a new immutable Artifact Revision. The picker reopens with the clone highlighted.

## Import a preset

1. Open the prompt preset picker with `P`, then press `i`.
2. Enter the path to a SillyTavern Chat Completion preset. Paths beginning with `~/` use the current home directory.
3. Press `Enter` to import.
4. Review the highlighted preset, then press `Enter` to apply it when working in Chat.

STcli validates the artifact type before it writes to the database. If the file is another artifact type, the import dialog stays open, preserves the path, and reports the detected and expected types.

A preset with embedded regex scripts produces an import warning. Import does not grant or execute those scripts. Script execution still requires an explicit Preset Script Grant for each exact digest, as described in [Chat Completion presets](presets.md).

## Import while creating a session

In New Session, move to the Preset field and cycle past the imported presets to `<Import preset...>`. Press `Enter`, enter the file path, and import it. New Session reopens with the imported preset selected.

The Character field uses the same artifact import dialog. Each caller resumes at its previous modal after a successful import or cancellation.

## Manage personas

Press `u` on the Sessions screen to open the persona manager.

| Key | Action |
|---|---|
| `Enter` | Select a persona when returning to New Session |
| `a` | Add a persona |
| `c` | Copy the highlighted persona |
| `e` | Edit the highlighted persona |
| `x` | Delete the highlighted persona |
| `i` | Import a SillyTavern `personas_*.json` or `personas.json` backup |
| `Esc` | Close the manager |

Personas are stored in `personas.json` under the STcli configuration directory.

## Select a persona for a new session

The Persona field in New Session cycles through saved personas. Selecting one fills the Session persona name and description. Cycle past the saved personas to `<+ Add new persona...>` or `<[Edit persona...]>`. After saving an inline change, New Session reopens with that persona selected.
