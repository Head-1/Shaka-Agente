//! API HTTP persistente do Shaka v0.5.0.
//!
//! O servidor mantém a coordenação da fila no host, não no modelo. Cada
//! trabalho passa pelo `AgentRuntime`, pelo SQLite e pelo auditor existente.

use axum::middleware::Next;
use axum::{
    Json, Router,
    body::Body,
    extract::{MatchedPath, Path, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shaka_core::{Action, ExecutionBudget, Principal, TaskEnvelope, TaskId, TenantId};
use shaka_observability::{AuditLogger, CorrelationContext, Telemetry};
use shaka_orchestrator::{AgentRuntime, CancellationToken, OrchestratorError};
use shaka_queue::{
    AuthSource, AuthenticatedPrincipal, CircuitBreaker, CircuitConfig, CircuitSnapshot,
    FinishOutcome, QueueError, QueueStore, SessionRecord, SubmitOutcome, TaskRecord, TaskStatus,
};
use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use thiserror::Error;
use tokio::{net::TcpListener, time::sleep};
use tracing::{Instrument, error, info, warn};
use uuid::Uuid;

/// Header HTTP usado para correlação operacional, sem carregar conteúdo de negócio.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

tokio::task_local! {
    static ACTIVE_CORRELATION: CorrelationContext;
}

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub bind_addr: SocketAddr,
    pub worker_count: usize,
    pub api_key: Option<String>,
    pub live_enabled: bool,
    pub live_confirmation: bool,
    pub lease_for: Duration,
    pub retry_base_delay: Duration,
    pub retry_max_delay: Duration,
    pub circuit: CircuitConfig,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            worker_count: 2,
            api_key: None,
            live_enabled: false,
            live_confirmation: false,
            lease_for: Duration::seconds(60),
            retry_base_delay: Duration::milliseconds(250),
            retry_max_delay: Duration::seconds(30),
            circuit: CircuitConfig::default(),
        }
    }
}

impl ApiConfig {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.worker_count == 0 || self.worker_count > 32 {
            return Err(ApiError::BadRequest(
                "worker_count deve estar entre 1 e 32".to_owned(),
            ));
        }
        if self.lease_for <= Duration::zero()
            || self.retry_base_delay < Duration::zero()
            || self.retry_max_delay < self.retry_base_delay
        {
            return Err(ApiError::BadRequest(
                "política de tempo da API é inválida".to_owned(),
            ));
        }
        if !self.bind_addr.ip().is_loopback()
            && self
                .api_key
                .as_ref()
                .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ApiError::BadRequest(
                "SHAKA_API_KEY não pode ser vazia".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("não autorizado")]
    Unauthorized,
    #[error("operação proibida")]
    Forbidden,
    #[error("recurso não encontrado")]
    NotFound,
    #[error("entrada inválida: {0}")]
    BadRequest(String),
    #[error("conflito de idempotência")]
    Conflict(String),
    #[error("rate limit excedido")]
    RateLimited { retry_after_seconds: u64 },
    #[error("quota excedida: {0}")]
    QuotaExceeded(String),
    #[error("falha interna persistente")]
    Internal,
}

