# Terminal user interface

Run the terminal user interface with:

```bash
stcli tui
```

## Branch from Chat

In Chat history, focus a Turn and press `b` to create a new child Branch at that Turn. The new Branch excludes the focused Turn from its inherited history and pre-fills the composer with that Turn's user content, ready to resend or edit. With the Greeting (or no Turn) focused, `b` branches from the start with an empty composer. Branch creation switches Chat to the new Branch and confirms with a toast; while a Generation Attempt is streaming, `b` is ignored.

Press `B` to open the Branch list popup. The popup includes newly created Branches.

## Manage sessions

On the Sessions screen, highlight a Session and press `c` to duplicate its root Branch lineage. Archived Sessions remain listed with an `[archived]` marker and can also be duplicated. The name prompt starts with `<name> (copy)` and advances to `<name> (copy 2)`, `<name> (copy 3)`, and so on when needed. Edit the name, then press `Enter` to create the independent, active Duplicated Session or `Esc` to cancel.

After duplication, the Sessions screen remains open and highlights the new Session. `Ctrl+c` keeps its global quit behavior.

## Recover generation settings in Chat

With Chat history focused, press `s` to edit the active Session's reasoning effort, temperature, and maximum response tokens. Reasoning effort can be omitted, set to `low`, `medium`, or `high` with `Left`/`Right`, or entered as a custom value. Press `Ctrl+S` to append a new Session Configuration Revision. Press `r` to regenerate the focused Turn with the updated settings.

## Manage provider profiles

Press `p` to open the provider profile picker. Press `c` to copy the highlighted profile. The profile editor opens with the source settings and a non-conflicting name such as `<name>-copy`. Change the model or other fields, then press `Ctrl+S`. Saving creates a new `[providers.<name>]` entry in `config.toml`; it does not replace the source profile.

Choose `Credential Store` to enter a Credential Alias and a masked API key. The secret is written to the operating-system Credential Store; only `credential_key` is written to `config.toml`. In edit mode, `[Configured in Keyring]` indicates that the profile already has a Credential Reference without retrieving the secret or opening the keyring. Leave the secret field blank to preserve the existing entry. Choose `Environment Variable` to configure `api_key_env` instead.

The picker also supports `a` to add, `e` to edit, and `x` to delete a profile. Deleting a profile does not delete its Credential Store entry because another profile can reference the same alias.


## Manage prompt presets

Press `P` from the Sessions screen or Chat view to open the prompt preset picker. The picker lists `No preset` and every imported Chat Completion preset.

| Key | Action |
|---|---|
| `Up` / `Down`, `k` / `j` | Move the preset selection |
| `Right` | Enter the prompt-order details list |
| `Left` | Return focus to the preset list |
| `Up` / `Down` (in details) | Move through prompt-order entries |
| `Space` (in details) | Toggle the focused Prompt Order Entry immediately |
| `Enter` (in list) | Open details for the highlighted preset |
| `Enter` (in details) | Select the highlighted preset; in Chat, re-pin the Session; in New Session, preselect it; on Sessions, close the picker |
| `i` | Import a preset from a file |
| `c` | Copy the highlighted preset and open the tuning form |
| `d` or `Tab` | Show or hide the detail inspector |
| `PgDn` / `PgUp` | Scroll the detail inspector down or up (when visible) |
| `Shift+Down` / `Shift+Up` | Scroll the detail inspector line by line (when visible) |
| `/` | Filter preset names by a case-insensitive substring |
| `Esc` | Clear an active filter; from details, return to the list; otherwise close the picker |
| `n` | From Sessions, open New Session with the highlighted preset selected |

