# Artifacts and external codecs

> **Audience:** Plugin authors and Core contributors.
> **Goal:** Understand Artifact import, storage, export, and the `stcli.artifact-codec/v1` contract.

An Artifact Revision is an immutable flat payload plus its kind, source format, semantic hash, source-blob hash, and import event. Media files are stored separately under the content-addressed asset store and linked to the revision by logical path.

## Import and export seams

`EngineCommand::ImportArtifact` is the public mutation seam for every format. It always returns the existing `EngineResult::ArtifactBundle` with a primary Artifact Revision, supplementary revisions, and an asset count.

`EngineQuery::ArtifactSource` is the public export seam. Artifacts imported by Core return their stored source. Artifacts imported through a codec run the exact encoder recorded in their provenance. `EngineQuery::ArtifactCodecProvenance` reports that provenance without running the Plugin.

Direct `Store` imports are the recovery-only canonical JSON bootstrap. They reject image and archive containers and never discover or execute Plugins.

## Codec registration

An external codec is a Wasm Plugin registered as an Artifact inspector. Its manifest and registration must contain exactly these capabilities:

- `artifact-codec`
- `inspect-artifact`

It must subscribe only to `inspect-artifact`, declare an `artifact_codec` block containing its interface versions and external formats, and must not declare settings, commands, macros, or prompt slots. Script and `st-bridge` runtimes cannot register as codecs.

STcli bundles `org.stcli.sillytavern-codec` 1.0.0 and installs it from embedded bytes without network access. It handles Character Card V1/V2/V3 JSON, PNG/APNG/WebP cards, CHARX archives, Lorebooks, and Chat Completion presets. Removing it creates a persistent opt-out; `stcli plugin restore-defaults` clears that marker and repairs the package and registration.

## Versioned operations

Every codec request and response carries `interface_version: "stcli.artifact-codec/v1"` and an `operation` discriminator.

- `detect`: receives base64 external source bytes and returns `compatible` plus compatibility items. The engine evaluates every registered codec and rejects an import when more than one claims it.
- `decode`: receives the same source and returns the external `format`, a flat Artifact bundle, and compatibility items.
- `encode`: receives the recorded external format and stored flat bundle, including supplementary Artifacts, then returns base64 external source bytes.

A decoded bundle declares the Artifact kind, stored source format, base64 payload, payload SHA-256, assets, and supplementary Artifacts. Each asset declares its logical path, base64 bytes, byte size, and SHA-256. Each supplementary Artifact declares its logical path, kind, source format, payload, byte size, SHA-256, and whether it originated from data embedded in the primary Artifact. Core recomputes declared values and validates embedded ownership rather than trusting them.

Compatibility items contain a stable lowercase `code` and a human-readable `message`. Import provenance retains accepted detection and decode items.

## Bounds

| Value | Limit |
|---|---:|
| External import or encoded export | 2 MiB |
| Decoded Artifact payload | 2 MiB |
| Assets per bundle | 64 |
| Supplementary Artifacts per bundle | 64 |
| All supplementary Artifact payloads | 8 MiB |
| One decoded asset | 4 MiB |
| All decoded assets | 8 MiB |
| Logical path | 512 bytes |
| Compatibility items | 32 |
| Compatibility message | 1024 bytes |
| Codec host JSON input/output | 16 MiB each |
| Codec Wasm memory | 64 MiB |

The Plugin host also applies its component, fuel, and wall-clock limits. Sources above 2 MiB skip codec execution; canonical JSON up to the Core Artifact limit can still use the bootstrap path. An external container without an accepting codec fails with guidance to run `stcli plugin restore-defaults`. Malformed codec proposals fail and never fall back after a codec claims ownership.

## Core validation and persistence

Core parses the codec's flat JSON payload with duplicate-key rejection, validates the declared Artifact kind against the payload, compares hashes, validates supported media, rejects unsafe or duplicate logical paths, checks embedded asset ownership, and enforces every bound above. Core does not parse SillyTavern image metadata or CHARX archives.

Only after validation does Core open the transaction that inserts the Artifact Revision, asset rows, references, provenance, and import events. If the flat payload already identifies an existing Artifact Revision, codec import rejects the collision rather than attaching provenance or assets to an immutable revision. If any database operation or commit fails, the transaction rolls back and newly created external asset files are removed. Portable Capsules retain the external revision source together with the accepted codec bundle and provenance, so import can restore flat payloads, assets, supplementary Artifacts, and exact codec ownership without executing codec code.

See [ADR 0014](adr/0014-artifact-codec-engine-hook.md) for the design decision and [Security](security.md#artifact-codecs) for the trust model.