impl From<QueueError> for ApiError {
    fn from(error: QueueError) -> Self {
        match error {
            QueueError::NotFound(_) => Self::NotFound,
            QueueError::Unauthorized => Self::Unauthorized,
            QueueError::Forbidden => Self::Forbidden,
            QueueError::RateLimited {
                retry_after_seconds,
            } => Self::RateLimited {
                retry_after_seconds,
            },
            QueueError::QuotaExceeded(name) => Self::QuotaExceeded(name),
            QueueError::IdempotencyConflict => {
                Self::Conflict("Idempotency-Key já foi usada com outro payload".to_owned())
            }
            QueueError::InvalidIdentifier(_)
            | QueueError::InvalidInput(_)
            | QueueError::Core(_) => Self::BadRequest(error.to_string()),
            QueueError::Sqlite(_) | QueueError::Serialization(_) => {
                warn!(?error, "falha persistente na API");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::RateLimited { .. } | Self::QuotaExceeded(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let retry_after = match &self {
            Self::RateLimited {
                retry_after_seconds,
            } => Some(*retry_after_seconds),
            _ => None,
        };
        let message = match &self {
            Self::Unauthorized => "não autorizado".to_owned(),
            Self::Forbidden => "operação proibida".to_owned(),
            Self::NotFound => "recurso não encontrado".to_owned(),
            Self::BadRequest(value) | Self::Conflict(value) => value.clone(),
            Self::RateLimited { .. } => "rate limit excedido".to_owned(),
            Self::QuotaExceeded(name) => format!("quota excedida: {name}"),
            Self::Internal => "falha interna persistente".to_owned(),
        };
        let request_id = active_request_id();
        let body = Json(json!({
            "error": message,
            "request_id": request_id.clone(),
        }));
        let mut response = (status, body).into_response();
        insert_request_id_header(&mut response, &request_id);
        if let Some(seconds) = retry_after {
            if let Ok(value) = seconds.to_string().parse() {
                response.headers_mut().insert("retry-after", value);
            }
        }
        response
    }
}

#[derive(Debug, Clone)]
struct ShutdownFlag(Arc<AtomicBool>);

impl Default for ShutdownFlag {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

impl ShutdownFlag {
    fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct ApiState {
    queue: Arc<QueueStore>,
    runtime: Arc<AgentRuntime>,
    audit: Arc<AuditLogger>,
    principal: Principal,
    config: Arc<ApiConfig>,
    breaker: Arc<CircuitBreaker>,
    cancellations: Arc<parking_lot::Mutex<HashMap<Uuid, CancellationToken>>>,
    shutdown: ShutdownFlag,
    telemetry: Arc<Telemetry>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiState")
            .field("queue", &"QueueStore")
            .field("runtime", &"AgentRuntime")
            .field("audit", &"AuditLogger")
            .field("principal", &self.principal)
            .field("config", &"ApiConfig")
            .field("breaker", &self.breaker)
            .field("cancellations", &self.cancellations.lock().len())
            .field("shutdown", &self.shutdown.is_cancelled())
            .field("telemetry", &"Telemetry")
            .finish()
    }
}

impl ApiState {
    pub fn new(
        queue: Arc<QueueStore>,
        runtime: Arc<AgentRuntime>,
        audit: Arc<AuditLogger>,
        principal: Principal,
        config: ApiConfig,
    ) -> Result<Self, ApiError> {
        config.validate()?;
        queue.bootstrap_principal(&principal)?;
        if !config.bind_addr.ip().is_loopback()
            && config.api_key.is_none()
            && !queue.has_active_tokens()?
        {
            return Err(ApiError::BadRequest(
                "bind não local exige SHAKA_API_KEY ou token IAM ativo".to_owned(),
            ));
        }
        let breaker = Arc::new(queue.circuit_breaker("agent-runtime", config.circuit)?);
        Ok(Self {
            queue,
            runtime,
            audit,
            principal,
            config: Arc::new(config),
            breaker,
            cancellations: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            shutdown: ShutdownFlag::default(),
            telemetry: Arc::new(Telemetry::default()),
        })
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(healthz))
            .route("/v1/sessions", post(create_session))
            .route("/v1/sessions/{session_id}", get(get_session))
            .route("/v1/sessions/{session_id}/tasks", post(submit_task))
            .route("/v1/tasks/{task_id}", get(get_task).delete(cancel_task))
            .with_state(self.clone())
            .layer(middleware::from_fn_with_state(
                self.clone(),
                request_context_middleware,
            ))
    }

    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    #[must_use]
    pub fn queue(&self) -> &Arc<QueueStore> {
        &self.queue
    }

    #[must_use]
    pub fn telemetry(&self) -> &Arc<Telemetry> {
        &self.telemetry
    }

    fn register_cancellation(&self, task_id: &TaskId, token: CancellationToken) {
        self.cancellations.lock().insert(task_id.0, token);
    }

    fn cancel_running(&self, task_id: &TaskId) {
        if let Some(token) = self.cancellations.lock().get(&task_id.0).cloned() {
            token.cancel();
        }
    }

    fn remove_cancellation(&self, task_id: &TaskId) {
        self.cancellations.lock().remove(&task_id.0);
    }

    fn authenticate(&self, headers: &HeaderMap) -> Result<AuthenticatedPrincipal, ApiError> {
        let provided = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        match provided {
            Some(token) => {
                if self.config.api_key.as_deref() == Some(token) {
                    Ok(AuthenticatedPrincipal {
                        principal: self.principal.clone(),
                        token_id: "static-api-key".to_owned(),
                        token_prefix: "static".to_owned(),
                        source: AuthSource::StaticApiKey,
                    })
                } else {
                    Ok(self.queue.authenticate_token(token)?)
                }
            }
            None if self.config.bind_addr.ip().is_loopback() && self.config.api_key.is_none() => {
                Ok(AuthenticatedPrincipal {
                    principal: self.principal.clone(),
                    token_id: "local-loopback".to_owned(),
                    token_prefix: "local".to_owned(),
                    source: AuthSource::StaticApiKey,
                })
            }
            None => Err(ApiError::Unauthorized),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    pub session_id: Uuid,
    pub tenant_id: TenantId,
    pub operator_id: shaka_core::OperatorId,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub metadata: Value,
}

impl From<SessionRecord> for SessionResponse {
    fn from(session: SessionRecord) -> Self {
        Self {
            session_id: session.session_id,
            tenant_id: session.principal.tenant_id,
            operator_id: session.principal.operator_id,
            created_at: session.created_at,
            last_seen_at: session.last_seen_at,
            metadata: session.metadata,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubmitTaskRequest {
    pub objective: String,
    #[serde(default)]
    pub priority: i32,
    pub max_attempts: Option<u32>,
    pub dry_run: Option<bool>,
    pub budget: Option<ExecutionBudget>,
}

#[derive(Debug, Serialize)]
pub struct TaskResponse {
    pub task_id: TaskId,
    pub session_id: Uuid,
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

impl From<TaskRecord> for TaskResponse {
    fn from(task: TaskRecord) -> Self {
        Self {
            task_id: task.task_id,
            session_id: task.session_id,
            status: task.status,
            priority: task.priority,
            attempts: task.attempts,
            max_attempts: task.max_attempts,
            next_attempt_at: task.next_attempt_at,
            cancel_requested: task.cancel_requested,
            lease_until: task.lease_until,
            result: task.result,
            last_error: task.last_error,
            created_at: task.created_at,
            updated_at: task.updated_at,
            completed_at: task.completed_at,
        }
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    queued_tasks: u64,
    circuit: CircuitSnapshot,
}

async fn healthz(State(state): State<ApiState>) -> Result<Json<HealthResponse>, ApiError> {
    let span = state
        .telemetry
        .operation_span("api.healthz", &active_correlation().unwrap_or_default());
    let _entered = span.enter();
    let queued_tasks = state.queue.queued_count()?;
    Ok(Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        queued_tasks,
        circuit: state.breaker.snapshot(),
    }))
}

async fn create_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<CreateSessionRequest>,
) -> Result<(StatusCode, Json<SessionResponse>), ApiError> {
    let span = state.telemetry.operation_span(
        "api.session.create",
        &active_correlation().unwrap_or_default(),
    );
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let metadata = bounded_json(request.metadata, 4_096)?;
    let session = state
        .queue
        .create_session(auth.principal.clone(), metadata)?;
    audit_principal(
        &state,
        &auth.principal,
        None,
        "api.session.create",
        "success",
        BTreeMap::new(),
    );
    Ok((StatusCode::CREATED, Json(session.into())))
}

async fn get_session(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SessionResponse>, ApiError> {
    let span = correlation_span(&state, "api.session.get", Some(session_id), None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    state
        .queue
        .touch_session(session_id, &auth.principal.tenant_id)?;
    let session = state
        .queue
        .get_session(session_id, &auth.principal.tenant_id)?;
    Ok(Json(session.into()))
}

async fn submit_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
    Json(request): Json<SubmitTaskRequest>,
) -> Result<(StatusCode, Json<TaskResponse>), ApiError> {
    let span = correlation_span(&state, "api.task.submit", Some(session_id), None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::BadRequest("header Idempotency-Key é obrigatório".to_owned()))?;
    state
        .queue
        .touch_session(session_id, &auth.principal.tenant_id)?;
    if request.objective.trim().is_empty() || request.objective.len() > 32_000 {
        return Err(ApiError::BadRequest(
            "objective vazio ou maior que 32000 caracteres".to_owned(),
        ));
    }
    if !(i32::MIN / 2..=i32::MAX / 2).contains(&request.priority) {
        return Err(ApiError::BadRequest("priority fora do limite".to_owned()));
    }
    let dry_run = request.dry_run.unwrap_or(true);
    if !dry_run
        && (!state.config.live_enabled
            || !state.config.live_confirmation
            || !auth.principal.allows(&Action::RunExternal))
    {
        return Err(ApiError::BadRequest(
            "execução live exige configuração explícita e principal administrador".to_owned(),
        ));
    }
    let mut envelope = TaskEnvelope::new(
        auth.principal.tenant_id.clone(),
        auth.principal.operator_id.clone(),
        request.objective.clone(),
    )
    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    envelope.dry_run = dry_run;
    if let Some(budget) = request.budget.clone() {
        envelope.budget = budget;
    }
    let max_attempts = request.max_attempts.unwrap_or(3);
    let fingerprint = fingerprint(&request, idempotency_key)?;
    let admission_span = correlation_span(
        &state,
        "queue.admission",
        Some(session_id),
        Some(&envelope.task_id),
    );
    let submission = state.queue.submit_task_governed(
        session_id,
        &auth.principal,
        idempotency_key,
        &fingerprint,
        &envelope,
        request.priority,
        max_attempts,
    );
    let (outcome, task) = match submission {
        Ok((outcome, task)) => {
            admission_span.record("outcome", submit_outcome_name(&outcome));
            admission_span.record("admission", submit_outcome_name(&outcome));
            (outcome, task)
        }
        Err(error) => {
            admission_span.record("outcome", "rejected");
            admission_span.record("admission", "rejected");
            admission_span.record("error_type", queue_error_type(&error));
            return Err(error.into());
        }
    };
    if outcome == SubmitOutcome::Created {
        audit_principal(
            &state,
            &auth.principal,
            Some(task.task_id.clone()),
            "api.task.submit",
            "success",
            BTreeMap::from([(String::from("priority"), task.priority.to_string())]),
        );
    }
    let status = if outcome == SubmitOutcome::Created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(task.into())))
}

async fn get_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(task_id): Path<Uuid>,
) -> Result<Json<TaskResponse>, ApiError> {
    let task_id = TaskId(task_id);
    let span = correlation_span(&state, "api.task.get", None, Some(&task_id));
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let record = state.queue.get_task(&task_id, &auth.principal.tenant_id)?;
    Ok(Json(record.into()))
}

async fn cancel_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(task_id): Path<Uuid>,
) -> Result<(StatusCode, Json<TaskResponse>), ApiError> {
    let task_id = TaskId(task_id);
    let span = correlation_span(&state, "api.task.cancel", None, Some(&task_id));
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let record = state
        .queue
        .request_cancel(&task_id, &auth.principal.tenant_id)?;
    state.cancel_running(&task_id);
    let status = if record.status == TaskStatus::CancelRequested {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    audit_principal(
        &state,
        &auth.principal,
        Some(task_id),
        "api.task.cancel",
        "success",
        BTreeMap::new(),
    );
    Ok((status, Json(record.into())))
}

pub async fn serve(state: ApiState) -> Result<(), ApiError> {
    let listener = TcpListener::bind(state.config.bind_addr)
        .await
        .map_err(|error| {
            error!(?error, "não foi possível abrir o listener HTTP");
            ApiError::Internal
        })?;
    let workers = start_workers(&state);
    info!(address = %state.config.bind_addr, workers = state.config.worker_count, "API persistente iniciada");
    let server_result = axum::serve(listener, state.router())
        .with_graceful_shutdown(shutdown_signal())
        .await;
    state.shutdown.cancel();
    for worker in workers {
        if let Err(error) = worker.await {
            warn!(?error, "worker da fila terminou com erro");
        }
    }
    server_result.map_err(|error| {
        error!(?error, "servidor HTTP terminou com erro");
        ApiError::Internal
    })
}

fn start_workers(state: &ApiState) -> Vec<tokio::task::JoinHandle<()>> {
    let recovery_span = correlation_span(state, "queue.lease.recover", None, None);
    match state.queue.recover_expired_leases(Utc::now()) {
        Ok(recovered) => {
            recovery_span.record("outcome", "success");
            recovery_span.record("lease_state", "recovered");
            recovery_span.record("lease_recovered", recovered);
        }
        Err(error) => {
            recovery_span.record("outcome", "failed");
            recovery_span.record("lease_state", "recovery_failed");
            recovery_span.record("error_type", queue_error_type(&error));
            warn!(?error, "não foi possível recuperar leases expirados");
        }
    }
    (0..state.config.worker_count)
        .map(|worker_id| {
            let worker_state = state.clone();
            tokio::spawn(async move { worker_loop(worker_id, worker_state).await })
        })
        .collect()
}

async fn worker_loop(worker_id: usize, state: ApiState) {
    loop {
        if state.shutdown.is_cancelled() {
            return;
        }
        let now = Utc::now();
        let circuit_span = correlation_span(&state, "queue.circuit.allow", None, None);
        match state.breaker.allow(now) {
            Ok(true) => {
                circuit_span.record("outcome", "allowed");
                circuit_span.record("circuit_state", state.breaker.snapshot().state.as_str());
            }
            Ok(false) => {
                circuit_span.record("outcome", "blocked");
                circuit_span.record("circuit_state", state.breaker.snapshot().state.as_str());
                sleep(Duration::milliseconds(100).to_std().unwrap_or_default()).await;
                continue;
            }
            Err(error) => {
                circuit_span.record("outcome", "failed");
                circuit_span.record("error_type", queue_error_type(&error));
                warn!(worker_id, ?error, "circuit breaker indisponível");
                sleep(Duration::milliseconds(250).to_std().unwrap_or_default()).await;
                continue;
            }
        }
        let claim_span = correlation_span(&state, "queue.claim", None, None);
        claim_span.record("worker_id", worker_id);
        match state.queue.claim_next(now, state.config.lease_for) {
            Ok(Some(task)) => {
                claim_span.record("outcome", "claimed");
                claim_span.record("lease_state", "held");
                execute_task(worker_id, state.clone(), task).await;
            }
            Ok(None) => {
                claim_span.record("outcome", "empty");
                sleep(Duration::milliseconds(50).to_std().unwrap_or_default()).await;
            }
            Err(error) => {
                claim_span.record("outcome", "failed");
                claim_span.record("error_type", queue_error_type(&error));
                warn!(worker_id, ?error, "falha ao obter tarefa da fila");
                sleep(Duration::milliseconds(250).to_std().unwrap_or_default()).await;
            }
        }
    }
}

async fn execute_task(worker_id: usize, state: ApiState, task: TaskRecord) {
    let task_id = task.task_id.clone();
    let task_span = correlation_span(&state, "worker.task.process", None, Some(&task_id));
    task_span.record("worker_id", worker_id);
    task_span.record("attempt", task.attempts);
    task_span.record("lease_state", "held");
    let token = CancellationToken::new();
    state.register_cancellation(&task_id, token.clone());
    let execution = state
        .runtime
        .run_with_cancellation(task.envelope.clone(), token)
        .instrument(task_span.clone())
        .await;
    state.remove_cancellation(&task_id);
    let now = Utc::now();
    match execution {
        Ok(result) => {
            task_span.record("outcome", "runtime_succeeded");
            record_circuit_success(worker_id, &state);
            finish_success(
                worker_id,
                &state,
                &task,
                &task_id,
                serde_json::to_value(&result).ok(),
                now,
            );
        }
        Err(error) => {
            let retryable = is_retryable(&error);
            task_span.record("outcome", "runtime_failed");
            task_span.record("retryable", retryable);
            task_span.record("error_type", orchestrator_error_type(&error));
            if retryable {
                record_circuit_failure(worker_id, &state, now);
            }
            finish_failure(worker_id, &state, &task, &task_id, &error, retryable, now);
        }
    }
}

fn record_circuit_success(worker_id: usize, state: &ApiState) {
    let circuit_span = correlation_span(state, "queue.circuit.record_success", None, None);
    match state.breaker.record_success() {
        Ok(()) => {
            circuit_span.record("outcome", "success");
            circuit_span.record("circuit_state", state.breaker.snapshot().state.as_str());
        }
        Err(error) => {
            circuit_span.record("outcome", "failed");
            circuit_span.record("error_type", queue_error_type(&error));
            warn!(
                worker_id,
                ?error,
                "não foi possível fechar o circuit breaker após sucesso"
            );
        }
    }
}

fn record_circuit_failure(worker_id: usize, state: &ApiState, now: DateTime<Utc>) {
    let circuit_span = correlation_span(state, "queue.circuit.record_failure", None, None);
    match state.breaker.record_failure(now) {
        Ok(()) => {
            circuit_span.record("outcome", "failure");
            circuit_span.record("circuit_state", state.breaker.snapshot().state.as_str());
        }
        Err(error) => {
            circuit_span.record("outcome", "failed");
            circuit_span.record("error_type", queue_error_type(&error));
            warn!(
                worker_id,
                ?error,
                "não foi possível registrar falha no circuit breaker"
            );
        }
    }
}

fn finish_success(
    worker_id: usize,
    state: &ApiState,
    task: &TaskRecord,
    task_id: &TaskId,
    result_json: Option<Value>,
    now: DateTime<Utc>,
) {
    let finish_span = correlation_span(state, "queue.finish", None, Some(task_id));
    match state.queue.finish_task(
        task_id,
        &task.tenant_id,
        result_json,
        None,
        false,
        now,
        state.config.retry_base_delay,
        state.config.retry_max_delay,
    ) {
        Ok(outcome) => {
            finish_span.record("outcome", finish_outcome_name(&outcome));
            finish_span.record("lease_state", "released");
            audit_task_finish(state, task, &outcome);
        }
        Err(error) => {
            finish_span.record("outcome", "failed");
            finish_span.record("error_type", queue_error_type(&error));
            warn!(
                worker_id,
                ?error,
                "não foi possível finalizar tarefa com sucesso"
            );
        }
    }
}

fn finish_failure(
    worker_id: usize,
    state: &ApiState,
    task: &TaskRecord,
    task_id: &TaskId,
    error: &OrchestratorError,
    retryable: bool,
    now: DateTime<Utc>,
) {
    let safe_error = shaka_core::redact_sensitive(&error.to_string());
    let finish_span = correlation_span(state, "queue.finish", None, Some(task_id));
    match state.queue.finish_task(
        task_id,
        &task.tenant_id,
        None,
        Some(safe_error.as_str()),
        retryable,
        now,
        state.config.retry_base_delay,
        state.config.retry_max_delay,
    ) {
        Ok(outcome) => {
            finish_span.record("outcome", finish_outcome_name(&outcome));
            finish_span.record("retryable", retryable);
            finish_span.record("lease_state", "released");
            if let FinishOutcome::Requeued {
                ref next_attempt_at,
            } = outcome
            {
                finish_span.record(
                    "retry_delay_ms",
                    (*next_attempt_at - now).num_milliseconds().max(0),
                );
            }
            audit_task_finish(state, task, &outcome);
        }
        Err(queue_error) => {
            finish_span.record("outcome", "failed");
            finish_span.record("error_type", queue_error_type(&queue_error));
            warn!(
                worker_id,
                ?queue_error,
                "não foi possível finalizar falha de tarefa"
            );
        }
    }
}

fn audit_task_finish(state: &ApiState, task: &TaskRecord, outcome: &FinishOutcome) {
    let outcome_name = match outcome {
        FinishOutcome::Succeeded => "succeeded",
        FinishOutcome::Requeued { .. } => "retry_scheduled",
        FinishOutcome::Failed => "failed",
        FinishOutcome::Cancelled => "cancelled",
    };
    audit_identity(
        state,
        &task.tenant_id,
        &task.envelope.operator_id,
        Some(task.task_id.clone()),
        "api.task.finish",
        outcome_name,
        BTreeMap::new(),
    );
}

fn is_retryable(error: &OrchestratorError) -> bool {
    matches!(
        error,
        OrchestratorError::Http(_) | OrchestratorError::DeadlineExceeded
    )
}

fn submit_outcome_name(outcome: &SubmitOutcome) -> &'static str {
    match outcome {
        SubmitOutcome::Created => "created",
        SubmitOutcome::Existing => "existing",
    }
}

fn finish_outcome_name(outcome: &FinishOutcome) -> &'static str {
    match outcome {
        FinishOutcome::Succeeded => "succeeded",
        FinishOutcome::Requeued { .. } => "retry_scheduled",
        FinishOutcome::Failed => "failed",
        FinishOutcome::Cancelled => "cancelled",
    }
}

fn queue_error_type(error: &QueueError) -> &'static str {
    match error {
        QueueError::Sqlite(_) => "sqlite",
        QueueError::Serialization(_) => "serialization",
        QueueError::Core(_) => "core",
        QueueError::InvalidIdentifier(_) => "invalid_identifier",
        QueueError::InvalidInput(_) => "invalid_input",
        QueueError::NotFound(_) => "not_found",
        QueueError::IdempotencyConflict => "idempotency_conflict",
        QueueError::Unauthorized => "unauthorized",
        QueueError::Forbidden => "forbidden",
        QueueError::QuotaExceeded(_) => "quota_exceeded",
        QueueError::RateLimited { .. } => "rate_limited",
    }
}

fn orchestrator_error_type(error: &OrchestratorError) -> &'static str {
    match error {
        OrchestratorError::Core(_) => "core",
        OrchestratorError::Memory(_) => "memory",
        OrchestratorError::Http(_) => "http",
        OrchestratorError::InvalidModelResponse(_) => "invalid_model_response",
        OrchestratorError::ToolNotFound(_) => "tool_not_found",
        OrchestratorError::ToolExecution(_) => "tool_execution",
        OrchestratorError::DeadlineExceeded => "deadline_exceeded",
        OrchestratorError::Cancelled => "cancelled",
    }
}

async fn request_context_middleware(
    State(state): State<ApiState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let context = correlation_from_request(&request);
    request.extensions_mut().insert(context.clone());
    let method = request.method().as_str().to_owned();
    let route_template = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_owned(), |path| path.as_str().to_owned());
    let span = state
        .telemetry
        .http_server_span(&context, &method, &route_template);
    let response = ACTIVE_CORRELATION
        .scope(context.clone(), next.run(request).instrument(span.clone()))
        .await;
    let status = response.status();
    span.record("http_status_code", status.as_u16());
    span.record("http_status_class", status_class(status));
    let mut response = response;
    insert_request_id_header(&mut response, context.request_id());
    response
}

