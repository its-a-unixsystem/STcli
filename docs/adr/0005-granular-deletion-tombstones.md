# Implement granular deletion as Tombstones plus Session Compaction

To support deleting individual Turns, Candidates, and Branches without violating the append-only Turn Trace invariant (ADR 0001), STcli uses a two-tier approach:

1. **Logical Deletion (Tombstoning)**: Operations like `turn delete`, `candidate delete`, and `branch delete` merely append a `*.deleted` event to the trace. The Session Projection treats these entities as skipped or hidden. This preserves the immutable history, avoids rewriting SQLite traces, and allows full replay/audit.
2. **Physical Compaction**: To manage disk space growth, especially on mobile devices, a session-wide `session compact` command physically reaps logically deleted entities from the database.

## Consequences

- **Strict Garbage Collection**: Compaction must be reference-safe. If a logically deleted Turn serves as the fork point for an active, un-deleted Branch, the `session compact` operation must leave the Turn physically intact to prevent corrupting the active fork's history.
- **Middle-of-branch Deletion**: Deleting a Turn in the middle of a Branch results in the Session Projection skipping that Turn when constructing context for future generations, effectively splicing the timeline together.
- **Terminology**: The commands are user-facing `delete` (which tombstones) and a system-level `compact` (which reaps), explicitly avoiding the `archive` and `purge` terminology which behaves differently at the Session level.
- **Hide vs Delete**: A distinct `turn hide` operation will be supported. Unlike `delete` (which removes the entity from the Projection entirely), `hide` sets a flag on the Turn so it remains in the Projection for UI rendering but is skipped by the Prompt Builder. Hidden turns survive compaction.
