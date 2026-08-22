//! Contratos centrais e tipos compartilhados do agente Shaka.

pub mod plan_verifier;

pub use plan_verifier::{
    PlanVerificationContext, PlanVerificationPhase, PlanVerificationReport, PlanVerificationStatus,
    PlanVerifier, PlanViolation, PlanViolationCode,
};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use uuid::Uuid;

pub type JsonValue = serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TaskId(pub Uuid);

impl TaskId {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TenantId(pub String);

impl TenantId {
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 128 {
            return Err(CoreError::InvalidIdentifier("tenant_id".to_owned()));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct OperatorId(pub String);

impl OperatorId {
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
    Operator,
    Reviewer,
    Administrator,
}

/// Ação protegida pelo host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Action {
    RunReadOnly,
    RunExternal,
    CreateSkill,
    ApproveSkill,
    RevokeSkill,
    Backup,
    Restore,
    VerifyAudit,
    PurgeMemory,
    ManageIam,
    ApprovePlan,
    ResumePlan,
    ResolvePlanUnknown,
}

/// Identidade autenticada e escopo de tenant usado nas decisões do host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Principal {
    pub operator_id: OperatorId,
    pub tenant_id: TenantId,
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

/// Remove padrões comuns de credenciais antes de gravar texto em logs ou memória.
#[must_use]
pub fn redact_sensitive(input: &str) -> String {
    let Some(key_pattern) = Regex::new(
        r"(?i)(api[_-]?key|access[_-]?token|secret|password|authorization)s*[:=]s*([^s,;]+)",
    )
    .ok() else {
        return input.to_owned();
    };
    let Some(bearer_pattern) = Regex::new(r"(?i)bearers+[A-Za-z0-9._~+/=-]+").ok() else {
        return input.to_owned();
    };
    let redacted = bearer_pattern.replace_all(input, "Bearer [REDACTED]");
    key_pattern
        .replace_all(&redacted, "$1=[REDACTED]")
        .into_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionBudget {
    pub max_steps: u32,
    pub max_tool_calls: u32,
    pub max_elapsed_ms: u64,
    pub max_cost_microunits: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskEnvelope {
    pub task_id: TaskId,
    pub tenant_id: TenantId,
    pub operator_id: OperatorId,
    pub objective: String,
    pub budget: ExecutionBudget,
    pub dry_run: bool,
    pub created_at: DateTime<Utc>,
}

impl TaskEnvelope {
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
            created_at: Utc::now(),
        })
    }
}

/// Identificador estável de um plano de execução.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PlanId(pub Uuid);

impl PlanId {
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
    #[default]
    DryRun,
    Live,
}

/// Classificação de risco monotônica de uma etapa ou plano.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PlanRisk {
    ReadOnly,
    Mutation,
    ExternalEffect,
    Irreversible,
}

impl PlanRisk {
    #[must_use]
    pub const fn requires_approval(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

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
    None,
    Reviewer,
    Administrator,
}

impl PlanApprovalRequirement {
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
    Draft,
    Proposed,
    Validated,
    AwaitingApproval,
    Approved,
    Running,
    Paused,
    Succeeded,
    Failed,
    CancelRequested,
    Cancelled,
    Compensating,
    Compensated,
    Unknown,
    Rejected,
}

impl PlanState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Compensated | Self::Rejected
        )
    }

    pub fn validate_transition(self, next: Self) -> Result<(), CoreError> {
        let valid = match self {
            Self::Draft => matches!(next, Self::Proposed | Self::Rejected),
            Self::Proposed => matches!(next, Self::Validated | Self::Rejected),
            Self::Validated => matches!(
                next,
                Self::AwaitingApproval | Self::Approved | Self::Rejected
            ),
            Self::AwaitingApproval => matches!(next, Self::Approved | Self::Rejected),
            Self::Approved => matches!(next, Self::Running | Self::Rejected),
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
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed,
    Blocked,
    AwaitingApproval,
    CancelRequested,
    Cancelled,
    Compensating,
    Compensated,
    Unknown,
}

impl PlanStepState {
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
            Self::Succeeded | Self::Blocked | Self::Cancelled | Self::Compensated => false,
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
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Predicados fechados e verificáveis pelo host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlanCondition {
    TaskStateIs { state: PlanTaskState },
    StepSucceeded { step_id: PlanStepId },
    ApprovalExists { step_id: Option<PlanStepId> },
    CapabilityGranted { capability: Capability },
    CircuitClosed,
    BudgetRemainingAtLeast { budget: ExecutionBudget },
    ArtifactDigestMatches { artifact: String, sha256: String },
    IdempotencyKeyUnused { key_ref: String },
    StateDigestMatches { sha256: String },
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
    ReadOnly { operation: String },
    Mutation { operation: String },
    ExecuteTool { tool_name: String },
    ExternalEffect { operation: String },
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

    #[must_use]
    pub const fn minimum_risk(&self) -> PlanRisk {
        match self {
            Self::ReadOnly { .. } => PlanRisk::ReadOnly,
            Self::Mutation { .. } | Self::ExecuteTool { .. } => PlanRisk::Mutation,
            Self::ExternalEffect { .. } => PlanRisk::ExternalEffect,
        }
    }
}

/// Etapa declarativa, imutável após a aprovação da revisão.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    pub step_id: PlanStepId,
    pub depends_on: Vec<PlanStepId>,
    pub action: PlanAction,
    pub preconditions: Vec<PlanCondition>,
    pub postconditions: Vec<PlanCondition>,
    pub risk: PlanRisk,
    pub approval: PlanApprovalRequirement,
    pub max_attempts: u32,
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
    pub plan_id: PlanId,
    pub task_id: TaskId,
    pub tenant_id: TenantId,
    pub operator_id: OperatorId,
    pub mode: PlanMode,
    pub risk: PlanRisk,
    pub approval: PlanApprovalRequirement,
    pub budget: ExecutionBudget,
    pub steps: Vec<PlanStep>,
}

/// Especificação canônica de um plano de execução.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSpec {
    pub plan_id: PlanId,
    pub task_id: TaskId,
    pub tenant_id: TenantId,
    pub operator_id: OperatorId,
    pub revision: u32,
    pub mode: PlanMode,
    pub risk: PlanRisk,
    pub approval: PlanApprovalRequirement,
    pub budget: ExecutionBudget,
    pub steps: Vec<PlanStep>,
    pub digest: String,
    pub state: PlanState,
    pub created_at: DateTime<Utc>,
}

