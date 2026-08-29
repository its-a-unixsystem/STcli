# Version compatibility behavior and content revisions

STcli exposes the bounded `sillytavern-1.18-core` Compatibility Profile instead of claiming universal SillyTavern compatibility. Imported artifacts and behavior-affecting Session configurations are immutable revisions pinned by Generation Attempts; changes create new revisions for future Turns rather than rewriting history.

## Consequences

Parity means one hundred percent of the checked-in profile manifest fixtures, not byte-identical HTTP serialization or an arbitrary percentage. Artifact Revision identity includes exact source bytes, so reformatting creates a new revision. Unsupported behavior is classified explicitly as preserved metadata, documented fallback, or hard unsupported.
