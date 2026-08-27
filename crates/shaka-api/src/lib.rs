//! API HTTP persistente do Shaka v0.8.2.
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
use shaka_core::{
    Action, ExecutionBudget, ExecutionContext, PlanApproval, PlanApprovalDecision, PlanId,
    PlanMode, PlanSpec, PlanSpecInput, PlanStepId, PlanStepState, Principal, TaskEnvelope, TaskId,
    TenantId,
};
use shaka_observability::{AuditLogger, CorrelationContext, Telemetry};
use shaka_orchestrator::{AgentRuntime, CancellationToken, OrchestratorError};
use shaka_queue::{
    AuthSource, AuthenticatedPrincipal, CircuitBreaker, CircuitConfig, CircuitSnapshot,
    CircuitState, FinishOutcome, LeaseToken, PlanApprovalOutcome, PlanCheckpoint, PlanClaimContext,
    PlanInspectionIssue, PlanInspectionReport, PlanInspectionStatus, PlanResolutionDecision,
    PlanResolutionOutcome, PlanTaskReference, QueueError, QueueStore, SessionRecord, SubmitOutcome,
    TaskRecord, TaskStatus,
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
use tokio::{
    net::TcpListener,
    time::{sleep, timeout},
};
use tracing::{Instrument, error, info, warn};
use uuid::Uuid;

/// Header HTTP usado para correlação operacional, sem carregar conteúdo de negócio.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

tokio::task_local! {
    static ACTIVE_CORRELATION: CorrelationContext;
}

/// Configuração host-side da API HTTP e dos workers da fila.
///
/// O padrão mantém o bind em loopback, desabilita execução live e usa uma
/// política bounded de lease, retry e circuit breaker. Um bind não local
/// exige `api_key` ou token IAM ativo antes de o servidor ser iniciado.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Endereço no qual o listener HTTP será aberto.
    pub bind_addr: SocketAddr,
    /// Número de workers de fila; o valor permitido está entre 1 e 32.
    pub worker_count: usize,
    /// Chave estática opcional para autenticação bearer.
    pub api_key: Option<String>,
    /// Habilita a configuração de execução live; permanece falso por padrão.
    pub live_enabled: bool,
    /// Confirmação adicional exigida junto de `live_enabled`.
    pub live_confirmation: bool,
    /// Duração da lease atribuída a uma tarefa reclamada.
    pub lease_for: Duration,
    /// Atraso inicial da política de retry.
    pub retry_base_delay: Duration,
    /// Limite superior do atraso de retry.
    pub retry_max_delay: Duration,
    /// Configuração persistente do circuit breaker do runtime.
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
    /// Valida limites operacionais e requisitos de autenticação da API.
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
        if self
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

/// Erros sanitizados da fronteira HTTP.
///
/// A conversão para resposta inclui `request_id`, mas não inclui tokens,
/// prompts brutos ou payloads sensíveis.
#[derive(Debug, Error)]
pub enum ApiError {
    /// A credencial está ausente ou não pode ser validada.
    #[error("não autorizado")]
    Unauthorized,
    /// O principal autenticado não possui a autoridade exigida.
    #[error("operação proibida")]
    Forbidden,
    /// O recurso não existe no tenant autenticado.
    #[error("recurso não encontrado")]
    NotFound,
    /// A entrada não atende ao contrato ou aos limites da API.
    #[error("entrada inválida: {0}")]
    BadRequest(String),
    /// A chave de idempotência foi reutilizada com intenção divergente.
    #[error("conflito de idempotência")]
    Conflict(String),
    /// O limite de requisições foi atingido; o retry deve respeitar o atraso.
    #[error("rate limit excedido")]
    RateLimited {
        /// Número mínimo de segundos sugerido antes de nova tentativa.
        retry_after_seconds: u64,
    },
    /// A quota persistente do tenant foi atingida.
    #[error("quota excedida: {0}")]
    QuotaExceeded(String),
    /// Falha persistente não exposta em detalhes ao cliente.
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
            QueueError::LeaseLost => Self::Conflict("lease da tarefa perdida".to_owned()),
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

/// Estado compartilhado pelo roteador HTTP e pelos workers host-side.
///
/// O estado mantém a fila, runtime, auditoria, principal autenticado,
/// circuit breaker e telemetria. Os recursos internos permanecem privados
/// para impedir que handlers ou consumidores contornem as políticas do host.
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
    /// Cria o estado da API e valida bind, autenticação e limites operacionais.
    ///
    /// Um bind fora do loopback é rejeitado sem API key ou token IAM ativo.
    pub fn new(
        queue: Arc<QueueStore>,
        runtime: Arc<AgentRuntime>,
        audit: Arc<AuditLogger>,
        principal: Principal,
        config: ApiConfig,
    ) -> Result<Self, ApiError> {
        config.validate()?;
        if !config.bind_addr.ip().is_loopback()
            && config.api_key.is_none()
            && !queue.has_active_tokens()?
        {
            return Err(ApiError::BadRequest(
                "bind não local exige SHAKA_API_KEY ou token IAM ativo".to_owned(),
            ));
        }
        queue.bootstrap_principal(&principal)?;
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

    /// Constrói o roteador com endpoints de sessões, tarefas e Plan Engine.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(healthz))
            .route("/readyz", get(readyz))
            .route("/v1/sessions", post(create_session))
            .route("/v1/sessions/{session_id}", get(get_session))
            .route("/v1/sessions/{session_id}/tasks", post(submit_task))
            .route("/v1/tasks/{task_id}", get(get_task).delete(cancel_task))
            .route("/v1/plans", post(create_plan))
            .route("/v1/plans/{plan_id}", get(get_plan))
            .route("/v1/plans/{plan_id}/validate", post(validate_plan))
            .route("/v1/plans/{plan_id}/approve", post(approve_plan))
            .route("/v1/plans/{plan_id}/resume", post(resume_plan))
            .route("/v1/plans/{plan_id}/cancel", post(cancel_plan))
            .route("/v1/plans/{plan_id}/checkpoints", get(get_plan_checkpoints))
            .with_state(self.clone())
            .layer(middleware::from_fn_with_state(
                self.clone(),
                request_context_middleware,
            ))
    }

    /// Retorna o principal local configurado para o estado da API.
    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Retorna a fila host-side usada pelos handlers e workers.
    #[must_use]
    pub fn queue(&self) -> &Arc<QueueStore> {
        &self.queue
    }

    /// Retorna a fachada de telemetria sem conceder autoridade de decisão.
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

    fn cancel_all_running(&self) {
        for token in self.cancellations.lock().values() {
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

/// Corpo da criação de sessão.
#[derive(Debug, Deserialize, Default)]
pub struct CreateSessionRequest {
    /// Metadados operacionais bounded persistidos com a sessão.
    #[serde(default)]
    pub metadata: Value,
}

/// Representação persistida de uma sessão pertencente a um tenant.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionResponse {
    /// Identificador da sessão.
    pub session_id: Uuid,
    /// Tenant ao qual a sessão pertence.
    pub tenant_id: TenantId,
    /// Operador que criou ou possui a sessão.
    pub operator_id: shaka_core::OperatorId,
    /// Instante de criação em UTC.
    pub created_at: DateTime<Utc>,
    /// Último acesso observado em UTC.
    pub last_seen_at: DateTime<Utc>,
    /// Metadados operacionais da sessão, sem segredo.
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

/// Corpo de submissão de tarefa.
///
/// `dry_run` ausente significa `true`; referências planejadas precisam conter
/// todos os identificadores e digest e continuam limitadas a dry-run.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubmitTaskRequest {
    /// Objetivo textual bounded da tarefa.
    pub objective: String,
    /// Prioridade usada pela seleção determinística da fila.
    #[serde(default)]
    pub priority: i32,
    /// Número máximo de tentativas; o padrão é três.
    pub max_attempts: Option<u32>,
    /// Define o modo seguro; ausente equivale a `true`.
    pub dry_run: Option<bool>,
    /// Orçamento de execução validado pelo host.
    pub budget: Option<ExecutionBudget>,
    /// ID da task definida pelo plano; obrigatório quando a referência planejada é usada.
    pub task_id: Option<TaskId>,
    /// Plano imutável que autoriza a task, quando o modo planejado está ativo.
    pub plan_id: Option<PlanId>,
    /// Revisão do plano imutável.
    pub plan_revision: Option<u32>,
    /// Digest SHA-256 da revisão do plano.
    pub plan_digest: Option<String>,
}

/// Estado serializável de uma tarefa e de sua lease.
#[derive(Debug, Serialize)]
pub struct TaskResponse {
    /// Identificador da tarefa.
    pub task_id: TaskId,
    /// Sessão que originou a tarefa.
    pub session_id: Uuid,
    /// Estado persistido da tarefa.
    pub status: TaskStatus,
    /// Prioridade usada pela fila.
    pub priority: i32,
    /// Número de tentativas já realizadas.
    pub attempts: u32,
    /// Limite total de tentativas.
    pub max_attempts: u32,
    /// Próximo instante elegível para tentativa.
    pub next_attempt_at: DateTime<Utc>,
    /// Indica solicitação de cancelamento cooperativo.
    pub cancel_requested: bool,
    /// Fim da lease atual, quando houver.
    pub lease_until: Option<DateTime<Utc>>,
    /// Resultado sanitizado, quando a tarefa terminou.
    pub result: Option<Value>,
    /// Último erro operacional sanitizado.
    pub last_error: Option<String>,
    /// Instante de criação em UTC.
    pub created_at: DateTime<Utc>,
    /// Instante da última atualização em UTC.
    pub updated_at: DateTime<Utc>,
    /// Instante de conclusão, quando houver.
    pub completed_at: Option<DateTime<Utc>>,
    /// Plano imutável associado, quando a tarefa é planejada.
    pub plan_id: Option<PlanId>,
    /// Revisão do plano associado.
    pub plan_revision: Option<u32>,
    /// Digest SHA-256 da revisão do plano.
    pub plan_digest: Option<String>,
    /// Etapa do plano atualmente locada pelo worker.
    pub plan_step_id: Option<PlanStepId>,
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
            plan_id: task.plan_id,
            plan_revision: task.plan_revision,
            plan_digest: task.plan_digest,
            plan_step_id: task.plan_step_id,
        }
    }
}

/// Corpo de uma decisão humana de aprovação de plano ou etapa.
#[derive(Debug, Deserialize)]
pub struct PlanApprovalRequest {
    /// Etapa opcional à qual a decisão fica limitada.
    pub step_id: Option<PlanStepId>,
    /// Decisão tipada que será revalidada pelo host.
    pub decision: PlanApprovalDecision,
    /// Validade solicitada, limitada a 1 segundo–7 dias.
    pub expires_in_seconds: Option<i64>,
}

/// Corpo da resolução humana de um plano em `unknown` para retomada.
#[derive(Debug, Deserialize)]
pub struct PlanResumeRequest {
    /// Digest da evidência externa vinculada à análise do incidente.
    pub evidence_digest: String,
}

/// Corpo de criação de plano; o input deve pertencer ao principal autenticado.
#[derive(Debug, Deserialize)]
pub struct PlanCreateRequest {
    /// Contrato do plano validado e persistido pelo host.
    #[serde(flatten)]
    pub input: PlanSpecInput,
}

#[derive(Debug, Serialize)]
struct PlanDetailResponse {
    plan: PlanSpec,
    step_states: BTreeMap<PlanStepId, PlanStepState>,
    integrity: PlanInspectionStatus,
    integrity_issue: Option<PlanInspectionIssue>,
    checkpoints_checked: u64,
    transitions_checked: u64,
    task: Option<TaskResponse>,
}

#[derive(Debug, Serialize)]
struct PlanApprovalResponse {
    outcome: PlanApprovalOutcome,
    plan: PlanDetailResponse,
}

#[derive(Debug, Serialize)]
struct PlanResolutionResponse {
    outcome: PlanResolutionOutcome,
    plan: PlanDetailResponse,
}

#[derive(Debug, Serialize)]
struct PlanCancelResponse {
    task: TaskResponse,
    plan: PlanDetailResponse,
}

#[derive(Debug, Serialize)]
struct PlanCheckpointResponse {
    sequence: u64,
    step_id: Option<PlanStepId>,
    phase: shaka_queue::PlanCheckpointPhase,
    status: shaka_queue::PlanCheckpointStatus,
    created_at: DateTime<Utc>,
}

impl From<PlanCheckpoint> for PlanCheckpointResponse {
    fn from(checkpoint: PlanCheckpoint) -> Self {
        Self {
            sequence: checkpoint.sequence,
            step_id: checkpoint.step_id,
            phase: checkpoint.phase,
            status: checkpoint.status,
            created_at: checkpoint.created_at,
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

#[derive(Debug, Serialize)]
struct ReadinessResponse {
    status: &'static str,
    version: &'static str,
    database_integrity: bool,
    audit_chain: shaka_memory::AuditVerification,
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

async fn readyz(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<ReadinessResponse>), ApiError> {
    let span = state
        .telemetry
        .operation_span("api.readyz", &active_correlation().unwrap_or_default());
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let queue_integrity = state.queue.verify_integrity()?;
    let audit_integrity = state.audit.verify_integrity().map_err(|error| {
        warn!(
            ?error,
            "não foi possível verificar a integridade do store de auditoria"
        );
        ApiError::Internal
    })?;
    let audit_chain = state
        .audit
        .verify_audit_chain(&auth.principal.tenant_id)
        .map_err(|error| {
            warn!(?error, "não foi possível verificar a cadeia de auditoria");
            ApiError::Internal
        })?;
    let queued_tasks = state.queue.queued_count()?;
    let circuit = state.breaker.snapshot();
    let database_integrity = queue_integrity && audit_integrity;
    let ready =
        database_integrity && audit_chain.valid && matches!(circuit.state, CircuitState::Closed);
    let status = if ready { "ready" } else { "failed" };
    let http_status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    Ok((
        http_status,
        Json(ReadinessResponse {
            status,
            version: env!("CARGO_PKG_VERSION"),
            database_integrity,
            audit_chain,
            queued_tasks,
            circuit,
        }),
    ))
}

fn plan_detail(
    state: &ApiState,
    inspection: PlanInspectionReport,
) -> Result<PlanDetailResponse, ApiError> {
    let task = match state
        .queue
        .get_task(&inspection.plan.task_id, &inspection.plan.tenant_id)
    {
        Ok(task) => Some(task.into()),
        Err(QueueError::NotFound(_)) => None,
        Err(error) => return Err(error.into()),
    };
    Ok(PlanDetailResponse {
        plan: inspection.plan,
        step_states: inspection.step_states,
        integrity: inspection.status,
        integrity_issue: inspection.issue,
        checkpoints_checked: inspection.checkpoints_checked,
        transitions_checked: inspection.transitions_checked,
        task,
    })
}

fn idempotency_key(headers: &HeaderMap) -> Result<&str, ApiError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::BadRequest("header Idempotency-Key é obrigatório".to_owned()))
}

async fn create_plan(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(request): Json<PlanCreateRequest>,
) -> Result<(StatusCode, Json<PlanSpec>), ApiError> {
    let span = correlation_span(&state, "plan.create", None, None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let input = request.input;
    if input.tenant_id != auth.principal.tenant_id
        || input.operator_id != auth.principal.operator_id
    {
        return Err(ApiError::Forbidden);
    }
    if input.mode != PlanMode::DryRun {
        return Err(ApiError::BadRequest(
            "planos live permanecem bloqueados na v0.8.2".to_owned(),
        ));
    }
    let plan = PlanSpec::new(input).map_err(|error| ApiError::BadRequest(error.to_string()))?;
    let plan_id = plan.plan_id.clone();
    let step_count = plan.steps.len();
    let risk = format!("{:?}", plan.risk);
    let persisted = state.queue.save_plan(&plan)?;
    span.record("outcome", "created");
    span.record("risk_class", risk);
    span.record("step_count", step_count as u64);
    span.record("mode", "dry_run");
    audit_principal(
        &state,
        &auth.principal,
        None,
        "plan.create",
        "success",
        BTreeMap::from([(String::from("plan_id"), plan_id.0.to_string())]),
    );
    Ok((StatusCode::CREATED, Json(persisted.plan)))
}

async fn get_plan(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(plan_id): Path<Uuid>,
) -> Result<Json<PlanDetailResponse>, ApiError> {
    let plan_id = PlanId(plan_id);
    let span = correlation_span(&state, "plan.show", None, None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let inspection = state
        .queue
        .inspect_plan(&auth.principal.tenant_id, &plan_id)?;
    audit_principal(
        &state,
        &auth.principal,
        None,
        "plan.show",
        "success",
        BTreeMap::from([(String::from("plan_id"), plan_id.0.to_string())]),
    );
    Ok(Json(plan_detail(&state, inspection)?))
}

async fn validate_plan(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(plan_id): Path<Uuid>,
) -> Result<Json<shaka_core::PlanVerificationReport>, ApiError> {
    let plan_id = PlanId(plan_id);
    let span = correlation_span(&state, "plan.validate", None, None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let report = state
        .queue
        .validate_plan(&auth.principal.tenant_id, &plan_id)?;
    span.record(
        "outcome",
        if report.is_executable() {
            "valid"
        } else {
            "blocked"
        },
    );
    audit_principal(
        &state,
        &auth.principal,
        None,
        "plan.validate",
        if report.is_executable() {
            "success"
        } else {
            "blocked"
        },
        BTreeMap::from([(String::from("plan_id"), plan_id.0.to_string())]),
    );
    Ok(Json(report))
}

async fn approve_plan(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(plan_id): Path<Uuid>,
    Json(request): Json<PlanApprovalRequest>,
) -> Result<Json<PlanApprovalResponse>, ApiError> {
    let plan_id = PlanId(plan_id);
    let span = correlation_span(&state, "plan.approve", None, None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let idempotency_key = idempotency_key(&headers)?;
    let persisted = state.queue.load_plan(&plan_id, &auth.principal.tenant_id)?;
    let expires_in = request.expires_in_seconds.unwrap_or(3600);
    if !(1..=604_800).contains(&expires_in) {
        return Err(ApiError::BadRequest(
            "expires_in_seconds deve estar entre 1 e 604800".to_owned(),
        ));
    }
    let required = match &request.step_id {
        Some(step_id) => persisted
            .plan
            .steps
            .iter()
            .find(|step| &step.step_id == step_id)
            .map(|step| step.approval.max(step.risk.minimum_approval()))
            .ok_or(ApiError::NotFound)?,
        None => persisted.plan.required_approval(),
    };
    let approval = PlanApproval {
        approval_id: QueueStore::approval_id_for_idempotency(
            &plan_id,
            persisted.plan.revision,
            idempotency_key,
        ),
        plan_id: plan_id.clone(),
        plan_digest: persisted.plan.digest.clone(),
        revision: persisted.plan.revision,
        tenant_id: auth.principal.tenant_id.clone(),
        approver: auth.principal.operator_id.clone(),
        approver_role: auth.principal.role.clone(),
        step_id: request.step_id,
        required,
        decision: request.decision,
        expires_at: Utc::now() + Duration::seconds(expires_in),
        revoked: false,
    };
    let outcome = state
        .queue
        .approve_plan(&auth.principal, &approval, idempotency_key)?;
    let inspection = state
        .queue
        .inspect_plan(&auth.principal.tenant_id, &plan_id)?;
    span.record("outcome", format!("{outcome:?}"));
    audit_principal(
        &state,
        &auth.principal,
        None,
        "plan.approve",
        "success",
        BTreeMap::from([
            (String::from("plan_id"), plan_id.0.to_string()),
            (String::from("decision"), format!("{:?}", approval.decision)),
        ]),
    );
    Ok(Json(PlanApprovalResponse {
        outcome,
        plan: plan_detail(&state, inspection)?,
    }))
}

async fn resume_plan(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(plan_id): Path<Uuid>,
    Json(request): Json<PlanResumeRequest>,
) -> Result<Json<PlanResolutionResponse>, ApiError> {
    let plan_id = PlanId(plan_id);
    let span = correlation_span(&state, "plan.resume", None, None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let key = idempotency_key(&headers)?;
    let outcome = state.queue.resolve_plan_unknown(
        &auth.principal,
        &plan_id,
        PlanResolutionDecision::Resume,
        key,
        Some(&request.evidence_digest),
    )?;
    let inspection = state
        .queue
        .inspect_plan(&auth.principal.tenant_id, &plan_id)?;
    span.record("outcome", format!("{outcome:?}"));
    audit_principal(
        &state,
        &auth.principal,
        None,
        "plan.resume",
        "success",
        BTreeMap::from([(String::from("plan_id"), plan_id.0.to_string())]),
    );
    Ok(Json(PlanResolutionResponse {
        outcome,
        plan: plan_detail(&state, inspection)?,
    }))
}

async fn cancel_plan(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(plan_id): Path<Uuid>,
) -> Result<Json<PlanCancelResponse>, ApiError> {
    let plan_id = PlanId(plan_id);
    let span = correlation_span(&state, "plan.cancel", None, None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let key = idempotency_key(&headers)?;
    let persisted = state.queue.load_plan(&plan_id, &auth.principal.tenant_id)?;
    let task = if persisted.plan.state == shaka_core::PlanState::Unknown {
        state.queue.resolve_plan_unknown(
            &auth.principal,
            &plan_id,
            PlanResolutionDecision::Cancel,
            key,
            None,
        )?;
        state
            .queue
            .get_task(&persisted.plan.task_id, &auth.principal.tenant_id)?
    } else {
        state
            .queue
            .request_cancel(&persisted.plan.task_id, &auth.principal.tenant_id)?
    };
    let inspection = state
        .queue
        .inspect_plan(&auth.principal.tenant_id, &plan_id)?;
    state.cancel_running(&task.task_id);
    span.record("outcome", "success");
    audit_principal(
        &state,
        &auth.principal,
        Some(task.task_id.clone()),
        "plan.cancel",
        "success",
        BTreeMap::from([(String::from("plan_id"), plan_id.0.to_string())]),
    );
    Ok(Json(PlanCancelResponse {
        task: task.into(),
        plan: plan_detail(&state, inspection)?,
    }))
}

async fn get_plan_checkpoints(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(plan_id): Path<Uuid>,
) -> Result<Json<Vec<PlanCheckpointResponse>>, ApiError> {
    let plan_id = PlanId(plan_id);
    let span = correlation_span(&state, "plan.checkpoints", None, None);
    let _entered = span.enter();
    let auth = authorize(&headers, &state)?;
    let checkpoints = state
        .queue
        .list_plan_checkpoints(&auth.principal.tenant_id, &plan_id)?;
    span.record("outcome", "success");
    audit_principal(
        &state,
        &auth.principal,
        None,
        "plan.checkpoints",
        "success",
        BTreeMap::from([(String::from("plan_id"), plan_id.0.to_string())]),
    );
    Ok(Json(
        checkpoints
            .into_iter()
            .map(PlanCheckpointResponse::from)
            .collect(),
    ))
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

#[allow(clippy::too_many_lines)]
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
    let plan_fields_present = request.plan_id.is_some()
        || request.plan_revision.is_some()
        || request.plan_digest.is_some()
        || request.task_id.is_some();
    let plan_reference = if plan_fields_present {
        let plan_id = request.plan_id.clone().ok_or_else(|| {
            ApiError::BadRequest("plan_id é obrigatório no modo planejado".to_owned())
        })?;
        let revision = request.plan_revision.ok_or_else(|| {
            ApiError::BadRequest("plan_revision é obrigatório no modo planejado".to_owned())
        })?;
        let digest = request.plan_digest.clone().ok_or_else(|| {
            ApiError::BadRequest("plan_digest é obrigatório no modo planejado".to_owned())
        })?;
        if request.task_id.is_none() {
            return Err(ApiError::BadRequest(
                "task_id é obrigatório no modo planejado".to_owned(),
            ));
        }
        if !dry_run {
            return Err(ApiError::BadRequest(
                "tasks planejadas exigem dry-run na v0.8.2".to_owned(),
            ));
        }
        Some(
            PlanTaskReference::new(plan_id, revision, digest)
                .map_err(|error| ApiError::BadRequest(error.to_string()))?,
        )
    } else {
        None
    };
    let mut envelope = TaskEnvelope::new(
        auth.principal.tenant_id.clone(),
        auth.principal.operator_id.clone(),
        request.objective.clone(),
    )
    .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    envelope.execution_context = ExecutionContext::from_principal(&auth.principal);
    envelope.dry_run = dry_run;
    if let Some(task_id) = request.task_id.clone() {
        if plan_reference.is_none() {
            return Err(ApiError::BadRequest(
                "task_id somente pode ser usado no modo planejado".to_owned(),
            ));
        }
        envelope.task_id = task_id;
    }
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
    let request_id = active_request_id();
    let submission = state.queue.submit_task_governed_with_plan_and_provenance(
        session_id,
        &auth.principal,
        idempotency_key,
        &fingerprint,
        &envelope,
        request.priority,
        max_attempts,
        plan_reference.as_ref(),
        Some(&request_id),
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

/// Executa o servidor HTTP e inicia os workers até o shutdown gracioso.
///
/// O bind já deve ter sido validado por [`ApiState::new`]. Falhas do listener
/// são convertidas em erro interno sem expor detalhes do sistema.
pub async fn serve(state: ApiState) -> Result<(), ApiError> {
    let listener = TcpListener::bind(state.config.bind_addr)
        .await
        .map_err(|error| {
            error!(?error, "não foi possível abrir o listener HTTP");
            ApiError::Internal
        })?;
    let workers = start_workers(&state).await;
    info!(address = %state.config.bind_addr, workers = state.config.worker_count, "API persistente iniciada");
    let server_result = axum::serve(listener, state.router())
        .with_graceful_shutdown(shutdown_signal())
        .await;
    state.shutdown.cancel();
    state.cancel_all_running();
    for mut worker in workers {
        match timeout(std::time::Duration::from_secs(5), &mut worker).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(?error, "worker da fila terminou com erro"),
            Err(_) => {
                warn!("worker da fila excedeu a janela de shutdown; abortando handle próprio");
                worker.abort();
                let _ = worker.await;
            }
        }
    }
    server_result.map_err(|error| {
        error!(?error, "servidor HTTP terminou com erro");
        ApiError::Internal
    })
}

async fn queue_blocking<T, F>(operation: F) -> Result<T, QueueError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, QueueError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            QueueError::InvalidInput(format!("operação de fila bloqueada terminou: {error}"))
        })?
}

async fn start_workers(state: &ApiState) -> Vec<tokio::task::JoinHandle<()>> {
    let recovery_span = correlation_span(state, "queue.lease.recover", None, None);
    let queue = Arc::clone(&state.queue);
    match queue_blocking(move || queue.recover_expired_leases(Utc::now())).await {
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
        let breaker = Arc::clone(&state.breaker);
        match queue_blocking(move || breaker.allow(now)).await {
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
        let plan_context = PlanClaimContext {
            circuit_closed: true,
            granted_capabilities: state.runtime.granted_capabilities(),
            remaining_budget: None,
            state_digest: None,
        };
        let queue = Arc::clone(&state.queue);
        let lease_for = state.config.lease_for;
        match queue_blocking(move || {
            queue.claim_next_with_plan_context(now, lease_for, &plan_context)
        })
        .await
        {
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

#[allow(clippy::too_many_lines)]
async fn execute_task(worker_id: usize, state: ApiState, task: TaskRecord) {
    let task_id = task.task_id.clone();
    let correlation = task
        .envelope
        .execution_context
        .provenance
        .request_id
        .as_deref()
        .and_then(|request_id| CorrelationContext::with_request_id(request_id).ok())
        .unwrap_or_else(|| {
            CorrelationContext::with_request_id(format!("task-{}", task_id.0)).unwrap_or_default()
        });
    ACTIVE_CORRELATION
        .scope(correlation, async move {
            let task_span = correlation_span(&state, "worker.task.process", None, Some(&task_id));
            task_span.record("worker_id", worker_id);
            task_span.record("attempt", task.attempts);
            task_span.record("lease_state", "held");
            if let Some(plan_id) = &task.plan_id {
                task_span.record("plan_mode", "planned");
                task_span.record("plan_revision", task.plan_revision.unwrap_or_default());
                task_span.record(
                    "plan_step",
                    task.plan_step_id
                        .as_ref()
                        .map_or("none", |id| id.0.as_str()),
                );
                task_span.record("plan_reference", plan_id.0.to_string());
            } else {
                task_span.record("plan_mode", "direct");
            }
            let Some(lease_token) = task.lease_token else {
                task_span.record("outcome", "lease_missing");
                let task_label = task_id.0.to_string();
                warn!(worker_id, task_id = %task_label, "task reclamada sem token de lease; execução bloqueada");
                return;
            };
            let token = CancellationToken::new();
            state.register_cancellation(&task_id, token.clone());
            let execution = state
                .runtime
                .run_with_cancellation_and_scope(
                    task.envelope.clone(),
                    token,
                    task.plan_execution_scope.clone(),
                )
                .instrument(task_span.clone())
                .await;
            state.remove_cancellation(&task_id);
            let now = Utc::now();
            match execution {
                Ok(result) => {
                    task_span.record("outcome", "runtime_succeeded");
                    record_circuit_success(worker_id, &state).await;
                    finish_success(
                        worker_id,
                        &state,
                        &task,
                        &task_id,
                        lease_token,
                        serde_json::to_value(&result).ok(),
                        now,
                    )
                    .await;
                }
                Err(error) => {
                    if matches!(error, OrchestratorError::Cancelled)
                        && state.shutdown.is_cancelled()
                    {
                        let queue = Arc::clone(&state.queue);
                        let cancel_task_id = task_id.clone();
                        let cancel_tenant_id = task.tenant_id.clone();
                        if let Err(cancel_error) = queue_blocking(move || {
                            queue.request_cancel(&cancel_task_id, &cancel_tenant_id).map(|_| ())
                        })
                        .await
                        {
                            warn!(
                                worker_id,
                                ?cancel_error,
                                "não foi possível marcar tarefa como cancelada durante shutdown"
                            );
                        }
                    }
                    let retryable = is_retryable(&error);
                    task_span.record("outcome", "runtime_failed");
                    task_span.record("retryable", retryable);
                    task_span.record("error_type", orchestrator_error_type(&error));
                    if retryable {
                        record_circuit_failure(worker_id, &state, now).await;
                    }
                    finish_failure(
                        worker_id,
                        &state,
                        &task,
                        lease_token,
                        &error,
                        retryable,
                        now,
                    )
                    .await;
                }
            }
        })
        .await;
}

async fn record_circuit_success(worker_id: usize, state: &ApiState) {
    let circuit_span = correlation_span(state, "queue.circuit.record_success", None, None);
    let breaker = Arc::clone(&state.breaker);
    match queue_blocking(move || breaker.record_success()).await {
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

async fn record_circuit_failure(worker_id: usize, state: &ApiState, now: DateTime<Utc>) {
    let circuit_span = correlation_span(state, "queue.circuit.record_failure", None, None);
    let breaker = Arc::clone(&state.breaker);
    match queue_blocking(move || breaker.record_failure(now)).await {
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

async fn finish_success(
    worker_id: usize,
    state: &ApiState,
    task: &TaskRecord,
    task_id: &TaskId,
    lease_token: LeaseToken,
    result_json: Option<Value>,
    now: DateTime<Utc>,
) {
    let finish_span = correlation_span(state, "queue.finish", None, Some(task_id));
    let plan_context = worker_plan_context(state);
    let queue = Arc::clone(&state.queue);
    let task_id_owned = task_id.clone();
    let tenant_id = task.tenant_id.clone();
    let retry_base_delay = state.config.retry_base_delay;
    let retry_max_delay = state.config.retry_max_delay;
    let outcome = queue_blocking(move || {
        queue.finish_task_with_plan_context(
            &task_id_owned,
            &tenant_id,
            lease_token,
            result_json,
            None,
            false,
            now,
            retry_base_delay,
            retry_max_delay,
            &plan_context,
        )
    })
    .await;
    match outcome {
        Ok(outcome) => {
            finish_span.record("outcome", finish_outcome_name(&outcome));
            finish_span.record("lease_state", "released");
            audit_task_finish(state, task, &outcome).await;
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

async fn finish_failure(
    worker_id: usize,
    state: &ApiState,
    task: &TaskRecord,
    lease_token: LeaseToken,
    error: &OrchestratorError,
    retryable: bool,
    now: DateTime<Utc>,
) {
    let safe_error = shaka_core::redact_sensitive(&error.to_string());
    let finish_span = correlation_span(state, "queue.finish", None, Some(&task.task_id));
    let plan_context = worker_plan_context(state);
    let queue = Arc::clone(&state.queue);
    let task_id = task.task_id.clone();
    let tenant_id = task.tenant_id.clone();
    let retry_base_delay = state.config.retry_base_delay;
    let retry_max_delay = state.config.retry_max_delay;
    let outcome = queue_blocking(move || {
        queue.finish_task_with_plan_context(
            &task_id,
            &tenant_id,
            lease_token,
            None,
            Some(safe_error.as_str()),
            retryable,
            now,
            retry_base_delay,
            retry_max_delay,
            &plan_context,
        )
    })
    .await;
    match outcome {
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
            audit_task_finish(state, task, &outcome).await;
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

fn worker_plan_context(state: &ApiState) -> PlanClaimContext {
    PlanClaimContext {
        circuit_closed: state.breaker.snapshot().state == shaka_queue::CircuitState::Closed,
        granted_capabilities: state.runtime.granted_capabilities(),
        remaining_budget: None,
        state_digest: None,
    }
}

async fn audit_task_finish(state: &ApiState, task: &TaskRecord, outcome: &FinishOutcome) {
    let outcome_name = match outcome {
        FinishOutcome::Succeeded => "succeeded",
        FinishOutcome::PlanStepSucceeded { .. } => "plan_step_succeeded",
        FinishOutcome::Compensated => "compensated",
        FinishOutcome::Requeued { .. } => "retry_scheduled",
        FinishOutcome::Failed => "failed",
        FinishOutcome::Cancelled => "cancelled",
    };
    let mut metadata = BTreeMap::new();
    if let Some(approval_id) = task
        .envelope
        .execution_context
        .provenance
        .admission_approval_id
    {
        metadata.insert("admission_approval_id".to_owned(), approval_id.to_string());
    }
    if let Some(context) = active_correlation() {
        metadata.insert(
            "correlation_request_id".to_owned(),
            context.request_id().to_owned(),
        );
        if let Some(trace_id) = context.trace_id() {
            metadata.insert("correlation_trace_id".to_owned(), trace_id.to_owned());
        }
    }
    let audit = Arc::clone(&state.audit);
    let tenant_id = task.tenant_id.clone();
    let operator_id = task.envelope.operator_id.0.clone();
    let task_id = Some(task.task_id.clone());
    let outcome_name = outcome_name.to_owned();
    match tokio::task::spawn_blocking(move || {
        audit.record(
            task_id,
            tenant_id,
            operator_id,
            "api.task.finish",
            outcome_name,
            metadata,
        )
    })
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => warn!(?error, "evento de finalização não pôde ser auditado"),
        Err(error) => warn!(?error, "thread de auditoria terminou com erro"),
    }
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
        FinishOutcome::PlanStepSucceeded { .. } => "plan_step_succeeded",
        FinishOutcome::Compensated => "compensated",
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
        QueueError::LeaseLost => "lease_lost",
    }
}

fn orchestrator_error_type(error: &OrchestratorError) -> &'static str {
    match error {
        OrchestratorError::Core(_) => "core",
        OrchestratorError::Memory(_) => "memory",
        OrchestratorError::Http(_) => "http",
        OrchestratorError::InvalidModelResponse(_) => "invalid_model_response",
        OrchestratorError::ToolNotFound(_) => "tool_not_found",
        OrchestratorError::PlanActionDenied(_) => "plan_action_denied",
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
        "task_id": request.task_id,
        "plan_id": request.plan_id,
        "plan_revision": request.plan_revision,
        "plan_digest": request.plan_digest,
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
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    warn!(?error, "não foi possível registrar SIGTERM");
                    if let Err(ctrl_c_error) = tokio::signal::ctrl_c().await {
                        warn!(?ctrl_c_error, "não foi possível aguardar Ctrl-C");
                    }
                    return;
                }
            };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    warn!(?error, "não foi possível aguardar Ctrl-C");
                }
            }
            _ = sigterm.recv() => {
                info!("SIGTERM recebido; iniciando shutdown graceful");
            }
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(?error, "não foi possível aguardar Ctrl-C");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use shaka_core::{
        ExecutionBudget, MAX_EXECUTION_STEPS, PlanAction, PlanApprovalRequirement, PlanRisk,
        PlanSpecInput, PlanStep,
    };
    use shaka_memory::MemoryStore;
    use shaka_orchestrator::{AgentModel, EchoTool, LocalModel, OutboundMessageTool, ToolRegistry};
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

    #[derive(Debug)]
    struct RequestScopedMessageModel;

    #[async_trait]
    impl AgentModel for RequestScopedMessageModel {
        async fn complete(
            &self,
            request: ModelRequest,
        ) -> Result<ModelResponse, OrchestratorError> {
            assert!(!request.tools.iter().any(|tool| tool.name == "send_message"));
            if request.prior_tool_results.is_empty() {
                Ok(ModelResponse {
                    content: String::new(),
                    tool_calls: vec![shaka_orchestrator::ModelToolCall {
                        tool_name: "send_message".to_owned(),
                        arguments: json!({"channel":"external","message":"blocked"}),
                    }],
                    estimated_cost_microunits: 0,
                })
            } else {
                Ok(ModelResponse {
                    content: "fim".to_owned(),
                    tool_calls: Vec::new(),
                    estimated_cost_microunits: 0,
                })
            }
        }
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn operator_request_cannot_use_admin_runtime_capability() {
        let memory = Arc::new(MemoryStore::in_memory().unwrap());
        let audit = Arc::new(AuditLogger::new(Arc::clone(&memory)));
        let mut tools = ToolRegistry::with_capabilities(shaka_core::CapabilitySet(vec![
            shaka_core::Capability::ExternalMessaging,
        ]));
        tools.register(Arc::new(OutboundMessageTool)).unwrap();
        let state = ApiState::new(
            Arc::new(QueueStore::in_memory().unwrap()),
            Arc::new(AgentRuntime::new(
                Arc::new(RequestScopedMessageModel),
                memory,
                tools,
            )),
            audit,
            principal(),
            ApiConfig::default(),
        )
        .unwrap();
        let operator = shaka_core::OperatorId::new("request-operator").unwrap();
        state
            .queue
            .create_user(
                &operator,
                &state.principal.tenant_id,
                &shaka_core::Role::Operator,
            )
            .unwrap();
        let issue = state
            .queue
            .issue_token(
                &operator,
                Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            )
            .unwrap();
        let session_response = state
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
        assert_eq!(session_response.status(), StatusCode::CREATED);
        let session: SessionResponse = serde_json::from_slice(
            &axum::body::to_bytes(session_response.into_body(), 1_048_576)
                .await
                .unwrap(),
        )
        .unwrap();
        let task_response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/sessions/{}/tasks", session.session_id))
                    .header("authorization", format!("Bearer {}", issue.token))
                    .header("x-request-id", "operator-request-1")
                    .header("idempotency-key", "request-context-1")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"objective":"tentar mensageria","dry_run":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(task_response.status(), StatusCode::ACCEPTED);
        let task: Value = serde_json::from_slice(
            &axum::body::to_bytes(task_response.into_body(), 1_048_576)
                .await
                .unwrap(),
        )
        .unwrap();
        let task_id = TaskId(Uuid::parse_str(task["task_id"].as_str().unwrap()).unwrap());
        let claimed = state
            .queue
            .claim_next(Utc::now(), Duration::seconds(30))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.task_id, task_id);
        assert_eq!(
            claimed
                .envelope
                .execution_context
                .provenance
                .request_id
                .as_deref(),
            Some("operator-request-1")
        );
        execute_task(0, state.clone(), claimed).await;
        let finished = state
            .queue
            .get_task(&task_id, &state.principal.tenant_id)
            .unwrap();
        assert_eq!(
            finished
                .envelope
                .execution_context
                .provenance
                .request_id
                .as_deref(),
            Some("operator-request-1")
        );
        let result = finished.result.unwrap();
        assert_eq!(result["success"], false);
        assert_eq!(
            result["tool_results"][0]["error_code"],
            "tool_execution_failed"
        );
        assert!(
            result["tool_results"][0]["output"]["serialized"]
                .as_str()
                .unwrap()
                .contains("capacidade não autorizada")
        );
    }

    #[test]
    fn empty_static_api_key_is_rejected_even_on_loopback() {
        let base = state();
        let config = ApiConfig {
            api_key: Some(String::new()),
            ..ApiConfig::default()
        };
        let configured = ApiState::new(
            base.queue.clone(),
            base.runtime.clone(),
            base.audit.clone(),
            base.principal.clone(),
            config,
        );
        assert!(matches!(
            configured,
            Err(ApiError::BadRequest(message)) if message.contains("não pode ser vazia")
        ));
    }

    #[tokio::test]
    async fn api_rejects_oversized_json_body_before_handler() {
        let payload = format!(
            r#"{{"metadata":{{"padding":"{}"}}}}"#,
            "x".repeat(3 * 1024 * 1024)
        );
        let response = state()
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/sessions")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn api_rejects_budget_above_host_maximum() {
        let state = state();
        let issue = state
            .queue
            .issue_token(
                &state.principal().operator_id,
                Some(Utc::now() + Duration::hours(1)),
            )
            .unwrap();
        let session_response = state
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
        assert_eq!(session_response.status(), StatusCode::CREATED);
        let session: SessionResponse = serde_json::from_slice(
            &axum::body::to_bytes(session_response.into_body(), 1_048_576)
                .await
                .unwrap(),
        )
        .unwrap();

        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/sessions/{}/tasks", session.session_id))
                    .header("authorization", format!("Bearer {}", issue.token))
                    .header("idempotency-key", "budget-above-max-1")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&json!({
                            "objective": "budget acima do máximo",
                            "dry_run": true,
                            "budget": {
                                "max_steps": MAX_EXECUTION_STEPS + 1,
                                "max_tool_calls": 16,
                                "max_elapsed_ms": 30_000,
                                "max_cost_microunits": 1_000_000
                            }
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1_048_576)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(body.to_string().contains("budget.max_steps"));
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
    async fn readiness_reports_operational_state_and_rejects_invalid_bearer() {
        let state = state();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/readyz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "ready");
        assert_eq!(payload["database_integrity"], true);
        assert_eq!(payload["audit_chain"]["valid"], true);

        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/readyz")
                    .header("authorization", "Bearer invalid-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn readiness_requires_bearer_when_api_key_is_configured() {
        let base = state();
        let configured = ApiState::new(
            base.queue.clone(),
            base.runtime.clone(),
            base.audit.clone(),
            base.principal.clone(),
            ApiConfig {
                api_key: Some("static-readiness-key".to_owned()),
                ..ApiConfig::default()
            },
        )
        .unwrap();
        let response = configured
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/readyz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = configured
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/readyz")
                    .header("authorization", "Bearer static-readiness-key")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readiness_fails_closed_when_circuit_is_open() {
        let state = state();
        for _ in 0..3 {
            state.breaker.record_failure(Utc::now()).unwrap();
        }
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/readyz")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let payload: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["circuit"]["state"], "open");
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
    #[allow(clippy::too_many_lines)]
    async fn plan_routes_enforce_tenant_mode_and_human_approval() {
        let state = state();
        let plan_id = PlanId::new();
        let task_id = TaskId::new();
        let input = PlanSpecInput {
            plan_id,
            task_id,
            tenant_id: state.principal.tenant_id.clone(),
            operator_id: state.principal.operator_id.clone(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::Mutation,
            approval: PlanApprovalRequirement::Reviewer,
            budget: ExecutionBudget::default(),
            steps: vec![PlanStep {
                step_id: PlanStepId::new("write-preview").unwrap(),
                depends_on: Vec::new(),
                action: PlanAction::Mutation {
                    operation: "write-preview".to_owned(),
                },
                preconditions: Vec::new(),
                postconditions: Vec::new(),
                risk: PlanRisk::Mutation,
                approval: PlanApprovalRequirement::Reviewer,
                max_attempts: 1,
                compensation_step_id: None,
            }],
        };
        let body = serde_json::to_vec(&input).unwrap();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/plans")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let plan: PlanSpec = serde_json::from_slice(&body).unwrap();
        assert_eq!(plan.plan_id, input.plan_id);
        assert_eq!(plan.mode, PlanMode::DryRun);

        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/plans/{}/validate", plan.plan_id.0))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let validation: shaka_core::PlanVerificationReport = serde_json::from_slice(&body).unwrap();
        assert!(validation.is_valid());

        let reviewer = shaka_core::OperatorId::new("reviewer-api").unwrap();
        state
            .queue
            .create_user(
                &reviewer,
                &state.principal.tenant_id,
                &shaka_core::Role::Reviewer,
            )
            .unwrap();
        let token = state
            .queue
            .issue_token(
                &reviewer,
                Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            )
            .unwrap();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/plans/{}/approve", plan.plan_id.0))
                    .header("authorization", format!("Bearer {}", token.token))
                    .header("idempotency-key", "api-approval-1")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"decision":"approved"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let approval: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(approval["outcome"], "Approved");
        assert_eq!(approval["plan"]["plan"]["state"], "approved");

        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/plans/{}/approve", plan.plan_id.0))
                    .header("authorization", format!("Bearer {}", token.token))
                    .header("idempotency-key", "api-approval-1")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"decision":"approved"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let replay: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(replay["outcome"], "Existing");

        let session = state
            .queue
            .create_session(state.principal.clone(), Value::Null)
            .unwrap();
        let mut envelope = TaskEnvelope::new(
            state.principal.tenant_id.clone(),
            state.principal.operator_id.clone(),
            "planned api task",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        state
            .queue
            .submit_task_governed_with_plan(
                session.session_id,
                &state.principal,
                "api-plan-task-1",
                "api-plan-task-fingerprint-1",
                &envelope,
                1,
                1,
                Some(&reference),
            )
            .unwrap();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/v1/plans/{}", plan.plan_id.0))
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
                    .uri(format!("/v1/plans/{}/cancel", plan.plan_id.0))
                    .header("idempotency-key", "api-cancel-1")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let cancelled: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(cancelled["plan"]["plan"]["state"], "cancelled");

        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri(format!("/v1/plans/{}/checkpoints", plan.plan_id.0))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap();
        let checkpoints: Value = serde_json::from_slice(&body).unwrap();
        assert!(
            checkpoints
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );

        let mut forged = input.clone();
        forged.tenant_id = TenantId::new("other-tenant").unwrap();
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/plans")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&forged).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        forged.tenant_id = state.principal.tenant_id.clone();
        forged.mode = PlanMode::Live;
        let response = state
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/plans")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&forged).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
        let issue = state
            .queue
            .issue_token(
                &operator,
                Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            )
            .unwrap();
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
    fn rejected_non_local_bind_does_not_persist_bootstrap_principal() {
        let memory = Arc::new(MemoryStore::in_memory().unwrap());
        let audit = Arc::new(AuditLogger::new(Arc::clone(&memory)));
        let mut tools = ToolRegistry::with_capabilities(shaka_core::CapabilitySet(Vec::new()));
        tools.register(Arc::new(EchoTool)).unwrap();
        let runtime = Arc::new(AgentRuntime::new(Arc::new(LocalModel), memory, tools));
        let queue = Arc::new(QueueStore::in_memory().unwrap());
        let principal = Principal {
            operator_id: shaka_core::OperatorId::new("bootstrap-operator").unwrap(),
            tenant_id: TenantId::new("bootstrap-tenant").unwrap(),
            role: shaka_core::Role::Administrator,
        };
        assert!(queue.list_tenants().unwrap().is_empty());
        let config = ApiConfig {
            bind_addr: "0.0.0.0:8080".parse().unwrap(),
            ..ApiConfig::default()
        };
        let result = ApiState::new(queue.clone(), runtime, audit, principal, config);
        assert!(matches!(
            result,
            Err(ApiError::BadRequest(message)) if message.contains("bind não local")
        ));
        assert!(queue.list_tenants().unwrap().is_empty());
        assert!(!queue.has_active_tokens().unwrap());
    }

    #[test]
    fn non_local_bind_with_api_key_bootstraps_principal() {
        let memory = Arc::new(MemoryStore::in_memory().unwrap());
        let audit = Arc::new(AuditLogger::new(Arc::clone(&memory)));
        let mut tools = ToolRegistry::with_capabilities(shaka_core::CapabilitySet(Vec::new()));
        tools.register(Arc::new(EchoTool)).unwrap();
        let queue = Arc::new(QueueStore::in_memory().unwrap());
        let principal = Principal {
            operator_id: shaka_core::OperatorId::new("api-key-operator").unwrap(),
            tenant_id: TenantId::new("api-key-tenant").unwrap(),
            role: shaka_core::Role::Administrator,
        };
        let _state = ApiState::new(
            queue.clone(),
            Arc::new(AgentRuntime::new(Arc::new(LocalModel), memory, tools)),
            audit,
            principal,
            ApiConfig {
                bind_addr: "0.0.0.0:8080".parse().unwrap(),
                api_key: Some("non-empty-key".to_owned()),
                ..ApiConfig::default()
            },
        )
        .unwrap();
        assert_eq!(queue.list_tenants().unwrap().len(), 1);
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
            ..Default::default()
        };
        let mut second = first.clone();
        second.objective = "two".to_owned();
        assert_ne!(
            fingerprint(&first, "key").unwrap(),
            fingerprint(&second, "key").unwrap()
        );
    }
}
