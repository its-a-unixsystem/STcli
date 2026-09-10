# Artifacts and external codecs

> **Audience:** Plugin authors and Core contributors.
> **Goal:** Understand Artifact import, storage, export, and the `stcli.artifact-codec/v1` contract.

An Artifact Revision is an immutable flat payload plus its kind, source format, semantic hash, source-blob hash, and import event. Media files are stored separately under the content-addressed asset store and linked to the revision by logical path.

## Import and export seams

`EngineCommand::ImportArtifact` is the public mutation seam for every format. It always returns the existing `EngineResult::ArtifactBundle` with a primary Artifact Revision, supplementary revisions, and an asset count.

`EngineQuery::ArtifactSource` is the public export seam. Artifacts imported by Core return their stored source. Artifacts imported through a codec run the exact encoder recorded in their provenance. `EngineQuery::ArtifactCodecProvenance` reports that provenance without running the Plugin.

Direct `Store` import methods keep the built-in Core decoders and do not discover Plugins.

## Codec registration

An external codec is a Wasm Plugin registered as an Artifact inspector. Its manifest and registration must contain exactly these capabilities:

- `artifact-codec`
- `inspect-artifact`

It must subscribe only to `inspect-artifact` and must not declare settings, commands, macros, or prompt slots. Script and `st-bridge` runtimes cannot register as codecs.

The proof package at [`plugins/ccv3-codec`](../plugins/ccv3-codec/) detects, decodes, and encodes CCv3 CHARX archives without a Core rebuild.

## Versioned operations

Every codec request and response carries `interface_version: "stcli.artifact-codec/v1"` and an `operation` discriminator.

- `detect`: receives base64 external source bytes and returns `compatible` plus compatibility items.
- `decode`: receives the same source and returns the external `format`, a flat Artifact bundle, and compatibility items.
- `encode`: receives the recorded external format and the stored flat bundle, then returns base64 external source bytes.

A decoded bundle declares the Artifact kind, `json` as its flat source format, base64 payload, payload SHA-256, and assets. Each asset declares its logical path, base64 bytes, byte size, and SHA-256. Core recomputes rather than trusts all declared values.

Compatibility items contain a stable lowercase `code` and a human-readable `message`. Import provenance retains accepted detection and decode items.

## Bounds

| Value | Limit |
|---|---:|
| External import or encoded export | 2 MiB |
| Decoded Artifact payload | 2 MiB |
| Assets per bundle | 64 |
| One decoded asset | 4 MiB |
| All decoded assets | 8 MiB |
| Logical path | 512 bytes |
| Compatibility items | 32 |
| Compatibility message | 1024 bytes |
| Codec host JSON input/output | 16 MiB each |
| Codec Wasm memory | 64 MiB |

The Plugin host also applies its component, fuel, and wall-clock limits. Oversized imports bypass codec discovery and use the native Core path. Oversized or malformed codec proposals fail; they never fall back after a codec has claimed compatibility.

## Core validation and persistence

Core validates JSON with duplicate-key rejection, derives the Artifact kind, compares the proposed kind and hashes, validates supported media, rejects unsafe or duplicate logical paths, checks CCv3 embedded asset references, and enforces every bound above.

Only after validation does Core open the transaction that inserts the Artifact Revision, asset rows, references, provenance, and import events. If the flat payload already identifies an existing Artifact Revision, codec import rejects the collision rather than attaching provenance or assets to an immutable revision. If any database operation or commit fails, the transaction rolls back and newly created external asset files are removed.

See [ADR 0014](adr/0014-artifact-codec-engine-hook.md) for the design decision and [Security](security.md#artifact-codecs) for the trust model.
