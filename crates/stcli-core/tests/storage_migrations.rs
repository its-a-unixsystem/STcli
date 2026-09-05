//! Storage migration harness.
//!
//! Historical stores are load-bearing fixtures: each `tests/fixtures/db/v{N}.sql`
//! is a diffable SQL dump of a small populated store at schema version `N`. The
//! harness proves that opening such a store migrates it to the current schema,
//! preserves candidate ancestry through the migration (the `20a7fa1` regression
//! shape), and rebuilds the same Session Projection from the authoritative Turn
//! Trace. A recorded projection hash beside the dumps is the equivalence check.
//!
//! Regenerate the fixtures and manifest with the previous-version shapes after a
//! schema change:
//!
//! ```bash
//! STCLI_REGENERATE_DB_FIXTURES=1 cargo test -p stcli-core --test storage_migrations
//! cargo test -p stcli-core --test storage_migrations
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, types::ValueRef};
use serde_json::{Value, json};
use stcli_core::{EntityId, StorageError, Store, session_projection_hash};
use stcli_testkit::{configuration, fixtures};
use tempfile::tempdir;

const REGENERATE_ENV: &str = "STCLI_REGENERATE_DB_FIXTURES";

/// Schema versions we ship historical fixtures for. The oldest is chosen so the
/// ratchet (`SCHEMA_VERSION <= max fixture version + 1`) forces a new dump on
/// every schema bump.
const FIXTURE_VERSIONS: [i64; 7] = [5, 6, 7, 8, 9, 10, 11];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn historical_stores_migrate_and_rebuild_to_recorded_projection() {
    if regenerating() {
        regenerate_fixtures().await;
    }

    let current = current_schema_version();
    let manifest = read_manifest();
    assert_eq!(
        manifest["schema_version"].as_i64().unwrap(),
        current,
        "manifest schema version is stale; regenerate with {REGENERATE_ENV}=1",
    );

    let mut recorded_hashes = Vec::new();
    for version in FIXTURE_VERSIONS {
        let entry = &manifest["fixtures"][version.to_string()];
        let expected_hash = entry["projection_hash"].as_str().unwrap();
        let expected_ancestry = ancestry_pairs(&entry["ancestry"]);

        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        load_dump(&path, &read_dump(version));

        let mut store = Store::open(&path).unwrap();
        assert_eq!(
            store.schema_version().unwrap(),
            current,
            "v{version} fixture did not migrate to the current schema",
        );

        // The migration alone — before any trace rebuild — must preserve the
        // candidate ancestry present in the fixture (the 20a7fa1 regression).
        assert_eq!(
            migrated_ancestry(&path),
            expected_ancestry,
            "v{version} migration dropped candidate ancestry",
        );

        store.rebuild_session_projections().unwrap();
        let rebuilt = projection_hash(&store);
        assert_eq!(
            rebuilt, expected_hash,
            "v{version} rebuilt projection does not match the recorded hash",
        );

        // Idempotence: opening (migrating) an already-migrated store and
        // rebuilding again leaves the projection hash unchanged.
        drop(store);
        let mut reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.schema_version().unwrap(), current);
        reopened.rebuild_session_projections().unwrap();
        assert_eq!(
            projection_hash(&reopened),
            expected_hash,
            "v{version} projection hash changed after a second migration",
        );

        recorded_hashes.push(rebuilt);
    }

    // Every historical fixture carries the same authoritative trace, so they
    // must all rebuild to one canonical projection.
    assert!(
        recorded_hashes.windows(2).all(|pair| pair[0] == pair[1]),
        "historical fixtures rebuilt to divergent projections: {recorded_hashes:?}",
    );
}

#[test]
fn opening_a_newer_store_fails_cleanly() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("stcli.sqlite3");
    let current = current_schema_version();
    let future = current + 1;
    {
        Store::open(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations(version) VALUES (?1)",
                [future],
            )
            .unwrap();
    }

    match Store::open(&path) {
        Err(StorageError::SchemaTooNew { found, supported }) => {
            assert_eq!(found, future);
            assert_eq!(supported, current);
        }
        Err(other) => panic!("expected SchemaTooNew, got {other:?}"),
        Ok(_) => panic!("expected SchemaTooNew, but the store opened"),
    }
}

