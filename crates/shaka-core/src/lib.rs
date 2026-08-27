//! Contratos centrais e tipos compartilhados do agente Shaka.

/// Verificação determinística da estrutura e das políticas de um plano.
pub mod plan_verifier;

pub use plan_verifier::{
    PlanVerificationContext, PlanVerificationPhase, PlanVerificationReport, PlanVerificationStatus,
    PlanVerifier, PlanViolation, PlanViolationCode,
};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};
use thiserror::Error;
use uuid::Uuid;

/// Alias do valor JSON usado nos contratos serializados do núcleo.
pub type JsonValue = serde_json::Value;

/// Identificador UUID de uma tarefa admitida pelo agente.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TaskId(pub Uuid);

impl TaskId {
    /// Gera um identificador de tarefa aleatório e único no processo.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// Identificador validado do tenant ao qual um recurso pertence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TenantId(pub String);

impl TenantId {
    /// Cria um tenant com identificador não vazio de até 128 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 128 {
            return Err(CoreError::InvalidIdentifier("tenant_id".to_owned()));
        }
        Ok(Self(value))
    }
}

/// Identificador validado do operador responsável por uma ação.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct OperatorId(pub String);

impl OperatorId {
    /// Cria um operador com identificador não vazio de até 128 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 128 {
            return Err(CoreError::InvalidIdentifier("operator_id".to_owned()));
        }
        Ok(Self(value))
    }
}

/// Papel RBAC associado ao principal que opera o agente.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    /// Operador limitado a execução somente leitura e criação de skills.
    Operator,
    /// Revisor autorizado a aprovar determinados artefatos e planos.
    Reviewer,
    /// Administrador com autorização para ações administrativas protegidas.
    Administrator,
}

/// Ação protegida pelo host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Action {
    /// Executa uma operação sem efeitos externos.
    RunReadOnly,
    /// Executa uma operação com efeito externo.
    RunExternal,
    /// Registra uma nova skill candidata.
    CreateSkill,
    /// Aprova uma skill para o próximo estágio do ciclo de vida.
    ApproveSkill,
    /// Revoga uma skill ativa.
    RevokeSkill,
    /// Cria ou exporta um backup permitido.
    Backup,
    /// Restaura dados a partir de um backup permitido.
    Restore,
    /// Verifica a cadeia de auditoria.
    VerifyAudit,
    /// Remove memória conforme a política de retenção.
    PurgeMemory,
    /// Gerencia identidades e autorização persistentes.
    ManageIam,
    /// Aprova um plano de execução.
    ApprovePlan,
    /// Retoma um plano pausado ou desconhecido sob governança.
    ResumePlan,
    /// Resolve explicitamente um plano em estado desconhecido.
    ResolvePlanUnknown,
}

/// Identidade autenticada e escopo de tenant usado nas decisões do host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Principal {
    /// Identidade do operador autenticado.
    pub operator_id: OperatorId,
    /// Tenant no qual o principal exerce autoridade.
    pub tenant_id: TenantId,
    /// Papel RBAC usado para avaliar as ações.
    pub role: Role,
}

impl Principal {
    /// Verifica se o principal possui autorização para a ação solicitada.
    #[must_use]
    pub fn allows(&self, action: &Action) -> bool {
        match self.role {
            Role::Administrator => true,
            Role::Reviewer => matches!(
                action,
                Action::RunReadOnly
                    | Action::ApproveSkill
                    | Action::VerifyAudit
                    | Action::Backup
                    | Action::ApprovePlan
            ),
            Role::Operator => matches!(action, Action::RunReadOnly | Action::CreateSkill),
        }
    }
}

/// Proveniência estável da intenção que originou uma execução.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestProvenance {
    /// Identificador de correlação fornecido pelo host de admissão.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Aprovação global usada na admissão de uma task planejada, quando houver.
    #[serde(default)]
    pub admission_approval_id: Option<Uuid>,
}

/// Contexto efetivo que deve acompanhar uma execução desde a admissão até o runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionContext {
    /// Papel do principal no momento da admissão.
    pub role: Role,
    /// Capabilities efetivas concedidas pelo host para esta execução.
    pub capabilities: CapabilitySet,
    /// Proveniência estável da requisição e da aprovação de admissão.
    #[serde(default)]
    pub provenance: RequestProvenance,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self {
            role: Role::Operator,
            capabilities: CapabilitySet::default(),
            provenance: RequestProvenance::default(),
        }
    }
}

impl ExecutionContext {
    /// Deriva capabilities sem privilégio implícito a partir do principal autenticado.
    #[must_use]
    pub fn from_principal(principal: &Principal) -> Self {
        Self {
            role: principal.role.clone(),
            capabilities: CapabilitySet::for_role(&principal.role),
            provenance: RequestProvenance::default(),
        }
    }

    /// Anexa proveniência derivada pelo host sem alterar a autorização efetiva.
    #[must_use]
    pub fn with_provenance(
        mut self,
        request_id: Option<String>,
        admission_approval_id: Option<Uuid>,
    ) -> Self {
        self.provenance = RequestProvenance {
            request_id,
            admission_approval_id,
        };
        self
    }
}

fn sensitive_text_pattern() -> &'static Result<Regex, regex::Error> {
    static PATTERN: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(concat!(
            r"(?i)\b(?:authorization|proxy-authorization)\s*[:=]\s*Bearer\s+[A-Za-z0-9._~+/=-]+|\bBearer\s+[A-Za-z0-9._~+/=-]+|",
            r#"(?:["']?)(?:api[_-]?key|access[_-]?token|secret|password|authorization|cookie|set-cookie|private[_-]?key|token)(?:["']?)"#,
            r#"\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s,;}\]]+)"#
        ))
    })
}

/// Remove padrões comuns de credenciais antes de gravar texto em logs ou memória.
#[must_use]
pub fn redact_sensitive(input: &str) -> String {
    match sensitive_text_pattern() {
        Ok(pattern) => pattern
            .replace_all(input, |captures: &regex::Captures<'_>| {
                let value = captures.get(0).map_or("", |match_| match_.as_str());
                if value.to_ascii_lowercase().starts_with("bearer") {
                    "Bearer [REDACTED]".to_owned()
                } else {
                    let key = value
                        .split([':', '='])
                        .next()
                        .unwrap_or("secret")
                        .trim_matches(|character| character == '"' || character == char::from(39));
                    format!("{key}=[REDACTED]")
                }
            })
            .into_owned(),
        Err(_) => "[REDACTION_FAILED]".to_owned(),
    }
}

/// Limites superiores host-side para evitar budgets patológicos.
pub const MAX_EXECUTION_STEPS: u32 = 256;
/// Limite superior de chamadas de ferramentas por execução.
pub const MAX_TOOL_CALLS: u32 = 512;
/// Limite superior de duração de uma execução, em milissegundos.
pub const MAX_EXECUTION_ELAPSED_MS: u64 = 300_000;
/// Limite superior de custo contabilizado, em microunits.
pub const MAX_EXECUTION_COST_MICROUNITS: u64 = 10_000_000;

/// Limites máximos de recursos para uma execução de tarefa ou plano.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionBudget {
    /// Número máximo de etapas permitidas.
    pub max_steps: u32,
    /// Número máximo de chamadas de ferramenta permitidas.
    pub max_tool_calls: u32,
    /// Duração máxima da execução em milissegundos.
    pub max_elapsed_ms: u64,
    /// Custo máximo contabilizado em microunidades.
    pub max_cost_microunits: u64,
}

