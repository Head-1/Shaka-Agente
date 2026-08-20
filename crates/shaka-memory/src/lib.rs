//! Persistência de memória do Shaka.
//!
//! O MVP usa `SQLite` por simplicidade operacional. A memória semântica mantém
//! documentos e metadados; a busca vetorial dedicada fica para uma fase futura.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, backup::Backup, params};
use serde::{Deserialize, Serialize};
use shaka_core::{AuditEvent, TaskId, TenantId};
use std::{path::Path, time::Duration as StdDuration};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("erro SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("erro de filesystem: {0}")]
    Io(String),
    #[error("erro de serialização: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("registro não encontrado: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpisodicRecord {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub task_id: Option<TaskId>,
    pub kind: String,
    pub content: String,
    pub outcome: String,
    pub cost_microunits: u64,
    pub elapsed_ms: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticRecord {
    pub id: Uuid,
    pub tenant_id: TenantId,
    pub title: String,
    pub content: String,
    pub source_episode_id: Option<Uuid>,
    pub version: u32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditVerification {
    pub valid: bool,
    pub checked_events: u64,
    pub failure_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkingMemoryItem {
    pub key: String,
    pub value: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct MemoryStore {
    connection: parking_lot::Mutex<Connection>,
}

fn restrict_file_permissions(path: impl AsRef<Path>) -> Result<(), MemoryError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| MemoryError::Io(error.to_string()))?;
    }
    Ok(())
}

impl MemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let connection = Connection::open(path)?;
        let store = Self {
            connection: parking_lot::Mutex::new(connection),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self, MemoryError> {
        let connection = Connection::open_in_memory()?;
        let store = Self {
            connection: parking_lot::Mutex::new(connection),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), MemoryError> {
        self.connection.lock().execute_batch(
            "             PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;

             CREATE TABLE IF NOT EXISTS episodic_memory (
                 id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 task_id TEXT,
                 kind TEXT NOT NULL,
                 content TEXT NOT NULL,
                 outcome TEXT NOT NULL,
                 cost_microunits INTEGER NOT NULL,
                 elapsed_ms INTEGER NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_episodic_tenant_created
                 ON episodic_memory (tenant_id, created_at DESC);
             CREATE TABLE IF NOT EXISTS semantic_memory (
                 id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 title TEXT NOT NULL,
                 content TEXT NOT NULL,
                 source_episode_id TEXT,
                 version INTEGER NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_semantic_tenant_title
                 ON semantic_memory (tenant_id, title);
             CREATE TABLE IF NOT EXISTS working_memory (
                 tenant_id TEXT NOT NULL,
                 task_id TEXT NOT NULL,
                 key TEXT NOT NULL,
                 value TEXT NOT NULL,
                 expires_at TEXT NOT NULL,
                 PRIMARY KEY (tenant_id, task_id, key)
             );
             CREATE TABLE IF NOT EXISTS audit_events (
                 event_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 task_id TEXT,
                 event_json TEXT NOT NULL,
                 event_hash TEXT NOT NULL,
                 occurred_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_audit_tenant_occurred
                 ON audit_events (tenant_id, occurred_at DESC);",
        )?;
        Ok(())
    }

    pub fn append_episode(&self, record: &EpisodicRecord) -> Result<(), MemoryError> {
        self.connection.lock().execute(
            "INSERT INTO episodic_memory
             (id, tenant_id, task_id, kind, content, outcome, cost_microunits, elapsed_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.id.to_string(),
                record.tenant_id.0,
                record.task_id.as_ref().map(|id| id.0.to_string()),
                record.kind,
                record.content,
                record.outcome,
                record.cost_microunits,
                record.elapsed_ms,
                record.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn recent_episodes(
        &self,
        tenant_id: &TenantId,
        limit: u32,
    ) -> Result<Vec<EpisodicRecord>, MemoryError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT id, task_id, kind, content, outcome, cost_microunits, elapsed_ms, created_at
             FROM episodic_memory WHERE tenant_id = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![tenant_id.0, limit], |row| {
            let task_id: Option<String> = row.get(1)?;
            let created_at: String = row.get(7)?;
            Ok(EpisodicRecord {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap_or_else(|_| Uuid::nil()),
                tenant_id: TenantId(tenant_id.0.clone()),
                task_id: task_id
                    .and_then(|value| Uuid::parse_str(&value).ok())
                    .map(TaskId),
                kind: row.get(2)?,
                content: row.get(3)?,
                outcome: row.get(4)?,
                cost_microunits: row.get(5)?,
                elapsed_ms: row.get(6)?,
                created_at: DateTime::parse_from_rfc3339(&created_at)
                    .map_or_else(|_| Utc::now(), |value| value.with_timezone(&Utc)),
            })
        })?;
        let mut episodes = Vec::new();
        for row in rows {
            episodes.push(row?);
        }
        Ok(episodes)
    }

    pub fn put_working(
        &self,
        tenant_id: &TenantId,
        task_id: &TaskId,
        item: &WorkingMemoryItem,
    ) -> Result<(), MemoryError> {
        self.connection.lock().execute(
            "INSERT INTO working_memory (tenant_id, task_id, key, value, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (tenant_id, task_id, key) DO UPDATE SET value = excluded.value, expires_at = excluded.expires_at",
            params![
                tenant_id.0,
                task_id.0.to_string(),
                item.key,
                item.value,
                item.expires_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn get_working(
        &self,
        tenant_id: &TenantId,
        task_id: &TaskId,
        key: &str,
    ) -> Result<Option<WorkingMemoryItem>, MemoryError> {
        let now = Utc::now().to_rfc3339();
        let connection = self.connection.lock();
        let result = connection
            .query_row(
                "SELECT value, expires_at FROM working_memory
                 WHERE tenant_id = ?1 AND task_id = ?2 AND key = ?3 AND expires_at > ?4",
                params![tenant_id.0, task_id.0.to_string(), key, now],
                |row| {
                    let expires_at: String = row.get(1)?;
                    Ok(WorkingMemoryItem {
                        key: key.to_owned(),
                        value: row.get(0)?,
                        expires_at: DateTime::parse_from_rfc3339(&expires_at)
                            .map_or_else(|_| Utc::now(), |value| value.with_timezone(&Utc)),
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    pub fn consolidate_episode(
        &self,
        tenant_id: &TenantId,
        episode_id: Uuid,
        title: &str,
        content: &str,
    ) -> Result<SemanticRecord, MemoryError> {
        let connection = self.connection.lock();
        let episode_exists: Option<String> = connection
            .query_row(
                "SELECT id FROM episodic_memory WHERE id = ?1 AND tenant_id = ?2",
                params![episode_id.to_string(), tenant_id.0],
                |row| row.get(0),
            )
            .optional()?;
        if episode_exists.is_none() {
            return Err(MemoryError::NotFound(format!("episódio {episode_id}")));
        }
        let record = SemanticRecord {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            title: title.to_owned(),
            content: content.to_owned(),
            source_episode_id: Some(episode_id),
            version: 1,
            created_at: Utc::now(),
        };
        connection.execute(
            "INSERT INTO semantic_memory
             (id, tenant_id, title, content, source_episode_id, version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id.to_string(),
                record.tenant_id.0,
                record.title,
                record.content,
                record.source_episode_id.map(|value| value.to_string()),
                record.version,
                record.created_at.to_rfc3339(),
            ],
        )?;
        Ok(record)
    }

    pub fn purge_older_than(
        &self,
        tenant_id: &TenantId,
        retention: Duration,
    ) -> Result<usize, MemoryError> {
        let cutoff = (Utc::now() - retention).to_rfc3339();
        let deleted = self.connection.lock().execute(
            "DELETE FROM episodic_memory WHERE tenant_id = ?1 AND created_at < ?2",
            params![tenant_id.0, cutoff],
        )?;
        Ok(deleted)
    }

    pub fn append_audit_event(&self, event: &AuditEvent) -> Result<AuditEvent, MemoryError> {
        let connection = self.connection.lock();
        let previous_hash: Option<String> = connection
            .query_row(
                "SELECT event_hash FROM audit_events WHERE tenant_id = ?1
                 ORDER BY occurred_at DESC, rowid DESC LIMIT 1",
                params![event.tenant_id.0],
                |row| row.get(0),
            )
            .optional()?;
        let chained = event.with_previous_hash(previous_hash);
        connection.execute(
            "INSERT INTO audit_events (event_id, tenant_id, task_id, event_json, event_hash, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                chained.event_id.to_string(),
                chained.tenant_id.0,
                chained.task_id.as_ref().map(|id| id.0.to_string()),
                serde_json::to_string(&chained)?,
                chained.event_hash,
                chained.occurred_at.to_rfc3339(),
            ],
        )?;
        Ok(chained)
    }

    pub fn verify_audit_chain(
        &self,
        tenant_id: &TenantId,
    ) -> Result<AuditVerification, MemoryError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT event_json FROM audit_events WHERE tenant_id = ?1
             ORDER BY occurred_at ASC, rowid ASC",
        )?;
        let rows = statement.query_map(params![tenant_id.0], |row| row.get::<_, String>(0))?;
        let mut previous_hash = None;
        let mut checked = 0_u64;
        for row in rows {
            let event: AuditEvent = serde_json::from_str(&row?)?;
            if event.tenant_id != *tenant_id
                || event.previous_hash != previous_hash
                || !event.has_valid_hash()
            {
                return Ok(AuditVerification {
                    valid: false,
                    checked_events: checked,
                    failure_at: Some(event.event_id.to_string()),
                });
            }
            previous_hash = Some(event.event_hash);
            checked += 1;
        }
        Ok(AuditVerification {
            valid: true,
            checked_events: checked,
            failure_at: None,
        })
    }

    pub fn verify_integrity(&self) -> Result<bool, MemoryError> {
        let result: String =
            self.connection
                .lock()
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result.eq_ignore_ascii_case("ok"))
    }

    pub fn backup_to(&self, destination: impl AsRef<Path>) -> Result<(), MemoryError> {
        let destination = destination.as_ref();
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| MemoryError::Io(error.to_string()))?;
        }
        let source = self.connection.lock();
        let mut target = Connection::open(destination)?;
        let backup = Backup::new(&source, &mut target)?;
        backup.run_to_completion(100, StdDuration::from_millis(25), None)?;
        restrict_file_permissions(destination)?;
        Ok(())
    }

    pub fn verify_integrity_at(path: impl AsRef<Path>) -> Result<bool, MemoryError> {
        let connection = Connection::open(path)?;
        let result: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result.eq_ignore_ascii_case("ok"))
    }

    pub fn restore_from(&self, source_path: impl AsRef<Path>) -> Result<(), MemoryError> {
        if !Self::verify_integrity_at(&source_path)? {
            return Err(MemoryError::Io(
                "integridade do backup de origem falhou".to_owned(),
            ));
        }
        let source = Connection::open(source_path)?;
        let mut target = self.connection.lock();
        let backup = Backup::new(&source, &mut target)?;
        backup.run_to_completion(100, StdDuration::from_millis(25), None)?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn episode_can_be_recorded_and_read() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-a").unwrap();
        let episode = EpisodicRecord {
            id: Uuid::new_v4(),
            tenant_id: tenant.clone(),
            task_id: None,
            kind: "test".to_owned(),
            content: "resultado".to_owned(),
            outcome: "success".to_owned(),
            cost_microunits: 10,
            elapsed_ms: 2,
            created_at: Utc::now(),
        };
        store.append_episode(&episode).unwrap();
        let episodes = store.recent_episodes(&tenant, 10).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].content, "resultado");
    }

    #[test]
    fn working_memory_expires() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-a").unwrap();
        let task = TaskId::new();
        let item = WorkingMemoryItem {
            key: "key".to_owned(),
            value: "value".to_owned(),
            expires_at: Utc::now() - Duration::seconds(1),
        };
        store.put_working(&tenant, &task, &item).unwrap();
        assert!(store.get_working(&tenant, &task, "key").unwrap().is_none());
    }

    #[test]
    fn audit_chain_is_verified() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-a").unwrap();
        let first = AuditEvent::new(
            None,
            tenant.clone(),
            "operator",
            "test.one",
            "success",
            std::collections::BTreeMap::default(),
            None,
        );
        let second = AuditEvent::new(
            None,
            tenant.clone(),
            "operator",
            "test.two",
            "success",
            std::collections::BTreeMap::default(),
            None,
        );
        store.append_audit_event(&first).unwrap();
        store.append_audit_event(&second).unwrap();
        let verification = store.verify_audit_chain(&tenant).unwrap();
        assert!(verification.valid);
        assert_eq!(verification.checked_events, 2);
    }

    #[test]
    fn backup_and_restore_preserve_episodes() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-a").unwrap();
        let episode = EpisodicRecord {
            id: Uuid::new_v4(),
            tenant_id: tenant.clone(),
            task_id: None,
            kind: "backup-test".to_owned(),
            content: "persistir".to_owned(),
            outcome: "success".to_owned(),
            cost_microunits: 0,
            elapsed_ms: 1,
            created_at: Utc::now(),
        };
        store.append_episode(&episode).unwrap();
        let backup = std::env::temp_dir().join(format!("shaka-backup-{}.db", Uuid::new_v4()));
        store.backup_to(&backup).unwrap();
        let restored = MemoryStore::open(&backup).unwrap();
        assert_eq!(restored.recent_episodes(&tenant, 10).unwrap().len(), 1);
        assert!(restored.verify_integrity().unwrap());
        std::fs::remove_file(backup).unwrap();
    }

    #[test]
    fn episodes_are_isolated_by_tenant() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant_a = TenantId::new("tenant-a").unwrap();
        let tenant_b = TenantId::new("tenant-b").unwrap();
        let episode = EpisodicRecord {
            id: Uuid::new_v4(),
            tenant_id: tenant_a.clone(),
            task_id: None,
            kind: "isolation".to_owned(),
            content: "private".to_owned(),
            outcome: "success".to_owned(),
            cost_microunits: 0,
            elapsed_ms: 1,
            created_at: Utc::now(),
        };
        store.append_episode(&episode).unwrap();
        assert!(store.recent_episodes(&tenant_b, 10).unwrap().is_empty());
    }
}