#[test]
fn schema_version_ratchet_requires_a_previous_version_fixture() {
    let current = current_schema_version();
    let max_fixture = FIXTURE_VERSIONS.iter().copied().max().unwrap();
    assert!(
        current <= max_fixture + 1,
        "SCHEMA_VERSION {current} outran the migration fixtures (max v{max_fixture}); add v{max_fixture} to FIXTURE_VERSIONS with its column shape and regenerate with {REGENERATE_ENV}=1",
    );
}

// ---------------------------------------------------------------------------
// Fixture regeneration
// ---------------------------------------------------------------------------

async fn regenerate_fixtures() {
    let source_directory = tempdir().unwrap();
    let source_path = source_directory.path().join("canonical.sqlite3");
    build_canonical_store(&source_path).await;

    fs::create_dir_all(fixtures_dir()).unwrap();
    let source = Connection::open(&source_path).unwrap();

    let mut manifest_fixtures = serde_json::Map::new();
    for version in FIXTURE_VERSIONS {
        let dump = dump_store(&source, version);
        fs::write(dump_path(version), &dump).unwrap();

        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        load_dump(&path, &dump);
        let ancestry = migrated_ancestry(&path);
        let mut store = Store::open(&path).unwrap();
        store.rebuild_session_projections().unwrap();
        let hash = projection_hash(&store);

        manifest_fixtures.insert(
            version.to_string(),
            json!({
                "projection_hash": hash,
                "ancestry": ancestry
                    .into_iter()
                    .map(|(child, parent)| json!([child, parent]))
                    .collect::<Vec<_>>(),
            }),
        );
    }

    let manifest = json!({
        "schema_version": current_schema_version(),
        "fixtures": manifest_fixtures,
    });
    fs::write(
        manifest_path(),
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
    )
    .unwrap();
}

/// Build one representative session with the real engine: two branches (a root
/// and a fork carrying `forked_from_turn_id`), several turns, a failed attempt
/// with no candidate, and a continued candidate whose `parent_candidate_id`
/// exercises the ancestry-preservation path.
async fn build_canonical_store(path: &Path) {
    let mut store = Store::open(path).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let session_id = created.session.session_id;
    let root = created.branch.branch_id;

    // Turn 1 on the root branch: a completed generation.
    let first = send_failed_turn(&mut store, session_id, root, "Tell me about the library").await;
    let first_attempt = last_attempt(&store, first);
    let first_candidate = record_completed_candidate(
        &mut store,
        session_id,
        first,
        first_attempt,
        None,
        "generated",
        "The library is vast and quiet.",
    );

    // A continued candidate on the same turn — ancestry with a non-null attempt.
    let continue_attempt = record_started_attempt(&mut store, session_id, first, first_attempt);
    record_completed_candidate(
        &mut store,
        session_id,
        first,
        continue_attempt,
        Some(first_candidate),
        "continued",
        "It holds ancient, brittle scrolls.",
    );

    // Editing the user turn forks a new branch anchored at the fork point; the
    // regeneration itself fails, leaving the fork branch and its turn behind.
    store
        .edit_user_turn(first, "Tell me about the archive".to_owned(), |_| {})
        .await
        .unwrap_err();

    // A second root-branch turn with its own completed candidate.
    let second = send_failed_turn(&mut store, session_id, root, "What else is there?").await;
    let second_attempt = last_attempt(&store, second);
    record_completed_candidate(
        &mut store,
        session_id,
        second,
        second_attempt,
        None,
        "generated",
        "A cartographer's table, covered in maps.",
    );

    store.rebuild_session_projections().unwrap();
}