impl ExecutionBudget {
    /// Valida limites de segurança antes de admitir ou executar uma task.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.max_steps == 0 || self.max_steps > MAX_EXECUTION_STEPS {
            return Err(CoreError::InvalidInput(format!(
                "budget.max_steps deve estar entre 1 e {MAX_EXECUTION_STEPS}"
            )));
        }
        if self.max_tool_calls > MAX_TOOL_CALLS {
            return Err(CoreError::InvalidInput(format!(
                "budget.max_tool_calls não pode exceder {MAX_TOOL_CALLS}"
            )));
        }
        if self.max_elapsed_ms == 0 || self.max_elapsed_ms > MAX_EXECUTION_ELAPSED_MS {
            return Err(CoreError::InvalidInput(format!(
                "budget.max_elapsed_ms deve estar entre 1 e {MAX_EXECUTION_ELAPSED_MS}"
            )));
        }
        if self.max_cost_microunits > MAX_EXECUTION_COST_MICROUNITS {
            return Err(CoreError::InvalidInput(format!(
                "budget.max_cost_microunits não pode exceder {MAX_EXECUTION_COST_MICROUNITS}"
            )));
        }
        Ok(())
    }
}

impl Default for ExecutionBudget {
    fn default() -> Self {
        Self {
            max_steps: 32,
            max_tool_calls: 16,
            max_elapsed_ms: 30_000,
            max_cost_microunits: 1_000_000,
        }
    }
}

/// Envelope persistente que vincula uma tarefa à identidade, tenant e budget efetivos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEnvelope {
    /// Identificador estável da tarefa admitida.
    pub task_id: TaskId,
    /// Tenant proprietário da tarefa.
    pub tenant_id: TenantId,
    /// Operador responsável pela admissão da tarefa.
    pub operator_id: OperatorId,
    /// Objetivo textual que será processado pelo agente.
    pub objective: String,
    /// Limites de passos, chamadas, tempo e custo da tarefa.
    pub budget: ExecutionBudget,
    /// Indica que a execução deve permanecer sem efeitos externos.
    pub dry_run: bool,
    /// Contexto efetivo por request usado nas decisões do host.
    #[serde(default)]
    pub execution_context: ExecutionContext,
    /// Instante de admissão da tarefa em UTC.
    pub created_at: DateTime<Utc>,
}

impl TaskEnvelope {
    /// Cria uma tarefa em `dry_run` com budget e contexto padrão.
    pub fn new(
        tenant_id: TenantId,
        operator_id: OperatorId,
        objective: impl Into<String>,
    ) -> Result<Self, CoreError> {
        let objective = objective.into();
        if objective.trim().is_empty() || objective.len() > 32_000 {
            return Err(CoreError::InvalidInput("objective".to_owned()));
        }
        Ok(Self {
            task_id: TaskId::new(),
            tenant_id,
            operator_id,
            objective,
            budget: ExecutionBudget::default(),
            dry_run: true,
            execution_context: ExecutionContext::default(),
            created_at: Utc::now(),
        })
    }
}

/// Identificador estável de um plano de execução.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PlanId(pub Uuid);

impl PlanId {
    /// Gera um identificador aleatório para um plano.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for PlanId {
    fn default() -> Self {
        Self::new()
    }
}

/// Identificador textual e estável de uma etapa dentro de um plano.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlanStepId(pub String);

impl PlanStepId {
    /// Cria um identificador de etapa após validar sua forma canônica.
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        validate_plan_key(&value, "step_id")?;
        Ok(Self(value))
    }
}

/// Modo explícito de execução do plano.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    /// Executa somente simulações e validações sem efeitos externos.
    #[default]
    DryRun,
    /// Permite execução ao vivo quando todas as políticas forem satisfeitas.
    Live,
}

/// Classificação de risco monotônica de uma etapa ou plano.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PlanRisk {
    /// Operação sem mutação ou efeito externo.
    ReadOnly,
    /// Operação que altera estado local persistente.
    Mutation,
    /// Operação que interage com sistema ou destinatário externo.
    ExternalEffect,
    /// Operação não reversível; bloqueada pela política atual.
    Irreversible,
}

impl PlanRisk {
    /// Informa se o risco exige aprovação humana antes da execução.
    #[must_use]
    pub const fn requires_approval(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    /// Retorna o nível mínimo de aprovação exigido pelo risco.
    #[must_use]
    pub const fn minimum_approval(self) -> PlanApprovalRequirement {
        match self {
            Self::ReadOnly => PlanApprovalRequirement::None,
            Self::Mutation => PlanApprovalRequirement::Reviewer,
            Self::ExternalEffect | Self::Irreversible => PlanApprovalRequirement::Administrator,
        }
    }
}

/// Nível mínimo de aprovação humana exigido por uma etapa ou plano.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PlanApprovalRequirement {
    /// Nenhuma aprovação humana adicional é exigida.
    None,
    /// Aprovação de revisor ou administrador é exigida.
    Reviewer,
    /// Aprovação exclusiva de administrador é exigida.
    Administrator,
}

impl PlanApprovalRequirement {
    /// Informa se um papel pode satisfazer este requisito de aprovação.
    #[must_use]
    pub const fn allows_role(self, role: &Role) -> bool {
        match self {
            Self::None => true,
            Self::Reviewer => matches!(role, Role::Reviewer | Role::Administrator),
            Self::Administrator => matches!(role, Role::Administrator),
        }
    }
}

/// Estado persistente do reducer de um plano.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    /// Plano criado, ainda sem proposta formal.
    Draft,
    /// Plano proposto para validação.
    Proposed,
    /// Plano que passou pela validação estrutural.
    Validated,
    /// Plano aguardando aprovação humana.
    AwaitingApproval,
    /// Plano autorizado para execução.
    Approved,
    /// Plano com execução em andamento.
    Running,
    /// Plano pausado e elegível para retomada governada.
    Paused,
    /// Plano concluído com sucesso.
    Succeeded,
    /// Plano encerrado por falha.
    Failed,
    /// Solicitação de cancelamento registrada.
    CancelRequested,
    /// Plano cancelado.
    Cancelled,
    /// Plano em compensação de efeitos parciais.
    Compensating,
    /// Compensação concluída.
    Compensated,
    /// Estado indeterminado que exige resolução explícita.
    Unknown,
    /// Plano rejeitado durante aprovação ou validação.
    Rejected,
}

impl PlanState {
    /// Indica se o plano não admite novas transições normais.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Compensated | Self::Rejected
        )
    }

    /// Valida a transição de estado contra a máquina de estados do plano.
    pub fn validate_transition(self, next: Self) -> Result<(), CoreError> {
        let valid = match self {
            Self::Draft => matches!(next, Self::Proposed | Self::Rejected),
            Self::Proposed => matches!(next, Self::Validated | Self::Rejected),
            Self::Validated => matches!(
                next,
                Self::AwaitingApproval | Self::Approved | Self::Rejected
            ),
            Self::AwaitingApproval => matches!(next, Self::Approved | Self::Rejected),
            Self::Approved => {
                matches!(next, Self::Running | Self::Rejected | Self::CancelRequested)
            }
            Self::Running => matches!(
                next,
                Self::Paused
                    | Self::Succeeded
                    | Self::Failed
                    | Self::CancelRequested
                    | Self::Compensating
                    | Self::Unknown
            ),
            Self::Paused => matches!(next, Self::Running | Self::CancelRequested | Self::Failed),
            Self::CancelRequested => {
                matches!(next, Self::Cancelled | Self::Unknown | Self::Compensating)
            }
            Self::Compensating => matches!(next, Self::Compensated | Self::Failed | Self::Unknown),
            Self::Unknown => matches!(
                next,
                Self::Running | Self::Failed | Self::Compensating | Self::Cancelled
            ),
            Self::Succeeded
            | Self::Failed
            | Self::Cancelled
            | Self::Compensated
            | Self::Rejected => false,
        };
        if valid {
            Ok(())
        } else {
            Err(CoreError::PlanInvalid(format!(
                "transição de plano inválida: {self:?} -> {next:?}"
            )))
        }
    }
}

