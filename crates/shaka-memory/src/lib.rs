//! Persistência de memória do Shaka.
//!
//! O MVP usa `SQLite` por simplicidade operacional. A memória semântica mantém
//! documentos e metadados; a busca vetorial dedicada fica para uma fase futura.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, backup::Backup, params};
use serde::{Deserialize, Serialize};
use shaka_core::{AuditEvent, TaskId, TenantId};
use std::{path::Path, time::Duration as StdDuration};
use thiserror::Error;
use uuid::Uuid;

/// Erros de persistência, validação e integridade da memória.
#[derive(Debug, Error)]
pub enum MemoryError {
    /// Falha de leitura ou escrita no SQLite.
    #[error("erro SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Falha de acesso ao filesystem durante backup ou restauração.
    #[error("erro de filesystem: {0}")]
    Io(String),
    /// Falha ao serializar ou desserializar um registro.
    #[error("erro de serialização: {0}")]
    Serialization(#[from] serde_json::Error),
    /// Registro solicitado que não existe no tenant consultado.
    #[error("registro não encontrado: {0}")]
    NotFound(String),
    /// Período de retenção que não respeita a política permitida.
    #[error("retenção inválida: {0}")]
    InvalidRetention(String),
    /// Registro persistido que não pôde ser interpretado com segurança.
    #[error("registro episódico inválido: {0}")]
    InvalidRecord(String),
}

/// Registro episódico associado a uma execução observada pelo tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpisodicRecord {
    /// Identificador único do episódio.
    pub id: Uuid,
    /// Tenant proprietário do episódio.
    pub tenant_id: TenantId,
    /// Tarefa de origem, quando o episódio veio de uma execução.
    pub task_id: Option<TaskId>,
    /// Classificação operacional do episódio.
    pub kind: String,
    /// Conteúdo persistido do episódio.
    pub content: String,
    /// Resultado observado da execução.
    pub outcome: String,
    /// Custo contabilizado em microunidades.
    pub cost_microunits: u64,
    /// Tempo decorrido da execução em milissegundos.
    pub elapsed_ms: u64,
    /// Instante de criação em UTC.
    pub created_at: DateTime<Utc>,
}

/// Registro semântico consolidado a partir de memória episódica.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticRecord {
    /// Identificador único do conhecimento consolidado.
    pub id: Uuid,
    /// Tenant proprietário do registro semântico.
    pub tenant_id: TenantId,
    /// Título de apresentação ou busca do conhecimento.
    pub title: String,
    /// Conteúdo consolidado.
    pub content: String,
    /// Episódio que originou o registro, quando conhecido.
    pub source_episode_id: Option<Uuid>,
    /// Versão lógica do registro semântico.
    pub version: u32,
    /// Instante de criação em UTC.
    pub created_at: DateTime<Utc>,
}

/// Resultado da verificação da cadeia de auditoria de um tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditVerification {
    /// Indica se todos os eventos verificados mantêm a cadeia válida.
    pub valid: bool,
    /// Quantidade de eventos examinados até o resultado.
    pub checked_events: u64,
    /// Identificador do primeiro evento inválido, quando encontrado.
    pub failure_at: Option<String>,
}

/// Item temporário de working memory com expiração explícita.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkingMemoryItem {
    /// Chave do item dentro da tarefa.
    pub key: String,
    /// Valor textual armazenado.
    pub value: String,
    /// Instante a partir do qual o item não é mais retornado.
    pub expires_at: DateTime<Utc>,
}

/// Store SQLite para memória episódica, semântica, temporária e auditoria.
#[derive(Debug)]
pub struct MemoryStore {
    connection: parking_lot::Mutex<Connection>,
}