impl PlanSpec {
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
        plan.validate_structure()?;
        plan.digest = plan.calculate_digest()?;
        Ok(plan)
    }

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
    Approved,
    Rejected,
}

/// Aprovação vinculada a tenant, revisão, etapa e digest imutável.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanApproval {
    pub approval_id: Uuid,
    pub plan_id: PlanId,
    pub plan_digest: String,
    pub revision: u32,
    pub tenant_id: TenantId,
    pub approver: OperatorId,
    pub approver_role: Role,
    pub step_id: Option<PlanStepId>,
    pub required: PlanApprovalRequirement,
    pub decision: PlanApprovalDecision,
    pub expires_at: DateTime<Utc>,
    pub revoked: bool,
}

impl PlanApproval {
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
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
    pub required_capabilities: Vec<Capability>,
    pub side_effect: SideEffect,
}

impl ToolDefinition {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Capability {
    Network,
    FilesystemRead,
    FilesystemWrite,
    CodeExecution,
    ExternalMessaging,
    MemoryWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SideEffect {
    ReadOnly,
    ExternalEffect,
    Mutation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CapabilitySet(pub Vec<Capability>);

impl CapabilitySet {
    #[must_use]
    pub fn allows(&self, required: &[Capability]) -> bool {
        required.iter().all(|cap| self.0.contains(cap))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub task_id: TaskId,
    pub tool_name: String,
    pub input: JsonValue,
    pub requested_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    pub tool_name: String,
    pub output: JsonValue,
    pub success: bool,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<Capability>,
    pub input_schema: JsonValue,
    pub output_schema: JsonValue,
    pub status: SkillStatus,
    pub artifact_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillStatus {
    Specified,
    Generated,
    Tested,
    Candidate,
    Active,
    Deprecated,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEvent {
    pub event_id: Uuid,
    pub task_id: Option<TaskId>,
    pub tenant_id: TenantId,
    pub actor: String,
    pub action: String,
    pub outcome: String,
    pub occurred_at: DateTime<Utc>,
    pub metadata: BTreeMap<String, String>,
    pub previous_hash: Option<String>,
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
pub enum CoreError {
    #[error("identificador inválido: {0}")]
    InvalidIdentifier(String),
    #[error("entrada inválida: {0}")]
    InvalidInput(String),
    #[error("violação de schema na ferramenta {tool}: {message}")]
    SchemaViolation { tool: String, message: String },
    #[error("capacidade não autorizada: {0:?}")]
    CapabilityDenied(Capability),
    #[error("operação requer aprovação explícita do operador")]
    ApprovalRequired,
    #[error("orçamento excedido: {0}")]
    BudgetExceeded(String),
    #[error("principal não autorizado para a ação: {0:?}")]
    Unauthorized(Action),
    #[error("plano inválido: {0}")]
    PlanInvalid(String),
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
        assert!(matches!(no_budget, Err(CoreError::PlanInvalid(_))));
    }

    #[test]
    fn sensitive_values_are_redacted() {
        let value = "api_key=secret-value Authorization: Bearer abc.def";
        let result = redact_sensitive(value);
        assert!(!result.contains("secret-value"));
        assert!(!result.contains("abc.def"));
    }
}