/// Estado persistente de uma etapa do plano.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepState {
    /// Etapa criada, aguardando que suas pré-condições sejam avaliadas.
    Pending,
    /// Etapa pronta para execução ou aprovação.
    Ready,
    /// Etapa em execução.
    Running,
    /// Etapa concluída com sucesso.
    Succeeded,
    /// Etapa encerrada por falha.
    Failed,
    /// Etapa impedida por uma condição ou política.
    Blocked,
    /// Etapa aguardando aprovação humana.
    AwaitingApproval,
    /// Solicitação de cancelamento da etapa registrada.
    CancelRequested,
    /// Etapa cancelada.
    Cancelled,
    /// Etapa em compensação.
    Compensating,
    /// Compensação da etapa concluída.
    Compensated,
    /// Estado indeterminado que exige resolução explícita.
    Unknown,
}

impl PlanStepState {
    /// Valida a transição de estado contra a máquina da etapa.
    pub fn validate_transition(self, next: Self) -> Result<(), CoreError> {
        let valid = match self {
            Self::Pending => matches!(next, Self::Ready | Self::Blocked | Self::Cancelled),
            Self::Ready => matches!(
                next,
                Self::Running | Self::AwaitingApproval | Self::Blocked | Self::Cancelled
            ),
            Self::AwaitingApproval => matches!(next, Self::Ready | Self::Blocked | Self::Cancelled),
            Self::Running => matches!(
                next,
                Self::Succeeded
                    | Self::Failed
                    | Self::CancelRequested
                    | Self::Unknown
                    | Self::Compensating
            ),
            Self::Failed => matches!(next, Self::Ready | Self::Compensating | Self::Unknown),
            Self::CancelRequested => matches!(next, Self::Cancelled | Self::Unknown),
            Self::Compensating => matches!(next, Self::Compensated | Self::Failed | Self::Unknown),
            Self::Unknown => matches!(
                next,
                Self::Ready | Self::Failed | Self::Compensating | Self::Cancelled
            ),
            Self::Succeeded => matches!(next, Self::Compensating),
            Self::Blocked | Self::Cancelled | Self::Compensated => false,
        };
        if valid {
            Ok(())
        } else {
            Err(CoreError::PlanInvalid(format!(
                "transição de etapa inválida: {self:?} -> {next:?}"
            )))
        }
    }
}

/// Estado de tarefa que pode ser usado como pré-condição sem depender do crate de fila.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanTaskState {
    /// Tarefa admitida e aguardando execução.
    Queued,
    /// Tarefa em execução.
    Running,
    /// Tarefa concluída com sucesso.
    Succeeded,
    /// Tarefa encerrada por falha.
    Failed,
    /// Tarefa cancelada.
    Cancelled,
}

/// Predicados fechados e verificáveis pelo host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlanCondition {
    /// Exige que a tarefa esteja no estado especificado.
    TaskStateIs {
        /// Estado persistente exigido para a tarefa.
        state: PlanTaskState,
    },
    /// Exige que uma etapa predecessora tenha sucesso.
    StepSucceeded {
        /// Identificador da etapa predecessora.
        step_id: PlanStepId,
    },
    /// Exige uma aprovação global ou específica da etapa.
    ApprovalExists {
        /// Etapa cuja aprovação deve existir; `None` representa aprovação global.
        step_id: Option<PlanStepId>,
    },
    /// Exige que uma capability tenha sido concedida ao contexto.
    CapabilityGranted {
        /// Capability exigida pela condição.
        capability: Capability,
    },
    /// Exige que o circuit breaker esteja fechado.
    CircuitClosed,
    /// Exige que o budget restante seja suficiente.
    BudgetRemainingAtLeast {
        /// Limites mínimos restantes para prosseguir.
        budget: ExecutionBudget,
    },
    /// Exige que um artefato corresponda ao hash aprovado.
    ArtifactDigestMatches {
        /// Identificador do artefato verificado.
        artifact: String,
        /// SHA-256 esperado do artefato.
        sha256: String,
    },
    /// Exige que uma referência de idempotência ainda não tenha sido usada.
    IdempotencyKeyUnused {
        /// Referência da chave de idempotência.
        key_ref: String,
    },
    /// Exige que o digest de estado corresponda ao esperado.
    StateDigestMatches {
        /// SHA-256 esperado do estado.
        sha256: String,
    },
}

impl PlanCondition {
    fn validate(&self) -> Result<(), CoreError> {
        match self {
            Self::ArtifactDigestMatches { artifact, sha256 } => {
                validate_plan_key(artifact, "artifact")?;
                validate_sha256(sha256, "artifact sha256")?;
            }
            Self::IdempotencyKeyUnused { key_ref } => validate_plan_key(key_ref, "key_ref")?,
            Self::StateDigestMatches { sha256 } => validate_sha256(sha256, "state sha256")?,
            Self::StepSucceeded { step_id }
            | Self::ApprovalExists {
                step_id: Some(step_id),
            } => validate_plan_key(&step_id.0, "step_id")?,
            Self::TaskStateIs { .. }
            | Self::ApprovalExists { step_id: None }
            | Self::CapabilityGranted { .. }
            | Self::CircuitClosed
            | Self::BudgetRemainingAtLeast { .. } => {}
        }
        Ok(())
    }
}

/// Ação allowlisted que uma etapa pode solicitar ao host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlanAction {
    /// Operação allowlisted sem mutação ou efeito externo.
    ReadOnly {
        /// Nome da operação somente leitura.
        operation: String,
    },
    /// Operação que altera estado local.
    Mutation {
        /// Nome da operação mutável.
        operation: String,
    },
    /// Chamada de ferramenta explicitamente registrada.
    ExecuteTool {
        /// Nome exato da ferramenta permitida.
        tool_name: String,
    },
    /// Operação com efeito em sistema ou destinatário externo.
    ExternalEffect {
        /// Nome da operação externa.
        operation: String,
    },
}

impl PlanAction {
    fn validate(&self) -> Result<(), CoreError> {
        match self {
            Self::ReadOnly { operation }
            | Self::Mutation { operation }
            | Self::ExternalEffect { operation } => validate_plan_key(operation, "operation"),
            Self::ExecuteTool { tool_name } => validate_plan_key(tool_name, "tool_name"),
        }
    }

    /// Retorna o risco mínimo inerente à ação declarada.
    #[must_use]
    pub const fn minimum_risk(&self) -> PlanRisk {
        match self {
            Self::ReadOnly { .. } => PlanRisk::ReadOnly,
            Self::Mutation { .. } | Self::ExecuteTool { .. } => PlanRisk::Mutation,
            Self::ExternalEffect { .. } => PlanRisk::ExternalEffect,
        }
    }
}

