# Resolve preset settings without implicitly trusting transformations

Selecting a Chat Completion preset contributes defaults to Effective Generation Settings; explicit Session configuration overrides the preset, and Compatibility Profile defaults apply last. The `sillytavern-1.18-core` profile does not execute embedded regex scripts: their presence produces machine-consumable compatibility feedback, and a future transformation seam must require a Preset Script Grant for the exact script digest rather than implicit trust. Candidate content preserves accepted raw text, while presentation-only transformations produce a rebuildable Candidate Rendering.

Every preset field is preserved. Fields with observable behavior in SillyTavern 1.18 Chat Completion are implemented for supported provider paths; all others receive an explicit compatibility classification rather than blind provider passthrough.

The execution model for future SillyTavern Extension adapters and persistence of extension-driven prompt activation remain undecided. Neither is required for core preset parity.

**Update (realized):** The anticipated transformation seam now exists. User-input and AI-output regex scripts execute at prompt assembly, gated on a per-digest `preset_script_grants` grant exactly as required above; stored content stays raw and transforms are applied transiently at consume time. Display-only (`markdownOnly`), slash-command, world-info, and reasoning placements, and macro expansion inside replacement strings, remain deferred (see [`regex_script.rs`](../../crates/stcli-core/src/regex_script.rs) and the parity matrix).
