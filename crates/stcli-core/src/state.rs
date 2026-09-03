use std::collections::BTreeMap;

use rusqlite::{Transaction, params, types::Type};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value, json};
use thiserror::Error;

use crate::{
    EntityId, Store, canonical_json,
    storage::{StorageError, append_event},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VariableScope {
    Local,
    Global,
}

impl VariableScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Global => "global",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct StateKey {
    pub scope: VariableScope,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StateCell {
    pub key: StateKey,
    pub value: Value,
    pub raw_value: String,
    pub owner: String,
    pub origin: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StateMutation {
    pub key: StateKey,
    pub before: Option<StateCell>,
    pub after: Option<StateCell>,
}

#[derive(Clone, Debug)]
pub struct StateTransaction {
    session_id: EntityId,
    baseline: BTreeMap<StateKey, StateCell>,
    writes: BTreeMap<StateKey, Option<StateCell>>,
}

impl StateTransaction {
    pub fn empty(session_id: EntityId) -> Self {
        Self {
            session_id,
            baseline: BTreeMap::new(),
            writes: BTreeMap::new(),
        }
    }

    pub fn session_id(&self) -> EntityId {
        self.session_id
    }

    pub fn cells(&self) -> Vec<StateCell> {
        self.baseline.values().cloned().collect()
    }

    pub fn get(&self, scope: VariableScope, name: &str) -> Option<&StateCell> {
        let key = StateKey {
            scope,
            name: name.to_owned(),
        };
        match self.writes.get(&key) {
            Some(value) => value.as_ref(),
            None => self.baseline.get(&key),
        }
    }

    pub fn get_local_then_global(&self, name: &str) -> Option<&StateCell> {
        self.get(VariableScope::Local, name)
            .or_else(|| self.get(VariableScope::Global, name))
    }

    pub fn local_namespace(&self, prefix: &str) -> BTreeMap<String, Value> {
        let qualified = format!("{prefix}.");
        let mut namespace = BTreeMap::new();
        for (key, cell) in &self.baseline {
            if key.scope == VariableScope::Local
                && let Some(relative) = key.name.strip_prefix(&qualified)
            {
                namespace.insert(relative.to_owned(), cell.value.clone());
            }
        }
        for (key, write) in &self.writes {
            if key.scope == VariableScope::Local
                && let Some(relative) = key.name.strip_prefix(&qualified)
            {
                match write {
                    Some(cell) => namespace.insert(relative.to_owned(), cell.value.clone()),
                    None => namespace.remove(relative),
                };
            }
        }
        namespace
    }

    pub fn set(
        &mut self,
        scope: VariableScope,
        name: impl Into<String>,
        value: Value,
        owner: impl Into<String>,
        origin: impl Into<String>,
    ) -> &StateCell {
        let key = StateKey {
            scope,
            name: name.into(),
        };
        let revision = self
            .get(scope, &key.name)
            .map_or(1, |cell| cell.revision + 1);
        let cell = StateCell {
            key: key.clone(),
            raw_value: raw_value(&value),
            value,
            owner: owner.into(),
            origin: origin.into(),
            revision,
        };
        self.writes.insert(key.clone(), Some(cell));
        self.writes[&key].as_ref().expect("inserted state cell")
    }

    pub fn set_raw(
        &mut self,
        scope: VariableScope,
        name: impl Into<String>,
        raw: impl Into<String>,
        owner: impl Into<String>,
        origin: impl Into<String>,
    ) -> &StateCell {
        let raw = raw.into();
        let value = coerce_legacy_read(&raw);
        let cell = self.set(scope, name, value, owner, origin);
        let key = cell.key.clone();
        self.writes
            .get_mut(&key)
            .and_then(Option::as_mut)
            .unwrap()
            .raw_value = raw;
        self.writes[&key].as_ref().unwrap()
    }

    pub fn add_raw(
        &mut self,
        scope: VariableScope,
        name: impl Into<String>,
        increment: &str,
        owner: impl Into<String>,
        origin: impl Into<String>,
    ) -> &StateCell {
        let name = name.into();
        let existing = self
            .get(scope, &name)
            .map(|cell| cell.raw_value.clone())
            .unwrap_or_default();
        let raw = if let (Some(left), Some(right)) = (number(&existing), number(increment)) {
            format_number(left + right)
        } else if let Ok(mut array) = serde_json::from_str::<Vec<Value>>(&existing) {
            array.push(coerce_legacy_read(increment));
            serde_json::to_string(&array).expect("state array is serializable")
        } else {
            format!("{existing}{increment}")
        };
        self.set_raw(scope, name, raw, owner, origin)
    }

    pub fn increment(
        &mut self,
        scope: VariableScope,
        name: impl Into<String>,
        delta: i64,
        owner: impl Into<String>,
        origin: impl Into<String>,
    ) -> &StateCell {
        self.add_raw(scope, name, &delta.to_string(), owner, origin)
    }

    pub fn delete(&mut self, scope: VariableScope, name: impl Into<String>) {
        self.writes.insert(
            StateKey {
                scope,
                name: name.into(),
            },
            None,
        );
    }

    pub fn mutations(&self) -> Vec<StateMutation> {
        self.writes
            .iter()
            .map(|(key, after)| StateMutation {
                key: key.clone(),
                before: self.baseline.get(key).cloned(),
                after: after.clone(),
            })
            .collect()
    }

    pub(crate) fn apply_recorded_mutations(&mut self, mutations: &[StateMutation]) {
        for mutation in mutations {
            self.writes
                .insert(mutation.key.clone(), mutation.after.clone());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }
}

impl Store {
    pub fn state_transaction(&self, session_id: EntityId) -> Result<StateTransaction, StateError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT scope_kind, name, value, raw_value, owner, origin, revision FROM state_cells WHERE (scope_kind = 'local' AND scope_id = ?1) OR (scope_kind = 'global' AND scope_id = 'global') ORDER BY scope_kind, name",
            )
            .map_err(StorageError::Sqlite)?;
        let baseline = statement
            .query_map([session_id.to_string()], |row| {
                let scope: String = row.get(0)?;
                let name: String = row.get(1)?;
                let value: Vec<u8> = row.get(2)?;
                let revision: i64 = row.get(6)?;
                let key = StateKey {
                    scope: match scope.as_str() {
                        "local" => VariableScope::Local,
                        "global" => VariableScope::Global,
                        _ => return Err(conversion_error(0, InvalidScope(scope))),
                    },
                    name,
                };
                Ok((
                    key.clone(),
                    StateCell {
                        key,
                        value: serde_json::from_slice(&value)
                            .map_err(|error| conversion_error(2, error))?,
                        raw_value: row.get(3)?,
                        owner: row.get(4)?,
                        origin: row.get(5)?,
                        revision: u64::try_from(revision)
                            .map_err(|error| conversion_error(6, error))?,
                    },
                ))
            })
            .map_err(StorageError::Sqlite)?
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(StorageError::Sqlite)?;
        Ok(StateTransaction {
            session_id,
            baseline,
            writes: BTreeMap::new(),
        })
    }

    pub fn commit_state_transaction(
        &mut self,
        attempt_id: EntityId,
        state: StateTransaction,
    ) -> Result<Vec<StateMutation>, StateError> {
        let mutations = state.mutations();
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        apply_state_mutations(&transaction, state.session_id, attempt_id, &mutations)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(mutations)
    }
}

pub(crate) fn apply_state_mutations(
    transaction: &Transaction<'_>,
    session_id: EntityId,
    attempt_id: EntityId,
    mutations: &[StateMutation],
) -> Result<(), StateError> {
    if mutations.is_empty() {
        return Ok(());
    }
    append_event(
        transaction,
        Some(session_id),
        "state.committed",
        &json!({
            "attempt_id": attempt_id,
            "mutations": mutations,
        }),
    )?;
    project_state_mutations(transaction, session_id, mutations)
}

pub(crate) fn project_state_mutations(
    transaction: &Transaction<'_>,
    session_id: EntityId,
    mutations: &[StateMutation],
) -> Result<(), StateError> {
    for mutation in mutations {
        let scope_id = match mutation.key.scope {
            VariableScope::Local => session_id.to_string(),
            VariableScope::Global => "global".to_owned(),
        };
        if let Some(cell) = &mutation.after {
            transaction
                .execute(
                    "INSERT INTO state_cells(scope_kind, scope_id, name, value, raw_value, owner, origin, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(scope_kind, scope_id, name) DO UPDATE SET value = excluded.value, raw_value = excluded.raw_value, owner = excluded.owner, origin = excluded.origin, revision = excluded.revision",
                    params![
                        cell.key.scope.as_str(),
                        scope_id,
                        cell.key.name,
                        canonical_json(&cell.value)?,
                        cell.raw_value,
                        cell.owner,
                        cell.origin,
                        cell.revision as i64,
                    ],
                )
                .map_err(StorageError::Sqlite)?;
        } else {
            transaction
                .execute(
                    "DELETE FROM state_cells WHERE scope_kind = ?1 AND scope_id = ?2 AND name = ?3",
                    params![mutation.key.scope.as_str(), scope_id, mutation.key.name],
                )
                .map_err(StorageError::Sqlite)?;
        }
    }
    Ok(())
}

pub(crate) fn apply_plugin_command_state_mutations(
    transaction: &Transaction<'_>,
    session_id: EntityId,
    command_execution_id: EntityId,
    mutations: &[StateMutation],
) -> Result<(), StateError> {
    if mutations.is_empty() {
        return Ok(());
    }
    append_event(
        transaction,
        Some(session_id),
        "state.committed",
        &json!({
            "command_execution_id": command_execution_id,
            "mutations": mutations,
        }),
    )?;
    project_state_mutations(transaction, session_id, mutations)
}

fn raw_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        _ => serde_json::to_string(value).expect("state value is serializable"),
    }
}

