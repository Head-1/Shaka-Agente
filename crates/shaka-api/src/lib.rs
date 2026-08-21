//! API HTTP persistente do Shaka v0.5.0.
//!
//! O servidor mantém a coordenação da fila no host, não no modelo. Cada
//! trabalho passa pelo `AgentRuntime`, pelo SQLite e pelo auditor existente.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use shaka_core::{Action, ExecutionBudget, Principal, TaskEnvelope, TaskId, TenantId};
use shaka_observability::AuditLogger;
use shaka_orchestrator::{AgentRuntime, CancellationToken, OrchestratorError};
use shaka_queue::{
    CircuitBreaker, CircuitConfig, CircuitSnapshot, FinishOutcome, QueueError, QueueStore,
    SessionRecord, SubmitOutcome, TaskRecord, TaskStatus,
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
use tracing::{error, info, warn};
use uuid::Uuid;

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
                .is_none_or(|value| value.trim().is_empty())
        {
            return Err(ApiError::BadRequest(
                "bind não local exige SHAKA_API_KEY".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("não autorizado")]
    Unauthorized,
    #[error("recurso não encontrado")]
    NotFound,
    #[error("entrada inválida: {0}")]
    BadRequest(String),
    #[error("conflito de idempotência")]
    Conflict(String),
    #[error("falha interna persistente")]
    Internal,
}

impl From<QueueError> for ApiError {
    fn from(error: QueueError) -> Self {
        match error {
            QueueError::NotFound(_) => Self::NotFound,
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
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &self {
            Self::Unauthorized => "não autorizado".to_owned(),
            Self::NotFound => "recurso não encontrado".to_owned(),
            Self::BadRequest(value) | Self::Conflict(value) => value.clone(),
            Self::Internal => "falha interna persistente".to_owned(),
        };
        let body = Json(json!({
            "error": message,
            "request_id": Uuid::new_v4(),
        }));
        (status, body).into_response()
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
    }

    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    #[must_use]
    pub fn queue(&self) -> &Arc<QueueStore> {
        &self.queue
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
}

#[derive(Debug, Deserialize, Default)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Serialize)]
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
    authorize(&headers, &state)?;
    let metadata = bounded_json(request.metadata, 4_096)?;
    let session = state
        .queue
        .create_session(state.principal.clone(), metadata)?;
    audit(
        &state,
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
    authorize(&headers, &state)?;
    state
        .queue
        .touch_session(session_id, &state.principal.tenant_id)?;
    let session = state
        .queue
        .get_session(session_id, &state.principal.tenant_id)?;
    Ok(Json(session.into()))
}

async fn submit_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
    Json(request): Json<SubmitTaskRequest>,
) -> Result<(StatusCode, Json<TaskResponse>), ApiError> {
    authorize(&headers, &state)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::BadRequest("header Idempotency-Key é obrigatório".to_owned()))?;
    state
        .queue
        .touch_session(session_id, &state.principal.tenant_id)?;
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
            || !state.principal.allows(&Action::RunExternal))
    {
        return Err(ApiError::BadRequest(
            "execução live exige configuração explícita e principal administrador".to_owned(),
        ));
    }
    let mut envelope = TaskEnvelope::new(
        state.principal.tenant_id.clone(),
        state.principal.operator_id.clone(),
        request.objective.clone(),
    )
    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    envelope.dry_run = dry_run;
    if let Some(budget) = request.budget.clone() {
        envelope.budget = budget;
    }
    let max_attempts = request.max_attempts.unwrap_or(3);
    let fingerprint = fingerprint(&request, idempotency_key)?;
    let (outcome, task) = state.queue.submit_task(
        session_id,
        &state.principal.tenant_id,
        idempotency_key,
        &fingerprint,
        &envelope,
        request.priority,
        max_attempts,
    )?;
    if outcome == SubmitOutcome::Created {
        audit(
            &state,
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
    authorize(&headers, &state)?;
    let record = state
        .queue
        .get_task(&TaskId(task_id), &state.principal.tenant_id)?;
    Ok(Json(record.into()))
}

async fn cancel_task(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(task_id): Path<Uuid>,
) -> Result<(StatusCode, Json<TaskResponse>), ApiError> {
    authorize(&headers, &state)?;
    let task_id = TaskId(task_id);
    let record = state
        .queue
        .request_cancel(&task_id, &state.principal.tenant_id)?;
    state.cancel_running(&task_id);
    let status = if record.status == TaskStatus::CancelRequested {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    audit(
        &state,
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
    if let Err(error) = state.queue.recover_expired_leases(Utc::now()) {
        warn!(?error, "não foi possível recuperar leases expirados");
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
        match state.breaker.allow(now) {
            Ok(true) => {}
            Ok(false) => {
                sleep(Duration::milliseconds(100).to_std().unwrap_or_default()).await;
                continue;
            }
            Err(error) => {
                warn!(worker_id, ?error, "circuit breaker indisponível");
                sleep(Duration::milliseconds(250).to_std().unwrap_or_default()).await;
                continue;
            }
        }
        match state.queue.claim_next(now, state.config.lease_for) {
            Ok(Some(task)) => execute_task(worker_id, state.clone(), task).await,
            Ok(None) => sleep(Duration::milliseconds(50).to_std().unwrap_or_default()).await,
            Err(error) => {
                warn!(worker_id, ?error, "falha ao obter tarefa da fila");
                sleep(Duration::milliseconds(250).to_std().unwrap_or_default()).await;
            }
        }
    }
}

async fn execute_task(worker_id: usize, state: ApiState, task: TaskRecord) {
    let task_id = task.task_id.clone();
    let token = CancellationToken::new();
    state.register_cancellation(&task_id, token.clone());
    let execution = state
        .runtime
        .run_with_cancellation(task.envelope.clone(), token)
        .await;
    state.remove_cancellation(&task_id);
    let now = Utc::now();
    match execution {
        Ok(result) => {
            if let Err(error) = state.breaker.record_success() {
                warn!(
                    worker_id,
                    ?error,
                    "não foi possível fechar o circuit breaker após sucesso"
                );
            }
            let result_json = serde_json::to_value(&result).ok();
            match state.queue.finish_task(
                &task_id,
                &task.tenant_id,
                result_json,
                None,
                false,
                now,
                state.config.retry_base_delay,
                state.config.retry_max_delay,
            ) {
                Ok(outcome) => audit_task_finish(&state, &task_id, &outcome),
                Err(error) => warn!(
                    worker_id,
                    ?error,
                    "não foi possível finalizar tarefa com sucesso"
                ),
            }
        }
        Err(error) => {
            let retryable = is_retryable(&error);
            if retryable {
                if let Err(record_error) = state.breaker.record_failure(now) {
                    warn!(
                        worker_id,
                        ?record_error,
                        "não foi possível registrar falha no circuit breaker"
                    );
                }
            }
            let safe_error = shaka_core::redact_sensitive(&error.to_string());
            match state.queue.finish_task(
                &task_id,
                &task.tenant_id,
                None,
                Some(safe_error.as_str()),
                retryable,
                now,
                state.config.retry_base_delay,
                state.config.retry_max_delay,
            ) {
                Ok(outcome) => audit_task_finish(&state, &task_id, &outcome),
                Err(queue_error) => warn!(
                    worker_id,
                    ?queue_error,
                    "não foi possível finalizar falha de tarefa"
                ),
            }
        }
    }
}

fn audit_task_finish(state: &ApiState, task_id: &TaskId, outcome: &FinishOutcome) {
    let outcome_name = match outcome {
        FinishOutcome::Succeeded => "succeeded",
        FinishOutcome::Requeued { .. } => "retry_scheduled",
        FinishOutcome::Failed => "failed",
        FinishOutcome::Cancelled => "cancelled",
    };
    audit(
        state,
        Some(task_id.clone()),
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

fn authorize(headers: &HeaderMap, state: &ApiState) -> Result<(), ApiError> {
    let Some(expected) = state.config.api_key.as_deref() else {
        return Ok(());
    };
    let provided = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if provided == Some(expected) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
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

fn audit(
    state: &ApiState,
    task_id: Option<TaskId>,
    action: &str,
    outcome: &str,
    metadata: std::collections::BTreeMap<String, String>,
) {
    if let Err(error) = state.audit.record(
        task_id,
        state.principal.tenant_id.clone(),
        state.principal.operator_id.0.clone(),
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

    #[test]
    fn non_local_bind_requires_authentication_key() {
        let config = ApiConfig {
            bind_addr: "0.0.0.0:8080".parse().unwrap(),
            ..ApiConfig::default()
        };
        assert!(config.validate().is_err());
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