async fn send_failed_turn(
    store: &mut Store,
    session_id: EntityId,
    branch_id: EntityId,
    content: &str,
) -> EntityId {
    store
        .send_message(session_id, branch_id, content.to_owned(), |_| {})
        .await
        .unwrap_err();
    store
        .turns_for_branch(branch_id)
        .unwrap()
        .pop()
        .unwrap()
        .turn_id
}

fn last_attempt(store: &Store, turn_id: EntityId) -> EntityId {
    store
        .attempts_for_turn(turn_id)
        .unwrap()
        .pop()
        .unwrap()
        .attempt_id
}

fn record_started_attempt(
    store: &mut Store,
    session_id: EntityId,
    turn_id: EntityId,
    template_attempt: EntityId,
) -> EntityId {
    let template = store.attempt(template_attempt).unwrap().unwrap();
    let attempt_id = EntityId::new();
    store
        .record_event(
            Some(session_id),
            "attempt.started",
            &json!({
                "attempt_id": attempt_id,
                "turn_id": turn_id,
                "config_hash": template.config_hash,
                "prompt_plan": template.prompt_plan,
            }),
        )
        .unwrap();
    attempt_id
}

fn record_completed_candidate(
    store: &mut Store,
    session_id: EntityId,
    turn_id: EntityId,
    attempt_id: EntityId,
    parent_candidate_id: Option<EntityId>,
    origin: &str,
    content: &str,
) -> EntityId {
    let candidate_id = EntityId::new();
    store
        .record_event(
            Some(session_id),
            "attempt.completed",
            &json!({
                "attempt_id": attempt_id,
                "turn_id": turn_id,
                "candidate_id": candidate_id,
                "parent_candidate_id": parent_candidate_id,
                "origin": origin,
                "content": content,
                "provider_request_hash": zero_hash(),
                "provider_receipt": {},
            }),
        )
        .unwrap();
    store.rebuild_session_projections().unwrap();
    candidate_id
}

fn zero_hash() -> String {
    format!("sha256:{}", "0".repeat(64))
}

// ---------------------------------------------------------------------------
// Projection hashing
// ---------------------------------------------------------------------------

/// Hash the full visible Session Projection of a store — every session, its
/// branches, turns, attempts, candidates, and state — under a version-neutral
/// ordering so the value depends only on projected meaning, not row order.
fn projection_hash(store: &Store) -> String {
    let mut sessions = store.sessions().unwrap();
    sessions.sort_by_key(|session| session.session_id.to_string());
    let sessions = sessions
        .into_iter()
        .map(|session| {
            let mut branches = store.branches(session.session_id).unwrap();
            branches.sort_by_key(|branch| branch.branch_id.to_string());
            let branches = branches
                .into_iter()
                .map(|branch| {
                    let mut turns = store.turns_for_branch(branch.branch_id).unwrap();
                    turns.sort_by_key(|turn| turn.turn_id.to_string());
                    let turns = turns
                        .into_iter()
                        .map(|turn| {
                            let mut attempts = store.attempts_for_turn(turn.turn_id).unwrap();
                            attempts.sort_by_key(|attempt| attempt.attempt_id.to_string());
                            let mut candidates = store.candidates_for_turn(turn.turn_id).unwrap();
                            candidates.sort_by_key(|candidate| candidate.candidate_id.to_string());
                            json!({
                                "turn": turn,
                                "attempts": attempts,
                                "candidates": candidates,
                            })
                        })
                        .collect::<Vec<_>>();
                    json!({"branch": branch, "turns": turns})
                })
                .collect::<Vec<_>>();
            let mut state = store.state_transaction(session.session_id).unwrap().cells();
            state.sort_by_key(|cell| (format!("{:?}", cell.key.scope), cell.key.name.clone()));
            json!({"session": session, "branches": branches, "state": state})
        })
        .collect::<Vec<_>>();
    session_projection_hash(&json!(sessions))
        .unwrap()
        .to_string()
}