/// Escopo host-side da etapa de plano atualmente aprovada e reclamada.
///
/// O escopo não é serializado na resposta pública nem aceito pelo cliente.
/// Ele é reconstruído a partir da revisão imutável do plano no claim e
/// revalidado no ponto de execução da ferramenta.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanExecutionScope {
    /// Plano cuja revisão foi vinculada à tarefa.
    pub plan_id: PlanId,
    /// Etapa atualmente autorizada.
    pub step_id: PlanStepId,
    /// Ação aprovada para a etapa.
    pub action: PlanAction,
}

impl PlanExecutionScope {
    /// Cria um escopo para a etapa selecionada pelo reducer persistente.
    #[must_use]
    pub const fn new(plan_id: PlanId, step_id: PlanStepId, action: PlanAction) -> Self {
        Self {
            plan_id,
            step_id,
            action,
        }
    }

    /// Informa se uma definição de ferramenta pertence exatamente ao escopo.
    ///
    /// Ações textuais (`ReadOnly`, `Mutation` e `ExternalEffect`) não são
    /// convertidas implicitamente em nomes de ferramentas: sem um registry
    /// explícito de operações, elas não concedem tool calls.
    #[must_use]
    pub fn allows_tool(&self, definition: &ToolDefinition) -> bool {
        matches!(&self.action, PlanAction::ExecuteTool { tool_name } if tool_name == &definition.name)
    }
}

/// Etapa declarativa, imutável após a aprovação da revisão.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    /// Identificador estável da etapa.
    pub step_id: PlanStepId,
    /// Etapas que precisam ser concluídas antes desta.
    pub depends_on: Vec<PlanStepId>,
    /// Ação declarada que a etapa pode solicitar.
    pub action: PlanAction,
    /// Condições verificadas antes da execução.
    pub preconditions: Vec<PlanCondition>,
    /// Condições verificadas após a execução.
    pub postconditions: Vec<PlanCondition>,
    /// Risco declarado para a etapa.
    pub risk: PlanRisk,
    /// Aprovação mínima exigida pela etapa.
    pub approval: PlanApprovalRequirement,
    /// Número máximo de tentativas da etapa.
    pub max_attempts: u32,
    /// Etapa usada para compensar efeitos parciais, quando declarada.
    pub compensation_step_id: Option<PlanStepId>,
}

impl PlanStep {
    fn validate(&self) -> Result<(), CoreError> {
        validate_plan_key(&self.step_id.0, "step_id")?;
        self.action.validate()?;
        if self.max_attempts == 0 || self.max_attempts > 10 {
            return Err(CoreError::PlanInvalid(format!(
                "max_attempts inválido na etapa {}",
                self.step_id.0
            )));
        }
        if self.risk == PlanRisk::Irreversible {
            return Err(CoreError::PlanInvalid(
                "ações irreversíveis estão bloqueadas por default".to_owned(),
            ));
        }
        if self.risk < self.action.minimum_risk() {
            return Err(CoreError::PlanInvalid(format!(
                "risco declarado menor que o risco da ação na etapa {}",
                self.step_id.0
            )));
        }
        if self.approval < self.risk.minimum_approval() {
            return Err(CoreError::PlanInvalid(format!(
                "aprovação insuficiente na etapa {}",
                self.step_id.0
            )));
        }
        for condition in self.preconditions.iter().chain(&self.postconditions) {
            condition.validate()?;
        }
        if let Some(compensation) = &self.compensation_step_id {
            validate_plan_key(&compensation.0, "compensation_step_id")?;
        }
        Ok(())
    }
}

/// Especificação canônica de um plano de execução.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSpecInput {
    /// Identificador do plano.
    pub plan_id: PlanId,
    /// Tarefa à qual o plano pertence.
    pub task_id: TaskId,
    /// Tenant proprietário do plano.
    pub tenant_id: TenantId,
    /// Operador responsável pela proposta.
    pub operator_id: OperatorId,
    /// Modo solicitado para a execução.
    pub mode: PlanMode,
    /// Risco máximo declarado para o plano.
    pub risk: PlanRisk,
    /// Aprovação mínima exigida pelo plano.
    pub approval: PlanApprovalRequirement,
    /// Budget aplicado a todas as etapas.
    pub budget: ExecutionBudget,
    /// Etapas declaradas na ordem de apresentação do plano.
    pub steps: Vec<PlanStep>,
}

/// Especificação canônica de um plano de execução.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSpec {
    /// Identificador do plano.
    pub plan_id: PlanId,
    /// Tarefa à qual o plano pertence.
    pub task_id: TaskId,
    /// Tenant proprietário do plano.
    pub tenant_id: TenantId,
    /// Operador responsável pelo plano.
    pub operator_id: OperatorId,
    /// Revisão monotônica do plano.
    pub revision: u32,
    /// Modo de execução aprovado.
    pub mode: PlanMode,
    /// Risco máximo declarado para o plano.
    pub risk: PlanRisk,
    /// Aprovação mínima exigida pelo plano.
    pub approval: PlanApprovalRequirement,
    /// Budget aplicado ao plano.
    pub budget: ExecutionBudget,
    /// Etapas declarativas do plano.
    pub steps: Vec<PlanStep>,
    /// Digest canônico usado para vincular a revisão.
    pub digest: String,
    /// Estado persistente atual do plano.
    pub state: PlanState,
    /// Instante de criação do plano em UTC.
    pub created_at: DateTime<Utc>,
}

impl PlanSpec {
    /// Cria uma especificação validada e calcula seu digest inicial.
    pub fn new(input: PlanSpecInput) -> Result<Self, CoreError> {
        let mut plan = Self {
            plan_id: input.plan_id,
            task_id: input.task_id,
            tenant_id: input.tenant_id,
            operator_id: input.operator_id,
            revision: 1,
            mode: input.mode,
            risk: input.risk,
            approval: input.approval,
            budget: input.budget,
            steps: input.steps,
            digest: String::new(),
            state: PlanState::Draft,
            created_at: Utc::now(),
        };
        plan.budget.validate()?;
        plan.validate_structure()?;
        plan.digest = plan.calculate_digest()?;
        Ok(plan)
    }

    /// Valida limites, dependências, riscos e consistência estrutural do plano.
    pub fn validate_structure(&self) -> Result<(), CoreError> {
        if self.revision == 0 {
            return Err(CoreError::PlanInvalid(
                "revision deve ser maior que zero".to_owned(),
            ));
        }
        if self.steps.is_empty() || self.steps.len() > 128 {
            return Err(CoreError::PlanInvalid(
                "plano deve conter entre 1 e 128 etapas".to_owned(),
            ));
        }
        if self.budget.max_steps == 0 || self.budget.max_elapsed_ms == 0 {
            return Err(CoreError::PlanInvalid(
                "orçamento do plano não permite execução".to_owned(),
            ));
        }
        if self.risk == PlanRisk::Irreversible {
            return Err(CoreError::PlanInvalid(
                "planos irreversíveis estão bloqueados por default".to_owned(),
            ));
        }
        if self.risk
            < self
                .steps
                .iter()
                .map(|step| step.risk)
                .max()
                .unwrap_or(PlanRisk::ReadOnly)
        {
            return Err(CoreError::PlanInvalid(
                "risco do plano é menor que o risco de uma etapa".to_owned(),
            ));
        }
        if self.approval < self.risk.minimum_approval() {
            return Err(CoreError::PlanInvalid(
                "aprovação do plano é insuficiente para seu risco".to_owned(),
            ));
        }
        let ids: BTreeSet<&PlanStepId> = self.steps.iter().map(|step| &step.step_id).collect();
        if ids.len() != self.steps.len() {
            return Err(CoreError::PlanInvalid("step_id duplicado".to_owned()));
        }
        for step in &self.steps {
            step.validate()?;
            for dependency in &step.depends_on {
                if dependency == &step.step_id || !ids.contains(dependency) {
                    return Err(CoreError::PlanInvalid(format!(
                        "dependência inválida na etapa {}",
                        step.step_id.0
                    )));
                }
            }
            if let Some(compensation) = &step.compensation_step_id {
                if compensation == &step.step_id || !ids.contains(compensation) {
                    return Err(CoreError::PlanInvalid(format!(
                        "compensação inválida na etapa {}",
                        step.step_id.0
                    )));
                }
            }
        }
        ensure_acyclic(&self.steps)
    }

