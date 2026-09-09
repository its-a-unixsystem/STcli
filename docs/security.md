# Security model

> **Audience:** Operators, Plugin authors, and contributors reviewing trust-sensitive changes.
> **Goal:** Identify what untrusted code and content can access, and which host checks remain authoritative.

STcli is local-first, has no telemetry, and treats imported content, model output, Plugins, Extensions, and provider responses as untrusted input. Core owns persistence, credentials, network policy, and resource limits.

## Secrets and network access

Provider credentials come from environment references or the platform Credential Store. Resolved secret values are injected only at request time and are redacted from stored errors, receipts, traces, and CLI output.

Plugins and Extensions do not receive raw sockets. Granted HTTPS calls cross the broker with an exact domain allow-list. Secondary Inference crosses the provider broker and records a background Generation Attempt. Denied or ungranted calls cannot bypass either broker.

## Plugin runtimes

Wasm Plugins run in Wasmtime with fuel, epoch timeout, component, input, output, and memory limits. The linker supplies only the exported `run` function contract and no WASI imports, so a component has no filesystem, network, clock, random, subprocess, native-library, or environment access.

Script Plugins run in bounded QuickJS without network or filesystem globals. SillyTavern Extensions use the separate `st-bridge` runtime and only its documented frozen context, namespaced state, prompt, command, and brokered-effect surfaces.

## Artifact codecs

An Artifact codec is a narrower Wasm-only Plugin role. Registration requires exactly `artifact-codec` and `inspect-artifact`; other capabilities, runtimes, subscriptions, settings, prompt slots, commands, and macros are rejected. Inputs contain source or flat Artifact bytes only. They contain no Session, Turn Trace, provider, broker, secret, clock, random, subprocess, or native-library handle.

A codec is an untrusted parser, not an Artifact authority. Core independently enforces:

- interface and operation versions;
- source, payload, asset, count, path, compatibility-report, host JSON, and Wasm memory bounds;
- declared byte sizes and SHA-256 hashes;
- duplicate-key-free JSON and the decoded Artifact kind;
- media validation, safe relative logical paths, uniqueness, and CCv3 embedded references;
- atomic database persistence and cleanup of newly created asset files after failure.

Detection only selects a codec. A codec that claims compatibility and then returns an invalid bundle causes a deterministic failure; Core does not hide the failure by trying its native parser. Ordinary Artifact reads use the stored flat payload. Codec code runs again only for explicit export, using the exact ID, version, component digest, interface version, and external format recorded at import.

See [ADR 0014](adr/0014-artifact-codec-engine-hook.md) and the [Artifact codec reference](artifacts.md).

## Local storage

SQLite holds structured state and content blobs. Media uses content-addressed files under the private data directory. On Unix, STcli creates data directories with mode `0700` and the database with mode `0600`.

Do not place secrets in literal generation settings, Artifact content, Plugin settings, or command arguments. Use environment or Credential References instead.
