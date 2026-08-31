# NemoPresetExt directives

`org.stcli.nemo-directives` is STcli's default Artifact-inspection plugin for the supported NemoPresetExt directive subset. It requests only `inspect-artifact` and receives the decoded preset in `input.artifact`.

The plugin returns one typed value:

```json
{
  "constraints": [
    { "kind": "named-group", "name": "response-style", "members": ["concise", "detailed"] }
  ],
  "diagnostics": [
    { "identifier": "concise", "severity": "warning", "kind": "warning", "message": "Author warning" }
  ]
}
```

Supported constraints are `@mutual-exclusive-group`, its legacy `@exclusive-with-category` alias, `@exclusive-with`, and `@max-one-per-category` with `@category`. Supported diagnostics are `@conflicts-with`, `@warning`, `@deprecated`, and unresolved references. Unknown directives are ignored. The Artifact is never modified.