const REQUIRED_SCHEMA_TABLES: [&str; 4] = [
    "episodic_memory",
    "semantic_memory",
    "working_memory",
    "audit_events",
];

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
    /// Abre ou cria um banco SQLite persistente e aplica seu schema idempotente.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(StdDuration::from_secs(5))?;
        let store = Self {
            connection: parking_lot::Mutex::new(connection),
        };
        store.migrate()?;
        Ok(store)
    }

    /// Cria um store SQLite efêmero, destinado a testes e validações locais.
    pub fn in_memory() -> Result<Self, MemoryError> {
        let connection = Connection::open_in_memory()?;
        connection.busy_timeout(StdDuration::from_secs(5))?;
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

    /// Persiste um episódio pertencente ao tenant do próprio registro.
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

    /// Retorna os episódios mais recentes isolados pelo tenant e limitados por quantidade.
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
        let mut rows = statement.query(params![tenant_id.0, limit])?;
        let mut episodes = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let id = Uuid::parse_str(&id)
                .map_err(|_| MemoryError::InvalidRecord("episodic id inválido".to_owned()))?;
            let task_id: Option<String> = row.get(1)?;
            let task_id = task_id
                .map(|value| {
                    Uuid::parse_str(&value).map(TaskId).map_err(|_| {
                        MemoryError::InvalidRecord("episodic task_id inválido".to_owned())
                    })
                })
                .transpose()?;
            let created_at: String = row.get(7)?;
            let created_at = DateTime::parse_from_rfc3339(&created_at)
                .map_err(|_| MemoryError::InvalidRecord("episodic created_at inválido".to_owned()))?
                .with_timezone(&Utc);
            episodes.push(EpisodicRecord {
                id,
                tenant_id: TenantId(tenant_id.0.clone()),
                task_id,
                kind: row.get(2)?,
                content: row.get(3)?,
                outcome: row.get(4)?,
                cost_microunits: row.get(5)?,
                elapsed_ms: row.get(6)?,
                created_at,
            });
        }
        Ok(episodes)
    }

    /// Insere ou substitui um item de working memory no escopo tenant/tarefa.
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

    /// Carrega um item de working memory somente enquanto ele não expirou.
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
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        result
            .map(|(value, expires_at)| {
                let expires_at = DateTime::parse_from_rfc3339(&expires_at)
                    .map_err(|_| {
                        MemoryError::InvalidRecord("working_memory expires_at inválido".to_owned())
                    })?
                    .with_timezone(&Utc);
                Ok(WorkingMemoryItem {
                    key: key.to_owned(),
                    value,
                    expires_at,
                })
            })
            .transpose()
    }

    /// Consolida um episódio existente do tenant em um registro semântico.
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

    /// Remove episódios do tenant anteriores ao período de retenção informado.
    pub fn purge_older_than(
        &self,
        tenant_id: &TenantId,
        retention: Duration,
    ) -> Result<usize, MemoryError> {
        if retention < Duration::zero() {
            return Err(MemoryError::InvalidRetention(
                "o período deve ser zero ou positivo".to_owned(),
            ));
        }
        let cutoff = (Utc::now() - retention).to_rfc3339();
        let deleted = self.connection.lock().execute(
            "DELETE FROM episodic_memory WHERE tenant_id = ?1 AND created_at < ?2",
            params![tenant_id.0, cutoff],
        )?;
        Ok(deleted)
    }

    /// Acrescenta um evento à cadeia de auditoria do tenant em transação imediata.
    pub fn append_audit_event(&self, event: &AuditEvent) -> Result<AuditEvent, MemoryError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous_hash: Option<String> = transaction
            .query_row(
                "SELECT event_hash FROM audit_events WHERE tenant_id = ?1
                 ORDER BY rowid DESC LIMIT 1",
                params![event.tenant_id.0],
                |row| row.get(0),
            )
            .optional()?;
        let chained = event.with_previous_hash(previous_hash);
        transaction.execute(
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
        transaction.commit()?;
        Ok(chained)
    }

    /// Verifica a cadeia de auditoria na ordem estrutural de commit do tenant.
    pub fn verify_audit_chain(
        &self,
        tenant_id: &TenantId,
    ) -> Result<AuditVerification, MemoryError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT event_json, event_hash FROM audit_events WHERE tenant_id = ?1
             ORDER BY rowid ASC",
        )?;
        let rows = statement.query_map(params![tenant_id.0], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut previous_hash = None;
        let mut checked = 0_u64;
        for row in rows {
            let (event_json, persisted_hash) = row?;
            let event: AuditEvent = serde_json::from_str(&event_json)?;
            if event.tenant_id != *tenant_id
                || event.previous_hash != previous_hash
                || event.event_hash != persisted_hash
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

    /// Executa `PRAGMA integrity_check` no banco atualmente aberto.
    pub fn verify_integrity(&self) -> Result<bool, MemoryError> {
        let result: String =
            self.connection
                .lock()
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result.eq_ignore_ascii_case("ok"))
    }

    /// Cria um backup consistente no destino e restringe suas permissões em Unix.
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

    /// Verifica a integridade de um arquivo SQLite sem abri-lo como store ativo.
    pub fn verify_integrity_at(path: impl AsRef<Path>) -> Result<bool, MemoryError> {
        let connection = Connection::open(path)?;
        let result: String =
            connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result.eq_ignore_ascii_case("ok"))
    }

    fn verify_required_schema_at(path: impl AsRef<Path>) -> Result<bool, MemoryError> {
        let connection = Connection::open(path)?;
        for table in REQUIRED_SCHEMA_TABLES {
            let exists: Option<String> = connection
                .query_row(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Restaura o store a partir de um snapshot cuja integridade e schema foram confirmados.
    pub fn restore_from(&self, source_path: impl AsRef<Path>) -> Result<(), MemoryError> {
        if !Self::verify_integrity_at(&source_path)? {
            return Err(MemoryError::Io(
                "integridade do backup de origem falhou".to_owned(),
            ));
        }
        if !Self::verify_required_schema_at(&source_path)? {
            return Err(MemoryError::Io(
                "schema do backup de origem é incompatível".to_owned(),
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
    use std::{
        collections::BTreeMap,
        sync::{Arc, Barrier, mpsc},
        thread,
    };

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
    fn malformed_episode_fails_closed_instead_of_synthesizing_fields() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-memory-corrupt").unwrap();
        store
            .connection
            .lock()
            .execute(
                "INSERT INTO episodic_memory
                 (id, tenant_id, task_id, kind, content, outcome, cost_microunits, elapsed_ms, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "not-a-uuid",
                    tenant.0,
                    "also-not-a-uuid",
                    "corrupt",
                    "payload",
                    "success",
                    0_i64,
                    0_i64,
                    "not-a-rfc3339-timestamp",
                ],
            )
            .unwrap();
        let result = store.recent_episodes(&tenant, 10);
        assert!(result.is_err());
    }

    #[test]
    fn malformed_working_memory_expiry_fails_closed() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-working-corrupt").unwrap();
        let task_id = TaskId::new();
        store
            .connection
            .lock()
            .execute(
                "INSERT INTO working_memory (tenant_id, task_id, key, value, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    tenant.0,
                    task_id.0.to_string(),
                    "corrupt-key",
                    "payload",
                    "9999-not-a-rfc3339-timestamp",
                ],
            )
            .unwrap();
        let result = store.get_working(&tenant, &task_id, "corrupt-key");
        assert!(result.is_err());
    }

    #[test]
    fn append_episode_waits_for_concurrent_writer() {
        let path =
            std::env::temp_dir().join(format!("shaka-memory-lock-{}.sqlite", Uuid::new_v4()));
        let store = Arc::new(MemoryStore::open(&path).unwrap());
        let tenant = TenantId::new("tenant-memory-lock").unwrap();
        let episode = EpisodicRecord {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            task_id: None,
            kind: "lock-test".to_owned(),
            content: "after-writer".to_owned(),
            outcome: "success".to_owned(),
            cost_microunits: 0,
            elapsed_ms: 0,
            created_at: Utc::now(),
        };
        let blocker = Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let worker_store = Arc::clone(&store);
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_store.append_episode(&episode)
        });
        started_rx.recv_timeout(StdDuration::from_secs(1)).unwrap();
        thread::sleep(StdDuration::from_millis(100));
        blocker.execute_batch("COMMIT").unwrap();
        assert!(worker.join().unwrap().is_ok());
        drop(blocker);
        std::fs::remove_file(path).unwrap();
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
    fn negative_retention_is_rejected_without_deleting_recent_episode() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-retention").unwrap();
        store
            .append_episode(&EpisodicRecord {
                id: Uuid::new_v4(),
                tenant_id: tenant.clone(),
                task_id: None,
                kind: "retention-test".to_owned(),
                content: "recent".to_owned(),
                outcome: "success".to_owned(),
                cost_microunits: 0,
                elapsed_ms: 0,
                created_at: Utc::now(),
            })
            .unwrap();

        assert!(matches!(
            store.purge_older_than(&tenant, Duration::days(-1)),
            Err(MemoryError::InvalidRetention(_))
        ));
        assert_eq!(store.recent_episodes(&tenant, 10).unwrap().len(), 1);
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
    fn audit_chain_uses_commit_order_not_event_timestamp() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-audit-clock").unwrap();
        let first = AuditEvent::new(
            None,
            tenant.clone(),
            "actor-one",
            "clock.first",
            "success",
            BTreeMap::new(),
            None,
        );
        store.append_audit_event(&first).unwrap();
        let mut second = AuditEvent::new(
            None,
            tenant.clone(),
            "actor-two",
            "clock.second",
            "success",
            BTreeMap::new(),
            None,
        );
        second.occurred_at = first.occurred_at - Duration::days(1);
        store.append_audit_event(&second).unwrap();
        let verification = store.verify_audit_chain(&tenant).unwrap();
        assert!(verification.valid);
        assert_eq!(verification.checked_events, 2);
    }

    #[test]
    fn audit_chain_remains_linear_across_store_instances() {
        let path =
            std::env::temp_dir().join(format!("shaka-audit-concurrency-{}.sqlite", Uuid::new_v4()));
        let tenant = TenantId::new("tenant-audit-concurrency").unwrap();
        let occurred_at = Utc::now();
        let workers = 16_usize;
        let stores = (0..workers)
            .map(|_| MemoryStore::open(&path).unwrap())
            .collect::<Vec<_>>();
        let barrier = Arc::new(Barrier::new(workers));
        let handles = stores
            .into_iter()
            .enumerate()
            .map(|(index, store)| {
                let barrier = Arc::clone(&barrier);
                let tenant = tenant.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let mut event = AuditEvent::new(
                        None,
                        tenant,
                        format!("actor-{index}"),
                        "concurrent.append",
                        "success",
                        BTreeMap::new(),
                        None,
                    );
                    event.occurred_at = occurred_at;
                    store.append_audit_event(&event)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            assert!(handle.join().unwrap().is_ok());
        }
        let verifier = MemoryStore::open(&path).unwrap();
        let verification = verifier.verify_audit_chain(&tenant).unwrap();
        assert!(verification.valid);
        assert_eq!(verification.checked_events, workers as u64);
        drop(verifier);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn restore_rejects_valid_but_incompatible_schema_without_mutating_target() {
        let source =
            std::env::temp_dir().join(format!("shaka-incompatible-source-{}.db", Uuid::new_v4()));
        let target =
            std::env::temp_dir().join(format!("shaka-incompatible-target-{}.db", Uuid::new_v4()));
        let tenant = TenantId::new("tenant-restore-schema").unwrap();
        let target_store = MemoryStore::open(&target).unwrap();
        target_store
            .append_episode(&EpisodicRecord {
                id: Uuid::new_v4(),
                tenant_id: tenant.clone(),
                task_id: None,
                kind: "restore-target".to_owned(),
                content: "preserve".to_owned(),
                outcome: "success".to_owned(),
                cost_microunits: 0,
                elapsed_ms: 0,
                created_at: Utc::now(),
            })
            .unwrap();
        let source_connection = Connection::open(&source).unwrap();
        source_connection
            .execute("CREATE TABLE unrelated (value TEXT NOT NULL)", [])
            .unwrap();
        source_connection
            .execute(
                "INSERT INTO unrelated VALUES ('valid sqlite, wrong schema')",
                [],
            )
            .unwrap();
        source_connection.close().unwrap();

        assert!(MemoryStore::verify_integrity_at(&source).unwrap());
        let error = target_store.restore_from(&source).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("schema do backup de origem é incompatível")
        );
        assert_eq!(
            target_store.recent_episodes(&tenant, 10).unwrap()[0].content,
            "preserve"
        );
        assert!(target_store.verify_integrity().unwrap());
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(target).unwrap();
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
    fn restore_replaces_target_with_consistent_source_snapshot() {
        let source =
            std::env::temp_dir().join(format!("shaka-restore-source-{}.db", Uuid::new_v4()));
        let target =
            std::env::temp_dir().join(format!("shaka-restore-valid-target-{}.db", Uuid::new_v4()));
        let source_tenant = TenantId::new("tenant-restore-valid").unwrap();
        let old_tenant = TenantId::new("tenant-old").unwrap();
        let source_store = MemoryStore::open(&source).unwrap();
        source_store
            .append_episode(&EpisodicRecord {
                id: Uuid::new_v4(),
                tenant_id: source_tenant.clone(),
                task_id: None,
                kind: "restore-source".to_owned(),
                content: "from source".to_owned(),
                outcome: "success".to_owned(),
                cost_microunits: 0,
                elapsed_ms: 0,
                created_at: Utc::now(),
            })
            .unwrap();
        let target_store = MemoryStore::open(&target).unwrap();
        target_store
            .append_episode(&EpisodicRecord {
                id: Uuid::new_v4(),
                tenant_id: old_tenant.clone(),
                task_id: None,
                kind: "restore-target".to_owned(),
                content: "old target".to_owned(),
                outcome: "success".to_owned(),
                cost_microunits: 0,
                elapsed_ms: 0,
                created_at: Utc::now(),
            })
            .unwrap();

        target_store.restore_from(&source).unwrap();
        assert_eq!(
            target_store.recent_episodes(&source_tenant, 10).unwrap()[0].content,
            "from source"
        );
        assert!(
            target_store
                .recent_episodes(&old_tenant, 10)
                .unwrap()
                .is_empty()
        );
        assert!(target_store.verify_integrity().unwrap());
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(target).unwrap();
    }

    #[test]
    fn restore_rejects_corrupt_source_without_mutating_target() {
        let source =
            std::env::temp_dir().join(format!("shaka-corrupt-source-{}.db", Uuid::new_v4()));
        let target =
            std::env::temp_dir().join(format!("shaka-restore-target-{}.db", Uuid::new_v4()));
        let tenant = TenantId::new("tenant-restore").unwrap();
        let target_store = MemoryStore::open(&target).unwrap();
        target_store
            .append_episode(&EpisodicRecord {
                id: Uuid::new_v4(),
                tenant_id: tenant.clone(),
                task_id: None,
                kind: "restore-target".to_owned(),
                content: "preserve".to_owned(),
                outcome: "success".to_owned(),
                cost_microunits: 0,
                elapsed_ms: 0,
                created_at: Utc::now(),
            })
            .unwrap();
        std::fs::write(&source, b"not a sqlite database").unwrap();

        assert!(target_store.restore_from(&source).is_err());
        assert_eq!(target_store.recent_episodes(&tenant, 10).unwrap().len(), 1);
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(target).unwrap();
    }

    #[test]
    fn audit_chain_detects_event_json_tampering() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-audit-tamper").unwrap();
        let event = AuditEvent::new(
            None,
            tenant.clone(),
            "operator",
            "tamper.test",
            "success",
            std::collections::BTreeMap::default(),
            None,
        );
        let stored = store.append_audit_event(&event).unwrap();
        let mut tampered: AuditEvent = {
            let connection = store.connection.lock();
            let json: String = connection
                .query_row(
                    "SELECT event_json FROM audit_events WHERE event_id = ?1",
                    params![stored.event_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::from_str(&json).unwrap()
        };
        tampered.outcome = "tampered".to_owned();
        let tampered_json = serde_json::to_string(&tampered).unwrap();
        store
            .connection
            .lock()
            .execute(
                "UPDATE audit_events SET event_json = ?1 WHERE event_id = ?2",
                params![tampered_json, stored.event_id.to_string()],
            )
            .unwrap();

        let verification = store.verify_audit_chain(&tenant).unwrap();
        assert!(!verification.valid);
        let failure_at = stored.event_id.to_string();
        assert_eq!(
            verification.failure_at.as_deref(),
            Some(failure_at.as_str())
        );
    }

    #[test]
    fn audit_chain_detects_persisted_hash_column_tampering() {
        let store = MemoryStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-audit-column").unwrap();
        let event = AuditEvent::new(
            None,
            tenant.clone(),
            "operator",
            "audit.column",
            "success",
            std::collections::BTreeMap::default(),
            None,
        );
        let stored = store.append_audit_event(&event).unwrap();
        store
            .connection
            .lock()
            .execute(
                "UPDATE audit_events SET event_hash = ?1 WHERE event_id = ?2",
                params!["tampered-persisted-hash", stored.event_id.to_string()],
            )
            .unwrap();

        let verification = store.verify_audit_chain(&tenant).unwrap();
        assert!(!verification.valid);
        let failure_at = stored.event_id.to_string();
        assert_eq!(
            verification.failure_at.as_deref(),
            Some(failure_at.as_str())
        );
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
