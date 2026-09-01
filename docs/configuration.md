# Configuration

Provider profiles are stored in `config/config.toml` under the STcli home directory.

## Provider credentials

STcli never stores literal API secrets in `config.toml` or the SQLite database. A provider profile can reference either an environment variable or an entry in the platform Credential Store.

```toml
[providers.openrouter]
id = "openrouter"
base_url = "https://openrouter.ai"
chat_completions_path = "/api/v1/chat/completions"
credential_key = "openrouter"
model = "anthropic/claude-sonnet-4"
stream = true
timeout_seconds = 120
```

`credential_key` is a Credential Reference. STcli uses the fixed service name `stcli` and the configured alias as the Credential Store account name. If `api_key_env` is also configured and its environment variable contains a non-empty value, that value takes precedence. An absent or empty environment value falls back to `credential_key`.

Manage Credential Store entries from an interactive terminal:

```text
stcli credentials set openrouter
stcli credentials list
stcli credentials delete openrouter
```

`set` reads the secret without terminal echo. `list` audits Credential References used by provider profiles and reports each entry as `configured`, `missing`, or `unavailable`. When both `api_key_env` and `credential_key` are configured and the environment variable holds a non-empty value, `list` reports `"credential_store_used": false`, because the environment variable takes precedence at provider construction. Credential Store access is bounded by a five-second timeout; an unresponsive system keyring fails with a `Store` error instead of hanging, and you can fall back to `api_key_env`:

```toml
[providers.openrouter]
api_key_env = "OPENROUTER_API_KEY"
```