fn correlation_from_request(request: &Request<Body>) -> CorrelationContext {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| CorrelationContext::with_request_id(value).ok())
        .unwrap_or_default();
    let Some((trace_id, span_id)) = request
        .headers()
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_traceparent)
    else {
        return request_id;
    };
    request_id
        .with_trace_ids(Some(trace_id), Some(span_id))
        .unwrap_or_default()
}

fn parse_traceparent(value: &str) -> Option<(String, String)> {
    let mut parts = value.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let flags = parts.next()?;
    if parts.next().is_some()
        || version.len() != 2
        || version.eq_ignore_ascii_case("ff")
        || trace_id.len() != 32
        || span_id.len() != 16
        || flags.len() != 2
        || !version
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        || !trace_id
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        || !span_id
            .chars()
            .all(|character| character.is_ascii_hexdigit())
        || !flags.chars().all(|character| character.is_ascii_hexdigit())
        || trace_id.chars().all(|character| character == '0')
        || span_id.chars().all(|character| character == '0')
    {
        return None;
    }
    Some((trace_id.to_ascii_lowercase(), span_id.to_ascii_lowercase()))
}

fn active_correlation() -> Option<CorrelationContext> {
    ACTIVE_CORRELATION.try_with(Clone::clone).ok()
}

fn active_request_id() -> String {
    active_correlation().map_or_else(
        || Uuid::new_v4().to_string(),
        |context| context.request_id().to_owned(),
    )
}

