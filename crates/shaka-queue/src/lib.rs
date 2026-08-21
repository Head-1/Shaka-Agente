//! Persistência e coordenação da fila de tarefas da API v0.5.0.
//!
//! O crate mantém as operações de fila em SQLite com transições explícitas,
//! idempotência por tenant e leases recuperáveis após reinicialização.

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shaka_core::{OperatorId, Principal, TaskEnvelope, TaskId, TenantId};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("erro SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("erro de serialização: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("erro de núcleo: {0}")]
    Core(#[from] shaka_core::CoreError),
    #[error("identificador inválido: {0}")]
    InvalidIdentifier(String),
    #[error("entrada inválida: {0}")]
    InvalidInput(String),
    #[error("registro não encontrado: {0}")]
    NotFound(String),
    #[error("chave de idempotência já usada com outro payload")]
    IdempotencyConflict,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    CancelRequested,
    Cancelled,
}

impl TaskStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::CancelRequested => "cancel_requested",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, QueueError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancel_requested" => Ok(Self::CancelRequested),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(QueueError::InvalidInput(format!(
                "status desconhecido: {other}"
            ))),
        }
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: Uuid,
    pub principal: Principal,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: TaskId,
    pub session_id: Uuid,
    pub tenant_id: TenantId,
    pub idempotency_key: String,
    pub request_fingerprint: String,
    pub envelope: TaskEnvelope,
    pub status: TaskStatus,
    pub priority: i32,
    pub attempts: u32,
    pub max_attempts: u32,
    pub next_attempt_at: DateTime<Utc>,
    pub cancel_requested: bool,
    pub lease_until: Option<DateTime<Utc>>,
    pub result: Option<Value>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubmitOutcome {
    Created,
    Existing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishOutcome {
    Succeeded,
    Requeued { next_attempt_at: DateTime<Utc> },
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }

    fn parse(value: &str) -> Result<Self, QueueError> {
        match value {
            "closed" => Ok(Self::Closed),
            "open" => Ok(Self::Open),
            "half_open" => Ok(Self::HalfOpen),
            other => Err(QueueError::InvalidInput(format!(
                "circuit state desconhecido: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CircuitSnapshot {
    pub name: String,
    pub state: CircuitState,
    pub failure_count: u32,
    pub opened_at: Option<DateTime<Utc>>,
    pub next_probe_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy)]
pub struct CircuitConfig {
    pub failure_threshold: u32,
    pub open_for: Duration,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            open_for: Duration::seconds(30),
        }
    }
}

#[derive(Debug)]
pub struct QueueStore {
    connection: Mutex<Connection>,
}

impl QueueStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, QueueError> {
        let connection = Connection::open(path)?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self, QueueError> {
        let connection = Connection::open_in_memory()?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), QueueError> {
        self.connection.lock().execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;

             CREATE TABLE IF NOT EXISTS api_sessions (
                 session_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL,
                 operator_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 last_seen_at TEXT NOT NULL,
                 metadata_json TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_api_sessions_tenant_seen
                 ON api_sessions (tenant_id, last_seen_at DESC);

             CREATE TABLE IF NOT EXISTS api_tasks (
                 task_id TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL REFERENCES api_sessions(session_id),
                 tenant_id TEXT NOT NULL,
                 idempotency_key TEXT NOT NULL,
                 request_fingerprint TEXT NOT NULL,
                 objective TEXT NOT NULL,
                 envelope_json TEXT NOT NULL,
                 status TEXT NOT NULL,
                 priority INTEGER NOT NULL,
                 attempts INTEGER NOT NULL,
                 max_attempts INTEGER NOT NULL,
                 next_attempt_at TEXT NOT NULL,
                 cancel_requested INTEGER NOT NULL DEFAULT 0,
                 lease_until TEXT,
                 result_json TEXT,
                 last_error TEXT,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 completed_at TEXT,
                 UNIQUE (tenant_id, idempotency_key)
             );
             CREATE INDEX IF NOT EXISTS idx_api_tasks_ready
                 ON api_tasks (status, cancel_requested, next_attempt_at, priority DESC, created_at ASC);
             CREATE INDEX IF NOT EXISTS idx_api_tasks_tenant_updated
                 ON api_tasks (tenant_id, updated_at DESC);

             CREATE TABLE IF NOT EXISTS api_circuit_breaker (
                 name TEXT PRIMARY KEY,
                 state TEXT NOT NULL,
                 failure_count INTEGER NOT NULL,
                 opened_at TEXT,
                 next_probe_at TEXT
             );",
        )?;
        Ok(())
    }

    pub fn create_session(
        &self,
        principal: Principal,
        metadata: Value,
    ) -> Result<SessionRecord, QueueError> {
        let now = Utc::now();
        let session = SessionRecord {
            session_id: Uuid::new_v4(),
            principal,
            created_at: now,
            last_seen_at: now,
            metadata,
        };
        let role = serde_json::to_string(&session.principal.role)?;
        let metadata_json = serde_json::to_string(&session.metadata)?;
        self.connection.lock().execute(
            "INSERT INTO api_sessions
             (session_id, tenant_id, operator_id, role, created_at, last_seen_at, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session.session_id.to_string(),
                session.principal.tenant_id.0,
                session.principal.operator_id.0,
                role,
                session.created_at.to_rfc3339(),
                session.last_seen_at.to_rfc3339(),
                metadata_json,
            ],
        )?;
        Ok(session)
    }

    pub fn get_session(
        &self,
        session_id: Uuid,
        tenant_id: &TenantId,
    ) -> Result<SessionRecord, QueueError> {
        let connection = self.connection.lock();
        let row = connection
            .query_row(
                "SELECT tenant_id, operator_id, role, created_at, last_seen_at, metadata_json
                 FROM api_sessions WHERE session_id = ?1 AND tenant_id = ?2",
                params![session_id.to_string(), tenant_id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| QueueError::NotFound(format!("session {session_id}")))?;
        let principal = Principal {
            tenant_id: TenantId::new(row.0)?,
            operator_id: OperatorId::new(row.1)?,
            role: serde_json::from_str(&row.2)?,
        };
        Ok(SessionRecord {
            session_id,
            principal,
            created_at: parse_datetime(&row.3)?,
            last_seen_at: parse_datetime(&row.4)?,
            metadata: serde_json::from_str(&row.5)?,
        })
    }

    pub fn touch_session(&self, session_id: Uuid, tenant_id: &TenantId) -> Result<(), QueueError> {
        let changed = self.connection.lock().execute(
            "UPDATE api_sessions SET last_seen_at = ?1
             WHERE session_id = ?2 AND tenant_id = ?3",
            params![Utc::now().to_rfc3339(), session_id.to_string(), tenant_id.0],
        )?;
        if changed == 0 {
            return Err(QueueError::NotFound(format!("session {session_id}")));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit_task(
        &self,
        session_id: Uuid,
        tenant_id: &TenantId,
        idempotency_key: &str,
        request_fingerprint: &str,
        envelope: &TaskEnvelope,
        priority: i32,
        max_attempts: u32,
    ) -> Result<(SubmitOutcome, TaskRecord), QueueError> {
        validate_key(idempotency_key, "idempotency_key", 256)?;
        validate_key(request_fingerprint, "request_fingerprint", 128)?;
        if max_attempts == 0 || max_attempts > 10 {
            return Err(QueueError::InvalidInput(
                "max_attempts deve estar entre 1 e 10".to_owned(),
            ));
        }
        if envelope.tenant_id != *tenant_id {
            return Err(QueueError::InvalidInput(
                "tenant do envelope não corresponde à sessão".to_owned(),
            ));
        }
        let now = Utc::now();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        if let Some(existing_id) = transaction
            .query_row(
                "SELECT task_id FROM api_tasks
                 WHERE tenant_id = ?1 AND idempotency_key = ?2",
                params![tenant_id.0, idempotency_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let existing = load_task(&transaction, &existing_id, tenant_id)?;
            if existing.request_fingerprint != request_fingerprint {
                return Err(QueueError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok((SubmitOutcome::Existing, existing));
        }
        let task_id = envelope.task_id.clone();
        let envelope_json = serde_json::to_string(&envelope)?;
        transaction.execute(
            "INSERT INTO api_tasks
             (task_id, session_id, tenant_id, idempotency_key, request_fingerprint,
              objective, envelope_json, status, priority, attempts, max_attempts,
              next_attempt_at, cancel_requested, lease_until, result_json, last_error,
              created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, 0, ?9, ?10, 0,
                     NULL, NULL, NULL, ?10, ?10, NULL)",
            params![
                task_id.0.to_string(),
                session_id.to_string(),
                tenant_id.0,
                idempotency_key,
                request_fingerprint,
                envelope.objective.clone(),
                envelope_json,
                priority,
                max_attempts,
                now.to_rfc3339(),
            ],
        )?;
        let record = load_task(&transaction, &task_id.0.to_string(), tenant_id)?;
        transaction.commit()?;
        Ok((SubmitOutcome::Created, record))
    }

    pub fn get_task(
        &self,
        task_id: &TaskId,
        tenant_id: &TenantId,
    ) -> Result<TaskRecord, QueueError> {
        let connection = self.connection.lock();
        load_task(&connection, &task_id.0.to_string(), tenant_id)
    }

    pub fn claim_next(
        &self,
        now: DateTime<Utc>,
        lease_for: Duration,
    ) -> Result<Option<TaskRecord>, QueueError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let candidate = transaction
            .query_row(
                "SELECT task_id, tenant_id FROM api_tasks
                 WHERE status = 'queued' AND cancel_requested = 0 AND next_attempt_at <= ?1
                 ORDER BY priority DESC, created_at ASC LIMIT 1",
                params![now.to_rfc3339()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((task_id, tenant_id)) = candidate else {
            transaction.commit()?;
            return Ok(None);
        };
        let lease_until = now + lease_for;
        transaction.execute(
            "UPDATE api_tasks SET status = 'running', attempts = attempts + 1,
             lease_until = ?1, updated_at = ?2 WHERE task_id = ?3 AND status = 'queued'",
            params![lease_until.to_rfc3339(), now.to_rfc3339(), task_id],
        )?;
        let task = load_task(&transaction, &task_id, &TenantId::new(tenant_id)?)?;
        transaction.commit()?;
        Ok(Some(task))
    }

    pub fn request_cancel(
        &self,
        task_id: &TaskId,
        tenant_id: &TenantId,
    ) -> Result<TaskRecord, QueueError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let task = load_task(&transaction, &task_id.0.to_string(), tenant_id)?;
        let now = Utc::now();
        if task.status.is_terminal() {
            transaction.commit()?;
            return Ok(task);
        }
        let (status, completed_at): (&str, Option<String>) = match task.status {
            TaskStatus::Queued => (TaskStatus::Cancelled.as_str(), Some(now.to_rfc3339())),
            TaskStatus::Running | TaskStatus::CancelRequested => {
                (TaskStatus::CancelRequested.as_str(), None)
            }
            TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled => (
                task.status.as_str(),
                task.completed_at.map(|value| value.to_rfc3339()),
            ),
        };
        transaction.execute(
            "UPDATE api_tasks SET status = ?1, cancel_requested = 1,
             updated_at = ?2, completed_at = ?3 WHERE task_id = ?4 AND tenant_id = ?5",
            params![
                status,
                now.to_rfc3339(),
                completed_at,
                task_id.0.to_string(),
                tenant_id.0,
            ],
        )?;
        let updated = load_task(&transaction, &task_id.0.to_string(), tenant_id)?;
        transaction.commit()?;
        Ok(updated)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_task(
        &self,
        task_id: &TaskId,
        tenant_id: &TenantId,
        result: Option<Value>,
        error: Option<&str>,
        retryable: bool,
        now: DateTime<Utc>,
        base_delay: Duration,
        max_delay: Duration,
    ) -> Result<FinishOutcome, QueueError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let task = load_task(&transaction, &task_id.0.to_string(), tenant_id)?;
        if task.status.is_terminal() {
            transaction.commit()?;
            return Ok(match task.status {
                TaskStatus::Succeeded => FinishOutcome::Succeeded,
                TaskStatus::Cancelled => FinishOutcome::Cancelled,
                _ => FinishOutcome::Failed,
            });
        }
        if task.cancel_requested || task.status == TaskStatus::CancelRequested {
            transaction.execute(
                "UPDATE api_tasks SET status = 'cancelled', cancel_requested = 1,
                 lease_until = NULL, updated_at = ?1, completed_at = ?1 WHERE task_id = ?2",
                params![now.to_rfc3339(), task_id.0.to_string()],
            )?;
            transaction.commit()?;
            return Ok(FinishOutcome::Cancelled);
        }
        let safe_error = error
            .as_ref()
            .map(|value| value.chars().take(4_096).collect::<String>());
        if retryable && task.attempts < task.max_attempts {
            let exponent = task.attempts.saturating_sub(1).min(10);
            let multiplier = 1_i64 << exponent;
            let delay_ms = base_delay
                .num_milliseconds()
                .saturating_mul(multiplier)
                .min(max_delay.num_milliseconds());
            let next_attempt_at = now + Duration::milliseconds(delay_ms.max(0));
            transaction.execute(
                "UPDATE api_tasks SET status = 'queued', lease_until = NULL,
                 next_attempt_at = ?1, result_json = NULL, last_error = ?2,
                 updated_at = ?3 WHERE task_id = ?4",
                params![
                    next_attempt_at.to_rfc3339(),
                    safe_error,
                    now.to_rfc3339(),
                    task_id.0.to_string(),
                ],
            )?;
            transaction.commit()?;
            return Ok(FinishOutcome::Requeued { next_attempt_at });
        }
        let status = if error.is_some() {
            TaskStatus::Failed
        } else {
            TaskStatus::Succeeded
        };
        let result_json = result
            .map(|value| serde_json::to_string(&value))
            .transpose()?;
        transaction.execute(
            "UPDATE api_tasks SET status = ?1, lease_until = NULL, result_json = ?2,
             last_error = ?3, updated_at = ?4, completed_at = ?4 WHERE task_id = ?5",
            params![
                status.as_str(),
                result_json,
                safe_error,
                now.to_rfc3339(),
                task_id.0.to_string(),
            ],
        )?;
        transaction.commit()?;
        Ok(if status == TaskStatus::Succeeded {
            FinishOutcome::Succeeded
        } else {
            FinishOutcome::Failed
        })
    }

    pub fn recover_expired_leases(&self, now: DateTime<Utc>) -> Result<u64, QueueError> {
        let changed = self.connection.lock().execute(
            "UPDATE api_tasks SET status = CASE WHEN cancel_requested = 1 THEN 'cancelled' ELSE 'queued' END,
             lease_until = NULL, completed_at = CASE WHEN cancel_requested = 1 THEN ?1 ELSE completed_at END,
             updated_at = ?1 WHERE status IN ('running', 'cancel_requested')
             AND lease_until IS NOT NULL AND lease_until < ?1",
            params![now.to_rfc3339()],
        )?;
        Ok(u64::try_from(changed).unwrap_or(u64::MAX))
    }

    pub fn queued_count(&self) -> Result<u64, QueueError> {
        let count = self.connection.lock().query_row(
            "SELECT COUNT(*) FROM api_tasks WHERE status IN ('queued', 'running', 'cancel_requested')",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    pub fn load_circuit(&self, name: &str) -> Result<CircuitSnapshot, QueueError> {
        validate_key(name, "circuit name", 128)?;
        let connection = self.connection.lock();
        let row = connection
            .query_row(
                "SELECT state, failure_count, opened_at, next_probe_at
                 FROM api_circuit_breaker WHERE name = ?1",
                params![name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((state, failure_count, opened_at, next_probe_at)) = row else {
            let snapshot = CircuitSnapshot {
                name: name.to_owned(),
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at: None,
                next_probe_at: None,
            };
            connection.execute(
                "INSERT INTO api_circuit_breaker (name, state, failure_count, opened_at, next_probe_at)
                 VALUES (?1, 'closed', 0, NULL, NULL)",
                params![name],
            )?;
            return Ok(snapshot);
        };
        Ok(CircuitSnapshot {
            name: name.to_owned(),
            state: CircuitState::parse(&state)?,
            failure_count: u32::try_from(failure_count).unwrap_or(u32::MAX),
            opened_at: opened_at.as_deref().map(parse_datetime).transpose()?,
            next_probe_at: next_probe_at.as_deref().map(parse_datetime).transpose()?,
        })
    }

    fn save_circuit(&self, snapshot: &CircuitSnapshot) -> Result<(), QueueError> {
        self.connection.lock().execute(
            "INSERT INTO api_circuit_breaker (name, state, failure_count, opened_at, next_probe_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name) DO UPDATE SET state = excluded.state,
             failure_count = excluded.failure_count, opened_at = excluded.opened_at,
             next_probe_at = excluded.next_probe_at",
            params![
                snapshot.name,
                snapshot.state.as_str(),
                snapshot.failure_count,
                snapshot.opened_at.map(|value| value.to_rfc3339()),
                snapshot.next_probe_at.map(|value| value.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn circuit_breaker(
        self: &Arc<Self>,
        name: impl Into<String>,
        config: CircuitConfig,
    ) -> Result<CircuitBreaker, QueueError> {
        let name = name.into();
        let snapshot = self.load_circuit(&name)?;
        Ok(CircuitBreaker {
            store: Arc::clone(self),
            config,
            snapshot: Mutex::new(snapshot),
        })
    }
}

#[derive(Debug)]
pub struct CircuitBreaker {
    store: Arc<QueueStore>,
    config: CircuitConfig,
    snapshot: Mutex<CircuitSnapshot>,
}

impl CircuitBreaker {
    pub fn allow(&self, now: DateTime<Utc>) -> Result<bool, QueueError> {
        let mut snapshot = self.snapshot.lock();
        match snapshot.state {
            CircuitState::Closed => Ok(true),
            CircuitState::Open => {
                if snapshot.next_probe_at.is_some_and(|probe| now >= probe) {
                    snapshot.state = CircuitState::HalfOpen;
                    snapshot.next_probe_at = None;
                    self.store.save_circuit(&snapshot)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            CircuitState::HalfOpen => Ok(false),
        }
    }

    pub fn record_success(&self) -> Result<(), QueueError> {
        let mut snapshot = self.snapshot.lock();
        snapshot.state = CircuitState::Closed;
        snapshot.failure_count = 0;
        snapshot.opened_at = None;
        snapshot.next_probe_at = None;
        self.store.save_circuit(&snapshot)
    }

    pub fn record_failure(&self, now: DateTime<Utc>) -> Result<(), QueueError> {
        let mut snapshot = self.snapshot.lock();
        snapshot.failure_count = snapshot.failure_count.saturating_add(1);
        if snapshot.state == CircuitState::HalfOpen
            || snapshot.failure_count >= self.config.failure_threshold
        {
            snapshot.state = CircuitState::Open;
            snapshot.opened_at = Some(now);
            snapshot.next_probe_at = Some(now + self.config.open_for);
        }
        self.store.save_circuit(&snapshot)
    }

    #[must_use]
    pub fn snapshot(&self) -> CircuitSnapshot {
        self.snapshot.lock().clone()
    }
}

fn validate_key(value: &str, field: &str, max_len: usize) -> Result<(), QueueError> {
    if value.trim().is_empty() || value.len() > max_len {
        return Err(QueueError::InvalidInput(format!(
            "{field} vazio ou maior que {max_len} caracteres"
        )));
    }
    Ok(())
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>, QueueError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Utc))
        .map_err(|error| QueueError::InvalidInput(format!("timestamp inválido: {error}")))
}

fn load_task(
    connection: &Connection,
    task_id: &str,
    tenant_id: &TenantId,
) -> Result<TaskRecord, QueueError> {
    let row = connection
        .query_row(
            "SELECT session_id, idempotency_key, request_fingerprint, envelope_json,
                    status, priority, attempts, max_attempts, next_attempt_at,
                    cancel_requested, lease_until, result_json, last_error,
                    created_at, updated_at, completed_at
             FROM api_tasks WHERE task_id = ?1 AND tenant_id = ?2",
            params![task_id, tenant_id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| QueueError::NotFound(format!("task {task_id}")))?;
    let task_id = Uuid::parse_str(task_id)
        .map_err(|error| QueueError::InvalidInput(format!("task_id inválido: {error}")))?;
    let session_id = Uuid::parse_str(&row.0)
        .map_err(|error| QueueError::InvalidInput(format!("session_id inválido: {error}")))?;
    Ok(TaskRecord {
        task_id: TaskId(task_id),
        session_id,
        tenant_id: tenant_id.clone(),
        idempotency_key: row.1,
        request_fingerprint: row.2,
        envelope: serde_json::from_str(&row.3)?,
        status: TaskStatus::parse(&row.4)?,
        priority: i32::try_from(row.5).unwrap_or(i32::MAX),
        attempts: u32::try_from(row.6).unwrap_or(u32::MAX),
        max_attempts: u32::try_from(row.7).unwrap_or(u32::MAX),
        next_attempt_at: parse_datetime(&row.8)?,
        cancel_requested: row.9 != 0,
        lease_until: row.10.as_deref().map(parse_datetime).transpose()?,
        result: row.11.as_deref().map(serde_json::from_str).transpose()?,
        last_error: row.12,
        created_at: parse_datetime(&row.13)?,
        updated_at: parse_datetime(&row.14)?,
        completed_at: row.15.as_deref().map(parse_datetime).transpose()?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use shaka_core::{ExecutionBudget, Role};

    fn principal() -> Principal {
        Principal {
            operator_id: OperatorId::new("operator").unwrap(),
            tenant_id: TenantId::new("tenant").unwrap(),
            role: Role::Administrator,
        }
    }

    fn envelope(principal: &Principal) -> TaskEnvelope {
        TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "objetivo de teste",
        )
        .unwrap()
    }

    #[test]
    fn session_and_task_are_persistent_and_idempotent() {
        let store = QueueStore::in_memory().unwrap();
        let principal = principal();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        let task_envelope = envelope(&principal);
        let (first, task) = store
            .submit_task(
                session.session_id,
                &principal.tenant_id,
                "idem-1",
                "fp-1",
                &task_envelope,
                5,
                3,
            )
            .unwrap();
        assert_eq!(first, SubmitOutcome::Created);
        let (second, existing) = store
            .submit_task(
                session.session_id,
                &principal.tenant_id,
                "idem-1",
                "fp-1",
                &envelope(&principal),
                1,
                3,
            )
            .unwrap();
        assert_eq!(second, SubmitOutcome::Existing);
        assert_eq!(task.task_id, existing.task_id);
        assert!(
            store
                .submit_task(
                    session.session_id,
                    &principal.tenant_id,
                    "idem-1",
                    "different",
                    &envelope(&principal),
                    1,
                    3,
                )
                .is_err()
        );
    }

    #[test]
    fn priority_and_cancel_transitions_are_atomic() {
        let store = QueueStore::in_memory().unwrap();
        let principal = principal();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        let low = envelope(&principal);
        let low_id = low.task_id.clone();
        store
            .submit_task(
                session.session_id,
                &principal.tenant_id,
                "low",
                "low",
                &low,
                1,
                3,
            )
            .unwrap();
        let high = envelope(&principal);
        store
            .submit_task(
                session.session_id,
                &principal.tenant_id,
                "high",
                "high",
                &high,
                10,
                3,
            )
            .unwrap();
        let claimed = store
            .claim_next(Utc::now(), Duration::seconds(30))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.priority, 10);
        let cancelled = store.request_cancel(&low_id, &principal.tenant_id).unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert!(
            store
                .claim_next(Utc::now(), Duration::seconds(30))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn retries_are_bounded_and_backed_off() {
        let store = QueueStore::in_memory().unwrap();
        let principal = principal();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        let task = envelope(&principal);
        let task_id = task.task_id.clone();
        store
            .submit_task(
                session.session_id,
                &principal.tenant_id,
                "retry",
                "retry",
                &task,
                0,
                2,
            )
            .unwrap();
        let claimed = store
            .claim_next(Utc::now(), Duration::seconds(30))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.attempts, 1);
        let now = Utc::now();
        let outcome = store
            .finish_task(
                &task_id,
                &principal.tenant_id,
                None,
                Some("temporário"),
                true,
                now,
                Duration::milliseconds(10),
                Duration::seconds(1),
            )
            .unwrap();
        assert!(matches!(outcome, FinishOutcome::Requeued { .. }));
        let task_after_retry = store.get_task(&task_id, &principal.tenant_id).unwrap();
        assert_eq!(task_after_retry.status, TaskStatus::Queued);
        assert_eq!(task_after_retry.attempts, 1);
    }

    #[test]
    fn circuit_breaker_opens_and_recovers() {
        let store = Arc::new(QueueStore::in_memory().unwrap());
        let breaker = store
            .circuit_breaker(
                "model",
                CircuitConfig {
                    failure_threshold: 2,
                    open_for: Duration::milliseconds(10),
                },
            )
            .unwrap();
        let now = Utc::now();
        assert!(breaker.allow(now).unwrap());
        breaker.record_failure(now).unwrap();
        assert!(breaker.allow(now).unwrap());
        breaker.record_failure(now).unwrap();
        assert!(!breaker.allow(now).unwrap());
        assert!(breaker.allow(now + Duration::milliseconds(11)).unwrap());
        breaker.record_success().unwrap();
        assert!(breaker.allow(Utc::now()).unwrap());
    }

    #[test]
    fn task_envelope_budget_survives_round_trip() {
        let store = QueueStore::in_memory().unwrap();
        let principal = principal();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        let mut task = envelope(&principal);
        task.budget = ExecutionBudget {
            max_steps: 4,
            max_tool_calls: 2,
            max_elapsed_ms: 1000,
            max_cost_microunits: 500,
        };
        let task_id = task.task_id.clone();
        store
            .submit_task(
                session.session_id,
                &principal.tenant_id,
                "budget",
                "budget",
                &task,
                0,
                1,
            )
            .unwrap();
        let loaded = store.get_task(&task_id, &principal.tenant_id).unwrap();
        assert_eq!(loaded.envelope.budget.max_steps, 4);
        assert_eq!(loaded.envelope.budget.max_cost_microunits, 500);
    }
}
