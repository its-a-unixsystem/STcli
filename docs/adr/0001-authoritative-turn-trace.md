# Make the Turn Trace authoritative

STcli stores commands and recorded effect outcomes as an append-only Turn Trace in local SQLite; Session Projections, indexes, and snapshots are rebuildable views. We rejected mutable session state as authority because it cannot explain or replay prior attempts, and rejected split SQLite/NDJSON authority because cross-store commits would weaken crash consistency.

## Consequences

Candidates, configuration changes, plugin effects, provider receipts, replay, archive, and purge are expressed through trace facts. The MVP does not semantically compact the trace; only explicit, reference-aware Session purge removes authoritative entries.
