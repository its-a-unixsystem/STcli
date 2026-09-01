use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ArtifactInspectorRegistration, ContentHash, EntityId, canonical_json, canonical_json_hash,
};

const TRACE_PAYLOAD_DOMAIN: &str = "stcli:trace-payload:v1";
const SCHEMA_VERSION: i64 = 11;

pub struct Store {
    pub(crate) connection: Connection,
    path: PathBuf,
    assets_root: PathBuf,
    pub(crate) egress: crate::EgressBroker,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_owned();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| StorageError::CreateDirectory {
            path: parent.to_owned(),
            source,
        })?;
        let assets_root = parent.join("assets").join("sha256");
        fs::create_dir_all(&assets_root).map_err(|source| StorageError::CreateDirectory {
            path: assets_root.clone(),
            source,
        })?;
        set_private_directory_permissions(&assets_root)?;
        let connection = Connection::open(&path).map_err(StorageError::Sqlite)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .map_err(StorageError::Sqlite)?;
        migrate(&connection)?;
        set_private_file_permissions(&path)?;
        Ok(Self {
            connection,
            path,
            assets_root,
            egress: crate::EgressBroker::live(),
        })
    }

    pub fn set_egress_broker(&mut self, broker: crate::EgressBroker) {
        self.egress = broker;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<i64, StorageError> {
        self.connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .map_err(StorageError::Sqlite)
    }

    pub fn journal_mode(&self) -> Result<String, StorageError> {
        self.connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(StorageError::Sqlite)
    }

    pub fn record_event(
        &mut self,
        session_id: Option<EntityId>,
        event_type: &str,
        payload: &Value,
    ) -> Result<TraceEventRecord, StorageError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(&transaction, session_id, event_type, payload)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(event)
    }

    pub fn trace_events(
        &self,
        session_id: Option<EntityId>,
    ) -> Result<Vec<TraceEventRecord>, StorageError> {
        let (sql, parameter) = if let Some(session_id) = session_id {
            (
                "SELECT sequence, event_id, session_id, event_type, payload, payload_hash FROM trace_events WHERE session_id = ?1 ORDER BY sequence",
                Some(session_id.to_string()),
            )
        } else {
            (
                "SELECT sequence, event_id, session_id, event_type, payload, payload_hash FROM trace_events ORDER BY sequence",
                None,
            )
        };
        let mut statement = self.connection.prepare(sql).map_err(StorageError::Sqlite)?;
        let rows = if let Some(parameter) = parameter {
            statement
                .query_map([parameter], decode_trace_event)
                .map_err(StorageError::Sqlite)?
        } else {
            statement
                .query_map([], decode_trace_event)
                .map_err(StorageError::Sqlite)?
        };
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
    }

    pub(crate) fn put_blob(
        transaction: &Transaction<'_>,
        hash: &str,
        data: &[u8],
    ) -> Result<(), StorageError> {
        transaction
            .execute(
                "INSERT OR IGNORE INTO content_blobs(hash, data) VALUES (?1, ?2)",
                params![hash, data],
            )
            .map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub(crate) fn add_blob_reference(
        transaction: &Transaction<'_>,
        owner_kind: &str,
        owner_id: &str,
        blob_hash: &str,
    ) -> Result<(), StorageError> {
        transaction
            .execute(
                "INSERT OR IGNORE INTO content_refs(owner_kind, owner_id, blob_hash) VALUES (?1, ?2, ?3)",
                params![owner_kind, owner_id, blob_hash],
            )
            .map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub fn remove_blob_references(
        &mut self,
        owner_kind: &str,
        owner_id: &str,
    ) -> Result<usize, StorageError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM content_refs WHERE owner_kind = ?1 AND owner_id = ?2",
                params![owner_kind, owner_id],
            )
            .map_err(StorageError::Sqlite)?;
        let removed = transaction
            .execute(
                "DELETE FROM content_blobs WHERE NOT EXISTS (SELECT 1 FROM content_refs WHERE content_refs.blob_hash = content_blobs.hash)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(removed)
    }

    pub fn blob(&self, hash: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.connection
            .query_row(
                "SELECT data FROM content_blobs WHERE hash = ?1",
                [hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::Sqlite)
    }

    pub fn put_asset(&mut self, data: &[u8]) -> Result<AssetRecord, StorageError> {
        let record = persist_asset(&self.assets_root, data)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        insert_asset(&transaction, &record)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        self.asset(&record.hash)?
            .ok_or_else(|| StorageError::MissingAsset(record.hash))
    }

    pub fn asset(&self, hash: &ContentHash) -> Result<Option<AssetRecord>, StorageError> {
        self.connection
            .query_row(
                "SELECT hash, mime_type, byte_size, created_at FROM assets WHERE hash = ?1",
                [hash.to_string()],
                decode_asset,
            )
            .optional()
            .map_err(StorageError::Sqlite)
    }

    pub fn asset_bytes(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>, StorageError> {
        let path = asset_path(&self.assets_root, hash);
        match fs::read(&path) {
            Ok(data) => Ok(Some(data)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StorageError::ReadAsset { path, source }),
        }
    }

    pub fn asset_references(
        &self,
        owner_kind: &str,
        owner_id: &str,
    ) -> Result<Vec<AssetReference>, StorageError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT owner_kind, owner_id, asset_hash, logical_path FROM asset_refs WHERE owner_kind = ?1 AND owner_id = ?2 ORDER BY logical_path",
            )
            .map_err(StorageError::Sqlite)?;
        let rows = statement
            .query_map(params![owner_kind, owner_id], decode_asset_reference)
            .map_err(StorageError::Sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
    }

    pub(crate) fn put_asset_in_transaction(
        transaction: &Transaction<'_>,
        assets_root: &Path,
        data: &[u8],
    ) -> Result<AssetRecord, StorageError> {
        let record = persist_asset(assets_root, data)?;
        insert_asset(transaction, &record)?;
        Ok(record)
    }

    pub(crate) fn validate_asset(data: &[u8]) -> Result<(), StorageError> {
        validate_asset(data).map(|_| ())
    }

    pub(crate) fn asset_file_exists(assets_root: &Path, hash: &ContentHash) -> bool {
        asset_path(assets_root, hash).exists()
    }

    pub(crate) fn remove_asset_file(
        assets_root: &Path,
        hash: &ContentHash,
    ) -> Result<(), StorageError> {
        let path = asset_path(assets_root, hash);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StorageError::WriteAsset { path, source }),
        }
    }

    pub(crate) fn add_asset_reference_in_transaction(
        transaction: &Transaction<'_>,
        owner_kind: &str,
        owner_id: &str,
        asset_hash: &ContentHash,
        logical_path: &str,
    ) -> Result<(), StorageError> {
        transaction
            .execute(
                "INSERT OR IGNORE INTO asset_refs(owner_kind, owner_id, asset_hash, logical_path) VALUES (?1, ?2, ?3, ?4)",
                params![owner_kind, owner_id, asset_hash.to_string(), logical_path],
            )
            .map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub(crate) fn assets_root(&self) -> &Path {
        &self.assets_root
    }

    pub fn recover_interrupted_attempts(&mut self) -> Result<RecoveryReport, StorageError> {
        let interrupted = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT attempts.attempt_id, attempts.turn_id, turns.session_id FROM attempts JOIN turns ON turns.turn_id = attempts.turn_id WHERE attempts.status = 'running' ORDER BY attempts.rowid",
                )
                .map_err(StorageError::Sqlite)?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(StorageError::Sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StorageError::Sqlite)?
        };
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let mut attempt_ids = Vec::with_capacity(interrupted.len());
        for (attempt_id, turn_id, session_id) in interrupted {
            let session_id = session_id
                .parse::<EntityId>()
                .map_err(|error| StorageError::InvalidIdentity(error.to_string()))?;
            let event = append_event(
                &transaction,
                Some(session_id),
                "attempt.recovered-incomplete",
                &serde_json::json!({
                    "attempt_id": attempt_id,
                    "turn_id": turn_id,
                }),
            )?;
            transaction
                .execute(
                    "UPDATE attempts SET status = 'incomplete', completed_event_id = ?1 WHERE attempt_id = ?2 AND status = 'running'",
                    params![event.event_id.to_string(), attempt_id],
                )
                .map_err(StorageError::Sqlite)?;
            attempt_ids.push(
                attempt_id
                    .parse::<EntityId>()
                    .map_err(|error| StorageError::InvalidIdentity(error.to_string()))?,
            );
        }
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(RecoveryReport { attempt_ids })
    }
    pub fn register_artifact_inspector(
        &self,
        registration: &ArtifactInspectorRegistration,
    ) -> Result<(), StorageError> {
        let body = serde_json::to_vec(registration).map_err(StorageError::Json)?;
        self.connection
            .execute(
                "INSERT INTO artifact_inspector_registrations(plugin_id, body)
                 VALUES (?1, ?2)
                 ON CONFLICT(plugin_id) DO UPDATE SET body = excluded.body",
                params![registration.id, body],
            )
            .map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub fn artifact_inspector(
        &self,
        plugin_id: &str,
    ) -> Result<Option<ArtifactInspectorRegistration>, StorageError> {
        let body = self
            .connection
            .query_row(
                "SELECT body FROM artifact_inspector_registrations WHERE plugin_id = ?1",
                [plugin_id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StorageError::Sqlite)?;
        body.map(|body| serde_json::from_slice(&body).map_err(StorageError::Json))
            .transpose()
    }

    pub fn artifact_inspectors(&self) -> Result<Vec<ArtifactInspectorRegistration>, StorageError> {
        let mut statement = self
            .connection
            .prepare("SELECT body FROM artifact_inspector_registrations ORDER BY plugin_id")
            .map_err(StorageError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(StorageError::Sqlite)?;
        rows.map(|row| {
            let body = row.map_err(StorageError::Sqlite)?;
            serde_json::from_slice(&body).map_err(StorageError::Json)
        })
        .collect()
    }

    pub fn unregister_artifact_inspector(&self, plugin_id: &str) -> Result<bool, StorageError> {
        Ok(self
            .connection
            .execute(
                "DELETE FROM artifact_inspector_registrations WHERE plugin_id = ?1",
                [plugin_id],
            )
            .map_err(StorageError::Sqlite)?
            > 0)
    }
}

fn persist_asset(assets_root: &Path, data: &[u8]) -> Result<AssetRecord, StorageError> {
    let mime_type = validate_asset(data)?;
    let hash = ContentHash::new(Sha256::digest(data).into());
    let path = asset_path(assets_root, &hash);
    if !path.exists() {
        let parent = path
            .parent()
            .expect("content-addressed asset path has a parent");
        fs::create_dir_all(parent).map_err(|source| StorageError::CreateDirectory {
            path: parent.to_owned(),
            source,
        })?;
        set_private_directory_permissions(parent)?;
        let temporary = parent.join(format!(".{}.{}.tmp", asset_hex(&hash), EntityId::new()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|source| StorageError::WriteAsset {
                    path: temporary.clone(),
                    source,
                })?;
            file.write_all(data)
                .and_then(|()| file.sync_all())
                .map_err(|source| StorageError::WriteAsset {
                    path: temporary.clone(),
                    source,
                })?;
            set_private_file_permissions(&temporary)?;
            fs::rename(&temporary, &path).map_err(|source| StorageError::WriteAsset {
                path: path.clone(),
                source,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    }

    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StorageError::Clock)?
        .as_secs()
        .to_string();
    Ok(AssetRecord {
        hash,
        mime_type: mime_type.to_owned(),
        byte_size: data.len(),
        created_at,
    })
}

fn validate_asset(data: &[u8]) -> Result<&'static str, StorageError> {
    if data.len() > crate::limits::MAX_ASSET_BYTES {
        return Err(StorageError::AssetTooLarge {
            size: data.len(),
            limit: crate::limits::MAX_ASSET_BYTES,
        });
    }
    let mime_type = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else if data.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        "image/gif"
    } else if data.len() >= 12
        && &data[4..8] == b"ftyp"
        && matches!(&data[8..12], b"avif" | b"avis")
    {
        "image/avif"
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        "audio/wav"
    } else if data.starts_with(b"ID3")
        || data
            .get(..2)
            .is_some_and(|header| header[0] == 0xff && header[1] & 0xe0 == 0xe0)
    {
        "audio/mpeg"
    } else if data.starts_with(b"OggS") {
        "audio/ogg"
    } else {
        return Err(StorageError::UnsupportedAsset);
    };
    Ok(mime_type)
}

fn insert_asset(transaction: &Transaction<'_>, record: &AssetRecord) -> Result<(), StorageError> {
    transaction
        .execute(
            "INSERT OR IGNORE INTO assets(hash, mime_type, byte_size, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                record.hash.to_string(),
                record.mime_type,
                record.byte_size as i64,
                record.created_at
            ],
        )
        .map_err(StorageError::Sqlite)?;
    Ok(())
}

fn asset_path(assets_root: &Path, hash: &ContentHash) -> PathBuf {
    let hex = asset_hex(hash);
    assets_root.join(&hex[..2]).join(hex)
}

fn asset_hex(hash: &ContentHash) -> String {
    hash.to_string()
        .strip_prefix("sha256:")
        .expect("ContentHash display includes its algorithm")
        .to_owned()
}

fn decode_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRecord> {
    let hash: String = row.get(0)?;
    let byte_size: i64 = row.get(2)?;
    Ok(AssetRecord {
        hash: hash.parse().map_err(|error| conversion_error(0, error))?,
        mime_type: row.get(1)?,
        byte_size: usize::try_from(byte_size).map_err(|error| conversion_error(2, error))?,
        created_at: row.get(3)?,
    })
}

fn decode_asset_reference(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetReference> {
    let asset_hash: String = row.get(2)?;
    Ok(AssetReference {
        owner_kind: row.get(0)?,
        owner_id: row.get(1)?,
        asset_hash: asset_hash
            .parse()
            .map_err(|error| conversion_error(2, error))?,
        logical_path: row.get(3)?,
    })
}

pub(crate) fn append_event(
    transaction: &Transaction<'_>,
    session_id: Option<EntityId>,
    event_type: &str,
    payload: &Value,
) -> Result<TraceEventRecord, StorageError> {
    let event_id = EntityId::new();
    let payload_bytes = canonical_json(payload).map_err(StorageError::Json)?;
    let payload_hash =
        canonical_json_hash(TRACE_PAYLOAD_DOMAIN, payload).map_err(StorageError::Json)?;
    transaction
        .execute(
            "INSERT INTO trace_events(event_id, session_id, event_type, payload, payload_hash) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event_id.to_string(),
                session_id.map(|id| id.to_string()),
                event_type,
                payload_bytes,
                payload_hash.to_string()
            ],
        )
        .map_err(StorageError::Sqlite)?;
    let sequence = transaction.last_insert_rowid();
    Ok(TraceEventRecord {
        sequence,
        event_id,
        session_id,
        event_type: event_type.to_owned(),
        payload: payload.clone(),
        payload_hash: payload_hash.to_string(),
    })
}

fn decode_trace_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TraceEventRecord> {
    let event_id: String = row.get(1)?;
    let session_id: Option<String> = row.get(2)?;
    let payload: Vec<u8> = row.get(4)?;
    Ok(TraceEventRecord {
        sequence: row.get(0)?,
        event_id: event_id
            .parse()
            .map_err(|error| conversion_error(1, error))?,
        session_id: session_id
            .map(|value| value.parse().map_err(|error| conversion_error(2, error)))
            .transpose()?,
        event_type: row.get(3)?,
        payload: serde_json::from_slice(&payload).map_err(|error| conversion_error(4, error))?,
        payload_hash: row.get(5)?,
    })
}

fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn migrate(connection: &Connection) -> Result<(), StorageError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY
            );",
        )
        .map_err(StorageError::Sqlite)?;
    let found: Option<i64> = connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .map_err(StorageError::Sqlite)?;
    if let Some(found) = found
        && found > SCHEMA_VERSION
    {
        return Err(StorageError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS content_blobs (
                hash TEXT PRIMARY KEY,
                data BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS content_refs (
                owner_kind TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                blob_hash TEXT NOT NULL REFERENCES content_blobs(hash),
                PRIMARY KEY(owner_kind, owner_id, blob_hash)
            );
            CREATE TABLE IF NOT EXISTS trace_events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                session_id TEXT,
                event_type TEXT NOT NULL,
                payload BLOB NOT NULL,
                payload_hash TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS trace_events_session_sequence
                ON trace_events(session_id, sequence);
            CREATE TABLE IF NOT EXISTS artifact_revisions (
                revision_hash TEXT PRIMARY KEY,
                artifact_kind TEXT NOT NULL,
                source_format TEXT NOT NULL,
                semantic_hash TEXT NOT NULL,
                source_blob_hash TEXT NOT NULL REFERENCES content_blobs(hash),
                imported_event_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS artifact_inspector_registrations (
                plugin_id TEXT PRIMARY KEY,
                body BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS assets (
                hash TEXT PRIMARY KEY,
                mime_type TEXT NOT NULL,
                byte_size INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS asset_refs (
                owner_kind TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                asset_hash TEXT NOT NULL REFERENCES assets(hash) ON DELETE CASCADE,
                logical_path TEXT NOT NULL,
                PRIMARY KEY(owner_kind, owner_id, asset_hash, logical_path)
            );
            CREATE TABLE IF NOT EXISTS session_config_revisions (
                revision_hash TEXT PRIMARY KEY,
                body BLOB NOT NULL,
                created_event_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                current_config_hash TEXT NOT NULL REFERENCES session_config_revisions(revision_hash),
                root_branch_id TEXT NOT NULL,
                archived INTEGER NOT NULL DEFAULT 0,
                created_event_id TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS branches (
                branch_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
                parent_branch_id TEXT REFERENCES branches(branch_id),
                forked_from_turn_id TEXT REFERENCES turns(turn_id),
                greeting_revision_hash TEXT NOT NULL REFERENCES artifact_revisions(revision_hash),
                greeting_index INTEGER NOT NULL,
                created_event_id TEXT NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS turns (
                turn_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
                branch_id TEXT NOT NULL REFERENCES branches(branch_id) ON DELETE CASCADE,
                user_content TEXT NOT NULL,
                selected_candidate_id TEXT,
                created_event_id TEXT NOT NULL,
                hidden INTEGER NOT NULL DEFAULT 0,
                deleted INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS attempts (
                attempt_id TEXT PRIMARY KEY,
                turn_id TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
                config_hash TEXT NOT NULL REFERENCES session_config_revisions(revision_hash),
                retry_of_attempt_id TEXT REFERENCES attempts(attempt_id),
                status TEXT NOT NULL,
                prompt_plan BLOB NOT NULL,
                effect_receipt BLOB,
                provider_request_hash TEXT,
                provider_receipt BLOB,
                error_message TEXT,
                created_event_id TEXT NOT NULL,
                completed_event_id TEXT
            );
            CREATE TABLE IF NOT EXISTS candidates (
                candidate_id TEXT PRIMARY KEY,
                turn_id TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
                attempt_id TEXT REFERENCES attempts(attempt_id),
                parent_candidate_id TEXT REFERENCES candidates(candidate_id),
                origin TEXT NOT NULL,
                content TEXT NOT NULL,
                created_event_id TEXT NOT NULL,
                hidden INTEGER NOT NULL DEFAULT 0,
                deleted INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS state_cells (
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
            CREATE TABLE IF NOT EXISTS capsules (
                capsule_hash TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                body_blob_hash TEXT NOT NULL REFERENCES content_blobs(hash)
            );
            CREATE TABLE IF NOT EXISTS capsule_imports (
                capsule_hash TEXT PRIMARY KEY REFERENCES capsules(capsule_hash) ON DELETE CASCADE,
                imported_session_id TEXT NOT NULL UNIQUE REFERENCES sessions(session_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS capsule_artifacts (
                capsule_hash TEXT NOT NULL REFERENCES capsules(capsule_hash) ON DELETE CASCADE,
                revision_hash TEXT NOT NULL REFERENCES artifact_revisions(revision_hash),
                PRIMARY KEY(capsule_hash, revision_hash)
            );
            ",
        )
        .map_err(StorageError::Sqlite)?;
    if !column_exists(connection, "sessions", "custom_name")? {
        connection
            .execute("ALTER TABLE sessions ADD COLUMN custom_name TEXT", [])
            .map_err(StorageError::Sqlite)?;
    }
    if !column_exists(connection, "branches", "forked_from_turn_id")? {
        connection
            .execute(
                "ALTER TABLE branches ADD COLUMN forked_from_turn_id TEXT REFERENCES turns(turn_id)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
    }
    for (table, column) in [
        ("branches", "deleted"),
        ("turns", "hidden"),
        ("turns", "deleted"),
        ("candidates", "hidden"),
        ("candidates", "deleted"),
    ] {
        if !column_exists(connection, table, column)? {
            connection
                .execute(
                    &format!("ALTER TABLE {table} ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"),
                    [],
                )
                .map_err(StorageError::Sqlite)?;
        }
    }
    let candidate_attempt_required =
        column_not_null(connection, "candidates", "attempt_id")?.unwrap_or(false);
    let candidate_has_parent = column_exists(connection, "candidates", "parent_candidate_id")?;
    if !candidate_has_parent {
        connection
            .execute(
                "ALTER TABLE candidates ADD COLUMN parent_candidate_id TEXT REFERENCES candidates(candidate_id)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
    }
    if candidate_attempt_required || !candidate_has_parent {
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = OFF;
                BEGIN;
                ALTER TABLE candidates RENAME TO candidates_legacy;
                CREATE TABLE candidates (
                    candidate_id TEXT PRIMARY KEY,
                    turn_id TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
                    attempt_id TEXT REFERENCES attempts(attempt_id),
                    parent_candidate_id TEXT REFERENCES candidates(candidate_id),
                    origin TEXT NOT NULL,
                    content TEXT NOT NULL,
                    created_event_id TEXT NOT NULL,
                    hidden INTEGER NOT NULL DEFAULT 0,
                    deleted INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id, hidden, deleted)
                    SELECT candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id,
                           COALESCE(hidden, 0), COALESCE(deleted, 0)
                    FROM candidates_legacy;
                DROP TABLE candidates_legacy;
                COMMIT;
                PRAGMA foreign_keys = ON;
                ",
            )
            .map_err(StorageError::Sqlite)?;
    }
    if !column_exists(connection, "attempts", "effect_receipt")? {
        connection
            .execute("ALTER TABLE attempts ADD COLUMN effect_receipt BLOB", [])
            .map_err(StorageError::Sqlite)?;
    }
    if found.unwrap_or_default() < 9 {
        connection
            .execute(
                "UPDATE branches SET deleted = 0 WHERE deleted = 1 AND branch_id IN (SELECT root_branch_id FROM sessions)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
    }
    connection
        .execute(
            "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
            [SCHEMA_VERSION],
        )
        .map_err(StorageError::Sqlite)?;
    Ok(())
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool, StorageError> {
    Ok(column_not_null(connection, table, column)?.is_some())
}

fn column_not_null(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<Option<bool>, StorageError> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(StorageError::Sqlite)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(3)? != 0))
        })
        .map_err(StorageError::Sqlite)?;
    for row in rows {
        let (name, not_null) = row.map_err(StorageError::Sqlite)?;
        if name == column {
            return Ok(Some(not_null));
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        StorageError::Permissions {
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), StorageError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        StorageError::Permissions {
            path: path.to_owned(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), StorageError> {
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssetRecord {
    pub hash: ContentHash,
    pub mime_type: String,
    pub byte_size: usize,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssetReference {
    pub owner_kind: String,
    pub owner_id: String,
    pub asset_hash: ContentHash,
    pub logical_path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryReport {
    pub attempt_ids: Vec<EntityId>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TraceEventRecord {
    pub sequence: i64,
    pub event_id: EntityId,
    pub session_id: Option<EntityId>,
    pub event_type: String,
    pub payload: Value,
    pub payload_hash: String,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("failed to create storage directory '{path}': {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to secure storage path '{path}': {source}")]
    Permissions {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("asset exceeds {limit} byte limit ({size} bytes)")]
    AssetTooLarge { size: usize, limit: usize },
    #[error("asset format is unsupported")]
    UnsupportedAsset,
    #[error("asset metadata {0} is missing after insertion")]
    MissingAsset(ContentHash),
    #[error("failed to write asset '{path}': {source}")]
    WriteAsset {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read asset '{path}': {source}")]
    ReadAsset {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("system clock is before the Unix epoch: {0}")]
    Clock(std::time::SystemTimeError),
    #[error(
        "store schema version {found} is newer than supported version {supported}; upgrade STcli"
    )]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("SQLite operation failed: {0}")]
    Sqlite(rusqlite::Error),
    #[error("JSON operation failed: {0}")]
    Json(serde_json::Error),
    #[error("stored identity is invalid: {0}")]
    InvalidIdentity(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn store_uses_wal_and_recovers_trace_events() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        let session_id = EntityId::new();
        {
            let mut store = Store::open(&path).unwrap();
            assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
            assert_eq!(store.journal_mode().unwrap().to_lowercase(), "wal");
            store
                .record_event(
                    Some(session_id),
                    "session.created",
                    &json!({"name": "test"}),
                )
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let events = store.trace_events(Some(session_id)).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "session.created");
        assert_eq!(events[0].payload, json!({"name": "test"}));
    }
    #[test]
    fn candidate_rebuild_preserves_parent_links() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        let parent = EntityId::new();
        let child = EntityId::new();
        {
            let store = Store::open(&path).unwrap();
            store
                .connection
                .execute_batch(&format!(
                    "PRAGMA foreign_keys = OFF;
                     ALTER TABLE candidates RENAME TO candidates_legacy;
                     CREATE TABLE candidates (
                         candidate_id TEXT PRIMARY KEY,
                         turn_id TEXT NOT NULL REFERENCES turns(turn_id) ON DELETE CASCADE,
                         attempt_id TEXT NOT NULL REFERENCES attempts(attempt_id),
                         parent_candidate_id TEXT REFERENCES candidates(candidate_id),
                         origin TEXT NOT NULL,
                         content TEXT NOT NULL,
                         created_event_id TEXT NOT NULL,
                         hidden INTEGER NOT NULL DEFAULT 0,
                         deleted INTEGER NOT NULL DEFAULT 0
                     );
                     INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id)
                         VALUES ('{parent}', '01M0ZVXKJ3GN413FMVXVAGGT37', '01M0ZVXKJ3GN413FMVXVAGGT38', NULL, 'generated', 'parent', 'event-parent'),
                                ('{child}', '01M0ZVXKJ3GN413FMVXVAGGT37', '01M0ZVXKJ3GN413FMVXVAGGT39', '{parent}', 'continued', 'child', 'event-child');
                     DROP TABLE candidates_legacy;
                     PRAGMA foreign_keys = ON;",
                    parent = parent,
                    child = child,
                ))
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT parent_candidate_id FROM candidates WHERE candidate_id = ?1",
                    [child.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            parent.to_string()
        );
    }

    #[test]
    fn unreferenced_blobs_are_collected() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        let mut store = Store::open(path).unwrap();
        let transaction = store.connection.transaction().unwrap();
        Store::put_blob(&transaction, "sha256:test", b"data").unwrap();
        Store::add_blob_reference(&transaction, "fixture", "one", "sha256:test").unwrap();
        transaction.commit().unwrap();
        assert_eq!(store.blob("sha256:test").unwrap(), Some(b"data".to_vec()));

        assert_eq!(store.remove_blob_references("fixture", "one").unwrap(), 1);
        assert_eq!(store.blob("sha256:test").unwrap(), None);
    }
    #[test]
    fn interrupted_attempts_recover_as_incomplete() {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let session_id = EntityId::new();
        let branch_id = EntityId::new();
        let turn_id = EntityId::new();
        let attempt_id = EntityId::new();
        store
            .connection
            .execute_batch(&format!(
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO sessions(session_id, current_config_hash, root_branch_id, archived, created_event_id)
                 VALUES ('{session_id}', 'sha256:{zeros}', '{branch_id}', 0, '{session_event}');
                 INSERT INTO turns(turn_id, session_id, branch_id, user_content, created_event_id)
                 VALUES ('{turn_id}', '{session_id}', '{branch_id}', 'interrupted', '{turn_event}');
                 INSERT INTO attempts(attempt_id, turn_id, config_hash, status, prompt_plan, created_event_id)
                 VALUES ('{attempt_id}', '{turn_id}', 'sha256:{zeros}', 'running', x'7b7d', '{attempt_event}');
                 PRAGMA foreign_keys = ON;",
                zeros = "0".repeat(64),
                session_event = EntityId::new(),
                turn_event = EntityId::new(),
                attempt_event = EntityId::new(),
            ))
            .unwrap();

        let report = store.recover_interrupted_attempts().unwrap();
        assert_eq!(report.attempt_ids, vec![attempt_id]);
        let status = store
            .connection
            .query_row(
                "SELECT status FROM attempts WHERE attempt_id = ?1",
                [attempt_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(status, "incomplete");
        assert_eq!(
            store
                .trace_events(Some(session_id))
                .unwrap()
                .last()
                .unwrap()
                .event_type,
            "attempt.recovered-incomplete"
        );
    }
}
