# External Artifact codecs run at a bounded pre-Artifact Wasm seam

## Status

accepted

## Context

External containers such as CCv3 CHARX, PNG, and WebP combine Artifact data with media assets. Adding every container parser to Core couples untrusted format handling to storage and requires a Core release for each format revision. Letting a codec write directly to SQLite or the asset directory would instead bypass Artifact validation, content addressing, and atomic persistence.

The existing Plugin host already runs WebAssembly Components without WASI imports and enforces component, input, output, memory, fuel, and wall-clock limits. The existing Artifact import and export engine operations are the stable caller-facing seams.

## Decision

A registered `artifact-codec` Plugin may run before Core creates an Artifact Revision. The codec interface is the versioned JSON protocol `stcli.artifact-codec/v1` carried through the existing `stcli:plugin@1.0.0` WIT `run` function.

Import has two codec operations:

1. `detect` receives at most 2 MiB of base64 source bytes and reports whether the codec accepts them, plus bounded machine-readable compatibility items.
2. `decode` proposes one flat JSON Artifact payload and bounded extracted assets. Every payload and asset includes its byte size and SHA-256 digest.

Core independently decodes and validates the proposed Artifact kind and JSON, checks the format, hashes, asset media, logical paths, duplicate paths, counts, individual sizes, total size, and embedded CCv3 asset references. Core then writes the Artifact Revision, assets, references, and codec provenance in one SQLite transaction, with external asset cleanup if the transaction fails. A rejected proposal persists nothing.

Export loads the recorded codec provenance and invokes the exact Plugin ID, semantic version, and component digest with an `encode` operation. Ordinary Artifact reads use the stored flat payload and never execute the codec. Built-in Core import and export remain the fallback for Artifacts without codec provenance and for import sources above the codec limit.

A codec registration is valid only when the Plugin:

- uses the Wasm runtime;
- subscribes only to `inspect-artifact`;
- requests and is registered with exactly `artifact-codec` and `inspect-artifact`;
- declares no prompt slots, commands, macros, or settings schema.

The Wasmtime linker provides no WASI or host imports. Codec inputs contain no Session, Turn Trace, provider, broker, secret, clock, random, subprocess, or native-library handle.

## Consequences

- Format parsers can ship independently without expanding Core's trusted parser set.
- The bundled `plugins/ccv3-codec` package implements all SillyTavern Artifact formats currently supported by Core and is materialized through the default-package lifecycle.
- Stored provenance makes codec selection deterministic and inspectable without executing code during ordinary reads.
- Export of a codec-originated Artifact requires the exact recorded component to remain installed. Removing or changing the active registration does not silently select another encoder.
- The v1 protocol uses base64 JSON, which copies data. Strict source, bundle, and host-memory limits make that cost explicit. A future binary interface requires a new interface version.
