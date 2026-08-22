//! Persistência e coordenação da fila de tarefas da API v0.5.0.
//!
//! O crate mantém as operações de fila em SQLite com transições explícitas,
//! idempotência por tenant e leases recuperáveis após reinicialização.

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use shaka_core::{OperatorId, PlanId, PlanStepId, Principal, Role, TaskEnvelope, TaskId, TenantId};
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

pub mod plan_store;

pub use plan_store::{
    PersistedPlan, PlanApprovalOutcome, PlanCheckpoint, PlanCheckpointPhase, PlanCheckpointStatus,
    PlanClaimContext, PlanInspectionIssue, PlanInspectionReport, PlanInspectionStatus,
    PlanResolutionDecision, PlanResolutionOutcome, PlanResumeReport, PlanResumeStatus,
    PlanStoreTransition, PlanTaskReference, PlanTransitionEntity, PlanTransitionState,
};

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
    #[error("não autorizado")]
    Unauthorized,
    #[error("operação proibida")]
    Forbidden,
    #[error("quota excedida: {0}")]
    QuotaExceeded(String),
    #[error("rate limit excedido; tente novamente em {retry_after_seconds}s")]
    RateLimited { retry_after_seconds: u64 },
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
    /// Plano imutável associado à task, quando o modo planejado está ativo.
    pub plan_id: Option<PlanId>,
    /// Revisão do plano associado à task.
    pub plan_revision: Option<u32>,
    /// Digest SHA-256 da revisão do plano associado à task.
    pub plan_digest: Option<String>,
    /// Etapa atualmente locada pelo worker, quando a task é planejada.
    pub plan_step_id: Option<PlanStepId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubmitOutcome {
    Created,
    Existing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FinishOutcome {
    Succeeded,
    /// A etapa planejada terminou e a task será reencaminhada para a próxima etapa.
    PlanStepSucceeded {
        next_attempt_at: DateTime<Utc>,
    },
    /// A compensação declarada terminou; a operação original não é reportada como sucesso.
    Compensated,
    Requeued {
        next_attempt_at: DateTime<Utc>,
    },
    Failed,
    Cancelled,
}

/// Origem de uma autenticação resolvida pelo host.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    Token,
    StaticApiKey,
}

/// Principal autenticado, sem expor o segredo bearer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthenticatedPrincipal {
    pub principal: Principal,
    pub token_id: String,
    pub token_prefix: String,
    pub source: AuthSource,
}

/// Resultado da emissão de um token. O campo `token` deve ser exibido uma única vez.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenIssue {
    pub token_id: String,
    pub token: String,
    pub token_prefix: String,
    pub principal: Principal,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Limites persistentes de um tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantLimits {
    pub tenant_id: TenantId,
    pub max_active_tasks: u32,
    pub max_daily_tasks: u32,
    pub max_daily_cost_microunits: u64,
    pub requests_per_window: u32,
    pub window_seconds: u32,
}

/// Registro administrativo resumido de um tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantRecord {
    pub tenant_id: TenantId,
    pub display_name: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub limits: TenantLimits,
}