/// The `(candidate, parent)` pairs recorded directly in the migrated candidates
/// table, before any trace rebuild — what the migration must carry across.
fn migrated_ancestry(path: &Path) -> Vec<(String, String)> {
    let connection = Connection::open(path).unwrap();
    if !column_exists(&connection, "candidates", "parent_candidate_id") {
        return Vec::new();
    }
    let mut statement = connection
        .prepare(
            "SELECT candidate_id, parent_candidate_id FROM candidates \
             WHERE parent_candidate_id IS NOT NULL ORDER BY candidate_id",
        )
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn ancestry_pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().unwrap().to_owned(),
                pair[1].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// SQL dump / load
// ---------------------------------------------------------------------------

fn load_dump(path: &Path, dump: &str) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(dump).unwrap();
}

/// Render the canonical store as a SQL dump at the shape of schema `version`:
/// the historical DDL, the populated rows projected onto that version's columns,
/// and the recorded schema-version marker.
fn dump_store(source: &Connection, version: i64) -> String {
    let mut out = String::new();
    out.push_str("PRAGMA foreign_keys = OFF;\n");
    out.push_str(&historical_ddl(version));
    for (table, columns) in populated_tables(version) {
        out.push_str(&dump_table(source, table, &columns));
    }
    out.push_str(&format!(
        "INSERT INTO schema_migrations(version) VALUES ({version});\n"
    ));
    out
}

fn dump_table(source: &Connection, table: &str, columns: &[&str]) -> String {
    let column_list = columns.join(", ");
    let statement_sql = format!("SELECT {column_list} FROM {table} ORDER BY rowid");
    let mut statement = source.prepare(&statement_sql).unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((0..columns.len())
                .map(|index| sql_literal(row.get_ref_unwrap(index)))
                .collect::<Vec<_>>())
        })
        .unwrap();
    let mut out = String::new();
    for row in rows {
        let values = row.unwrap().join(", ");
        out.push_str(&format!(
            "INSERT INTO {table}({column_list}) VALUES ({values});\n"
        ));
    }
    out
}

fn sql_literal(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".to_owned(),
        ValueRef::Integer(integer) => integer.to_string(),
        ValueRef::Real(real) => format!("{real:?}"),
        ValueRef::Text(bytes) => {
            format!("'{}'", String::from_utf8_lossy(bytes).replace('\'', "''"))
        }
        ValueRef::Blob(bytes) => {
            let mut hex = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                hex.push_str(&format!("{byte:02x}"));
            }
            format!("x'{hex}'")
        }
    }
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> bool {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .any(|name| name == column)
}

// ---------------------------------------------------------------------------
// Historical schemas
// ---------------------------------------------------------------------------

/// Tables and their column projection for a given historical version, in
/// foreign-key dependency order.
fn populated_tables(version: i64) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut branches: Vec<&str> = if version >= 6 {
        vec![
            "branch_id",
            "session_id",
            "parent_branch_id",
            "forked_from_turn_id",
            "greeting_revision_hash",
            "greeting_index",
            "created_event_id",
        ]
    } else {
        vec![
            "branch_id",
            "session_id",
            "parent_branch_id",
            "greeting_revision_hash",
            "greeting_index",
            "created_event_id",
        ]
    };
    if version >= 8 {
        branches.push("deleted");
    }
    let mut attempts = vec![
        "attempt_id",
        "turn_id",
        "config_hash",
        "retry_of_attempt_id",
        "status",
        "prompt_plan",
    ];
    if version >= 7 {
        attempts.push("effect_receipt");
    }
    attempts.extend([
        "provider_request_hash",
        "provider_receipt",
        "error_message",
        "created_event_id",
        "completed_event_id",
    ]);
    let mut candidates = vec!["candidate_id", "turn_id", "attempt_id"];
    if version >= 6 {
        candidates.push("parent_candidate_id");
    }
    candidates.extend(["origin", "content", "created_event_id"]);
    if version >= 8 {
        candidates.extend(["hidden", "deleted"]);
    }
    let sessions = if version >= 8 {
        vec![
            "session_id",
            "current_config_hash",
            "root_branch_id",
            "archived",
            "created_event_id",
            "custom_name",
        ]
    } else {
        vec![
            "session_id",
            "current_config_hash",
            "root_branch_id",
            "archived",
            "created_event_id",
        ]
    };
    let mut turns = vec![
        "turn_id",
        "session_id",
        "branch_id",
        "user_content",
        "selected_candidate_id",
        "created_event_id",
    ];
    if version >= 8 {
        turns.extend(["hidden", "deleted"]);
    }

    vec![
        ("content_blobs", vec!["hash", "data"]),
        ("content_refs", vec!["owner_kind", "owner_id", "blob_hash"]),
        (
            "trace_events",
            vec![
                "sequence",
                "event_id",
                "session_id",
                "event_type",
                "payload",
                "payload_hash",
            ],
        ),
        (
            "artifact_revisions",
            vec![
                "revision_hash",
                "artifact_kind",
                "source_format",
                "semantic_hash",
                "source_blob_hash",
                "imported_event_id",
            ],
        ),
        (
            "session_config_revisions",
            vec!["revision_hash", "body", "created_event_id"],
        ),
        ("sessions", sessions),
        ("branches", branches),
        ("turns", turns),
        ("attempts", attempts),
        ("candidates", candidates),
    ]
}