fn coerce_legacy_read(raw: &str) -> Value {
    if raw.is_empty() {
        return Value::String(String::new());
    }
    if let Some(value) = number(raw) {
        return Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(raw.to_owned()));
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn number(value: &str) -> Option<f64> {
    if value.trim().is_empty() {
        None
    } else {
        value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
    }
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

#[derive(Debug, Error)]
#[error("invalid state scope '{0}'")]
struct InvalidScope(String);

#[derive(Debug, Error)]
pub enum StateError {
    #[error("state storage failed: {0}")]
    Storage(#[from] StorageError),
    #[error("state JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn local_state_takes_precedence_over_global_state() {
        let session_id = EntityId::new();
        let mut transaction = StateTransaction {
            session_id,
            baseline: BTreeMap::new(),
            writes: BTreeMap::new(),
        };
        transaction.set_raw(VariableScope::Global, "score", "1", "test", "fixture");
        transaction.set_raw(VariableScope::Local, "score", "2", "test", "fixture");
        assert_eq!(
            transaction
                .get_local_then_global("score")
                .unwrap()
                .raw_value,
            "2"
        );
    }

    #[test]
    fn add_uses_numeric_array_and_string_semantics() {
        let session_id = EntityId::new();
        let mut transaction = StateTransaction {
            session_id,
            baseline: BTreeMap::new(),
            writes: BTreeMap::new(),
        };
        transaction.add_raw(VariableScope::Local, "number", "2", "test", "fixture");
        transaction.add_raw(VariableScope::Local, "number", "3", "test", "fixture");
        transaction.set_raw(VariableScope::Local, "array", "[]", "test", "fixture");
        transaction.add_raw(VariableScope::Local, "array", "item", "test", "fixture");
        transaction.add_raw(VariableScope::Local, "text", "a", "test", "fixture");
        transaction.add_raw(VariableScope::Local, "text", "b", "test", "fixture");
        assert_eq!(
            transaction
                .get(VariableScope::Local, "number")
                .unwrap()
                .raw_value,
            "5"
        );
        assert_eq!(
            transaction
                .get(VariableScope::Local, "array")
                .unwrap()
                .raw_value,
            r#"["item"]"#
        );
        assert_eq!(
            transaction
                .get(VariableScope::Local, "text")
                .unwrap()
                .raw_value,
            "ab"
        );
    }

    #[test]
    fn committed_cells_reload_with_provenance() {
        let directory = tempdir().unwrap();
        let session_id = EntityId::new();
        let attempt_id = EntityId::new();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let mut transaction = store.state_transaction(session_id).unwrap();
        transaction.set_raw(VariableScope::Local, "mood", "happy", "macro", "setvar");
        store
            .commit_state_transaction(attempt_id, transaction)
            .unwrap();
        let reloaded = store.state_transaction(session_id).unwrap();
        let mood = reloaded.get(VariableScope::Local, "mood").unwrap();
        assert_eq!(mood.raw_value, "happy");
        assert_eq!(mood.owner, "macro");
        assert_eq!(mood.origin, "setvar");
        assert_eq!(mood.revision, 1);
    }
}