    /// Calcula o SHA-256 canônico da revisão sem incluir estado ou timestamp voláteis.
    pub fn calculate_digest(&self) -> Result<String, CoreError> {
        let mut material = self.clone();
        material.digest.clear();
        material.state = PlanState::Draft;
        material.created_at = DateTime::<Utc>::UNIX_EPOCH;
        material
            .steps
            .sort_by(|left, right| left.step_id.cmp(&right.step_id));
        let canonical = serde_json::to_vec(&material)
            .map_err(|error| CoreError::PlanInvalid(format!("digest não serializável: {error}")))?;
        Ok(hex::encode(Sha256::digest(canonical)))
    }

    /// Verifica se o digest persistido corresponde ao conteúdo canônico da revisão.
    pub fn verify_digest(&self) -> Result<(), CoreError> {
        let expected = self.calculate_digest()?;
        if self.digest == expected {
            Ok(())
        } else {
            Err(CoreError::PlanInvalid(
                "digest do plano não corresponde ao conteúdo".to_owned(),
            ))
        }
    }

    #[must_use]
    /// Retorna o maior nível de aprovação exigido pelo plano e suas etapas.
    pub fn required_approval(&self) -> PlanApprovalRequirement {
        self.steps
            .iter()
            .map(|step| step.approval.max(step.risk.minimum_approval()))
            .max()
            .unwrap_or(PlanApprovalRequirement::None)
            .max(self.approval)
            .max(self.risk.minimum_approval())
    }
}

/// Decisão persistente de uma aprovação humana.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanApprovalDecision {
    /// Decisão humana que autoriza o escopo correspondente.
    Approved,
    /// Decisão humana que impede a execução do escopo correspondente.
    Rejected,
}

/// Aprovação vinculada a tenant, revisão, etapa e digest imutável.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanApproval {
    /// Identificador único da decisão de aprovação.
    pub approval_id: Uuid,
    /// Plano ao qual a aprovação está vinculada.
    pub plan_id: PlanId,
    /// Digest canônico do plano aprovado.
    pub plan_digest: String,
    /// Revisão exata do plano aprovada.
    pub revision: u32,
    /// Tenant no qual a aprovação é válida.
    pub tenant_id: TenantId,
    /// Operador que tomou a decisão.
    pub approver: OperatorId,
    /// Papel RBAC usado na decisão.
    pub approver_role: Role,
    /// Etapa aprovada; `None` representa aprovação global.
    pub step_id: Option<PlanStepId>,
    /// Nível de aprovação concedido.
    pub required: PlanApprovalRequirement,
    /// Decisão registrada pelo aprovador.
    pub decision: PlanApprovalDecision,
    /// Instante UTC após o qual a aprovação deixa de valer.
    pub expires_at: DateTime<Utc>,
    /// Indica que a aprovação foi revogada antes da expiração.
    pub revoked: bool,
}

impl PlanApproval {
    /// Valida a aprovação contra o plano, revisão, tenant, etapa e instante atuais.
    pub fn validate_for(
        &self,
        plan: &PlanSpec,
        step_id: Option<&PlanStepId>,
        now: DateTime<Utc>,
    ) -> Result<(), CoreError> {
        if self.decision != PlanApprovalDecision::Approved {
            return Err(CoreError::PlanApprovalInvalid(
                "decisão não aprovada".to_owned(),
            ));
        }
        if self.revoked || self.expires_at <= now {
            return Err(CoreError::PlanApprovalInvalid(
                "aprovação expirada ou revogada".to_owned(),
            ));
        }
        if self.approver == plan.operator_id {
            return Err(CoreError::PlanApprovalInvalid(
                "o proponente não pode aprovar o próprio plano".to_owned(),
            ));
        }
        if self.plan_id != plan.plan_id
            || self.plan_digest != plan.digest
            || self.revision != plan.revision
            || self.tenant_id != plan.tenant_id
            || self.step_id.as_ref() != step_id
        {
            return Err(CoreError::PlanApprovalInvalid(
                "aprovação não corresponde ao plano, revisão, tenant ou etapa".to_owned(),
            ));
        }
        let required_for_scope = match step_id {
            Some(wanted) => plan
                .steps
                .iter()
                .find(|step| &step.step_id == wanted)
                .map(|step| step.approval.max(step.risk.minimum_approval()))
                .ok_or_else(|| {
                    CoreError::PlanApprovalInvalid("etapa de aprovação inexistente".to_owned())
                })?,
            None => plan.required_approval(),
        };
        if !self.required.allows_role(&self.approver_role) || self.required < required_for_scope {
            return Err(CoreError::PlanApprovalInvalid(
                "papel ou nível de aprovação insuficiente".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_plan_key(value: &str, field: &str) -> Result<(), CoreError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CoreError::InvalidIdentifier(field.to_owned()));
    }
    Ok(())
}

fn validate_sha256(value: &str, field: &str) -> Result<(), CoreError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreError::InvalidIdentifier(field.to_owned()));
    }
    Ok(())
}