/// The DDL of a historical store at `version`. Columns absent at that version
/// (granular-deletion flags, `forked_from_turn_id`, `effect_receipt`, candidate
/// ancestry) are omitted so `Store::open` exercises the real converge paths.
fn historical_ddl(version: i64) -> String {
    let branch_fork = if version >= 6 {
        "\n                forked_from_turn_id TEXT REFERENCES turns(turn_id),"
    } else {
        ""
    };
    let attempt_effect = if version >= 7 {
        "\n                effect_receipt BLOB,"
    } else {
        ""
    };
    let candidate_attempt = if version >= 7 {
        "attempt_id TEXT REFERENCES attempts(attempt_id)"
    } else {
        "attempt_id TEXT NOT NULL REFERENCES attempts(attempt_id)"
    };
    let candidate_parent = if version >= 6 {
        ",\n    parent_candidate_id TEXT REFERENCES candidates(candidate_id)"
    } else {
        ""
    };
    let session_custom_name = if version >= 8 {
        ",\n    custom_name TEXT"
    } else {
        ""
    };
    let branch_deleted = if version >= 8 {
        ",\n    deleted INTEGER NOT NULL DEFAULT 0"
    } else {
        ""
    };
    let turn_deletion = if version >= 8 {
        ",\n    hidden INTEGER NOT NULL DEFAULT 0,\n    deleted INTEGER NOT NULL DEFAULT 0"
    } else {
        ""
    };
    let candidate_deletion = if version >= 8 {
        ",\n    hidden INTEGER NOT NULL DEFAULT 0,\n    deleted INTEGER NOT NULL DEFAULT 0"
    } else {
        ""
    };

    format!(
        "\
CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
CREATE TABLE content_blobs (hash TEXT PRIMARY KEY, data BLOB NOT NULL);
CREATE TABLE content_refs (
    owner_kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    blob_hash TEXT NOT NULL REFERENCES content_blobs(hash),
    PRIMARY KEY(owner_kind, owner_id, blob_hash)
);
CREATE TABLE trace_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    session_id TEXT,
    event_type TEXT NOT NULL,
    payload BLOB NOT NULL,
    payload_hash TEXT NOT NULL
);
CREATE INDEX trace_events_session_sequence ON trace_events(session_id, sequence);
CREATE TABLE artifact_revisions (
    revision_hash TEXT PRIMARY KEY,
    artifact_kind TEXT NOT NULL,
    source_format TEXT NOT NULL,
    semantic_hash TEXT NOT NULL,
    source_blob_hash TEXT NOT NULL REFERENCES content_blobs(hash),
    imported_event_id TEXT NOT NULL
);
CREATE TABLE session_config_revisions (
    revision_hash TEXT PRIMARY KEY,
    body BLOB NOT NULL,
    created_event_id TEXT NOT NULL
);
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    current_config_hash TEXT NOT NULL REFERENCES session_config_revisions(revision_hash),
    root_branch_id TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    created_event_id TEXT NOT NULL{session_custom_name}
);
CREATE TABLE branches (
    branch_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    parent_branch_id TEXT REFERENCES branches(branch_id),{branch_fork}
    greeting_revision_hash TEXT NOT NULL REFERENCES artifact_revisions(revision_hash),
    greeting_index INTEGER NOT NULL,
    created_event_id TEXT NOT NULL{branch_deleted}
);
CREATE TABLE turns (
    turn_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    branch_id TEXT NOT NULL REFERENCES branches(branch_id) ON DELETE CASCADE,
    user_content TEXT NOT NULL,
    selected_candidate_id TEXT,
    created_event_id TEXT NOT NULL{turn_deletion}
);
CREATE TABLE attempts (
    attempt_id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
    config_hash TEXT NOT NULL REFERENCES session_config_revisions(revision_hash),
    retry_of_attempt_id TEXT REFERENCES attempts(attempt_id),
    status TEXT NOT NULL,
    prompt_plan BLOB NOT NULL,{attempt_effect}
    provider_request_hash TEXT,
    provider_receipt BLOB,
    error_message TEXT,
    created_event_id TEXT NOT NULL,
    completed_event_id TEXT
);
CREATE TABLE candidates (
    candidate_id TEXT PRIMARY KEY,
    turn_id TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
    {candidate_attempt}{candidate_parent},
    origin TEXT NOT NULL,
    content TEXT NOT NULL,
    created_event_id TEXT NOT NULL{candidate_deletion}
);
CREATE TABLE state_cells (
    scope_kind TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    name TEXT NOT NULL,
    value BLOB NOT NULL,
    raw_value TEXT NOT NULL,
    owner TEXT NOT NULL,
    origin TEXT NOT NULL,
    revision INTEGER NOT NULL,
    PRIMARY KEY(scope_kind, scope_id, name)
);
CREATE TABLE capsules (
    capsule_hash TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    body_blob_hash TEXT NOT NULL REFERENCES content_blobs(hash)
);
CREATE TABLE capsule_imports (
    capsule_hash TEXT PRIMARY KEY REFERENCES capsules(capsule_hash) ON DELETE CASCADE,
    imported_session_id TEXT NOT NULL UNIQUE REFERENCES sessions(session_id) ON DELETE CASCADE
);
CREATE TABLE capsule_artifacts (
    capsule_hash TEXT NOT NULL REFERENCES capsules(capsule_hash) ON DELETE CASCADE,
    revision_hash TEXT NOT NULL REFERENCES artifact_revisions(revision_hash),
    PRIMARY KEY(capsule_hash, revision_hash)
);
"
    )
}

// ---------------------------------------------------------------------------
// Paths / helpers
// ---------------------------------------------------------------------------

fn regenerating() -> bool {
    std::env::var_os(REGENERATE_ENV).as_deref() == Some("1".as_ref())
}

fn current_schema_version() -> i64 {
    let directory = tempdir().unwrap();
    let store = Store::open(directory.path().join("probe.sqlite3")).unwrap();
    store.schema_version().unwrap()
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/db")
}

fn dump_path(version: i64) -> PathBuf {
    fixtures_dir().join(format!("v{version}.sql"))
}

fn manifest_path() -> PathBuf {
    fixtures_dir().join("expected.json")
}

fn read_dump(version: i64) -> String {
    fs::read_to_string(dump_path(version)).unwrap_or_else(|error| {
        panic!(
            "missing migration fixture {}: {error}; regenerate with {REGENERATE_ENV}=1",
            dump_path(version).display()
        )
    })
}

fn read_manifest() -> Value {
    let text = fs::read_to_string(manifest_path()).unwrap_or_else(|error| {
        panic!(
            "missing migration manifest {}: {error}; regenerate with {REGENERATE_ENV}=1",
            manifest_path().display()
        )
    });
    serde_json::from_str(&text).unwrap()
}
