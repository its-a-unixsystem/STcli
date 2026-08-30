# External content-addressed filesystem storage for media assets

**Status:** accepted

STcli stores textual and structured JSON artifacts (cards, lorebooks, presets) directly in SQLite `content_blobs`, but binary media assets (avatars, expression sprites, backgrounds, audio) are persisted in a dedicated filesystem content-addressed store (`$STCLI_DATA/assets/sha256/`) with SQLite tracking metadata (`assets`) and owner references (`asset_refs`). We rejected storing binary media in SQLite `content_blobs` because multi-megabyte image and audio bundles cause write-ahead log (WAL) bloat, degrade query latency, and make database vacuuming expensive; and rejected unhashed filesystem storage because deduplication and tamper detection across multi-asset card archives (`.charx`) require immutable content hashes.

## Considered Options

- **Store all binary assets in SQLite `content_blobs`.** Rejected: character containers with large animated APNGs, multi-expression bundles, and audio clips would rapidly bloat `stcli.sqlite3` into gigabytes, increasing memory usage and checkpointing latency.
- **Hybrid size threshold (inlining small assets <= 512 KB into SQLite).** Rejected: introduces dual storage paths, inconsistent GC mechanisms, and complicates zero-copy filesystem serving across TUI and future webviews.
- **Dedicated content-addressed filesystem directory with SQLite reference tracking.** Selected: keeps the database lightweight and fast while enabling zero-copy asset reading and deterministic SHA-256 deduplication.

## Consequences

- **Filesystem and database sync:** Binary assets are written atomically to disk (temporary file and rename) before SQLite commits metadata and references in the same transaction.
- **Reference-counted garbage collection:** Removing an artifact revision or character removes corresponding `asset_refs`; explicit or scheduled pruning (`Store::prune_unreferenced_assets`) purges unreferenced disk files.
- **Magic-byte validation and limits:** Ingestion strictly validates magic bytes against a media allowlist (PNG, APNG, WebP, JPEG, GIF, AVIF, WAV, MP3, OGG) and enforces a 32 MB per-asset limit, rejecting arbitrary binaries.
