# Terminal user interface

Run the terminal user interface with:

```bash
stcli tui
```

## Manage prompt presets

Press `P` from the Sessions screen or Chat view to open the prompt preset picker. The picker lists `No preset` and every imported Chat Completion preset.

| Key | Action |
|---|---|
| `Up` / `Down`, `k` / `j` | Move the selection |
| `Enter` | Select the highlighted preset in Chat; close the picker on Sessions |
| `i` | Import a preset from a file |
| `d` or `Tab` | Show or hide the detail inspector |
| `/` | Filter preset names by a case-insensitive substring |
| `Esc` | Clear an active filter, or close the picker |
| `n` | From Sessions, open New Session with the highlighted preset selected |

The detail inspector shows the prompt count, selected order profile, system-prompt state, prompt order, `temperature`, `top_p`, `max_tokens`, and embedded regex scripts. Script entries are marked `[inert — requires grant]`.

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
