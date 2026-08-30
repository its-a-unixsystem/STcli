# Text Completion prompts

STcli can talk to two kinds of provider endpoint:

- **Chat Completion**: The engine sends a list of role-tagged messages. This is the default.
- **Text Completion**: The engine sends one flat text prompt. The provider returns raw text.

Text Completion suits local backends and instruct-tuned models that expect a single formatted string. This document explains how to configure a Text Completion provider profile and how STcli builds the flat prompt.

> **Note**: Text Completion is untested against a live provider. The code is implemented, but no end-to-end run has confirmed the behavior yet. Use it with care and report problems.

For the argument syntax of every command, see [`cli.md`](cli.md). For domain terms, see [`CONTEXT.md`](../CONTEXT.md).

## Contents

- [How Text Completion differs](#how-text-completion-differs)
- [Configure a Text Completion profile](#configure-a-text-completion-profile)
- [Provider profile fields](#provider-profile-fields)
- [Instruct template fields](#instruct-template-fields)
- [Context formatting fields](#context-formatting-fields)
- [The story string](#the-story-string)
- [How the flat prompt is built](#how-the-flat-prompt-is-built)

## How Text Completion differs

In Chat Completion mode, the engine keeps each message separate. The provider joins them.

In Text Completion mode, the engine joins everything into one string before it sends the request. It wraps each message with instruct sequences. It builds a story block from the character and world data. It computes stop sequences from the template.

Both modes share the same prompt pipeline. Lore, macros, regex, and token budgeting run first. Only the final formatting step differs.

## Configure a Text Completion profile

You set Text Completion through a provider profile, not through a session flag. The `session create` flags always build a Chat Completion provider.

Follow these steps to use Text Completion:

1. Write a profile file that sets `format_mode` to `text-completion`. See the example that follows.
2. Add the profile with `stcli profile add <name> --file <path>`.
3. Create a session with `stcli session create --character <revision> --provider-profile <name>`.

This is a minimal Text Completion profile in JSON:

```json
{
  "id": "local-koboldcpp",
  "base_url": "https://127.0.0.1:5001",
  "chat_completions_path": "/v1/chat/completions",
  "format_mode": "text-completion",
  "completions_path": "/v1/completions",
  "model": "local-model",
  "stream": true,
  "timeout_seconds": 120,
  "instruct_template": {
    "input_sequence": "### Instruction:\n",
    "output_sequence": "### Response:\n",
    "system_sequence": "### System:\n",
    "wrap": true,
    "stop_sequence": "### Instruction:"
  },
  "context_formatting": {
    "story_string": "{{system}}\n{{description}}\n{{personality}}\n{{scenario}}",
    "chat_start": "***",
    "example_separator": "***"
  }
}
```

A Text Completion profile must set `completions_path`, `instruct_template`, and `context_formatting`. STcli rejects the profile at request time when one is missing.

## Provider profile fields

These fields select and drive Text Completion. All other provider fields work the same as Chat Completion. For the shared fields, see the [provider profile format](cli.md#provider-profile-file-format).

| Field | Type | Behavior |
| --- | --- | --- |
| `format_mode` | string | `chat-completion` (default) or `text-completion`. |
| `completions_path` | string | Endpoint path for text requests. Required for Text Completion. Joined to `base_url`. |
| `instruct_template` | object | Wraps each message with role sequences. See [Instruct template fields](#instruct-template-fields). |
| `context_formatting` | object | Builds the story block and separators. See [Context formatting fields](#context-formatting-fields). |

The `chat_completions_path` field stays required in both modes. STcli uses `completions_path` for the request when `format_mode` is `text-completion`.

## Instruct template fields

The instruct template wraps each message with a start sequence and a suffix. Every field defaults to an empty string or `false`. Set only the fields your model needs.

| Field | Type | Behavior |
| --- | --- | --- |
| `input_sequence` | string | Start sequence before a user message. |
| `output_sequence` | string | Start sequence before an assistant message. |
| `system_sequence` | string | Start sequence before a system message. |
| `input_suffix` | string | Text after a user message. |
| `output_suffix` | string | Text after an assistant message. |
| `system_suffix` | string | Text after a system message. |
| `first_input_sequence` | string | Overrides `input_sequence` for the first user message. |
| `last_input_sequence` | string | Overrides `input_sequence` for the last user message. |
| `first_output_sequence` | string | Overrides `output_sequence` for the first assistant message. |
| `last_output_sequence` | string | Overrides `output_sequence` for the final assistant reply. |
| `last_system_sequence` | string | Overrides `system_sequence` for the last system message. |
| `stop_sequence` | string | Stop string sent to the provider. |
| `wrap` | boolean | When `true`, adds a newline after each sequence. |
| `macro` | boolean | When `true`, evaluates macros inside the sequences (such as `{{user}}`, `{{char}}`, `{{personaDescription}}`, and `{{name}}`). |
| `names_behavior` | string | `none`, `force` (default), or `always`. `always` adds a `Name:` prefix to each message. |
| `skip_examples` | boolean | When `true`, writes example messages as plain `Name: text` lines. |
| `system_same_as_user` | boolean | When `true`, formats system messages like user messages. |
| `sequences_as_stop_strings` | boolean | When `true`, adds the role sequences to the stop strings. |
| `story_string_prefix` | string | Text before the story block. |
| `story_string_suffix` | string | Text after the story block. |

## Context formatting fields

Context formatting builds the story block and the separators between conversation parts.

| Field | Type | Behavior |
| --- | --- | --- |
| `story_string` | string | Handlebars template for the story block. See [The story string](#the-story-string). |
| `example_separator` | string | Separator before each example dialogue block. |
| `chat_start` | string | Separator before the live chat starts. |
| `turn_separator` | string | Separator between turns. |
| `use_stop_strings` | boolean | When `true`, sends the template stop strings to the provider. |
| `names_as_stop_strings` | boolean | When `true`, adds `Persona:` and `Character:` to the stop strings. |

## The story string

The story string is a [Handlebars](https://handlebarsjs.com/) template. STcli fills it with the character, persona, and world data, then puts the result at the top of the prompt.

You can use these placeholders in `story_string`:

- `{{system}}`: The main system prompt.
- `{{description}}`: The character description.
- `{{personality}}`: The character personality.
- `{{scenario}}`: The scenario text.
- `{{wiBefore}}`: World info placed before the character.
- `{{wiAfter}}`: World info placed after the character.
- `{{persona}}`: The persona name.
- `{{user}}`: The persona name.
- `{{char}}`: The character name.
- `{{personaDescription}}`: The rendered user persona description.
- `{{persona_description}}`: Alias for `{{personaDescription}}`.

STcli renders the template with no HTML escaping. The placeholder values keep their exact text.

## How the flat prompt is built

STcli builds the flat prompt in this order:

1. The engine renders the story string and wraps it with the story prefix and suffix.
2. The engine walks each conversation message in order.
3. Before a message, the engine adds a separator when the block changes (example, chat start, or new turn).
4. The engine wraps each message with its role sequence and suffix.
5. At the end, the engine adds the assistant output sequence to prompt the reply.
6. When an assistant prefill is set, the engine appends it after the final sequence.

STcli also computes the stop sequences from the instruct template and context formatting. It sends them with the request so the provider stops at the right point.

To preview the exact request without a provider call, add `--dry-run` to `message send`. For prompt inspection, see [`guide.md`](guide.md#inspect-a-prompt).