The detail inspector shows the prompt count, selected order profile, system-prompt state, prompt order, and every prompt's identifier, role, and full content. It also shows Compatibility Warnings, `temperature`, `reasoning_effort`, `top_p`, `max_tokens`, and embedded regex scripts. Prompt Order Entries show their current enabled state. Toggling creates a new immutable Artifact Revision, selects it in the refreshed picker, and shows a toast naming the preset and short revision suffix. In Chat, the toast also says that the open Session was re-pinned. Rows show a short revision suffix only when multiple revisions share a preset label, and the open Session's revision is marked `pinned`. Script entries are marked `[inert — requires grant]`. When the content exceeds the pane, scroll with `PgUp`/`PgDn` or `Shift+Up`/`Shift+Down`.

For presets using the supported NemoPresetExt directives, enabling an exclusive entry auto-disables every enabled sibling in the same atomic update. The toast names the auto-disabled entries. `@conflicts-with`, `@warning`, `@deprecated`, and unresolved references appear as non-blocking Compatibility Warnings in the details and at flip time.

Disabling a structural marker such as `chatHistory`, `worldInfoBefore`, or `worldInfoAfter` is permitted for compatibility. The picker warns at flip time, and subsequent turn preparation and Dry Runs carry a non-blocking warning that names the disabled marker.

In Chat, `Ctrl+Space` sets a Prompt Order Override for the focused entry without editing the preset Artifact Revision. The list labels effective values as `preset` or `override`. Press `r` on an overridden entry to remove the override and inherit the preset default again.

To duplicate a preset, highlight it and press `c`. Set the new name, temperature, reasoning effort, maximum context tokens, maximum response tokens, and `use_sysprompt`. Clear reasoning effort to omit it from the clone. Press `Ctrl+S` to import the clone as a new immutable Artifact Revision. The picker reopens with the clone highlighted.

## Browse and import artifact files

The artifact import dialog combines a path input bar with an interactive directory browser. It opens when importing character cards or presets from the TUI. Preset imports also show an optional Name field.

| Key | Action |
|---|---|
| `Up` / `Down`, `k` / `j` | Move the selection in the directory list |
| `Enter`, `Right`, `l` | Open a directory or import the highlighted file |
| `Backspace`, `Left`, `h` | Move up to the parent directory (also via the `..` entry) |
| `Tab` | For presets: move from Path to Name to the directory list. For character cards: complete the path segment, or move focus to the list. In the list: return focus to Path |
| `Down` | Move from Path to Name for presets, then to the directory list |
| `Up` on the top row | Return focus from the list to Name for presets, then to Path |
| `.` or `Ctrl+H` | Toggle hidden dotfiles in the directory list |
| `Esc` | Cancel and return to the previous dialog |

The browser lists only files matching the expected artifact format: `.png`, `.apng`, `.webp`, `.charx`, and `.json` for character cards, `.json` for presets. Subdirectories always remain visible and traversable. Hidden dotfiles are hidden by default. Directories that cannot be read show an `[Access Denied]` notice, and the `▲`/`▼` markers in the list title indicate more entries above or below.

Typing in the path bar supports `~/` home expansion and relative paths resolved against the browsed directory. For character-card imports, pressing `Tab` completes the current path segment: a single match completes in place (directories gain a trailing `/`), and ambiguous prefixes extend to the longest common prefix and show a match count. Entering a directory path navigates the browser into it; entering a file path imports the file.

When importing a preset, enter a custom Name to set its `preset_name`. If Name is empty, STcli uses the embedded `preset_name`, then the embedded `name`, then the filename stem. This keeps the preset label readable in the preset picker and New Session form.

The browser reopens in the directory visited by the previous import for the duration of the application session.

STcli validates the artifact type before it writes to the database. If the file is another artifact type, the import dialog stays open, preserves the path, and reports the detected and expected types.

A preset with embedded regex scripts produces an import warning. Import does not grant or execute those scripts. Script execution still requires an explicit Preset Script Grant for each exact digest, as described in [Chat Completion presets](presets.md).

## Import while creating a session

In New Session, move to the Preset field and cycle past the imported presets to `<Import preset...>`. Press `Enter`, enter the file path and an optional custom name, and import it. New Session reopens with the imported preset selected.

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