fn ensure_acyclic(steps: &[PlanStep]) -> Result<(), CoreError> {
    fn visit(
        id: &PlanStepId,
        steps: &BTreeMap<PlanStepId, &PlanStep>,
        visiting: &mut BTreeSet<PlanStepId>,
        visited: &mut BTreeSet<PlanStepId>,
    ) -> bool {
        if visited.contains(id) {
            return true;
        }
        if !visiting.insert(id.clone()) {
            return false;
        }
        let acyclic = steps.get(id).is_some_and(|step| {
            step.depends_on
                .iter()
                .all(|dependency| visit(dependency, steps, visiting, visited))
        });
        visiting.remove(id);
        if acyclic {
            visited.insert(id.clone());
        }
        acyclic
    }

    let by_id: BTreeMap<PlanStepId, &PlanStep> = steps
        .iter()
        .map(|step| (step.step_id.clone(), step))
        .collect();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    if by_id
        .keys()
        .all(|id| visit(id, &by_id, &mut visiting, &mut visited))
    {
        Ok(())
    } else {
        Err(CoreError::PlanInvalid(
            "grafo de etapas contém ciclo".to_owned(),
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Definição host-side de uma ferramenta e das autorizações que ela exige.
pub struct ToolDefinition {
    /// Nome estável usado para selecionar a ferramenta.
    pub name: String,
    /// Descrição apresentada ao componente que propõe a chamada.
    pub description: String,
    /// Schema JSON validado para os argumentos de entrada.
    pub input_schema: JsonValue,
    /// Capabilities que o contexto efetivo precisa conceder.
    pub required_capabilities: Vec<Capability>,
    /// Classificação do efeito produzido pela ferramenta.
    pub side_effect: SideEffect,
}

impl ToolDefinition {
    /// Valida a entrada contra o schema JSON declarado pela ferramenta.
    pub fn validate_input(&self, input: &JsonValue) -> Result<(), CoreError> {
        let validator = jsonschema::validator_for(&self.input_schema).map_err(|error| {
            CoreError::SchemaViolation {
                tool: self.name.clone(),
                message: format!("schema inválido: {error}"),
            }
        })?;
        if let Err(error) = validator.validate(input) {
            return Err(CoreError::SchemaViolation {
                tool: self.name.clone(),
                message: error.to_string(),
            });
        }
        Ok(())
    }
}

/// Capability que pode ser concedida explicitamente pelo host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Permite acesso à rede quando concedido pelo host.
    Network,
    /// Permite leitura de filesystem quando concedida pelo host.
    FilesystemRead,
    /// Permite escrita de filesystem quando concedida pelo host.
    FilesystemWrite,
    /// Permite execução de código isolado quando concedida pelo host.
    CodeExecution,
    /// Permite envio a destinatários externos quando concedida pelo host.
    ExternalMessaging,
    /// Permite mutações na memória persistente quando concedida pelo host.
    MemoryWrite,
}

/// Classificação do efeito produzido por uma ferramenta ou ação.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SideEffect {
    /// Operação sem mutação persistente ou efeito externo.
    ReadOnly,
    /// Operação que interage com um sistema ou destinatário externo.
    ExternalEffect,
    /// Operação que altera estado persistente local.
    Mutation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
/// Conjunto de capabilities efetivas concedidas a uma execução.
pub struct CapabilitySet(pub Vec<Capability>);

impl CapabilitySet {
    /// Retorna a matriz canônica de capabilities efetivas para um papel RBAC.
    #[must_use]
    pub fn for_role(role: &Role) -> Self {
        match role {
            Role::Operator | Role::Reviewer => Self::default(),
            Role::Administrator => Self(vec![
                Capability::Network,
                Capability::FilesystemRead,
                Capability::FilesystemWrite,
                Capability::CodeExecution,
                Capability::ExternalMessaging,
                Capability::MemoryWrite,
            ]),
        }
    }

    #[must_use]
    /// Informa se todas as capabilities exigidas estão presentes no conjunto.
    pub fn allows(&self, required: &[Capability]) -> bool {
        required.iter().all(|cap| self.0.contains(cap))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Pedido de execução de uma ferramenta associado à tarefa originadora.
pub struct ToolCall {
    /// Tarefa que originou a chamada.
    pub task_id: TaskId,
    /// Nome exato da ferramenta solicitada.
    pub tool_name: String,
    /// Argumentos da chamada em JSON.
    pub input: JsonValue,
    /// Instante UTC em que a chamada foi solicitada.
    pub requested_at: DateTime<Utc>,
}

/// Resultado serializável devolvido por uma ferramenta ao runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    /// Nome da ferramenta que produziu o resultado.
    pub tool_name: String,
    /// Saída estruturada ou erro sanitizado da ferramenta.
    pub output: JsonValue,
    /// Indica se a execução terminou com sucesso.
    pub success: bool,
    /// Código estável de erro, quando a execução falhou.
    pub error_code: Option<String>,
}

/// Manifesto declarativo de identidade, permissões, schemas e estado de uma skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillManifest {
    /// Nome estável da skill no registry.
    pub name: String,
    /// Versão do manifesto declarado.
    pub version: String,
    /// Descrição da finalidade da skill.
    pub description: String,
    /// Capabilities que o artefato declara precisar.
    pub permissions: Vec<Capability>,
    /// Schema JSON aceito pela skill.
    pub input_schema: JsonValue,
    /// Schema JSON produzido pela skill.
    pub output_schema: JsonValue,
    /// Estado atual no ciclo de vida governado.
    pub status: SkillStatus,
    /// Hash SHA-256 do artefato aprovado, quando houver.
    pub artifact_sha256: Option<String>,
}

/// Estados possíveis no ciclo de vida governado de uma skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillStatus {
    /// Skill apenas especificada, sem artefato testado.
    Specified,
    /// Artefato gerado, ainda sem candidatura aprovada.
    Generated,
    /// Artefato testado, ainda não elegível para execução.
    Tested,
    /// Skill registrada para revisão e aprovação.
    Candidate,
    /// Skill aprovada e elegível após revalidação do runtime.
    Active,
    /// Skill substituída e não recomendada para novas ativações.
    Deprecated,
    /// Skill revogada e impedida de execução.
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
/// Evento auditável encadeado por hash e associado a um tenant.
pub struct AuditEvent {
    /// Identificador único do evento.
    pub event_id: Uuid,
    /// Tarefa relacionada, quando o evento pertence a uma execução.
    pub task_id: Option<TaskId>,
    /// Tenant ao qual o evento pertence.
    pub tenant_id: TenantId,
    /// Ator que originou a transição ou observação.
    pub actor: String,
    /// Ação registrada no evento.
    pub action: String,
    /// Resultado observado da ação.
    pub outcome: String,
    /// Instante UTC da ocorrência.
    pub occurred_at: DateTime<Utc>,
    /// Metadados sanitizados e determinísticos do evento.
    pub metadata: BTreeMap<String, String>,
    /// Hash do evento anterior na cadeia, quando houver.
    pub previous_hash: Option<String>,
    /// Hash calculado para este evento.
    pub event_hash: String,
}

impl AuditEvent {
    fn calculate_hash(&self) -> String {
        let canonical = format!(
            "{}|{}|{}|{}|{}|{:?}|{:?}",
            self.event_id,
            self.occurred_at,
            self.actor,
            self.action,
            self.outcome,
            self.metadata,
            self.previous_hash
        );
        hex::encode(Sha256::digest(canonical.as_bytes()))
    }

    /// Recalcula o hash do evento usando o elo anterior da cadeia.
    #[must_use]
    pub fn with_previous_hash(&self, previous_hash: Option<String>) -> Self {
        let mut event = self.clone();
        event.previous_hash = previous_hash;
        event.event_hash = event.calculate_hash();
        event
    }

    /// Verifica se o hash persistido corresponde ao conteúdo do evento.
    #[must_use]
    pub fn has_valid_hash(&self) -> bool {
        self.event_hash == self.calculate_hash()
    }

    #[must_use]
    /// Cria um evento com identificador, timestamp e hash inicial da cadeia.
    pub fn new(
        task_id: Option<TaskId>,
        tenant_id: TenantId,
        actor: impl Into<String>,
        action: impl Into<String>,
        outcome: impl Into<String>,
        metadata: BTreeMap<String, String>,
        previous_hash: Option<String>,
    ) -> Self {
        let event_id = Uuid::new_v4();
        let occurred_at = Utc::now();
        let actor = actor.into();
        let action = action.into();
        let outcome = outcome.into();
        let event_hash = hex::encode(Sha256::digest(
            format!(
                "{event_id}|{occurred_at}|{actor}|{action}|{outcome}|{metadata:?}|{previous_hash:?}"
            )
            .as_bytes(),
        ));
        Self {
            event_id,
            task_id,
            tenant_id,
            actor,
            action,
            outcome,
            occurred_at,
            metadata,
            previous_hash,
            event_hash,
        }
    }
}

#[derive(Debug, Error)]
/// Erros de validação de identidade, schema, autorização, budget e governança.
pub enum CoreError {
    /// Identificador fora do formato ou limite aceito.
    #[error("identificador inválido: {0}")]
    InvalidIdentifier(String),
    /// Entrada que viola um limite ou pré-condição do núcleo.
    #[error("entrada inválida: {0}")]
    InvalidInput(String),
    /// Entrada de ferramenta que não satisfaz seu schema declarado.
    #[error("violação de schema na ferramenta {tool}: {message}")]
    SchemaViolation {
        /// Nome da ferramenta cujo schema foi violado.
        tool: String,
        /// Descrição sanitizada da violação encontrada.
        message: String,
    },
    /// Capability exigida sem concessão no contexto efetivo.
    #[error("capacidade não autorizada: {0:?}")]
    CapabilityDenied(Capability),
    /// Operação que depende de uma aprovação humana explícita.
    #[error("operação requer aprovação explícita do operador")]
    ApprovalRequired,
    /// Limite de execução ou custo ultrapassado.
    #[error("orçamento excedido: {0}")]
    BudgetExceeded(String),
    /// Principal sem autorização para a ação solicitada.
    #[error("principal não autorizado para a ação: {0:?}")]
    Unauthorized(Action),
    /// Plano que viola sua estrutura, riscos, dependências ou digest.
    #[error("plano inválido: {0}")]
    PlanInvalid(String),
    /// Aprovação incompatível, expirada, revogada ou insuficiente para o plano.
    #[error("aprovação de plano inválida: {0}")]
    PlanApprovalInvalid(String),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn task_requires_non_empty_objective() {
        let tenant = TenantId::new("demo").unwrap();
        let operator = OperatorId::new("operator").unwrap();
        assert!(TaskEnvelope::new(tenant, operator, "  ").is_err());
    }

    #[test]
    fn execution_budget_accepts_defaults_and_exact_host_limits() {
        assert!(ExecutionBudget::default().validate().is_ok());
        let at_limits = ExecutionBudget {
            max_steps: MAX_EXECUTION_STEPS,
            max_tool_calls: MAX_TOOL_CALLS,
            max_elapsed_ms: MAX_EXECUTION_ELAPSED_MS,
            max_cost_microunits: MAX_EXECUTION_COST_MICROUNITS,
        };
        assert!(at_limits.validate().is_ok());
    }

    #[test]
    fn execution_budget_rejects_each_invalid_boundary() {
        let invalid_budgets = [
            ExecutionBudget {
                max_steps: 0,
                ..ExecutionBudget::default()
            },
            ExecutionBudget {
                max_steps: MAX_EXECUTION_STEPS + 1,
                ..ExecutionBudget::default()
            },
            ExecutionBudget {
                max_tool_calls: MAX_TOOL_CALLS + 1,
                ..ExecutionBudget::default()
            },
            ExecutionBudget {
                max_elapsed_ms: 0,
                ..ExecutionBudget::default()
            },
            ExecutionBudget {
                max_elapsed_ms: MAX_EXECUTION_ELAPSED_MS + 1,
                ..ExecutionBudget::default()
            },
            ExecutionBudget {
                max_cost_microunits: MAX_EXECUTION_COST_MICROUNITS + 1,
                ..ExecutionBudget::default()
            },
        ];
        for budget in invalid_budgets {
            assert!(matches!(budget.validate(), Err(CoreError::InvalidInput(_))));
        }
    }

    #[test]
    fn execution_budget_allows_zero_tool_calls_and_zero_cost() {
        let budget = ExecutionBudget {
            max_tool_calls: 0,
            max_cost_microunits: 0,
            ..ExecutionBudget::default()
        };
        assert!(budget.validate().is_ok());
    }

    #[test]
    fn execution_context_derives_least_privilege_from_role() {
        let operator = Principal {
            operator_id: OperatorId::new("operator").unwrap(),
            tenant_id: TenantId::new("tenant").unwrap(),
            role: Role::Operator,
        };
        let operator_context = ExecutionContext::from_principal(&operator);
        assert!(
            !operator_context
                .capabilities
                .allows(&[Capability::CodeExecution])
        );
        assert!(
            !operator_context
                .capabilities
                .allows(&[Capability::ExternalMessaging])
        );

        let administrator = Principal {
            role: Role::Administrator,
            ..operator
        };
        let administrator_context = ExecutionContext::from_principal(&administrator);
        assert!(
            administrator_context
                .capabilities
                .allows(&[Capability::CodeExecution, Capability::ExternalMessaging])
        );
        assert_eq!(operator_context.provenance, RequestProvenance::default());
    }

    #[test]
    fn request_provenance_round_trips_and_defaults_for_legacy_envelopes() {
        let provenance = RequestProvenance {
            request_id: Some("request-1".to_owned()),
            admission_approval_id: Some(Uuid::nil()),
        };
        let context = ExecutionContext::from_principal(&Principal {
            operator_id: OperatorId::new("operator").unwrap(),
            tenant_id: TenantId::new("tenant").unwrap(),
            role: Role::Operator,
        })
        .with_provenance(
            provenance.request_id.clone(),
            provenance.admission_approval_id,
        );
        let encoded = serde_json::to_string(&context).unwrap();
        let decoded: ExecutionContext = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.provenance, provenance);

        let legacy: ExecutionContext =
            serde_json::from_str(r#"{"role":"Operator","capabilities":[]}"#).unwrap();
        assert_eq!(legacy.provenance, RequestProvenance::default());
    }

    #[test]
    fn capability_policy_is_explicit_for_every_role() {
        assert_eq!(
            CapabilitySet::for_role(&Role::Operator),
            CapabilitySet::default()
        );
        assert_eq!(
            CapabilitySet::for_role(&Role::Reviewer),
            CapabilitySet::default()
        );
        assert_eq!(
            CapabilitySet::for_role(&Role::Administrator),
            CapabilitySet(vec![
                Capability::Network,
                Capability::FilesystemRead,
                Capability::FilesystemWrite,
                Capability::CodeExecution,
                Capability::ExternalMessaging,
                Capability::MemoryWrite,
            ])
        );
    }

    #[test]
    fn capability_set_is_deny_by_default() {
        let capabilities = CapabilitySet(vec![Capability::MemoryWrite]);
        assert!(!capabilities.allows(&[Capability::Network]));
        assert!(capabilities.allows(&[Capability::MemoryWrite]));
    }

    #[test]
    fn schema_validation_rejects_missing_required_field() {
        let tool = ToolDefinition {
            name: "test".to_owned(),
            description: "test".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"]
            }),
            required_capabilities: Vec::new(),
            side_effect: SideEffect::ReadOnly,
        };
        assert!(tool.validate_input(&serde_json::json!({})).is_err());
        assert!(
            tool.validate_input(&serde_json::json!({"message": "ok"}))
                .is_ok()
        );
    }

    #[test]
    fn roles_are_deny_by_default_for_external_actions() {
        let principal = Principal {
            operator_id: OperatorId::new("operator").unwrap(),
            tenant_id: TenantId::new("tenant").unwrap(),
            role: Role::Operator,
        };
        assert!(principal.allows(&Action::RunReadOnly));
        assert!(!principal.allows(&Action::RunExternal));
        assert!(!principal.allows(&Action::ApproveSkill));
    }

    fn plan_step(
        step_id: &str,
        depends_on: &[&str],
        action: PlanAction,
        risk: PlanRisk,
        approval: PlanApprovalRequirement,
    ) -> PlanStep {
        PlanStep {
            step_id: PlanStepId::new(step_id).unwrap(),
            depends_on: depends_on
                .iter()
                .map(|value| PlanStepId::new(*value).unwrap())
                .collect(),
            action,
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            risk,
            approval,
            max_attempts: 1,
            compensation_step_id: None,
        }
    }

    #[test]
    fn plan_defaults_to_dry_run_and_has_verifiable_digest() {
        let plan = PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: OperatorId::new("operator").unwrap(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget: ExecutionBudget::default(),
            steps: vec![plan_step(
                "read",
                &[],
                PlanAction::ReadOnly {
                    operation: "inspect".to_owned(),
                },
                PlanRisk::ReadOnly,
                PlanApprovalRequirement::None,
            )],
        })
        .unwrap();
        assert_eq!(plan.mode, PlanMode::DryRun);
        assert_eq!(plan.state, PlanState::Draft);
        assert_eq!(plan.digest.len(), 64);
        assert!(plan.verify_digest().is_ok());
        let mut reordered = plan.clone();
        reordered.steps.reverse();
        assert_eq!(
            plan.calculate_digest().unwrap(),
            reordered.calculate_digest().unwrap()
        );
    }

    #[test]
    fn plan_rejects_cycles_and_unknown_dependencies() {
        let cyclic = PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: OperatorId::new("operator").unwrap(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget: ExecutionBudget::default(),
            steps: vec![
                plan_step(
                    "a",
                    &["b"],
                    PlanAction::ReadOnly {
                        operation: "a".to_owned(),
                    },
                    PlanRisk::ReadOnly,
                    PlanApprovalRequirement::None,
                ),
                plan_step(
                    "b",
                    &["a"],
                    PlanAction::ReadOnly {
                        operation: "b".to_owned(),
                    },
                    PlanRisk::ReadOnly,
                    PlanApprovalRequirement::None,
                ),
            ],
        });
        assert!(matches!(cyclic, Err(CoreError::PlanInvalid(_))));

        let unknown_dependency = PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: OperatorId::new("operator").unwrap(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget: ExecutionBudget::default(),
            steps: vec![plan_step(
                "a",
                &["missing"],
                PlanAction::ReadOnly {
                    operation: "a".to_owned(),
                },
                PlanRisk::ReadOnly,
                PlanApprovalRequirement::None,
            )],
        });
        assert!(matches!(unknown_dependency, Err(CoreError::PlanInvalid(_))));
    }

    #[test]
    fn plan_requires_approval_for_mutation_and_rejects_self_approval() {
        let plan = PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: OperatorId::new("operator").unwrap(),
            mode: PlanMode::Live,
            risk: PlanRisk::Mutation,
            approval: PlanApprovalRequirement::Reviewer,
            budget: ExecutionBudget::default(),
            steps: vec![plan_step(
                "mutate",
                &[],
                PlanAction::Mutation {
                    operation: "update".to_owned(),
                },
                PlanRisk::Mutation,
                PlanApprovalRequirement::Reviewer,
            )],
        })
        .unwrap();
        assert_eq!(plan.required_approval(), PlanApprovalRequirement::Reviewer);
        let valid = PlanApproval {
            approval_id: Uuid::new_v4(),
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.digest.clone(),
            revision: plan.revision,
            tenant_id: plan.tenant_id.clone(),
            approver: OperatorId::new("reviewer").unwrap(),
            approver_role: Role::Reviewer,
            step_id: None,
            required: PlanApprovalRequirement::Reviewer,
            decision: PlanApprovalDecision::Approved,
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            revoked: false,
        };
        assert!(valid.validate_for(&plan, None, Utc::now()).is_ok());

        let mut self_approval = valid;
        self_approval.approver = plan.operator_id.clone();
        assert!(self_approval.validate_for(&plan, None, Utc::now()).is_err());
    }

    #[test]
    fn plan_state_transitions_are_fail_closed() {
        assert!(
            PlanState::Draft
                .validate_transition(PlanState::Proposed)
                .is_ok()
        );
        assert!(
            PlanState::Draft
                .validate_transition(PlanState::Running)
                .is_err()
        );
        assert!(
            PlanStepState::Running
                .validate_transition(PlanStepState::Unknown)
                .is_ok()
        );
        assert!(
            PlanStepState::Succeeded
                .validate_transition(PlanStepState::Running)
                .is_err()
        );
    }

    #[test]
    fn plan_blocks_irreversible_actions_and_empty_budget() {
        let irreversible = PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: OperatorId::new("operator").unwrap(),
            mode: PlanMode::Live,
            risk: PlanRisk::Irreversible,
            approval: PlanApprovalRequirement::Administrator,
            budget: ExecutionBudget::default(),
            steps: vec![plan_step(
                "destroy",
                &[],
                PlanAction::ExternalEffect {
                    operation: "destroy".to_owned(),
                },
                PlanRisk::Irreversible,
                PlanApprovalRequirement::Administrator,
            )],
        });
        assert!(matches!(irreversible, Err(CoreError::PlanInvalid(_))));

        let budget = ExecutionBudget {
            max_elapsed_ms: 0,
            ..ExecutionBudget::default()
        };
        let no_budget = PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: OperatorId::new("operator").unwrap(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget,
            steps: vec![plan_step(
                "read",
                &[],
                PlanAction::ReadOnly {
                    operation: "inspect".to_owned(),
                },
                PlanRisk::ReadOnly,
                PlanApprovalRequirement::None,
            )],
        });
        assert!(matches!(no_budget, Err(CoreError::InvalidInput(_))));
    }

    #[test]
    fn sensitive_values_are_redacted() {
        let value = "api_key=secret-value Authorization: Bearer abc.def";
        let result = redact_sensitive(value);
        assert!(!result.contains("secret-value"));
        assert!(!result.contains("abc.def"));
    }

    #[test]
    fn sensitive_values_with_quotes_and_common_keys_are_redacted() {
        let api_key_value = ["sec", "ret"].concat();
        let access_token_value = "tok-value-123";
        let password_value = "hidden";
        let bearer_value = "abc.def";
        let private_key_value = "pem-secret";
        let value = format!(
            "{}='{}' {}: {} {}=\"{}\" {}: Bearer {} {}={}",
            "api_key",
            api_key_value,
            "access-token",
            access_token_value,
            "password",
            password_value,
            "Authorization",
            bearer_value,
            "private_key",
            private_key_value,
        );
        let result = redact_sensitive(&value);
        for secret in [
            api_key_value.as_str(),
            access_token_value,
            password_value,
            bearer_value,
            private_key_value,
        ] {
            assert!(
                !result.contains(secret),
                "segredo permaneceu na saída: {secret}; saída: {result}"
            );
        }
        assert!(result.contains("api_key=[REDACTED]"));
        assert!(result.contains("access-token=[REDACTED]"));
        assert!(result.contains("password=[REDACTED]"));
        assert!(result.contains("Authorization=[REDACTED]"));
        assert!(result.contains("private_key=[REDACTED]"));
    }
}