/// Registro administrativo resumido de um usuário.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRecord {
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
    pub role: Role,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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

    #[allow(clippy::too_many_lines)]
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
                 plan_id TEXT,
                 plan_revision INTEGER,
                 plan_digest TEXT,
                 plan_step_id TEXT,
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
             );
             CREATE TABLE IF NOT EXISTS api_tenants (
                 tenant_id TEXT PRIMARY KEY,
                 display_name TEXT NOT NULL,
                 active INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS api_users (
                 operator_id TEXT PRIMARY KEY,
                 tenant_id TEXT NOT NULL REFERENCES api_tenants(tenant_id),
                 role TEXT NOT NULL,
                 active INTEGER NOT NULL DEFAULT 1,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_api_users_tenant
                 ON api_users (tenant_id, active);
             CREATE TABLE IF NOT EXISTS api_tokens (
                 token_id TEXT PRIMARY KEY,
                 token_hash TEXT NOT NULL UNIQUE,
                 token_prefix TEXT NOT NULL,
                 operator_id TEXT NOT NULL REFERENCES api_users(operator_id),
                 created_at TEXT NOT NULL,
                 expires_at TEXT,
                 revoked_at TEXT,
                 last_used_at TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_api_tokens_operator
                 ON api_tokens (operator_id, revoked_at);
             CREATE TABLE IF NOT EXISTS api_tenant_limits (
                 tenant_id TEXT PRIMARY KEY REFERENCES api_tenants(tenant_id),
                 max_active_tasks INTEGER NOT NULL,
                 max_daily_tasks INTEGER NOT NULL,
                 max_daily_cost_microunits INTEGER NOT NULL,
                 requests_per_window INTEGER NOT NULL,
                 window_seconds INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS api_rate_windows (
                 scope_key TEXT NOT NULL,
                 window_start TEXT NOT NULL,
                 request_count INTEGER NOT NULL,
                 PRIMARY KEY(scope_key, window_start)
             );
             CREATE INDEX IF NOT EXISTS idx_api_rate_windows_start
                 ON api_rate_windows (window_start);

             CREATE TABLE IF NOT EXISTS shaka_schema_versions (
                 component TEXT PRIMARY KEY,
                 version INTEGER NOT NULL
             );
             INSERT OR IGNORE INTO shaka_schema_versions (component, version)
                 VALUES ('plan_store', 3);

             CREATE TABLE IF NOT EXISTS plans (
                 plan_id TEXT NOT NULL,
                 tenant_id TEXT NOT NULL,
                 task_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 plan_json TEXT NOT NULL,
                 state TEXT NOT NULL,
                 mode TEXT NOT NULL,
                 risk TEXT NOT NULL,
                 digest TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY (plan_id, revision)
             );
             CREATE INDEX IF NOT EXISTS idx_plans_tenant_updated
                 ON plans (tenant_id, updated_at DESC);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_plans_tenant_digest
                 ON plans (tenant_id, plan_id, revision, digest);

             CREATE TABLE IF NOT EXISTS plan_steps (
                 plan_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 step_id TEXT NOT NULL,
                 depends_json TEXT NOT NULL,
                 action_json TEXT NOT NULL,
                 preconditions_json TEXT NOT NULL,
                 postconditions_json TEXT NOT NULL,
                 state TEXT NOT NULL,
                 attempts INTEGER NOT NULL DEFAULT 0,
                 max_attempts INTEGER NOT NULL,
                 compensation_step_id TEXT,
                 PRIMARY KEY (plan_id, revision, step_id),
                 FOREIGN KEY (plan_id, revision) REFERENCES plans(plan_id, revision)
             );

             CREATE TABLE IF NOT EXISTS plan_checkpoints (
                 plan_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 sequence INTEGER NOT NULL,
                 step_id TEXT,
                 phase TEXT NOT NULL,
                 status TEXT NOT NULL,
                 state_digest TEXT,
                 created_at TEXT NOT NULL,
                 PRIMARY KEY (plan_id, revision, sequence),
                 FOREIGN KEY (plan_id, revision) REFERENCES plans(plan_id, revision)
             );

             CREATE TABLE IF NOT EXISTS plan_approvals (
                 approval_id TEXT PRIMARY KEY,
                 plan_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 tenant_id TEXT NOT NULL,
                 step_id TEXT,
                 approval_json TEXT NOT NULL,
                 revoked INTEGER NOT NULL DEFAULT 0,
                 expires_at TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 idempotency_key TEXT,
                 FOREIGN KEY (plan_id, revision) REFERENCES plans(plan_id, revision)
             );
             CREATE INDEX IF NOT EXISTS idx_plan_approvals_scope
                 ON plan_approvals (plan_id, revision, step_id, revoked, expires_at);

             CREATE TABLE IF NOT EXISTS plan_transitions (
                 transition_id TEXT PRIMARY KEY,
                 plan_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 sequence INTEGER NOT NULL,
                 entity TEXT NOT NULL,
                 entity_id TEXT,
                 transition_json TEXT NOT NULL,
                 idempotency_key TEXT NOT NULL,
                 previous_hash TEXT,
                 event_hash TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE (plan_id, revision, idempotency_key),
                 UNIQUE (plan_id, revision, sequence),
                 FOREIGN KEY (plan_id, revision) REFERENCES plans(plan_id, revision)
             );
             CREATE INDEX IF NOT EXISTS idx_plan_transitions_scope
                 ON plan_transitions (plan_id, revision, sequence);

             CREATE TABLE IF NOT EXISTS plan_compensations (
                 plan_id TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 step_id TEXT NOT NULL,
                 compensation_step_id TEXT NOT NULL,
                 PRIMARY KEY (plan_id, revision, step_id),
                 FOREIGN KEY (plan_id, revision) REFERENCES plans(plan_id, revision)
             );",
        )?;
        self.ensure_api_task_plan_columns()?;
        self.ensure_plan_approval_columns()?;
        let schema_version = self.connection.lock().query_row(
            "SELECT version FROM shaka_schema_versions WHERE component = 'plan_store'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if schema_version > 3 {
            return Err(QueueError::InvalidInput(
                "schema plan_store mais novo que o suportado".to_owned(),
            ));
        }
        if schema_version < 3 {
            self.connection.lock().execute(
                "UPDATE shaka_schema_versions SET version = 3 WHERE component = 'plan_store'",
                [],
            )?;
        }
        Ok(())
    }

    fn ensure_plan_approval_columns(&self) -> Result<(), QueueError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare("PRAGMA table_info(plan_approvals)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if !columns.iter().any(|column| column == "idempotency_key") {
            connection.execute(
                "ALTER TABLE plan_approvals ADD COLUMN idempotency_key TEXT",
                [],
            )?;
        }
        connection.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_approvals_idempotency
             ON plan_approvals (plan_id, revision, idempotency_key)",
            [],
        )?;
        Ok(())
    }

    fn ensure_api_task_plan_columns(&self) -> Result<(), QueueError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare("PRAGMA table_info(api_tasks)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (name, definition) in [
            ("plan_id", "TEXT"),
            ("plan_revision", "INTEGER"),
            ("plan_digest", "TEXT"),
            ("plan_step_id", "TEXT"),
        ] {
            if !columns.iter().any(|column| column == name) {
                connection.execute(
                    &format!("ALTER TABLE api_tasks ADD COLUMN {name} {definition}"),
                    [],
                )?;
            }
        }
        connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_api_tasks_plan_claim
             ON api_tasks (plan_id, plan_revision, plan_step_id, status)",
            [],
        )?;
        Ok(())
    }

    /// Garante que o principal legado da configuração exista no IAM persistente.
    pub fn bootstrap_principal(&self, principal: &Principal) -> Result<(), QueueError> {
        validate_key(&principal.tenant_id.0, "tenant_id", 128)?;
        validate_key(&principal.operator_id.0, "operator_id", 128)?;
        let now = Utc::now().to_rfc3339();
        let role = serde_json::to_string(&principal.role)?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO api_tenants
             (tenant_id, display_name, active, created_at) VALUES (?1, ?2, 1, ?3)",
            params![principal.tenant_id.0, principal.tenant_id.0, now],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO api_users
             (operator_id, tenant_id, role, active, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, ?4)",
            params![principal.operator_id.0, principal.tenant_id.0, role, now],
        )?;
        ensure_limits_tx(&transaction, &principal.tenant_id, &now)?;
        transaction.commit()?;
        Ok(())
    }

    /// Cria um tenant administrativo com limites iniciais conservadores.
    pub fn create_tenant(
        &self,
        tenant_id: &TenantId,
        display_name: &str,
    ) -> Result<TenantRecord, QueueError> {
        validate_key(&tenant_id.0, "tenant_id", 128)?;
        validate_key(display_name, "display_name", 256)?;
        let now = Utc::now();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO api_tenants (tenant_id, display_name, active, created_at)
             VALUES (?1, ?2, 1, ?3)",
            params![tenant_id.0, display_name, now.to_rfc3339()],
        )?;
        ensure_limits_tx(&transaction, tenant_id, &now.to_rfc3339())?;
        transaction.commit()?;
        drop(connection);
        self.get_tenant(tenant_id)
    }

    /// Cria ou rejeita um usuário de tenant.
    pub fn create_user(
        &self,
        operator_id: &OperatorId,
        tenant_id: &TenantId,
        role: &Role,
    ) -> Result<UserRecord, QueueError> {
        validate_key(&operator_id.0, "operator_id", 128)?;
        let now = Utc::now();
        let role_json = serde_json::to_string(role)?;
        let changed = self.connection.lock().execute(
            "INSERT INTO api_users
             (operator_id, tenant_id, role, active, created_at, updated_at)
             SELECT ?1, ?2, ?3, 1, ?4, ?4
             WHERE EXISTS (SELECT 1 FROM api_tenants WHERE tenant_id = ?2 AND active = 1)",
            params![operator_id.0, tenant_id.0, role_json, now.to_rfc3339()],
        )?;
        if changed == 0 {
            return Err(QueueError::InvalidInput(
                "tenant inexistente/inativo ou usuário já existente".to_owned(),
            ));
        }
        self.get_user(operator_id)
    }

    /// Emite um token bearer opaco e devolve o segredo somente nesta chamada.
    pub fn issue_token(
        &self,
        operator_id: &OperatorId,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<TokenIssue, QueueError> {
        let user = self.get_user(operator_id)?;
        if !user.active {
            return Err(QueueError::Unauthorized);
        }
        let token_id = format!("tok_{}", Uuid::new_v4());
        let token = format!("shk_{}_{}", Uuid::new_v4(), Uuid::new_v4());
        let token_prefix = token.chars().take(12).collect::<String>();
        let token_hash = sha256_hex(&token);
        let now = Utc::now();
        self.connection.lock().execute(
            "INSERT INTO api_tokens
             (token_id, token_hash, token_prefix, operator_id, created_at, expires_at, revoked_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
            params![
                token_id,
                token_hash,
                token_prefix,
                operator_id.0,
                now.to_rfc3339(),
                expires_at.map(|value| value.to_rfc3339()),
            ],
        )?;
        Ok(TokenIssue {
            token_id,
            token,
            token_prefix,
            principal: Principal {
                operator_id: user.operator_id,
                tenant_id: user.tenant_id,
                role: user.role,
            },
            expires_at,
        })
    }

    /// Revoga um token sem revelar se o segredo original ainda existe.
    pub fn revoke_token(&self, token_id: &str) -> Result<(), QueueError> {
        let changed = self.connection.lock().execute(
            "UPDATE api_tokens SET revoked_at = ?1 WHERE token_id = ?2 AND revoked_at IS NULL",
            params![Utc::now().to_rfc3339(), token_id],
        )?;
        if changed == 0 {
            return Err(QueueError::NotFound(format!("token {token_id}")));
        }
        Ok(())
    }

    /// Resolve um bearer por hash, validando usuário, tenant, expiração e revogação.
    pub fn authenticate_token(&self, token: &str) -> Result<AuthenticatedPrincipal, QueueError> {
        if token.trim().is_empty() || token.len() > 512 {
            return Err(QueueError::Unauthorized);
        }
        let token_hash = sha256_hex(token);
        let now = Utc::now();
        let connection = self.connection.lock();
        let row = connection
            .query_row(
                "SELECT t.token_id, t.token_prefix, u.operator_id, u.tenant_id, u.role
                 FROM api_tokens t
                 JOIN api_users u ON u.operator_id = t.operator_id
                 JOIN api_tenants n ON n.tenant_id = u.tenant_id
                 WHERE t.token_hash = ?1 AND t.revoked_at IS NULL AND u.active = 1 AND n.active = 1
                   AND (t.expires_at IS NULL OR t.expires_at > ?2)",
                params![token_hash, now.to_rfc3339()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or(QueueError::Unauthorized)?;
        drop(connection);
        self.connection.lock().execute(
            "UPDATE api_tokens SET last_used_at = ?1 WHERE token_hash = ?2",
            params![now.to_rfc3339(), token_hash],
        )?;
        Ok(AuthenticatedPrincipal {
            principal: Principal {
                operator_id: OperatorId::new(row.2)?,
                tenant_id: TenantId::new(row.3)?,
                role: serde_json::from_str(&row.4)?,
            },
            token_id: row.0,
            token_prefix: row.1,
            source: AuthSource::Token,
        })
    }

    /// Informa se existe pelo menos um token persistente ativo.
    pub fn has_active_tokens(&self) -> Result<bool, QueueError> {
        let connection = self.connection.lock();
        Ok(connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM api_tokens t
                 JOIN api_users u ON u.operator_id = t.operator_id
                 JOIN api_tenants n ON n.tenant_id = u.tenant_id
                 WHERE t.revoked_at IS NULL AND u.active = 1 AND n.active = 1
                   AND (t.expires_at IS NULL OR t.expires_at > ?1)
             )",
            params![Utc::now().to_rfc3339()],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }

    /// Lista tenants e seus limites para operações administrativas locais.
    pub fn list_tenants(&self) -> Result<Vec<TenantRecord>, QueueError> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare(
            "SELECT n.tenant_id, n.display_name, n.active, n.created_at,
                    l.max_active_tasks, l.max_daily_tasks, l.max_daily_cost_microunits,
                    l.requests_per_window, l.window_seconds
             FROM api_tenants n JOIN api_tenant_limits l ON l.tenant_id = n.tenant_id
             ORDER BY n.tenant_id",
        )?;
        let rows = statement.query_map([], load_tenant_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(QueueError::from)
    }

    /// Atualiza os limites de um tenant.
    pub fn set_limits(&self, limits: TenantLimits) -> Result<TenantLimits, QueueError> {
        validate_limits(&limits)?;
        let changed = self.connection.lock().execute(
            "UPDATE api_tenant_limits SET max_active_tasks = ?1, max_daily_tasks = ?2,
                    max_daily_cost_microunits = ?3, requests_per_window = ?4, window_seconds = ?5
             WHERE tenant_id = ?6",
            params![
                limits.max_active_tasks,
                limits.max_daily_tasks,
                limits.max_daily_cost_microunits,
                limits.requests_per_window,
                limits.window_seconds,
                limits.tenant_id.0,
            ],
        )?;
        if changed == 0 {
            return Err(QueueError::NotFound(format!(
                "tenant {}",
                limits.tenant_id.0
            )));
        }
        Ok(limits)
    }

    /// Obtém os limites de um tenant.
    pub fn get_limits(&self, tenant_id: &TenantId) -> Result<TenantLimits, QueueError> {
        let connection = self.connection.lock();
        connection
            .query_row(
                "SELECT max_active_tasks, max_daily_tasks, max_daily_cost_microunits,
                        requests_per_window, window_seconds
                 FROM api_tenant_limits WHERE tenant_id = ?1",
                params![tenant_id.0],
                |row| {
                    Ok(TenantLimits {
                        tenant_id: tenant_id.clone(),
                        max_active_tasks: row.get(0)?,
                        max_daily_tasks: row.get(1)?,
                        max_daily_cost_microunits: row.get(2)?,
                        requests_per_window: row.get(3)?,
                        window_seconds: row.get(4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| QueueError::NotFound(format!("tenant {}", tenant_id.0)))
    }

    fn get_tenant(&self, tenant_id: &TenantId) -> Result<TenantRecord, QueueError> {
        let connection = self.connection.lock();
        connection
            .query_row(
                "SELECT n.tenant_id, n.display_name, n.active, n.created_at,
                        l.max_active_tasks, l.max_daily_tasks, l.max_daily_cost_microunits,
                        l.requests_per_window, l.window_seconds
                 FROM api_tenants n JOIN api_tenant_limits l ON l.tenant_id = n.tenant_id
                 WHERE n.tenant_id = ?1",
                params![tenant_id.0],
                load_tenant_row,
            )
            .optional()?
            .ok_or_else(|| QueueError::NotFound(format!("tenant {}", tenant_id.0)))
    }

    fn get_user(&self, operator_id: &OperatorId) -> Result<UserRecord, QueueError> {
        let connection = self.connection.lock();
        let row = connection
            .query_row(
                "SELECT operator_id, tenant_id, role, active, created_at, updated_at
                 FROM api_users WHERE operator_id = ?1",
                params![operator_id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| QueueError::NotFound(format!("user {}", operator_id.0)))?;
        Ok(UserRecord {
            operator_id: OperatorId::new(row.0)?,
            tenant_id: TenantId::new(row.1)?,
            role: serde_json::from_str(&row.2)?,
            active: row.3 != 0,
            created_at: parse_datetime(&row.4)?,
            updated_at: parse_datetime(&row.5)?,
        })
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

    /// Submete uma tarefa com autorização contextual, rate limit e quotas na mesma transação.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn submit_task_governed(
        &self,
        session_id: Uuid,
        principal: &Principal,
        idempotency_key: &str,
        request_fingerprint: &str,
        envelope: &TaskEnvelope,
        priority: i32,
        max_attempts: u32,
    ) -> Result<(SubmitOutcome, TaskRecord), QueueError> {
        self.submit_task_governed_with_plan(
            session_id,
            principal,
            idempotency_key,
            request_fingerprint,
            envelope,
            priority,
            max_attempts,
            None,
        )
    }

    /// Submete uma task governada e, quando informado, valida sua revisão de plano na admissão.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn submit_task_governed_with_plan(
        &self,
        session_id: Uuid,
        principal: &Principal,
        idempotency_key: &str,
        request_fingerprint: &str,
        envelope: &TaskEnvelope,
        priority: i32,
        max_attempts: u32,
        plan_reference: Option<&PlanTaskReference>,
    ) -> Result<(SubmitOutcome, TaskRecord), QueueError> {
        validate_key(idempotency_key, "idempotency_key", 256)?;
        validate_key(request_fingerprint, "request_fingerprint", 128)?;
        if max_attempts == 0 || max_attempts > 10 {
            return Err(QueueError::InvalidInput(
                "max_attempts deve estar entre 1 e 10".to_owned(),
            ));
        }
        if envelope.tenant_id != principal.tenant_id
            || envelope.operator_id != principal.operator_id
        {
            return Err(QueueError::Forbidden);
        }
        let now = Utc::now();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let session_operator = transaction
            .query_row(
                "SELECT operator_id FROM api_sessions
                 WHERE session_id = ?1 AND tenant_id = ?2",
                params![session_id.to_string(), principal.tenant_id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| QueueError::NotFound(format!("session {session_id}")))?;
        if session_operator != principal.operator_id.0 {
            return Err(QueueError::Forbidden);
        }
        if let Some(reference) = plan_reference {
            plan_store::verify_plan_admission_tx(&transaction, principal, envelope, reference)?;
        }
        let limits = load_limits_tx(&transaction, &principal.tenant_id)?;
        let window_start = fixed_window_start(now, limits.window_seconds);
        for scope in [
            format!("tenant:{}:submit", principal.tenant_id.0),
            format!("operator:{}:submit", principal.operator_id.0),
        ] {
            let count = transaction
                .query_row(
                    "SELECT request_count FROM api_rate_windows
                     WHERE scope_key = ?1 AND window_start = ?2",
                    params![scope, window_start.to_rfc3339()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            if count >= i64::from(limits.requests_per_window) {
                let retry_after = u64::try_from(
                    (window_start + Duration::seconds(i64::from(limits.window_seconds)) - now)
                        .num_seconds()
                        .max(1),
                )
                .unwrap_or(1);
                return Err(QueueError::RateLimited {
                    retry_after_seconds: retry_after,
                });
            }
        }
        for scope in [
            format!("tenant:{}:submit", principal.tenant_id.0),
            format!("operator:{}:submit", principal.operator_id.0),
        ] {
            transaction.execute(
                "INSERT INTO api_rate_windows (scope_key, window_start, request_count)
                 VALUES (?1, ?2, 1)
                 ON CONFLICT(scope_key, window_start)
                 DO UPDATE SET request_count = request_count + 1",
                params![scope, window_start.to_rfc3339()],
            )?;
        }
        if let Some(existing_id) = transaction
            .query_row(
                "SELECT task_id FROM api_tasks
                 WHERE tenant_id = ?1 AND idempotency_key = ?2",
                params![principal.tenant_id.0, idempotency_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let existing = load_task(&transaction, &existing_id, &principal.tenant_id)?;
            if existing.request_fingerprint != request_fingerprint
                || !same_plan_reference(
                    existing.plan_id.as_ref(),
                    existing.plan_revision,
                    existing.plan_digest.as_deref(),
                    plan_reference,
                )
            {
                return Err(QueueError::IdempotencyConflict);
            }
            transaction.commit()?;
            return Ok((SubmitOutcome::Existing, existing));
        }
        let active_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM api_tasks
             WHERE tenant_id = ?1 AND status IN ('queued', 'running', 'cancel_requested')",
            params![principal.tenant_id.0],
            |row| row.get(0),
        )?;
        if active_count >= i64::from(limits.max_active_tasks) {
            return Err(QueueError::QuotaExceeded("max_active_tasks".to_owned()));
        }
        let day_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap_or(now.naive_utc());
        let day_start = DateTime::<Utc>::from_naive_utc_and_offset(day_start, Utc);
        let daily_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM api_tasks WHERE tenant_id = ?1 AND created_at >= ?2",
            params![principal.tenant_id.0, day_start.to_rfc3339()],
            |row| row.get(0),
        )?;
        if daily_count >= i64::from(limits.max_daily_tasks) {
            return Err(QueueError::QuotaExceeded("max_daily_tasks".to_owned()));
        }
        let mut daily_cost = 0_u64;
        let mut statement = transaction.prepare(
            "SELECT envelope_json FROM api_tasks WHERE tenant_id = ?1 AND created_at >= ?2",
        )?;
        let rows = statement.query_map(
            params![principal.tenant_id.0, day_start.to_rfc3339()],
            |row| row.get::<_, String>(0),
        )?;
        for row in rows {
            let json = row?;
            let stored: TaskEnvelope = serde_json::from_str(&json)?;
            daily_cost = daily_cost.saturating_add(stored.budget.max_cost_microunits);
        }
        drop(statement);
        if daily_cost.saturating_add(envelope.budget.max_cost_microunits)
            > limits.max_daily_cost_microunits
        {
            return Err(QueueError::QuotaExceeded(
                "max_daily_cost_microunits".to_owned(),
            ));
        }
        if let Some(reference) = plan_reference {
            plan_store::record_plan_admission_tx(&transaction, principal, envelope, reference)?;
        }
        let task_id = envelope.task_id.clone();
        let envelope_json = serde_json::to_string(envelope)?;
        transaction.execute(
            "INSERT INTO api_tasks
             (task_id, session_id, tenant_id, idempotency_key, request_fingerprint,
              objective, envelope_json, status, priority, attempts, max_attempts,
              next_attempt_at, cancel_requested, lease_until, result_json, last_error,
              created_at, updated_at, completed_at, plan_id, plan_revision, plan_digest, plan_step_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, 0, ?9, ?10, 0,
                     NULL, NULL, NULL, ?10, ?10, NULL, ?11, ?12, ?13, NULL)",
            params![
                task_id.0.to_string(),
                session_id.to_string(),
                principal.tenant_id.0,
                idempotency_key,
                request_fingerprint,
                envelope.objective.clone(),
                envelope_json,
                priority,
                max_attempts,
                now.to_rfc3339(),
                plan_reference.map(|reference| reference.plan_id.0.to_string()),
                plan_reference.map(|reference| reference.revision),
                plan_reference.map(|reference| reference.digest.clone()),
            ],
        )?;
        let record = load_task(&transaction, &task_id.0.to_string(), &principal.tenant_id)?;
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
        self.claim_next_with_plan_context(now, lease_for, &PlanClaimContext::default())
    }

    /// Faz claim de uma task direta ou planejada usando facts host-side do worker.
    pub fn claim_next_with_plan_context(
        &self,
        now: DateTime<Utc>,
        lease_for: Duration,
        plan_context: &PlanClaimContext,
    ) -> Result<Option<TaskRecord>, QueueError> {
        if lease_for <= Duration::zero() {
            return Err(QueueError::InvalidInput(
                "lease_for deve ser maior que zero".to_owned(),
            ));
        }
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let mut statement = transaction.prepare(
            "SELECT task_id, tenant_id FROM api_tasks
             WHERE status = 'queued' AND cancel_requested = 0 AND next_attempt_at <= ?1
             ORDER BY priority DESC, created_at ASC LIMIT 64",
        )?;
        let candidates = statement
            .query_map(params![now.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let lease_until = now + lease_for;
        for (task_id, tenant_id_raw) in candidates {
            let tenant_id = TenantId::new(tenant_id_raw)?;
            let task = load_task(&transaction, &task_id, &tenant_id)?;
            let selected_step = if task.plan_id.is_some() {
                let reference = plan_store::task_reference(&task)?;
                match plan_store::prepare_planned_claim_tx(
                    &transaction,
                    &task,
                    &reference,
                    plan_context,
                    now,
                )? {
                    Some(step_id) => Some(step_id),
                    None => continue,
                }
            } else {
                None
            };
            transaction.execute(
                "UPDATE api_tasks SET status = 'running', attempts = attempts + 1,
                 lease_until = ?1, plan_step_id = ?2, updated_at = ?3
                 WHERE task_id = ?4 AND tenant_id = ?5 AND status = 'queued'",
                params![
                    lease_until.to_rfc3339(),
                    selected_step.as_ref().map(|step_id| step_id.0.clone()),
                    now.to_rfc3339(),
                    task_id,
                    tenant_id.0,
                ],
            )?;
            let claimed = load_task(&transaction, &task_id, &tenant_id)?;
            transaction.commit()?;
            return Ok(Some(claimed));
        }
        transaction.commit()?;
        Ok(None)
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
        if task.plan_id.is_some() {
            plan_store::cancel_planned_task_tx(&transaction, &task, now)?;
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
        self.finish_task_with_plan_context(
            task_id,
            tenant_id,
            result,
            error,
            retryable,
            now,
            base_delay,
            max_delay,
            &PlanClaimContext::default(),
        )
    }

    /// Finaliza uma task usando facts host-side para as pós-condições do plano.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_task_with_plan_context(
        &self,
        task_id: &TaskId,
        tenant_id: &TenantId,
        result: Option<Value>,
        error: Option<&str>,
        retryable: bool,
        now: DateTime<Utc>,
        base_delay: Duration,
        max_delay: Duration,
        plan_context: &PlanClaimContext,
    ) -> Result<FinishOutcome, QueueError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let task = load_task(&transaction, &task_id.0.to_string(), tenant_id)?;
        if task.plan_id.is_some() {
            let outcome = plan_store::finish_planned_step_tx(
                &transaction,
                &task,
                result,
                error,
                retryable,
                now,
                base_delay,
                max_delay,
                plan_context,
            )?;
            transaction.commit()?;
            return Ok(outcome);
        }
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
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let mut statement = transaction.prepare(
            "SELECT task_id, tenant_id FROM api_tasks
             WHERE status IN ('running', 'cancel_requested')
               AND lease_until IS NOT NULL AND lease_until < ?1
             ORDER BY updated_at ASC LIMIT 128",
        )?;
        let candidates = statement
            .query_map(params![now.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut recovered = 0_u64;
        for (task_id, tenant_id_raw) in candidates {
            let tenant_id = TenantId::new(tenant_id_raw)?;
            let task = load_task(&transaction, &task_id, &tenant_id)?;
            if task.plan_id.is_some() {
                plan_store::recover_expired_plan_lease_tx(&transaction, &task, now)?;
            } else {
                transaction.execute(
                    "UPDATE api_tasks SET status = CASE WHEN cancel_requested = 1 THEN 'cancelled' ELSE 'queued' END,
                     lease_until = NULL,
                     completed_at = CASE WHEN cancel_requested = 1 THEN ?1 ELSE completed_at END,
                     updated_at = ?1 WHERE task_id = ?2 AND tenant_id = ?3",
                    params![now.to_rfc3339(), task_id, tenant_id.0],
                )?;
            }
            recovered = recovered.saturating_add(1);
        }
        transaction.commit()?;
        Ok(recovered)
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

fn same_plan_reference(
    plan_id: Option<&PlanId>,
    revision: Option<u32>,
    digest: Option<&str>,
    reference: Option<&PlanTaskReference>,
) -> bool {
    match (plan_id, revision, digest, reference) {
        (None, None, None, None) => true,
        (Some(plan_id), Some(revision), Some(digest), Some(reference)) => {
            plan_id == &reference.plan_id
                && revision == reference.revision
                && digest == reference.digest
        }
        _ => false,
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

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn default_limits(tenant_id: &TenantId) -> TenantLimits {
    TenantLimits {
        tenant_id: tenant_id.clone(),
        max_active_tasks: 32,
        max_daily_tasks: 1_000,
        max_daily_cost_microunits: 10_000_000,
        requests_per_window: 120,
        window_seconds: 60,
    }
}

fn validate_limits(limits: &TenantLimits) -> Result<(), QueueError> {
    if limits.max_active_tasks == 0 || limits.max_active_tasks > 100_000 {
        return Err(QueueError::InvalidInput(
            "max_active_tasks deve estar entre 1 e 100000".to_owned(),
        ));
    }
    if limits.max_daily_tasks == 0 || limits.max_daily_tasks > 1_000_000 {
        return Err(QueueError::InvalidInput(
            "max_daily_tasks deve estar entre 1 e 1000000".to_owned(),
        ));
    }
    if limits.max_daily_cost_microunits == 0 {
        return Err(QueueError::InvalidInput(
            "max_daily_cost_microunits deve ser positivo".to_owned(),
        ));
    }
    if limits.requests_per_window == 0 || limits.requests_per_window > 1_000_000 {
        return Err(QueueError::InvalidInput(
            "requests_per_window deve estar entre 1 e 1000000".to_owned(),
        ));
    }
    if limits.window_seconds == 0 || limits.window_seconds > 86_400 {
        return Err(QueueError::InvalidInput(
            "window_seconds deve estar entre 1 e 86400".to_owned(),
        ));
    }
    Ok(())
}

fn fixed_window_start(now: DateTime<Utc>, window_seconds: u32) -> DateTime<Utc> {
    let seconds = now.timestamp();
    let window = i64::from(window_seconds.max(1));
    let start = seconds - seconds.rem_euclid(window);
    DateTime::<Utc>::from_timestamp(start, 0).unwrap_or(now)
}

fn load_limits_tx(
    transaction: &rusqlite::Transaction<'_>,
    tenant_id: &TenantId,
) -> Result<TenantLimits, QueueError> {
    transaction
        .query_row(
            "SELECT max_active_tasks, max_daily_tasks, max_daily_cost_microunits,
                    requests_per_window, window_seconds
             FROM api_tenant_limits WHERE tenant_id = ?1",
            params![tenant_id.0],
            |row| {
                Ok(TenantLimits {
                    tenant_id: tenant_id.clone(),
                    max_active_tasks: row.get(0)?,
                    max_daily_tasks: row.get(1)?,
                    max_daily_cost_microunits: row.get(2)?,
                    requests_per_window: row.get(3)?,
                    window_seconds: row.get(4)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| QueueError::NotFound(format!("tenant {}", tenant_id.0)))
}

fn ensure_limits_tx(
    transaction: &rusqlite::Transaction<'_>,
    tenant_id: &TenantId,
    now: &str,
) -> Result<(), QueueError> {
    let limits = default_limits(tenant_id);
    transaction.execute(
        "INSERT OR IGNORE INTO api_tenant_limits
         (tenant_id, max_active_tasks, max_daily_tasks, max_daily_cost_microunits,
          requests_per_window, window_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            tenant_id.0,
            limits.max_active_tasks,
            limits.max_daily_tasks,
            limits.max_daily_cost_microunits,
            limits.requests_per_window,
            limits.window_seconds,
        ],
    )?;
    let _ = now;
    Ok(())
}

fn load_tenant_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TenantRecord> {
    let tenant_id = row.get::<_, String>(0)?;
    let display_name = row.get::<_, String>(1)?;
    let active = row.get::<_, i64>(2)? != 0;
    let created_at = row.get::<_, String>(3)?;
    let limits = TenantLimits {
        tenant_id: TenantId::new(tenant_id.clone())
            .map_err(|error| to_sql_error(QueueError::Core(error)))?,
        max_active_tasks: row.get(4)?,
        max_daily_tasks: row.get(5)?,
        max_daily_cost_microunits: row.get(6)?,
        requests_per_window: row.get(7)?,
        window_seconds: row.get(8)?,
    };
    Ok(TenantRecord {
        tenant_id: limits.tenant_id.clone(),
        display_name,
        active,
        created_at: parse_datetime(&created_at).map_err(to_sql_error)?,
        limits,
    })
}

fn to_sql_error(error: QueueError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
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
                    created_at, updated_at, completed_at,
                    plan_id, plan_revision, plan_digest, plan_step_id
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
                    row.get::<_, Option<String>>(16)?,
                    row.get::<_, Option<i64>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                    row.get::<_, Option<String>>(19)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| QueueError::NotFound(format!("task {task_id}")))?;
    let task_id = Uuid::parse_str(task_id)
        .map_err(|error| QueueError::InvalidInput(format!("task_id inválido: {error}")))?;
    let session_id = Uuid::parse_str(&row.0)
        .map_err(|error| QueueError::InvalidInput(format!("session_id inválido: {error}")))?;
    let plan_id = row
        .16
        .as_deref()
        .map(|value| {
            Uuid::parse_str(value)
                .map(PlanId)
                .map_err(|error| QueueError::InvalidInput(format!("plan_id inválido: {error}")))
        })
        .transpose()?;
    let plan_revision = row
        .17
        .map(|value| {
            u32::try_from(value).map_err(|error| {
                QueueError::InvalidInput(format!("plan_revision inválida: {error}"))
            })
        })
        .transpose()?;
    let plan_step_id = row.19.as_deref().map(PlanStepId::new).transpose()?;
    if plan_id.is_some() != plan_revision.is_some()
        || plan_id.is_some() != row.18.is_some()
        || plan_step_id.is_some() && plan_id.is_none()
    {
        return Err(QueueError::InvalidInput(
            "referência de plano parcialmente persistida".to_owned(),
        ));
    }
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
        plan_id,
        plan_revision,
        plan_digest: row.18,
        plan_step_id,
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
    fn iam_token_is_resolved_without_persisting_plaintext_in_contract() {
        let store = QueueStore::in_memory().unwrap();
        let tenant = TenantId::new("tenant-iam").unwrap();
        store.create_tenant(&tenant, "Tenant IAM").unwrap();
        let operator = OperatorId::new("user-iam").unwrap();
        store
            .create_user(&operator, &tenant, &Role::Operator)
            .unwrap();
        let issue = store.issue_token(&operator, None).unwrap();
        assert!(issue.token.starts_with("shk_"));
        let authenticated = store.authenticate_token(&issue.token).unwrap();
        assert_eq!(authenticated.principal.tenant_id, tenant);
        assert_eq!(authenticated.principal.operator_id, operator);
        assert_eq!(authenticated.source, AuthSource::Token);
        store.revoke_token(&issue.token_id).unwrap();
        assert!(matches!(
            store.authenticate_token(&issue.token),
            Err(QueueError::Unauthorized)
        ));
    }

    #[test]
    fn governed_submission_applies_quota_and_idempotency() {
        let store = QueueStore::in_memory().unwrap();
        let principal = principal();
        store.bootstrap_principal(&principal).unwrap();
        store
            .set_limits(TenantLimits {
                tenant_id: principal.tenant_id.clone(),
                max_active_tasks: 1,
                max_daily_tasks: 10,
                max_daily_cost_microunits: 2_000_000,
                requests_per_window: 10,
                window_seconds: 60,
            })
            .unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        let first_envelope = envelope(&principal);
        let (first, task) = store
            .submit_task_governed(
                session.session_id,
                &principal,
                "governed-1",
                "fp-governed-1",
                &first_envelope,
                1,
                1,
            )
            .unwrap();
        assert_eq!(first, SubmitOutcome::Created);
        let replay = store
            .submit_task_governed(
                session.session_id,
                &principal,
                "governed-1",
                "fp-governed-1",
                &first_envelope,
                1,
                1,
            )
            .unwrap();
        assert_eq!(replay.0, SubmitOutcome::Existing);
        let second = store.submit_task_governed(
            session.session_id,
            &principal,
            "governed-2",
            "fp-governed-2",
            &envelope(&principal),
            1,
            1,
        );
        assert!(matches!(second, Err(QueueError::QuotaExceeded(_))));
        assert_eq!(task.task_id, replay.1.task_id);
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