fn correlation_span(
    state: &ApiState,
    operation: &str,
    session_id: Option<Uuid>,
    task_id: Option<&TaskId>,
) -> tracing::Span {
    let mut context = active_correlation().unwrap_or_default();
    if let Ok(enriched) = context
        .clone()
        .with_session_id(session_id.map(|id| id.to_string()))
    {
        context = enriched;
    }
    if let Ok(enriched) = context
        .clone()
        .with_task_id(task_id.map(|id| id.0.to_string()))
    {
        context = enriched;
    }
    state.telemetry.operation_span(operation, &context)
}

fn insert_request_id_header(response: &mut Response, request_id: &str) {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
}

fn status_class(status: StatusCode) -> &'static str {
    match status.as_u16() {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

fn authorize(headers: &HeaderMap, state: &ApiState) -> Result<AuthenticatedPrincipal, ApiError> {
    state.authenticate(headers)
}

fn bounded_json(value: Value, max_bytes: usize) -> Result<Value, ApiError> {
    let size = serde_json::to_vec(&value)
        .map_err(|_| ApiError::BadRequest("metadata JSON inválido".to_owned()))?
        .len();
    if size > max_bytes {
        return Err(ApiError::BadRequest("metadata excede o limite".to_owned()));
    }
    Ok(value)
}

fn fingerprint(request: &SubmitTaskRequest, idempotency_key: &str) -> Result<String, ApiError> {
    let canonical = serde_json::to_vec(&json!({
        "objective": request.objective,
        "priority": request.priority,
        "max_attempts": request.max_attempts.unwrap_or(3),
        "dry_run": request.dry_run.unwrap_or(true),
        "budget": request.budget,
    }))
    .map_err(|_| ApiError::BadRequest("payload não serializável".to_owned()))?;
    let mut hasher = Sha256::new();
    hasher.update(idempotency_key.as_bytes());
    hasher.update([0]);
    hasher.update(canonical);
    Ok(hex::encode(hasher.finalize()))
}

fn audit_principal(
    state: &ApiState,
    principal: &Principal,
    task_id: Option<TaskId>,
    action: &str,
    outcome: &str,
    metadata: std::collections::BTreeMap<String, String>,
) {
    audit_identity(
        state,
        &principal.tenant_id,
        &principal.operator_id,
        task_id,
        action,
        outcome,
        metadata,
    );
}

fn audit_identity(
    state: &ApiState,
    tenant_id: &TenantId,
    operator_id: &shaka_core::OperatorId,
    task_id: Option<TaskId>,
    action: &str,
    outcome: &str,
    metadata: std::collections::BTreeMap<String, String>,
) {
    let mut metadata = metadata;
    if let Some(context) = active_correlation() {
        metadata.insert(
            "correlation_request_id".to_owned(),
            context.request_id().to_owned(),
        );
        if let Some(trace_id) = context.trace_id() {
            metadata.insert("correlation_trace_id".to_owned(), trace_id.to_owned());
        }
    }
    if let Err(error) = state.audit.record(
        task_id,
        tenant_id.clone(),
        operator_id.0.clone(),
        action,
        outcome,
        metadata,
    ) {
        warn!(?error, action, "evento da API não pôde ser auditado");
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(?error, "não foi possível aguardar Ctrl-C");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use shaka_memory::MemoryStore;
    use shaka_orchestrator::{AgentModel, EchoTool, LocalModel, ToolRegistry};
    use shaka_orchestrator::{ModelRequest, ModelResponse};
    use tower::ServiceExt;

    fn principal() -> Principal {
        Principal {
            operator_id: shaka_core::OperatorId::new("operator").unwrap(),
            tenant_id: TenantId::new("tenant").unwrap(),
            role: shaka_core::Role::Administrator,
        }
    }

    fn state() -> ApiState {
        let memory = Arc::new(MemoryStore::in_memory().unwrap());
        let audit = Arc::new(AuditLogger::new(Arc::clone(&memory)));
        let mut tools = ToolRegistry::with_capabilities(shaka_core::CapabilitySet(vec![
            shaka_core::Capability::MemoryWrite,
        ]));
        tools.register(Arc::new(EchoTool)).unwrap();
        let runtime = Arc::new(AgentRuntime::new(Arc::new(LocalModel), memory, tools));
        ApiState::new(
            Arc::new(QueueStore::in_memory().unwrap()),
            runtime,
            audit,
            principal(),
            ApiConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn traceparent_parser_accepts_only_safe_nonzero_ids() {
        assert_eq!(
            parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            Some((
                "4bf92f3577b34da6a3ce929d0e0e4736".to_owned(),
                "00f067aa0ba902b7".to_owned(),
            ))
        );
        assert!(
            parse_traceparent("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none()
        );
        assert!(
            parse_traceparent("00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01").is_none()
        );
        assert!(
            parse_traceparent("ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").is_none()
        );
        assert!(parse_traceparent("not-a-traceparent").is_none());
    }

    #[test]
    fn queue_and_runtime_telemetry_taxonomies_are_stable_and_message_free() {
        assert_eq!(submit_outcome_name(&SubmitOutcome::Created), "created");
        assert_eq!(submit_outcome_name(&SubmitOutcome::Existing), "existing");
        assert_eq!(finish_outcome_name(&FinishOutcome::Cancelled), "cancelled");
        assert_eq!(
            queue_error_type(&QueueError::RateLimited {
                retry_after_seconds: 5,
            }),
            "rate_limited"
        );
        let runtime_error = OrchestratorError::ToolExecution("secret payload".to_owned());
        assert_eq!(orchestrator_error_type(&runtime_error), "tool_execution");
        assert_ne!(orchestrator_error_type(&runtime_error), "secret payload");
    }

    #[tokio::test]
    async fn request_id_is_preserved_in_success_response() {
        let state = state();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .header(REQUEST_ID_HEADER, "req-health-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(REQUEST_ID_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("req-health-1")
        );
    }

    #[tokio::test]
    async fn invalid_request_id_is_replaced_and_error_body_matches_header() {
        let state = state();
        let task_id = Uuid::new_v4();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri(format!("/v1/tasks/{task_id}"))
                    .header(REQUEST_ID_HEADER, "contains spaces")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let header_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("request ID header")
            .to_owned();
        assert_ne!(header_id, "contains spaces");
        assert!(CorrelationContext::with_request_id(&header_id).is_ok());
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["request_id"].as_str(), Some(header_id.as_str()));
    }

    #[tokio::test]
    async fn health_and_session_routes_work_without_auth_key() {
        let state = state();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"metadata":{"source":"test"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn idempotency_and_cancel_are_exposed_over_http() {
        let state = state();
        let session = state
            .queue
            .create_session(state.principal.clone(), Value::Null)
            .unwrap();
        let body = r#"{"objective":"hello","priority":3}"#;
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/sessions/{}/tasks", session.session_id))
                    .header("content-type", "application/json")
                    .header("idempotency-key", "same")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/sessions/{}/tasks", session.session_id))
                    .header("content-type", "application/json")
                    .header("idempotency-key", "same")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[derive(Debug)]
    struct SlowModel;

    #[async_trait]
    impl AgentModel for SlowModel {
        async fn complete(
            &self,
            _request: ModelRequest,
        ) -> Result<ModelResponse, OrchestratorError> {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            Ok(ModelResponse {
                content: "slow".to_owned(),
                tool_calls: Vec::new(),
                estimated_cost_microunits: 0,
            })
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_model_call() {
        let memory = Arc::new(MemoryStore::in_memory().unwrap());
        let runtime = AgentRuntime::new(
            Arc::new(SlowModel),
            memory,
            ToolRegistry::with_capabilities(shaka_core::CapabilitySet::default()),
        );
        let principal = principal();
        let envelope =
            TaskEnvelope::new(principal.tenant_id, principal.operator_id, "cancelar").unwrap();
        let token = CancellationToken::new();
        let task = tokio::spawn({
            let token = token.clone();
            async move { runtime.run_with_cancellation(envelope, token).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        token.cancel();
        let result = task.await.unwrap();
        assert!(matches!(result, Err(OrchestratorError::Cancelled)));
    }

    #[tokio::test]
    async fn bearer_token_selects_tenant_and_revocation_denies() {
        let state = state();
        let tenant = TenantId::new("tenant-two").unwrap();
        state.queue.create_tenant(&tenant, "Tenant Two").unwrap();
        let operator = shaka_core::OperatorId::new("operator-two").unwrap();
        state
            .queue
            .create_user(&operator, &tenant, &shaka_core::Role::Operator)
            .unwrap();
        let issue = state.queue.issue_token(&operator, None).unwrap();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("authorization", format!("Bearer {}", issue.token))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"metadata":{"source":"iam"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let session: SessionResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(session.tenant_id, tenant);
        state.queue.revoke_token(&issue.token_id).unwrap();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("authorization", format!("Bearer {}", issue.token))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn non_local_bind_allows_persistent_iam_configuration() {
        let config = ApiConfig {
            bind_addr: "0.0.0.0:8080".parse().unwrap(),
            ..ApiConfig::default()
        };
        assert!(config.validate().is_ok());
        let config_with_key = ApiConfig {
            bind_addr: "0.0.0.0:8080".parse().unwrap(),
            api_key: Some("local-test-key".to_owned()),
            ..ApiConfig::default()
        };
        assert!(config_with_key.validate().is_ok());
    }

    #[test]
    fn fingerprint_changes_when_payload_changes() {
        let first = SubmitTaskRequest {
            objective: "one".to_owned(),
            priority: 0,
            max_attempts: None,
            dry_run: None,
            budget: None,
        };
        let mut second = first.clone();
        second.objective = "two".to_owned();
        assert_ne!(
            fingerprint(&first, "key").unwrap(),
            fingerprint(&second, "key").unwrap()
        );
    }
}
