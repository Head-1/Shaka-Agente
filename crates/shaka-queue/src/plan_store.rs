//! Persistência, inspeção e recovery do Plan Engine em SQLite.
//!
//! As transições são append-only e encadeadas por SHA-256; inconsistências ou
//! fronteiras ambíguas não são convertidas em sucesso automaticamente.

use super::{FinishOutcome, QueueError, QueueStore, TaskRecord, TaskStatus};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use shaka_core::{
    Capability, ExecutionBudget, PlanApproval, PlanApprovalDecision, PlanApprovalRequirement,
    PlanId, PlanMode, PlanSpec, PlanState, PlanStep, PlanStepId, PlanStepState, PlanTaskState,
    PlanVerificationContext, PlanVerificationPhase, PlanVerificationReport, PlanVerifier,
    Principal, Role, TaskEnvelope, TenantId,
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// Snapshot persistente de um plano pertencente a um tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedPlan {
    /// Snapshot do plano e da revisão persistida.
    pub plan: PlanSpec,
    /// Instante da última atualização em UTC.
    pub updated_at: DateTime<Utc>,
}

/// Referência imutável usada para vincular uma task a uma revisão de plano.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanTaskReference {
    /// Identificador do plano imutável.
    pub plan_id: PlanId,
    /// Revisão do plano vinculada à tarefa.
    pub revision: u32,
    /// Digest SHA-256 canônico da revisão.
    pub digest: String,
}

impl PlanTaskReference {
    /// Cria uma referência bounded e valida o formato do digest.
    pub fn new(
        plan_id: PlanId,
        revision: u32,
        digest: impl Into<String>,
    ) -> Result<Self, QueueError> {
        if revision == 0 {
            return Err(QueueError::InvalidInput(
                "revisão do plano deve ser maior que zero".to_owned(),
            ));
        }
        let digest = digest.into();
        validate_sha256(&digest, "plan_digest")?;
        Ok(Self {
            plan_id,
            revision,
            digest,
        })
    }
}

/// Fatos fornecidos pelo host durante o claim de uma etapa planejada.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanClaimContext {
    /// Indica se o circuit breaker permite o claim.
    pub circuit_closed: bool,
    /// Capabilities concedidas pelo host para a etapa.
    pub granted_capabilities: Vec<Capability>,
    /// Orçamento restante observado no host.
    pub remaining_budget: Option<ExecutionBudget>,
    /// Digest do estado persistido usado na validação.
    pub state_digest: Option<String>,
}

/// Fase de um checkpoint persistido.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanCheckpointPhase {
    /// Verificação estrutural e de admissão.
    Preflight,
    /// Registro de decisão humana.
    Approval,
    /// Resolução de uma fronteira ambígua.
    Resolution,
    /// Fronteira imediatamente anterior à etapa.
    BeforeStep,
    /// Fronteira imediatamente posterior à etapa.
    AfterStep,
    /// Execução de compensação declarada.
    Compensation,
    /// Reconciliação após falha ou reinício.
    Recovery,
}

impl PlanCheckpointPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Approval => "approval",
            Self::Resolution => "resolution",
            Self::BeforeStep => "before_step",
            Self::AfterStep => "after_step",
            Self::Compensation => "compensation",
            Self::Recovery => "recovery",
        }
    }

    fn parse(value: &str) -> Result<Self, QueueError> {
        match value {
            "preflight" => Ok(Self::Preflight),
            "approval" => Ok(Self::Approval),
            "resolution" => Ok(Self::Resolution),
            "before_step" => Ok(Self::BeforeStep),
            "after_step" => Ok(Self::AfterStep),
            "compensation" => Ok(Self::Compensation),
            "recovery" => Ok(Self::Recovery),
            other => Err(QueueError::InvalidInput(format!(
                "fase de checkpoint desconhecida: {other}"
            ))),
        }
    }
}

/// Resultado persistido de um checkpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanCheckpointStatus {
    /// Checkpoint criado, mas ainda sem conclusão.
    Pending,
    /// Checkpoint concluído.
    Succeeded,
    /// Checkpoint terminou com falha conhecida.
    Failed,
    /// Resultado não determinável; exige tratamento fail-closed.
    Unknown,
}

impl PlanCheckpointStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> Result<Self, QueueError> {
        match value {
            "pending" => Ok(Self::Pending),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "unknown" => Ok(Self::Unknown),
            other => Err(QueueError::InvalidInput(format!(
                "status de checkpoint desconhecido: {other}"
            ))),
        }
    }
}

/// Entidade afetada por uma transição do reducer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanTransitionEntity {
    /// A transição altera o estado do plano.
    Plan,
    /// A transição altera o estado de uma etapa.
    Step,
}

impl PlanTransitionEntity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Step => "step",
        }
    }
}

/// Estado tipado usado nos dois reducers persistentes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "entity")]
pub enum PlanTransitionState {
    /// Estado anterior ou posterior do plano.
    Plan(PlanState),
    /// Estado anterior ou posterior de uma etapa.
    Step(PlanStepState),
}

/// Transição append-only com encadeamento SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStoreTransition {
    /// Identificador da transição.
    pub transition_id: Uuid,
    /// Plano ao qual a transição pertence.
    pub plan_id: PlanId,
    /// Revisão do plano.
    pub revision: u32,
    /// Sequência monotônica dentro da revisão.
    pub sequence: u64,
    /// Entidade cujo estado foi alterado.
    pub entity: PlanTransitionEntity,
    /// Etapa afetada, quando a entidade é `Step`.
    pub entity_id: Option<PlanStepId>,
    /// Estado anterior validado.
    pub from_state: PlanTransitionState,
    /// Estado posterior validado.
    pub to_state: PlanTransitionState,
    /// Chave que torna a transição idempotente.
    pub idempotency_key: String,
    /// Hash da transição anterior na cadeia.
    pub previous_hash: Option<String>,
    /// Hash SHA-256 do material canônico do evento.
    pub event_hash: String,
    /// Instante de criação em UTC.
    pub created_at: DateTime<Utc>,
}

impl PlanStoreTransition {
    /// Cria uma transição e calcula seu hash de evento sem aceitar hash fornecido pelo chamador.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan_id: PlanId,
        revision: u32,
        sequence: u64,
        entity: PlanTransitionEntity,
        entity_id: Option<PlanStepId>,
        from_state: PlanTransitionState,
        to_state: PlanTransitionState,
        idempotency_key: impl Into<String>,
        previous_hash: Option<String>,
        created_at: DateTime<Utc>,
    ) -> Result<Self, QueueError> {
        let idempotency_key = idempotency_key.into();
        validate_key(&idempotency_key, "idempotency_key", 256)?;
        if sequence == 0 || revision == 0 {
            return Err(QueueError::InvalidInput(
                "sequence e revision devem ser maiores que zero".to_owned(),
            ));
        }
        validate_transition_shape(entity, entity_id.as_ref(), from_state, to_state)?;
        let mut transition = Self {
            transition_id: Uuid::new_v4(),
            plan_id,
            revision,
            sequence,
            entity,
            entity_id,
            from_state,
            to_state,
            idempotency_key,
            previous_hash,
            event_hash: String::new(),
            created_at,
        };
        transition.event_hash = transition.calculate_hash()?;
        Ok(transition)
    }

    /// Recalcula o hash da transição sem incluir o próprio hash no material canônico.
    pub fn calculate_hash(&self) -> Result<String, QueueError> {
        let material = TransitionHashMaterial {
            transition_id: self.transition_id,
            plan_id: &self.plan_id,
            revision: self.revision,
            sequence: self.sequence,
            entity: self.entity,
            entity_id: self.entity_id.as_ref(),
            from_state: self.from_state,
            to_state: self.to_state,
            idempotency_key: &self.idempotency_key,
            previous_hash: self.previous_hash.as_deref(),
            created_at: self.created_at,
        };
        let bytes = serde_json::to_vec(&material)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    /// Verifica a integridade do evento e do encadeamento local.
    pub fn verify_hash(&self) -> Result<(), QueueError> {
        let expected = self.calculate_hash()?;
        if self.event_hash == expected {
            Ok(())
        } else {
            Err(QueueError::InvalidInput(
                "hash de transição não corresponde ao conteúdo".to_owned(),
            ))
        }
    }
}

#[derive(Serialize)]
struct TransitionHashMaterial<'a> {
    transition_id: Uuid,
    plan_id: &'a PlanId,
    revision: u32,
    sequence: u64,
    entity: PlanTransitionEntity,
    entity_id: Option<&'a PlanStepId>,
    from_state: PlanTransitionState,
    to_state: PlanTransitionState,
    idempotency_key: &'a str,
    previous_hash: Option<&'a str>,
    created_at: DateTime<Utc>,
}

/// Checkpoint de fronteira persistido antes ou depois de uma etapa.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanCheckpoint {
    /// Plano ao qual o checkpoint pertence.
    pub plan_id: PlanId,
    /// Revisão do plano.
    pub revision: u32,
    /// Sequência monotônica do checkpoint.
    pub sequence: u64,
    /// Etapa associada, quando aplicável.
    pub step_id: Option<PlanStepId>,
    /// Fronteira operacional registrada.
    pub phase: PlanCheckpointPhase,
    /// Resultado persistido da fronteira.
    pub status: PlanCheckpointStatus,
    /// Digest do estado observado no checkpoint.
    pub state_digest: Option<String>,
    /// Instante de criação em UTC.
    pub created_at: DateTime<Utc>,
}

/// Resultado da reconstrução do reducer após reinício.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanResumeStatus {
    /// O reducer foi reconstruído sem inconsistência.
    Stable,
    /// A recuperação preservou a incerteza como `unknown`.
    RecoveredUnknown,
    /// A cadeia não pode ser reconciliada com segurança.
    Inconsistent,
}

/// Resultado bounded de uma inspeção somente leitura do reducer.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanInspectionStatus {
    /// A inspeção somente leitura confirmou a cadeia.
    Stable,
    /// A inspeção encontrou divergência ou sequência inválida.
    Inconsistent,
}

/// Classe estável de inconsistência encontrada durante uma inspeção.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanInspectionIssue {
    /// Hash ou encadeamento de transições inválido.
    TransitionChainInvalid,
    /// Estado reduzido diverge do snapshot persistido.
    ReducerDiverged,
    /// Sequência de checkpoints inválida.
    CheckpointSequenceInvalid,
}

/// Relatório bounded de inspeção sem efeitos colaterais.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanInspectionReport {
    /// Plano inspecionado.
    pub plan: PlanSpec,
    /// Estado reduzido por etapa.
    pub step_states: BTreeMap<PlanStepId, PlanStepState>,
    /// Resultado da inspeção.
    pub status: PlanInspectionStatus,
    /// Problema estável, quando a inspeção foi inconsistente.
    pub issue: Option<PlanInspectionIssue>,
    /// Número de checkpoints verificados.
    pub checkpoints_checked: u64,
    /// Número de transições verificadas.
    pub transitions_checked: u64,
}

/// Relatório bounded da retomada de um plano.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanResumeReport {
    /// Plano reconstruído.
    pub plan: PlanSpec,
    /// Estado reduzido por etapa após a tentativa de recovery.
    pub step_states: BTreeMap<PlanStepId, PlanStepState>,
    /// Resultado da reconstrução.
    pub status: PlanResumeStatus,
    /// Número de checkpoints verificados.
    pub checkpoints_checked: u64,
    /// Número de transições verificadas.
    pub transitions_checked: u64,
    /// Motivo sanitizado da inconsistência, quando houver.
    pub inconsistency: Option<String>,
}

type PlanReducerRows = (
    Vec<PlanStoreTransition>,
    Vec<PlanCheckpoint>,
    PlanState,
    BTreeMap<PlanStepId, PlanStepState>,
);

/// Resultado idempotente de uma decisão humana sobre o plano.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanApprovalOutcome {
    /// A decisão foi persistida e aceita pelo contrato.
    Approved,
    /// A decisão humana rejeitou o escopo solicitado.
    Rejected,
    /// A mesma chave já havia produzido uma decisão.
    Existing,
}

/// Decisão humana explícita para um plano em estado ambíguo.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanResolutionDecision {
    /// Retoma o fluxo após evidência e revisão humana.
    Resume,
    /// Inicia a compensação declarada pelo plano.
    Compensate,
    /// Cancela o plano e impede novas etapas.
    Cancel,
}

/// Resultado idempotente da resolução humana.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanResolutionOutcome {
    /// O plano foi retomado.
    Resumed,
    /// A compensação declarada foi iniciada.
    Compensating,
    /// O plano foi cancelado.
    Cancelled,
    /// A mesma chave já havia produzido uma resolução.
    Existing,
}

impl QueueStore {
    /// Inspeciona o reducer sem aplicar recovery ou alterar o estado persistido.
    pub fn inspect_plan(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
    ) -> Result<PlanInspectionReport, QueueError> {
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let (transitions, checkpoints, db_plan_state, db_step_states) =
            self.load_reducer_rows(plan_id, persisted.plan.revision, tenant_id)?;
        let mut computed_plan_state = PlanState::Draft;
        let mut computed_steps: BTreeMap<PlanStepId, PlanStepState> = persisted
            .plan
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), PlanStepState::Pending))
            .collect();
        let mut previous_hash = None;
        let mut expected_sequence = 1_u64;
        let mut status = PlanInspectionStatus::Stable;
        let mut issue = None;
        for transition in &transitions {
            if transition.sequence != expected_sequence
                || transition.previous_hash != previous_hash
                || transition.verify_hash().is_err()
            {
                status = PlanInspectionStatus::Inconsistent;
                issue = Some(PlanInspectionIssue::TransitionChainInvalid);
                break;
            }
            if apply_transition_state(&mut computed_plan_state, &mut computed_steps, transition)
                .is_err()
            {
                status = PlanInspectionStatus::Inconsistent;
                issue = Some(PlanInspectionIssue::ReducerDiverged);
                break;
            }
            previous_hash = Some(transition.event_hash.clone());
            expected_sequence = expected_sequence.saturating_add(1);
        }
        if status == PlanInspectionStatus::Stable
            && (computed_plan_state != db_plan_state || computed_steps != db_step_states)
        {
            status = PlanInspectionStatus::Inconsistent;
            issue = Some(PlanInspectionIssue::ReducerDiverged);
        }
        if status == PlanInspectionStatus::Stable && !check_checkpoint_sequence(&checkpoints) {
            status = PlanInspectionStatus::Inconsistent;
            issue = Some(PlanInspectionIssue::CheckpointSequenceInvalid);
        }
        let mut plan = persisted.plan;
        plan.state = db_plan_state;
        Ok(PlanInspectionReport {
            plan,
            step_states: db_step_states,
            status,
            issue,
            checkpoints_checked: checkpoints.len() as u64,
            transitions_checked: transitions.len() as u64,
        })
    }

    /// Executa somente o preflight determinístico, sem alterar o plano.
    pub fn validate_plan(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
    ) -> Result<PlanVerificationReport, QueueError> {
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let approvals = load_approvals_tx(&transaction, &persisted.plan)?;
        transaction.commit()?;
        let mut context = PlanVerificationContext::new(PlanVerificationPhase::Preflight);
        context.approvals = approvals;
        context.now = Utc::now();
        Ok(PlanVerifier::default().verify(&persisted.plan, &context))
    }

    /// Lista checkpoints de uma revisão sem executar recovery.
    pub fn list_plan_checkpoints(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
    ) -> Result<Vec<PlanCheckpoint>, QueueError> {
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let (_, checkpoints, _, _) =
            self.load_reducer_rows(plan_id, persisted.plan.revision, tenant_id)?;
        if !check_checkpoint_sequence(&checkpoints) {
            return Err(QueueError::InvalidInput(
                "sequência de checkpoints inválida".to_owned(),
            ));
        }
        Ok(checkpoints)
    }

    /// Deriva um UUID estável para uma aprovação a partir da chave de idempotência.
    #[must_use]
    pub fn approval_id_for_idempotency(
        plan_id: &PlanId,
        revision: u32,
        idempotency_key: &str,
    ) -> Uuid {
        let material = format!(
            "shaka:plan:approval:{}:{revision}:{idempotency_key}",
            plan_id.0
        );
        let digest = Sha256::digest(material.as_bytes());
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        Uuid::from_bytes(bytes)
    }

    /// Persiste uma revisão de plano e suas etapas de forma append-only e idempotente.
    pub fn save_plan(&self, plan: &PlanSpec) -> Result<PersistedPlan, QueueError> {
        plan.validate_structure()?;
        plan.verify_digest()?;
        let plan_json = serde_json::to_string(plan)?;
        let mode = serde_json::to_string(&plan.mode)?;
        let risk = serde_json::to_string(&plan.risk)?;
        let now = Utc::now();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT state, digest, updated_at
                 FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
                params![plan.plan_id.0.to_string(), plan.revision, plan.tenant_id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((state, digest, updated_at)) = existing {
            if digest != plan.digest {
                return Err(QueueError::InvalidInput(
                    "revisão de plano existente é append-only".to_owned(),
                ));
            }
            let persisted_state = parse_plan_state(&state)?;
            transaction.commit()?;
            let mut persisted_plan = plan.clone();
            persisted_plan.state = persisted_state;
            return Ok(PersistedPlan {
                plan: persisted_plan,
                updated_at: parse_datetime(&updated_at)?,
            });
        }
        transaction.execute(
            "INSERT INTO plans
             (plan_id, tenant_id, task_id, revision, plan_json, state, mode, risk, digest, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                plan.plan_id.0.to_string(),
                plan.tenant_id.0,
                plan.task_id.0.to_string(),
                plan.revision,
                plan_json,
                plan_state_str(plan.state),
                mode,
                risk,
                plan.digest,
                plan.created_at.to_rfc3339(),
                now.to_rfc3339(),
            ],
        )?;
        for step in &plan.steps {
            transaction.execute(
                "INSERT INTO plan_steps
                 (plan_id, revision, step_id, depends_json, action_json, preconditions_json,
                  postconditions_json, state, attempts, max_attempts, compensation_step_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', 0, ?8, ?9)",
                params![
                    plan.plan_id.0.to_string(),
                    plan.revision,
                    step.step_id.0,
                    serde_json::to_string(&step.depends_on)?,
                    serde_json::to_string(&step.action)?,
                    serde_json::to_string(&step.preconditions)?,
                    serde_json::to_string(&step.postconditions)?,
                    step.max_attempts,
                    step.compensation_step_id.as_ref().map(|id| id.0.clone()),
                ],
            )?;
            if let Some(compensation_step_id) = &step.compensation_step_id {
                transaction.execute(
                    "INSERT INTO plan_compensations
                     (plan_id, revision, step_id, compensation_step_id)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        plan.plan_id.0.to_string(),
                        plan.revision,
                        step.step_id.0,
                        compensation_step_id.0,
                    ],
                )?;
            }
        }
        transaction.commit()?;
        Ok(PersistedPlan {
            plan: plan.clone(),
            updated_at: now,
        })
    }

    /// Carrega uma revisão de plano isolada por tenant e com digest verificado.
    pub fn load_plan(
        &self,
        plan_id: &PlanId,
        tenant_id: &TenantId,
    ) -> Result<PersistedPlan, QueueError> {
        let connection = self.connection.lock();
        let row = connection
            .query_row(
                "SELECT plan_json, state, digest, revision, updated_at
                 FROM plans WHERE plan_id = ?1 AND tenant_id = ?2
                 ORDER BY revision DESC LIMIT 1",
                params![plan_id.0.to_string(), tenant_id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, u32>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| QueueError::NotFound(format!("plan {}", plan_id.0)))?;
        let mut plan: PlanSpec = serde_json::from_str(&row.0)?;
        if plan.plan_id != *plan_id
            || plan.tenant_id != *tenant_id
            || plan.revision != row.3
            || plan.digest != row.2
        {
            return Err(QueueError::InvalidInput(
                "snapshot do plano não corresponde às colunas persistidas".to_owned(),
            ));
        }
        plan.state = parse_plan_state(&row.1)?;
        plan.validate_structure()?;
        plan.verify_digest()?;
        Ok(PersistedPlan {
            plan,
            updated_at: parse_datetime(&row.4)?,
        })
    }

    /// Persiste um checkpoint com sequência monotônica por revisão.
    pub fn append_plan_checkpoint(
        &self,
        tenant_id: &TenantId,
        checkpoint: &PlanCheckpoint,
    ) -> Result<(), QueueError> {
        let persisted = self.load_plan(&checkpoint.plan_id, tenant_id)?;
        if checkpoint.revision != persisted.plan.revision || checkpoint.sequence == 0 {
            return Err(QueueError::InvalidInput(
                "checkpoint não corresponde à revisão do plano".to_owned(),
            ));
        }
        if let Some(step_id) = &checkpoint.step_id {
            if !persisted
                .plan
                .steps
                .iter()
                .any(|step| &step.step_id == step_id)
            {
                return Err(QueueError::NotFound(format!("step {}", step_id.0)));
            }
        }
        if let Some(state_digest) = &checkpoint.state_digest {
            validate_sha256(state_digest, "state_digest")?;
        }
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT sequence FROM plan_checkpoints
                 WHERE plan_id = ?1 AND revision = ?2 ORDER BY sequence DESC LIMIT 1",
                params![checkpoint.plan_id.0.to_string(), checkpoint.revision],
                |row| row.get::<_, u64>(0),
            )
            .optional()?;
        if previous.is_some_and(|sequence| checkpoint.sequence <= sequence) {
            let same = transaction
                .query_row(
                    "SELECT phase, status, step_id, state_digest, created_at
                     FROM plan_checkpoints WHERE plan_id = ?1 AND revision = ?2 AND sequence = ?3",
                    params![
                        checkpoint.plan_id.0.to_string(),
                        checkpoint.revision,
                        checkpoint.sequence
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()?;
            if same.is_some_and(|row| {
                row.0 == checkpoint.phase.as_str()
                    && row.1 == checkpoint.status.as_str()
                    && row.2 == checkpoint.step_id.as_ref().map(|id| id.0.clone())
                    && row.3 == checkpoint.state_digest
                    && row.4 == checkpoint.created_at.to_rfc3339()
            }) {
                transaction.commit()?;
                return Ok(());
            }
            return Err(QueueError::IdempotencyConflict);
        }
        if previous.is_some_and(|sequence| checkpoint.sequence != sequence + 1) {
            return Err(QueueError::InvalidInput(
                "sequência de checkpoint não é contígua".to_owned(),
            ));
        }
        transaction.execute(
            "INSERT INTO plan_checkpoints
             (plan_id, revision, sequence, step_id, phase, status, state_digest, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                checkpoint.plan_id.0.to_string(),
                checkpoint.revision,
                checkpoint.sequence,
                checkpoint.step_id.as_ref().map(|id| id.0.clone()),
                checkpoint.phase.as_str(),
                checkpoint.status.as_str(),
                checkpoint.state_digest,
                checkpoint.created_at.to_rfc3339(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Registra uma transição validada pelo reducer e atualiza o snapshot na mesma transação.
    pub fn record_plan_transition(
        &self,
        tenant_id: &TenantId,
        transition: &PlanStoreTransition,
    ) -> Result<(), QueueError> {
        let persisted = self.load_plan(&transition.plan_id, tenant_id)?;
        if transition.revision != persisted.plan.revision {
            return Err(QueueError::InvalidInput(
                "transição não corresponde à revisão carregada".to_owned(),
            ));
        }
        transition.verify_hash()?;
        validate_transition_shape(
            transition.entity,
            transition.entity_id.as_ref(),
            transition.from_state,
            transition.to_state,
        )?;
        let mut connection = self.connection.lock();
        let transaction_db =
            connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let duplicate = transaction_db
            .query_row(
                "SELECT transition_json, event_hash FROM plan_transitions
                 WHERE plan_id = ?1 AND revision = ?2 AND idempotency_key = ?3",
                params![
                    transition.plan_id.0.to_string(),
                    transition.revision,
                    transition.idempotency_key
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((_, event_hash)) = duplicate {
            if event_hash == transition.event_hash {
                transaction_db.commit()?;
                return Ok(());
            }
            return Err(QueueError::IdempotencyConflict);
        }
        let last = transaction_db
            .query_row(
                "SELECT sequence, event_hash FROM plan_transitions
                 WHERE plan_id = ?1 AND revision = ?2 ORDER BY sequence DESC LIMIT 1",
                params![transition.plan_id.0.to_string(), transition.revision],
                |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let expected_sequence = last.as_ref().map_or(1, |row| row.0 + 1);
        let expected_previous = last.map(|row| row.1);
        if transition.sequence != expected_sequence || transition.previous_hash != expected_previous
        {
            return Err(QueueError::InvalidInput(
                "transição não continua a cadeia persistida".to_owned(),
            ));
        }
        apply_transition_tx(&transaction_db, tenant_id, transition)?;
        insert_transition_tx(&transaction_db, transition)?;
        transaction_db.commit()?;
        Ok(())
    }

    /// Persiste uma aprovação legada após revalidar seu vínculo ao plano.
    pub fn save_plan_approval(
        &self,
        tenant_id: &TenantId,
        approval: &PlanApproval,
    ) -> Result<(), QueueError> {
        let persisted = self.load_plan(&approval.plan_id, tenant_id)?;
        if approval.revision != persisted.plan.revision || approval.tenant_id != *tenant_id {
            return Err(QueueError::Forbidden);
        }
        approval.validate_for(&persisted.plan, approval.step_id.as_ref(), Utc::now())?;
        let approval_json = serde_json::to_string(approval)?;
        let connection = self.connection.lock();
        connection.execute(
            "INSERT INTO plan_approvals
             (approval_id, plan_id, revision, tenant_id, step_id, approval_json, revoked,
              expires_at, created_at, idempotency_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                approval.approval_id.to_string(),
                approval.plan_id.0.to_string(),
                approval.revision,
                approval.tenant_id.0,
                approval.step_id.as_ref().map(|id| id.0.clone()),
                approval_json,
                i64::from(approval.revoked),
                approval.expires_at.to_rfc3339(),
                Utc::now().to_rfc3339(),
                format!("legacy:{}", approval.approval_id),
            ],
        )?;
        Ok(())
    }

    /// Persiste uma aprovação ou rejeição com separação de funções e idempotência.
    #[allow(clippy::too_many_lines)]
    pub fn approve_plan(
        &self,
        principal: &Principal,
        approval: &PlanApproval,
        idempotency_key: &str,
    ) -> Result<PlanApprovalOutcome, QueueError> {
        if !principal.allows(&shaka_core::Action::ApprovePlan)
            || principal.operator_id != approval.approver
            || principal.role != approval.approver_role
        {
            return Err(QueueError::Forbidden);
        }
        validate_key(idempotency_key, "approval_idempotency_key", 256)?;
        let reference = PlanTaskReference::new(
            approval.plan_id.clone(),
            approval.revision,
            approval.plan_digest.clone(),
        )?;
        let now = Utc::now();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let plan = load_plan_tx(&transaction, &principal.tenant_id, &reference)?;
        validate_approval_shape(&plan, principal, approval, now)?;
        let approval_json = serde_json::to_string(approval)?;
        let existing = transaction
            .query_row(
                "SELECT approval_json FROM plan_approvals
                 WHERE plan_id = ?1 AND revision = ?2 AND idempotency_key = ?3",
                params![
                    approval.plan_id.0.to_string(),
                    approval.revision,
                    idempotency_key
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(existing_json) = existing {
            let existing_approval: PlanApproval = serde_json::from_str(&existing_json)?;
            if same_approval_intent(&existing_approval, approval) {
                transaction.commit()?;
                return Ok(PlanApprovalOutcome::Existing);
            }
            return Err(QueueError::IdempotencyConflict);
        }
        transaction.execute(
            "INSERT INTO plan_approvals
             (approval_id, plan_id, revision, tenant_id, step_id, approval_json, revoked,
              expires_at, created_at, idempotency_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                approval.approval_id.to_string(),
                approval.plan_id.0.to_string(),
                approval.revision,
                approval.tenant_id.0,
                approval.step_id.as_ref().map(|id| id.0.clone()),
                approval_json,
                i64::from(approval.revoked),
                approval.expires_at.to_rfc3339(),
                now.to_rfc3339(),
                idempotency_key,
            ],
        )?;
        let outcome = match approval.decision {
            PlanApprovalDecision::Approved => {
                if approval.step_id.is_some() {
                    PlanApprovalOutcome::Approved
                } else {
                    let mut state = plan.state;
                    if state == PlanState::Draft {
                        append_plan_transition_tx(
                            &transaction,
                            principal,
                            &plan,
                            PlanState::Draft,
                            PlanState::Proposed,
                            &format!("approval:proposed:{}", approval.approval_id),
                            now,
                        )?;
                        state = PlanState::Proposed;
                    }
                    if state == PlanState::Proposed {
                        append_plan_transition_tx(
                            &transaction,
                            principal,
                            &plan,
                            PlanState::Proposed,
                            PlanState::Validated,
                            &format!("approval:validated:{}", approval.approval_id),
                            now,
                        )?;
                        state = PlanState::Validated;
                    }
                    if !matches!(state, PlanState::Validated | PlanState::AwaitingApproval) {
                        return Err(QueueError::InvalidInput(
                            "plano não está aguardando aprovação".to_owned(),
                        ));
                    }
                    append_plan_transition_tx(
                        &transaction,
                        principal,
                        &plan,
                        state,
                        PlanState::Approved,
                        &format!("approval:approved:{}", approval.approval_id),
                        now,
                    )?;
                    PlanApprovalOutcome::Approved
                }
            }
            PlanApprovalDecision::Rejected => {
                if !matches!(
                    plan.state,
                    PlanState::Draft
                        | PlanState::Proposed
                        | PlanState::Validated
                        | PlanState::AwaitingApproval
                ) {
                    return Err(QueueError::InvalidInput(
                        "plano não pode mais ser rejeitado".to_owned(),
                    ));
                }
                append_plan_transition_tx(
                    &transaction,
                    principal,
                    &plan,
                    plan.state,
                    PlanState::Rejected,
                    &format!("approval:rejected:{}", approval.approval_id),
                    now,
                )?;
                PlanApprovalOutcome::Rejected
            }
        };
        insert_checkpoint_tx(
            &transaction,
            &plan.plan_id,
            plan.revision,
            approval.step_id.as_ref(),
            PlanCheckpointPhase::Approval,
            if outcome == PlanApprovalOutcome::Approved {
                PlanCheckpointStatus::Succeeded
            } else {
                PlanCheckpointStatus::Failed
            },
            None,
            now,
        )?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Resolve um plano `unknown` por uma decisão humana idempotente.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn resolve_plan_unknown(
        &self,
        principal: &Principal,
        plan_id: &PlanId,
        decision: PlanResolutionDecision,
        idempotency_key: &str,
        evidence_digest: Option<&str>,
    ) -> Result<PlanResolutionOutcome, QueueError> {
        if !principal.allows(&shaka_core::Action::ResolvePlanUnknown) {
            return Err(QueueError::Forbidden);
        }
        validate_key(idempotency_key, "resolution_idempotency_key", 256)?;
        if let Some(evidence_digest) = evidence_digest {
            validate_sha256(evidence_digest, "resolution evidence digest")?;
        }
        if matches!(decision, PlanResolutionDecision::Resume) && evidence_digest.is_none() {
            return Err(QueueError::InvalidInput(
                "retomada de unknown exige digest de evidência".to_owned(),
            ));
        }
        let persisted = self.load_plan(plan_id, &principal.tenant_id)?;
        let reference = PlanTaskReference::new(
            persisted.plan.plan_id.clone(),
            persisted.plan.revision,
            persisted.plan.digest.clone(),
        )?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing_json) = transaction
            .query_row(
                "SELECT transition_json FROM plan_transitions
                 WHERE plan_id = ?1 AND revision = ?2 AND idempotency_key = ?3",
                params![
                    plan_id.0.to_string(),
                    persisted.plan.revision,
                    idempotency_key
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let existing: PlanStoreTransition = serde_json::from_str(&existing_json)?;
            let expected = match decision {
                PlanResolutionDecision::Resume => PlanState::Running,
                PlanResolutionDecision::Compensate => PlanState::Compensating,
                PlanResolutionDecision::Cancel => PlanState::Cancelled,
            };
            if existing.to_state == PlanTransitionState::Plan(expected) {
                transaction.commit()?;
                return Ok(PlanResolutionOutcome::Existing);
            }
            return Err(QueueError::IdempotencyConflict);
        }
        let current_state = transaction.query_row(
            "SELECT state FROM plans
             WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
            params![
                plan_id.0.to_string(),
                persisted.plan.revision,
                principal.tenant_id.0
            ],
            |row| row.get::<_, String>(0),
        )?;
        if parse_plan_state(&current_state)? != PlanState::Unknown {
            return Err(QueueError::InvalidInput(
                "resolução exige plano em unknown".to_owned(),
            ));
        }
        let outcome = match decision {
            PlanResolutionDecision::Resume => {
                let unknown_steps = load_step_states_tx(&transaction, &persisted.plan)?
                    .into_iter()
                    .filter(|(_, state)| *state == PlanStepState::Unknown)
                    .collect::<Vec<_>>();
                for (step_id, state) in unknown_steps {
                    append_step_transition_tx(
                        &transaction,
                        &persisted.plan,
                        &step_id,
                        state,
                        PlanStepState::Ready,
                        &format!("{idempotency_key}:step:{}", step_id.0),
                        Utc::now(),
                    )?;
                }
                append_plan_transition_tx(
                    &transaction,
                    principal,
                    &persisted.plan,
                    PlanState::Unknown,
                    PlanState::Running,
                    idempotency_key,
                    Utc::now(),
                )?;
                update_task_for_resolution(
                    &transaction,
                    &persisted.plan,
                    TaskStatus::Queued,
                    false,
                    None,
                )?;
                PlanResolutionOutcome::Resumed
            }
            PlanResolutionDecision::Compensate => {
                let states = load_step_states_tx(&transaction, &persisted.plan)?;
                let compensation_sources: Vec<(PlanStepId, PlanStepId, PlanStepState)> = persisted
                    .plan
                    .steps
                    .iter()
                    .filter_map(|step| {
                        let compensation = step.compensation_step_id.as_ref()?;
                        let state = states.get(&step.step_id).copied()?;
                        if matches!(
                            state,
                            PlanStepState::Succeeded
                                | PlanStepState::Failed
                                | PlanStepState::Unknown
                                | PlanStepState::Compensating
                        ) {
                            Some((step.step_id.clone(), compensation.clone(), state))
                        } else {
                            None
                        }
                    })
                    .collect();
                if compensation_sources.is_empty() {
                    return Err(QueueError::InvalidInput(
                        "plano não possui compensação declarada elegível".to_owned(),
                    ));
                }
                let compensation_targets: BTreeSet<PlanStepId> = compensation_sources
                    .iter()
                    .map(|(_, compensation_id, _)| compensation_id.clone())
                    .collect();
                for (source_id, _, state) in &compensation_sources {
                    if *state != PlanStepState::Compensating {
                        append_step_transition_tx(
                            &transaction,
                            &persisted.plan,
                            source_id,
                            *state,
                            PlanStepState::Compensating,
                            &format!("{idempotency_key}:source:{}", source_id.0),
                            Utc::now(),
                        )?;
                    }
                }
                let states = load_step_states_tx(&transaction, &persisted.plan)?;
                for compensation_id in compensation_targets {
                    if let Some(state) = states.get(&compensation_id).copied()
                        && matches!(state, PlanStepState::Unknown | PlanStepState::Failed)
                    {
                        append_step_transition_tx(
                            &transaction,
                            &persisted.plan,
                            &compensation_id,
                            state,
                            PlanStepState::Ready,
                            &format!("{idempotency_key}:target:{}", compensation_id.0),
                            Utc::now(),
                        )?;
                    }
                }
                append_plan_transition_tx(
                    &transaction,
                    principal,
                    &persisted.plan,
                    PlanState::Unknown,
                    PlanState::Compensating,
                    idempotency_key,
                    Utc::now(),
                )?;
                update_task_for_resolution(
                    &transaction,
                    &persisted.plan,
                    TaskStatus::Queued,
                    false,
                    None,
                )?;
                PlanResolutionOutcome::Compensating
            }
            PlanResolutionDecision::Cancel => {
                let states = load_step_states_tx(&transaction, &persisted.plan)?;
                if states.values().any(|state| {
                    matches!(
                        state,
                        PlanStepState::Running
                            | PlanStepState::CancelRequested
                            | PlanStepState::Compensating
                            | PlanStepState::Unknown
                    )
                }) {
                    return Err(QueueError::InvalidInput(
                        "cancelamento bloqueado enquanto houver fronteira ativa ou unknown"
                            .to_owned(),
                    ));
                }
                for (step_id, state) in states
                    .into_iter()
                    .filter(|(_, state)| *state == PlanStepState::Unknown)
                {
                    append_step_transition_tx(
                        &transaction,
                        &persisted.plan,
                        &step_id,
                        state,
                        PlanStepState::Cancelled,
                        &format!("{idempotency_key}:step:{}", step_id.0),
                        Utc::now(),
                    )?;
                }
                append_plan_transition_tx(
                    &transaction,
                    principal,
                    &persisted.plan,
                    PlanState::Unknown,
                    PlanState::Cancelled,
                    idempotency_key,
                    Utc::now(),
                )?;
                update_task_for_resolution(
                    &transaction,
                    &persisted.plan,
                    TaskStatus::Cancelled,
                    true,
                    Some("plano unknown cancelado por operador"),
                )?;
                PlanResolutionOutcome::Cancelled
            }
        };
        insert_checkpoint_tx(
            &transaction,
            &reference.plan_id,
            reference.revision,
            None,
            PlanCheckpointPhase::Resolution,
            if matches!(outcome, PlanResolutionOutcome::Cancelled) {
                PlanCheckpointStatus::Failed
            } else {
                PlanCheckpointStatus::Succeeded
            },
            evidence_digest,
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(outcome)
    }

    /// Marca explicitamente uma fronteira ambígua como `unknown` sem retry automático.
    pub fn mark_plan_unknown(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
        step_id: Option<&PlanStepId>,
        state_digest: Option<&str>,
    ) -> Result<(), QueueError> {
        if let Some(state_digest) = state_digest {
            validate_sha256(state_digest, "state_digest")?;
        }
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        mark_plan_unknown_tx(
            &transaction,
            tenant_id,
            &persisted.plan,
            step_id,
            state_digest,
            Utc::now(),
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Reconstrói o reducer e converte qualquer fronteira ambígua em `unknown`.
    pub fn resume_plan(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
    ) -> Result<PlanResumeReport, QueueError> {
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let (transitions, checkpoints, db_plan_state, db_step_states) =
            self.load_reducer_rows(plan_id, persisted.plan.revision, tenant_id)?;
        let mut computed_plan_state = PlanState::Draft;
        let mut computed_steps: BTreeMap<PlanStepId, PlanStepState> = persisted
            .plan
            .steps
            .iter()
            .map(|step| (step.step_id.clone(), PlanStepState::Pending))
            .collect();
        let mut previous_hash = None;
        let mut expected_sequence = 1_u64;
        let mut inconsistency = None;
        for transition in &transitions {
            if transition.sequence != expected_sequence
                || transition.previous_hash != previous_hash
                || transition.verify_hash().is_err()
            {
                inconsistency = Some("cadeia de transições inválida".to_owned());
                break;
            }
            if let Err(error) =
                apply_transition_state(&mut computed_plan_state, &mut computed_steps, transition)
            {
                inconsistency = Some(safe_error(&error));
                break;
            }
            previous_hash = Some(transition.event_hash.clone());
            expected_sequence = expected_sequence.saturating_add(1);
        }
        if inconsistency.is_none()
            && (computed_plan_state != db_plan_state || computed_steps != db_step_states)
        {
            inconsistency = Some("snapshot do reducer diverge da cadeia".to_owned());
        }
        if inconsistency.is_none() && !check_checkpoint_sequence(&checkpoints) {
            inconsistency = Some("sequência de checkpoints inválida".to_owned());
        }
        if let Some(reason) = inconsistency {
            let forced = self.force_unknown(tenant_id, plan_id, persisted.plan.revision)?;
            return Ok(PlanResumeReport {
                plan: forced.0,
                step_states: forced.1,
                status: PlanResumeStatus::Inconsistent,
                checkpoints_checked: checkpoints.len() as u64 + 1,
                transitions_checked: transitions.len() as u64,
                inconsistency: Some(reason),
            });
        }
        let active_plan = matches!(
            db_plan_state,
            PlanState::Running | PlanState::CancelRequested | PlanState::Compensating
        );
        let active_steps: Vec<(PlanStepId, PlanStepState)> = db_step_states
            .iter()
            .filter(|(_, state)| {
                matches!(
                    state,
                    PlanStepState::Running
                        | PlanStepState::CancelRequested
                        | PlanStepState::Compensating
                )
            })
            .map(|(step_id, state)| (step_id.clone(), *state))
            .collect();
        if active_plan || !active_steps.is_empty() {
            let recovered = self.recover_unknown(
                tenant_id,
                plan_id,
                persisted.plan.revision,
                db_plan_state,
                &active_steps,
                &persisted.plan.digest,
            )?;
            return Ok(PlanResumeReport {
                plan: recovered.0,
                step_states: recovered.1,
                status: PlanResumeStatus::RecoveredUnknown,
                checkpoints_checked: checkpoints.len() as u64 + 1,
                transitions_checked: transitions.len() as u64,
                inconsistency: None,
            });
        }
        let mut stable_plan = persisted.plan;
        stable_plan.state = db_plan_state;
        Ok(PlanResumeReport {
            plan: stable_plan,
            step_states: db_step_states,
            status: PlanResumeStatus::Stable,
            checkpoints_checked: checkpoints.len() as u64,
            transitions_checked: transitions.len() as u64,
            inconsistency: None,
        })
    }

    fn load_reducer_rows(
        &self,
        plan_id: &PlanId,
        revision: u32,
        tenant_id: &TenantId,
    ) -> Result<PlanReducerRows, QueueError> {
        let connection = self.connection.lock();
        let db_plan_state = connection.query_row(
            "SELECT state FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
            params![plan_id.0.to_string(), revision, tenant_id.0],
            |row| row.get::<_, String>(0),
        )?;
        let mut transition_statement = connection.prepare(
            "SELECT transition_json FROM plan_transitions
             WHERE plan_id = ?1 AND revision = ?2 ORDER BY sequence ASC",
        )?;
        let transitions = transition_statement
            .query_map(params![plan_id.0.to_string(), revision], |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| {
                let json = row?;
                serde_json::from_str::<PlanStoreTransition>(&json)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(QueueError::from)?;
        let mut checkpoint_statement = connection.prepare(
            "SELECT sequence, step_id, phase, status, state_digest, created_at
             FROM plan_checkpoints WHERE plan_id = ?1 AND revision = ?2 ORDER BY sequence ASC",
        )?;
        let checkpoints = checkpoint_statement
            .query_map(params![plan_id.0.to_string(), revision], |row| {
                let step_id = row.get::<_, Option<String>>(1)?;
                Ok((
                    row.get::<_, u64>(0)?,
                    step_id,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .map(|row| {
                let (sequence, step_id, phase, status, state_digest, created_at) = row?;
                Ok::<PlanCheckpoint, rusqlite::Error>(PlanCheckpoint {
                    plan_id: plan_id.clone(),
                    revision,
                    sequence,
                    step_id: step_id.map(PlanStepId::new).transpose().map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                    phase: PlanCheckpointPhase::parse(&phase).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                    status: PlanCheckpointStatus::parse(&status).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                    state_digest,
                    created_at: parse_datetime(&created_at).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(QueueError::from)?;
        let mut step_statement = connection.prepare(
            "SELECT step_id, state FROM plan_steps
             WHERE plan_id = ?1 AND revision = ?2 ORDER BY step_id ASC",
        )?;
        let step_states = step_statement
            .query_map(params![plan_id.0.to_string(), revision], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (step_id, state) = row?;
                Ok::<(PlanStepId, PlanStepState), rusqlite::Error>((
                    PlanStepId::new(step_id).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                    parse_step_state(&state).map_err(|error| {
                        rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                    })?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(QueueError::from)?;
        Ok((
            transitions,
            checkpoints,
            parse_plan_state(&db_plan_state)?,
            step_states,
        ))
    }

    fn recover_unknown(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
        revision: u32,
        plan_state: PlanState,
        active_steps: &[(PlanStepId, PlanStepState)],
        state_digest: &str,
    ) -> Result<(PlanSpec, BTreeMap<PlanStepId, PlanStepState>), QueueError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_plan_state = transaction.query_row(
            "SELECT state FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
            params![plan_id.0.to_string(), revision, tenant_id.0],
            |row| row.get::<_, String>(0),
        )?;
        let current_plan_state = parse_plan_state(&current_plan_state)?;
        if current_plan_state == PlanState::Unknown {
            transaction.commit()?;
            drop(connection);
            let persisted = self.load_plan(plan_id, tenant_id)?;
            let (_, _, _, current_states) = self.load_reducer_rows(plan_id, revision, tenant_id)?;
            return Ok((persisted.plan, current_states));
        }
        if current_plan_state != plan_state {
            return Err(QueueError::InvalidInput(
                "plano mudou durante o recovery".to_owned(),
            ));
        }
        let current_active_steps = Self::active_steps_tx(&transaction, plan_id, revision)?;
        if current_active_steps != active_steps {
            return Err(QueueError::InvalidInput(
                "etapas ativas mudaram durante o recovery".to_owned(),
            ));
        }
        let mut states = Self::append_recovery_transitions_tx(
            &transaction,
            plan_id,
            revision,
            plan_state,
            active_steps,
        )?;
        transaction.execute(
            "UPDATE plans SET state = 'unknown', updated_at = ?1
             WHERE plan_id = ?2 AND revision = ?3 AND tenant_id = ?4",
            params![
                Utc::now().to_rfc3339(),
                plan_id.0.to_string(),
                revision,
                tenant_id.0
            ],
        )?;
        transaction.execute(
            "UPDATE plan_steps SET state = 'unknown'
             WHERE plan_id = ?1 AND revision = ?2
               AND state IN ('running', 'cancel_requested', 'compensating')",
            params![plan_id.0.to_string(), revision],
        )?;
        let checkpoint_sequence = next_checkpoint_sequence_tx(&transaction, plan_id, revision)?;
        transaction.execute(
            "INSERT INTO plan_checkpoints
             (plan_id, revision, sequence, step_id, phase, status, state_digest, created_at)
             VALUES (?1, ?2, ?3, NULL, 'recovery', 'unknown', ?4, ?5)",
            params![
                plan_id.0.to_string(),
                revision,
                checkpoint_sequence,
                state_digest,
                Utc::now().to_rfc3339()
            ],
        )?;
        transaction.commit()?;
        drop(connection);
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let (_, _, _, current_states) = self.load_reducer_rows(plan_id, revision, tenant_id)?;
        for step in &persisted.plan.steps {
            let step_id = step.step_id.clone();
            let state = current_states
                .get(&step_id)
                .copied()
                .unwrap_or(PlanStepState::Pending);
            states.entry(step_id).or_insert(state);
        }
        Ok((persisted.plan, states))
    }

    fn active_steps_tx(
        transaction: &Transaction<'_>,
        plan_id: &PlanId,
        revision: u32,
    ) -> Result<Vec<(PlanStepId, PlanStepState)>, QueueError> {
        let mut statement = transaction.prepare(
            "SELECT step_id, state FROM plan_steps
             WHERE plan_id = ?1 AND revision = ?2
               AND state IN ('running', 'cancel_requested', 'compensating')
             ORDER BY step_id ASC",
        )?;
        let mut rows = statement.query(params![plan_id.0.to_string(), revision])?;
        let mut steps = Vec::new();
        while let Some(row) = rows.next()? {
            let step_id = PlanStepId::new(row.get::<_, String>(0)?)?;
            let state = parse_step_state(&row.get::<_, String>(1)?)?;
            steps.push((step_id, state));
        }
        Ok(steps)
    }

    fn append_recovery_transitions_tx(
        transaction: &Transaction<'_>,
        plan_id: &PlanId,
        revision: u32,
        plan_state: PlanState,
        active_steps: &[(PlanStepId, PlanStepState)],
    ) -> Result<BTreeMap<PlanStepId, PlanStepState>, QueueError> {
        let last = last_transition_tx(transaction, plan_id, revision)?;
        let mut sequence = last.as_ref().map_or(1, |row| row.0 + 1);
        let mut previous_hash = last.map(|row| row.1);
        if !matches!(plan_state, PlanState::Unknown) {
            let transition = PlanStoreTransition::new(
                plan_id.clone(),
                revision,
                sequence,
                PlanTransitionEntity::Plan,
                None,
                PlanTransitionState::Plan(plan_state),
                PlanTransitionState::Plan(PlanState::Unknown),
                format!("recovery:plan:{sequence}"),
                previous_hash,
                Utc::now(),
            )?;
            insert_transition_tx(transaction, &transition)?;
            previous_hash = Some(transition.event_hash);
            sequence += 1;
        }
        let mut states = BTreeMap::new();
        for (step_id, state) in active_steps {
            let transition = PlanStoreTransition::new(
                plan_id.clone(),
                revision,
                sequence,
                PlanTransitionEntity::Step,
                Some(step_id.clone()),
                PlanTransitionState::Step(*state),
                PlanTransitionState::Step(PlanStepState::Unknown),
                format!("recovery:step:{}:{sequence}", step_id.0),
                previous_hash,
                Utc::now(),
            )?;
            insert_transition_tx(transaction, &transition)?;
            previous_hash = Some(transition.event_hash);
            sequence += 1;
            states.insert(step_id.clone(), PlanStepState::Unknown);
        }
        Ok(states)
    }

    fn force_unknown(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
        revision: u32,
    ) -> Result<(PlanSpec, BTreeMap<PlanStepId, PlanStepState>), QueueError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE plans SET state = 'unknown', updated_at = ?1
             WHERE plan_id = ?2 AND revision = ?3 AND tenant_id = ?4",
            params![
                Utc::now().to_rfc3339(),
                plan_id.0.to_string(),
                revision,
                tenant_id.0
            ],
        )?;
        transaction.execute(
            "UPDATE plan_steps SET state = 'unknown'
             WHERE plan_id = ?1 AND revision = ?2
               AND state IN ('running', 'cancel_requested', 'compensating')",
            params![plan_id.0.to_string(), revision],
        )?;
        let recovery_checkpoint_exists = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM plan_checkpoints
                 WHERE plan_id = ?1 AND revision = ?2
                   AND phase = 'recovery' AND status = 'unknown'
             )",
            params![plan_id.0.to_string(), revision],
            |row| row.get::<_, i64>(0),
        )? != 0;
        if !recovery_checkpoint_exists {
            let checkpoint_sequence = next_checkpoint_sequence_tx(&transaction, plan_id, revision)?;
            transaction.execute(
                "INSERT INTO plan_checkpoints
                 (plan_id, revision, sequence, step_id, phase, status, state_digest, created_at)
                 SELECT ?1, ?2, ?3, NULL, 'recovery', 'unknown', digest, ?4
                 FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?5",
                params![
                    plan_id.0.to_string(),
                    revision,
                    checkpoint_sequence,
                    Utc::now().to_rfc3339(),
                    tenant_id.0
                ],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let (_, _, _, states) = self.load_reducer_rows(plan_id, revision, tenant_id)?;
        Ok((persisted.plan, states))
    }
}

fn same_approval_intent(left: &PlanApproval, right: &PlanApproval) -> bool {
    left.approval_id == right.approval_id
        && left.plan_id == right.plan_id
        && left.plan_digest == right.plan_digest
        && left.revision == right.revision
        && left.tenant_id == right.tenant_id
        && left.approver == right.approver
        && left.approver_role == right.approver_role
        && left.step_id == right.step_id
        && left.required == right.required
        && left.decision == right.decision
        && left.revoked == right.revoked
}

fn validate_approval_shape(
    plan: &PlanSpec,
    principal: &Principal,
    approval: &PlanApproval,
    now: DateTime<Utc>,
) -> Result<(), QueueError> {
    if approval.plan_id != plan.plan_id
        || approval.revision != plan.revision
        || approval.plan_digest != plan.digest
        || approval.tenant_id != plan.tenant_id
        || approval.tenant_id != principal.tenant_id
        || approval.approver != principal.operator_id
        || approval.approver_role != principal.role
        || approval.revoked
        || approval.expires_at <= now
    {
        return Err(QueueError::Forbidden);
    }
    if let Some(step_id) = &approval.step_id
        && !plan.steps.iter().any(|step| &step.step_id == step_id)
    {
        return Err(QueueError::NotFound(format!("step {}", step_id.0)));
    }
    let required_for_scope = approval
        .step_id
        .as_ref()
        .and_then(|step_id| plan.steps.iter().find(|step| &step.step_id == step_id))
        .map_or_else(
            || plan.required_approval(),
            |step| step.approval.max(step.risk.minimum_approval()),
        );
    if approval.required < required_for_scope
        || !approval.required.allows_role(&approval.approver_role)
    {
        return Err(QueueError::Forbidden);
    }
    if approval.decision == PlanApprovalDecision::Approved {
        approval.validate_for(plan, approval.step_id.as_ref(), now)?;
    }
    Ok(())
}

fn update_task_for_resolution(
    transaction: &Transaction<'_>,
    plan: &PlanSpec,
    status: TaskStatus,
    cancel_requested: bool,
    last_error: Option<&str>,
) -> Result<(), QueueError> {
    let now = Utc::now();
    let changed = transaction.execute(
        "UPDATE api_tasks SET status = ?1, cancel_requested = ?2,
         lease_until = NULL, lease_token = NULL, plan_step_id = NULL, last_error = ?3,
         updated_at = ?4, completed_at = CASE WHEN ?2 = 1 THEN ?4 ELSE NULL END
         WHERE task_id = ?5 AND tenant_id = ?6 AND plan_id = ?7 AND plan_revision = ?8",
        params![
            status.as_str(),
            i64::from(cancel_requested),
            last_error,
            now.to_rfc3339(),
            plan.task_id.0.to_string(),
            plan.tenant_id.0,
            plan.plan_id.0.to_string(),
            plan.revision,
        ],
    )?;
    if changed != 1 {
        return Err(QueueError::NotFound(
            "task planejada não encontrada".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn verify_plan_admission_tx(
    transaction: &Transaction<'_>,
    principal: &Principal,
    envelope: &TaskEnvelope,
    reference: &PlanTaskReference,
) -> Result<PlanSpec, QueueError> {
    let plan = load_plan_tx(transaction, &principal.tenant_id, reference)?;
    if plan.task_id != envelope.task_id || plan.operator_id != principal.operator_id {
        return Err(QueueError::Forbidden);
    }
    if plan.mode != PlanMode::DryRun {
        return Err(QueueError::InvalidInput(
            "execução live planejada ainda não possui executor tipado".to_owned(),
        ));
    }
    if !matches!(
        plan.state,
        PlanState::Draft
            | PlanState::Proposed
            | PlanState::Validated
            | PlanState::AwaitingApproval
            | PlanState::Approved
    ) {
        return Err(QueueError::InvalidInput(
            "plano não está em estado admitível".to_owned(),
        ));
    }
    let mut context = PlanVerificationContext::new(PlanVerificationPhase::Preflight);
    context.approvals = load_approvals_tx(transaction, &plan)?;
    let report = PlanVerifier::default().verify(&plan, &context);
    if !report.is_executable() {
        return Err(QueueError::InvalidInput(
            "plano não passou pela verificação preflight".to_owned(),
        ));
    }
    Ok(plan)
}

pub(crate) fn record_plan_admission_tx(
    transaction: &Transaction<'_>,
    principal: &Principal,
    envelope: &TaskEnvelope,
    reference: &PlanTaskReference,
) -> Result<Option<Uuid>, QueueError> {
    let plan = verify_plan_admission_tx(transaction, principal, envelope, reference)?;
    let mut state = plan.state;
    let now = Utc::now();
    let admission_approval_id = if plan.required_approval() == PlanApprovalRequirement::None {
        None
    } else {
        load_approvals_tx(transaction, &plan)?
            .into_iter()
            .find(|approval| {
                approval.step_id.is_none() && approval.validate_for(&plan, None, now).is_ok()
            })
            .map(|approval| approval.approval_id)
    };
    if state == PlanState::Draft {
        append_plan_transition_tx(
            transaction,
            principal,
            &plan,
            PlanState::Draft,
            PlanState::Proposed,
            "admission:proposed",
            now,
        )?;
        state = PlanState::Proposed;
    }
    if state == PlanState::Proposed {
        append_plan_transition_tx(
            transaction,
            principal,
            &plan,
            PlanState::Proposed,
            PlanState::Validated,
            "admission:validated",
            now,
        )?;
        state = PlanState::Validated;
    }
    if state == PlanState::Validated || state == PlanState::AwaitingApproval {
        append_plan_transition_tx(
            transaction,
            principal,
            &plan,
            state,
            PlanState::Approved,
            "admission:approved",
            now,
        )?;
    }
    insert_checkpoint_tx(
        transaction,
        &plan.plan_id,
        plan.revision,
        None,
        PlanCheckpointPhase::Preflight,
        PlanCheckpointStatus::Succeeded,
        Some(&plan.digest),
        now,
    )?;
    Ok(admission_approval_id)
}
#[allow(clippy::too_many_lines)]
pub(crate) fn prepare_planned_claim_tx(
    transaction: &Transaction<'_>,
    task: &TaskRecord,
    reference: &PlanTaskReference,
    claim_context: &PlanClaimContext,
    now: DateTime<Utc>,
) -> Result<Option<PlanStep>, QueueError> {
    let plan = load_plan_tx(transaction, &task.tenant_id, reference)?;
    if plan.task_id != task.task_id || plan.mode != PlanMode::DryRun {
        return Err(QueueError::Forbidden);
    }
    if task.plan_step_id.is_some() {
        return Err(QueueError::InvalidInput(
            "task planejada já possui etapa locada".to_owned(),
        ));
    }
    if !matches!(
        plan.state,
        PlanState::Approved | PlanState::Running | PlanState::Compensating
    ) {
        return Ok(None);
    }
    let states = load_step_states_tx(transaction, &plan)?;
    let succeeded_steps: BTreeSet<PlanStepId> = states
        .iter()
        .filter(|(_, state)| **state == PlanStepState::Succeeded)
        .map(|(step_id, _)| step_id.clone())
        .collect();
    let approvals = load_approvals_tx(transaction, &plan)?;
    let mut preflight_context = PlanVerificationContext::new(PlanVerificationPhase::Preflight);
    preflight_context.approvals.clone_from(&approvals);
    preflight_context.now = now;
    if !PlanVerifier::default()
        .verify(&plan, &preflight_context)
        .is_executable()
    {
        return Ok(None);
    }
    let compensation_targets: BTreeSet<PlanStepId> = if plan.state == PlanState::Compensating {
        plan.steps
            .iter()
            .filter(|step| states.get(&step.step_id) == Some(&PlanStepState::Compensating))
            .filter_map(|step| step.compensation_step_id.clone())
            .collect()
    } else {
        BTreeSet::new()
    };
    let mut candidates: Vec<&PlanStep> = plan
        .steps
        .iter()
        .filter(|step| {
            matches!(
                states.get(&step.step_id),
                Some(PlanStepState::Ready | PlanStepState::Pending)
            ) && (plan.state != PlanState::Compensating
                || compensation_targets.contains(&step.step_id))
        })
        .collect();
    candidates.sort_by(|left, right| left.step_id.cmp(&right.step_id));
    let remaining_budget = claim_context
        .remaining_budget
        .clone()
        .unwrap_or_else(|| task.envelope.budget.clone());
    let selected = candidates.into_iter().find(|step| {
        if !step
            .depends_on
            .iter()
            .all(|dependency| succeeded_steps.contains(dependency))
        {
            return false;
        }
        let mut context = PlanVerificationContext::new(PlanVerificationPhase::StepReady)
            .for_step(step.step_id.clone());
        context.task_state = Some(PlanTaskState::Running);
        context.succeeded_steps.clone_from(&succeeded_steps);
        context
            .granted_capabilities
            .clone_from(&claim_context.granted_capabilities);
        context.circuit_closed = claim_context.circuit_closed;
        context.remaining_budget = remaining_budget.clone();
        context.state_digest.clone_from(&claim_context.state_digest);
        context.approvals.clone_from(&approvals);
        context.now = now;
        PlanVerifier::default()
            .verify(&plan, &context)
            .is_executable()
    });
    let Some(selected) = selected else {
        return Ok(None);
    };
    if plan.state == PlanState::Approved {
        append_plan_transition_tx(
            transaction,
            &Principal {
                operator_id: plan.operator_id.clone(),
                tenant_id: plan.tenant_id.clone(),
                role: shaka_core::Role::Administrator,
            },
            &plan,
            PlanState::Approved,
            PlanState::Running,
            &format!("claim:plan:{}", selected.step_id.0),
            now,
        )?;
    } else if !matches!(plan.state, PlanState::Running | PlanState::Compensating) {
        return Ok(None);
    }
    let current_step_state = states
        .get(&selected.step_id)
        .copied()
        .ok_or_else(|| QueueError::NotFound(format!("step {}", selected.step_id.0)))?;
    let current_attempts: u32 = transaction.query_row(
        "SELECT attempts FROM plan_steps
         WHERE plan_id = ?1 AND revision = ?2 AND step_id = ?3",
        params![
            plan.plan_id.0.to_string(),
            plan.revision,
            selected.step_id.0
        ],
        |row| row.get(0),
    )?;
    let claim_attempt = current_attempts.saturating_add(1);
    if current_step_state == PlanStepState::Pending {
        append_step_transition_tx(
            transaction,
            &plan,
            &selected.step_id,
            PlanStepState::Pending,
            PlanStepState::Ready,
            &format!("claim:ready:{}:{claim_attempt}", selected.step_id.0),
            now,
        )?;
    }
    append_step_transition_tx(
        transaction,
        &plan,
        &selected.step_id,
        PlanStepState::Ready,
        PlanStepState::Running,
        &format!("claim:running:{}:{claim_attempt}", selected.step_id.0),
        now,
    )?;
    transaction.execute(
        "UPDATE plan_steps SET attempts = attempts + 1
         WHERE plan_id = ?1 AND revision = ?2 AND step_id = ?3",
        params![
            plan.plan_id.0.to_string(),
            plan.revision,
            selected.step_id.0
        ],
    )?;
    insert_checkpoint_tx(
        transaction,
        &plan.plan_id,
        plan.revision,
        Some(&selected.step_id),
        PlanCheckpointPhase::BeforeStep,
        PlanCheckpointStatus::Pending,
        claim_context.state_digest.as_deref(),
        now,
    )?;
    Ok(Some(selected.clone()))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn finish_planned_step_tx(
    transaction: &Transaction<'_>,
    task: &TaskRecord,
    result: Option<Value>,
    error: Option<&str>,
    retryable: bool,
    now: DateTime<Utc>,
    base_delay: chrono::Duration,
    max_delay: chrono::Duration,
    claim_context: &PlanClaimContext,
) -> Result<FinishOutcome, QueueError> {
    let reference = task_reference(task)?;
    let plan = load_plan_tx(transaction, &task.tenant_id, &reference)?;
    let step_id = task
        .plan_step_id
        .as_ref()
        .ok_or_else(|| QueueError::InvalidInput("task planejada sem etapa locada".to_owned()))?;
    let (step_state, attempts, max_attempts) = transaction.query_row(
        "SELECT state, attempts, max_attempts FROM plan_steps
         WHERE plan_id = ?1 AND revision = ?2 AND step_id = ?3",
        params![plan.plan_id.0.to_string(), plan.revision, step_id.0],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
            ))
        },
    )?;
    let step_state = parse_step_state(&step_state)?;
    if !matches!(
        step_state,
        PlanStepState::Running | PlanStepState::CancelRequested
    ) {
        return Err(QueueError::InvalidInput(
            "etapa planejada não está em execução".to_owned(),
        ));
    }
    let (task_status, cancel_requested): (String, i64) = transaction.query_row(
        "SELECT status, cancel_requested FROM api_tasks WHERE task_id = ?1 AND tenant_id = ?2",
        params![task.task_id.0.to_string(), task.tenant_id.0],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if cancel_requested != 0 || task_status == TaskStatus::CancelRequested.as_str() {
        mark_plan_unknown_tx(
            transaction,
            &task.tenant_id,
            &plan,
            Some(step_id),
            claim_context.state_digest.as_deref(),
            now,
        )?;
        transaction.execute(
            "UPDATE api_tasks SET status = 'cancelled', cancel_requested = 1,
             lease_until = NULL, lease_token = NULL, plan_step_id = NULL, updated_at = ?1, completed_at = ?1
             WHERE task_id = ?2 AND tenant_id = ?3",
            params![
                now.to_rfc3339(),
                task.task_id.0.to_string(),
                task.tenant_id.0
            ],
        )?;
        return Ok(FinishOutcome::Cancelled);
    }
    let mut success = error.is_none();
    let mut safe_error = error.map(|value| value.chars().take(4_096).collect::<String>());
    if success {
        let states = load_step_states_tx(transaction, &plan)?;
        let mut context =
            PlanVerificationContext::new(PlanVerificationPhase::PostStep).for_step(step_id.clone());
        context.task_state = Some(PlanTaskState::Succeeded);
        context.succeeded_steps = states
            .iter()
            .filter(|(id, state)| **state == PlanStepState::Succeeded || *id == step_id)
            .map(|(id, _)| id.clone())
            .collect();
        context
            .granted_capabilities
            .clone_from(&task.envelope.execution_context.capabilities.0);
        context.circuit_closed = claim_context.circuit_closed;
        context.remaining_budget = claim_context
            .remaining_budget
            .clone()
            .unwrap_or_else(|| task.envelope.budget.clone());
        context.state_digest.clone_from(&claim_context.state_digest);
        context.approvals = load_approvals_tx(transaction, &plan)?;
        context.now = now;
        let report = PlanVerifier::default().verify(&plan, &context);
        if !report.is_executable() {
            success = false;
            safe_error = Some("pós-condição planejada não satisfeita".to_owned());
        }
    }
    if success {
        append_step_transition_tx(
            transaction,
            &plan,
            step_id,
            PlanStepState::Running,
            PlanStepState::Succeeded,
            &format!("finish:succeeded:{}:{}", step_id.0, attempts),
            now,
        )?;
        insert_checkpoint_tx(
            transaction,
            &plan.plan_id,
            plan.revision,
            Some(step_id),
            PlanCheckpointPhase::AfterStep,
            PlanCheckpointStatus::Succeeded,
            claim_context.state_digest.as_deref(),
            now,
        )?;
        if plan.state == PlanState::Compensating {
            return complete_compensation_tx(transaction, &plan, step_id, result, now);
        }
        let remaining = transaction.query_row(
            "SELECT COUNT(*) FROM plan_steps
             WHERE plan_id = ?1 AND revision = ?2 AND state != 'succeeded'",
            params![plan.plan_id.0.to_string(), plan.revision],
            |row| row.get::<_, i64>(0),
        )?;
        if remaining == 0 {
            append_plan_transition_tx(
                transaction,
                &Principal {
                    operator_id: plan.operator_id.clone(),
                    tenant_id: plan.tenant_id.clone(),
                    role: shaka_core::Role::Administrator,
                },
                &plan,
                PlanState::Running,
                PlanState::Succeeded,
                "finish:plan:succeeded",
                now,
            )?;
            let result_json = result
                .map(|value| serde_json::to_string(&value))
                .transpose()?;
            transaction.execute(
                "UPDATE api_tasks SET status = 'succeeded', lease_until = NULL, lease_token = NULL,
                 plan_step_id = NULL, result_json = ?1, last_error = NULL,
                 updated_at = ?2, completed_at = ?2 WHERE task_id = ?3 AND tenant_id = ?4",
                params![
                    result_json,
                    now.to_rfc3339(),
                    task.task_id.0.to_string(),
                    task.tenant_id.0
                ],
            )?;
            return Ok(FinishOutcome::Succeeded);
        }
        let next_attempt_at = now;
        transaction.execute(
            "UPDATE api_tasks SET status = 'queued', lease_until = NULL, lease_token = NULL,
             plan_step_id = NULL, result_json = NULL, last_error = NULL,
             next_attempt_at = ?1, updated_at = ?1 WHERE task_id = ?2 AND tenant_id = ?3",
            params![
                next_attempt_at.to_rfc3339(),
                task.task_id.0.to_string(),
                task.tenant_id.0
            ],
        )?;
        return Ok(FinishOutcome::PlanStepSucceeded { next_attempt_at });
    }
    append_step_transition_tx(
        transaction,
        &plan,
        step_id,
        PlanStepState::Running,
        PlanStepState::Failed,
        &format!("finish:failed:{}:{}", step_id.0, attempts),
        now,
    )?;
    insert_checkpoint_tx(
        transaction,
        &plan.plan_id,
        plan.revision,
        Some(step_id),
        PlanCheckpointPhase::AfterStep,
        PlanCheckpointStatus::Failed,
        claim_context.state_digest.as_deref(),
        now,
    )?;
    if plan.state == PlanState::Compensating {
        mark_plan_unknown_tx(
            transaction,
            &task.tenant_id,
            &plan,
            Some(step_id),
            claim_context.state_digest.as_deref(),
            now,
        )?;
        transaction.execute(
            "UPDATE api_tasks SET status = 'failed', lease_until = NULL, lease_token = NULL,
             plan_step_id = NULL, result_json = NULL, last_error = ?1,
             updated_at = ?2, completed_at = ?2 WHERE task_id = ?3 AND tenant_id = ?4",
            params![
                "falha durante compensação; resolução humana necessária",
                now.to_rfc3339(),
                task.task_id.0.to_string(),
                task.tenant_id.0
            ],
        )?;
        return Ok(FinishOutcome::Failed);
    }
    if retryable && attempts < max_attempts {
        append_step_transition_tx(
            transaction,
            &plan,
            step_id,
            PlanStepState::Failed,
            PlanStepState::Ready,
            &format!("retry:ready:{}:{}", step_id.0, attempts),
            now,
        )?;
        let exponent = attempts.saturating_sub(1).min(10);
        let multiplier = 1_i64 << exponent;
        let delay_ms = base_delay
            .num_milliseconds()
            .saturating_mul(multiplier)
            .min(max_delay.num_milliseconds())
            .max(0);
        let next_attempt_at = now + chrono::Duration::milliseconds(delay_ms);
        transaction.execute(
            "UPDATE api_tasks SET status = 'queued', lease_until = NULL, lease_token = NULL,
             plan_step_id = NULL, result_json = NULL, last_error = ?1,
             next_attempt_at = ?2, updated_at = ?3 WHERE task_id = ?4 AND tenant_id = ?5",
            params![
                safe_error,
                next_attempt_at.to_rfc3339(),
                now.to_rfc3339(),
                task.task_id.0.to_string(),
                task.tenant_id.0
            ],
        )?;
        return Ok(FinishOutcome::Requeued { next_attempt_at });
    }
    if plan.state == PlanState::Running {
        append_plan_transition_tx(
            transaction,
            &Principal {
                operator_id: plan.operator_id.clone(),
                tenant_id: plan.tenant_id.clone(),
                role: shaka_core::Role::Administrator,
            },
            &plan,
            PlanState::Running,
            PlanState::Failed,
            "finish:plan:failed",
            now,
        )?;
    }
    transaction.execute(
        "UPDATE api_tasks SET status = 'failed', lease_until = NULL, lease_token = NULL,
         plan_step_id = NULL, result_json = NULL, last_error = ?1,
         updated_at = ?2, completed_at = ?2 WHERE task_id = ?3 AND tenant_id = ?4",
        params![
            safe_error,
            now.to_rfc3339(),
            task.task_id.0.to_string(),
            task.tenant_id.0
        ],
    )?;
    Ok(FinishOutcome::Failed)
}

fn complete_compensation_tx(
    transaction: &Transaction<'_>,
    plan: &PlanSpec,
    compensation_step_id: &PlanStepId,
    result: Option<Value>,
    now: DateTime<Utc>,
) -> Result<FinishOutcome, QueueError> {
    let source_ids = {
        let mut statement = transaction.prepare(
            "SELECT step_id FROM plan_steps
             WHERE plan_id = ?1 AND revision = ?2 AND compensation_step_id = ?3
               AND state = 'compensating' ORDER BY step_id ASC",
        )?;
        statement
            .query_map(
                params![
                    plan.plan_id.0.to_string(),
                    plan.revision,
                    compensation_step_id.0
                ],
                |row| row.get::<_, String>(0),
            )?
            .map(|row| {
                let value = row?;
                PlanStepId::new(value)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    for source_id in source_ids {
        append_step_transition_tx(
            transaction,
            plan,
            &source_id,
            PlanStepState::Compensating,
            PlanStepState::Compensated,
            &format!("compensation:source:{}", source_id.0),
            now,
        )?;
    }
    insert_checkpoint_tx(
        transaction,
        &plan.plan_id,
        plan.revision,
        Some(compensation_step_id),
        PlanCheckpointPhase::Compensation,
        PlanCheckpointStatus::Succeeded,
        None,
        now,
    )?;
    let remaining_sources = transaction.query_row(
        "SELECT COUNT(*) FROM plan_steps
         WHERE plan_id = ?1 AND revision = ?2 AND state = 'compensating'",
        params![plan.plan_id.0.to_string(), plan.revision],
        |row| row.get::<_, i64>(0),
    )?;
    if remaining_sources == 0 {
        append_plan_transition_tx(
            transaction,
            &Principal {
                operator_id: plan.operator_id.clone(),
                tenant_id: plan.tenant_id.clone(),
                role: Role::Administrator,
            },
            plan,
            PlanState::Compensating,
            PlanState::Compensated,
            "compensation:plan:compensated",
            now,
        )?;
        let result_json = result
            .map(|value| serde_json::to_string(&value))
            .transpose()?;
        transaction.execute(
            "UPDATE api_tasks SET status = 'failed', lease_until = NULL, lease_token = NULL,
             plan_step_id = NULL, result_json = ?1,
             last_error = ?2, updated_at = ?3, completed_at = ?3
             WHERE task_id = ?4 AND tenant_id = ?5",
            params![
                result_json,
                "operação original compensada; não representa sucesso da task",
                now.to_rfc3339(),
                plan.task_id.0.to_string(),
                plan.tenant_id.0
            ],
        )?;
        return Ok(FinishOutcome::Compensated);
    }
    transaction.execute(
        "UPDATE api_tasks SET status = 'queued', lease_until = NULL, lease_token = NULL,
         plan_step_id = NULL, result_json = NULL, last_error = NULL,
         next_attempt_at = ?1, updated_at = ?1
         WHERE task_id = ?2 AND tenant_id = ?3",
        params![
            now.to_rfc3339(),
            plan.task_id.0.to_string(),
            plan.tenant_id.0
        ],
    )?;
    Ok(FinishOutcome::PlanStepSucceeded {
        next_attempt_at: now,
    })
}

pub(crate) fn cancel_planned_task_tx(
    transaction: &Transaction<'_>,
    task: &TaskRecord,
    now: DateTime<Utc>,
) -> Result<(), QueueError> {
    let reference = task_reference(task)?;
    let plan = load_plan_tx(transaction, &task.tenant_id, &reference)?;
    if plan.state == PlanState::Unknown || plan.state == PlanState::Compensating {
        return Err(QueueError::InvalidInput(
            "cancelamento planejado exige resolução da fronteira atual".to_owned(),
        ));
    }
    if task.status == TaskStatus::Queued {
        if matches!(plan.state, PlanState::Approved | PlanState::Running) {
            append_plan_transition_tx(
                transaction,
                &Principal {
                    operator_id: plan.operator_id.clone(),
                    tenant_id: plan.tenant_id.clone(),
                    role: Role::Administrator,
                },
                &plan,
                plan.state,
                PlanState::CancelRequested,
                "cancel:plan:requested",
                now,
            )?;
        }
        let states = load_step_states_tx(transaction, &plan)?;
        for (step_id, state) in states.into_iter().filter(|(_, state)| {
            matches!(
                state,
                PlanStepState::Pending | PlanStepState::Ready | PlanStepState::AwaitingApproval
            )
        }) {
            append_step_transition_tx(
                transaction,
                &plan,
                &step_id,
                state,
                PlanStepState::Cancelled,
                &format!("cancel:step:{}", step_id.0),
                now,
            )?;
        }
        let current_state = transaction.query_row(
            "SELECT state FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
            params![plan.plan_id.0.to_string(), plan.revision, plan.tenant_id.0],
            |row| row.get::<_, String>(0),
        )?;
        if parse_plan_state(&current_state)? == PlanState::CancelRequested {
            append_plan_transition_tx(
                transaction,
                &Principal {
                    operator_id: plan.operator_id.clone(),
                    tenant_id: plan.tenant_id.clone(),
                    role: Role::Administrator,
                },
                &plan,
                PlanState::CancelRequested,
                PlanState::Cancelled,
                "cancel:plan:completed",
                now,
            )?;
        }
    } else if task.status == TaskStatus::Running && plan.state == PlanState::Running {
        append_plan_transition_tx(
            transaction,
            &Principal {
                operator_id: plan.operator_id.clone(),
                tenant_id: plan.tenant_id.clone(),
                role: Role::Administrator,
            },
            &plan,
            PlanState::Running,
            PlanState::CancelRequested,
            "cancel:plan:requested",
            now,
        )?;
    }
    Ok(())
}

pub(crate) fn recover_expired_plan_lease_tx(
    transaction: &Transaction<'_>,
    task: &TaskRecord,
    now: DateTime<Utc>,
) -> Result<(), QueueError> {
    let reference = task_reference(task)?;
    let plan = load_plan_tx(transaction, &task.tenant_id, &reference)?;
    mark_plan_unknown_tx(
        transaction,
        &task.tenant_id,
        &plan,
        task.plan_step_id.as_ref(),
        None,
        now,
    )?;
    let status = if task.cancel_requested {
        TaskStatus::Cancelled.as_str()
    } else {
        TaskStatus::Failed.as_str()
    };
    transaction.execute(
        "UPDATE api_tasks SET status = ?1, lease_until = NULL, lease_token = NULL, plan_step_id = NULL,
         updated_at = ?2, completed_at = ?2, last_error = ?3
         WHERE task_id = ?4 AND tenant_id = ?5",
        params![
            status,
            now.to_rfc3339(),
            "lease planejada expirada; estado requer resolução",
            task.task_id.0.to_string(),
            task.tenant_id.0
        ],
    )?;
    Ok(())
}

pub(crate) fn mark_plan_unknown_tx(
    transaction: &Transaction<'_>,
    tenant_id: &TenantId,
    plan: &PlanSpec,
    step_id: Option<&PlanStepId>,
    state_digest: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), QueueError> {
    let current_plan_state = transaction.query_row(
        "SELECT state FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
        params![plan.plan_id.0.to_string(), plan.revision, tenant_id.0],
        |row| row.get::<_, String>(0),
    )?;
    let current_plan_state = parse_plan_state(&current_plan_state)?;
    if current_plan_state != PlanState::Unknown {
        if current_plan_state
            .validate_transition(PlanState::Unknown)
            .is_ok()
        {
            append_plan_transition_tx(
                transaction,
                &Principal {
                    operator_id: plan.operator_id.clone(),
                    tenant_id: plan.tenant_id.clone(),
                    role: shaka_core::Role::Administrator,
                },
                plan,
                current_plan_state,
                PlanState::Unknown,
                &format!("recovery:plan:unknown:{}", now.to_rfc3339()),
                now,
            )?;
        } else {
            transaction.execute(
                "UPDATE plans SET state = 'unknown', updated_at = ?1
                 WHERE plan_id = ?2 AND revision = ?3 AND tenant_id = ?4",
                params![
                    now.to_rfc3339(),
                    plan.plan_id.0.to_string(),
                    plan.revision,
                    tenant_id.0
                ],
            )?;
        }
    }
    if let Some(step_id) = step_id {
        let current_step_state = transaction
            .query_row(
                "SELECT state FROM plan_steps
                 WHERE plan_id = ?1 AND revision = ?2 AND step_id = ?3",
                params![plan.plan_id.0.to_string(), plan.revision, step_id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|value| parse_step_state(&value))
            .transpose()?;
        if let Some(current_step_state) = current_step_state {
            if current_step_state != PlanStepState::Unknown {
                if current_step_state
                    .validate_transition(PlanStepState::Unknown)
                    .is_ok()
                {
                    append_step_transition_tx(
                        transaction,
                        plan,
                        step_id,
                        current_step_state,
                        PlanStepState::Unknown,
                        &format!("recovery:step:{}:unknown:{}", step_id.0, now.to_rfc3339()),
                        now,
                    )?;
                } else {
                    transaction.execute(
                        "UPDATE plan_steps SET state = 'unknown'
                         WHERE plan_id = ?1 AND revision = ?2 AND step_id = ?3",
                        params![plan.plan_id.0.to_string(), plan.revision, step_id.0],
                    )?;
                }
            }
        }
    }
    insert_checkpoint_tx(
        transaction,
        &plan.plan_id,
        plan.revision,
        step_id,
        PlanCheckpointPhase::Recovery,
        PlanCheckpointStatus::Unknown,
        state_digest,
        now,
    )?;
    Ok(())
}

fn load_plan_tx(
    transaction: &Transaction<'_>,
    tenant_id: &TenantId,
    reference: &PlanTaskReference,
) -> Result<PlanSpec, QueueError> {
    let row = transaction
        .query_row(
            "SELECT plan_json, state, digest FROM plans
             WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
            params![
                reference.plan_id.0.to_string(),
                reference.revision,
                tenant_id.0
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| QueueError::NotFound(format!("plan {}", reference.plan_id.0)))?;
    if row.2 != reference.digest {
        return Err(QueueError::InvalidInput(
            "digest da task não corresponde ao plano persistido".to_owned(),
        ));
    }
    let mut plan: PlanSpec = serde_json::from_str(&row.0)?;
    plan.state = parse_plan_state(&row.1)?;
    if plan.plan_id != reference.plan_id
        || plan.revision != reference.revision
        || plan.tenant_id != *tenant_id
        || plan.digest != reference.digest
    {
        return Err(QueueError::InvalidInput(
            "referência da task não corresponde ao snapshot do plano".to_owned(),
        ));
    }
    plan.validate_structure()?;
    plan.verify_digest()?;
    Ok(plan)
}

fn load_approvals_tx(
    transaction: &Transaction<'_>,
    plan: &PlanSpec,
) -> Result<Vec<PlanApproval>, QueueError> {
    let mut statement = transaction.prepare(
        "SELECT approval_json FROM plan_approvals
         WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3 AND revoked = 0
         ORDER BY created_at ASC",
    )?;
    let rows = statement.query_map(
        params![plan.plan_id.0.to_string(), plan.revision, plan.tenant_id.0],
        |row| row.get::<_, String>(0),
    )?;
    rows.map(|row| {
        let json = row?;
        serde_json::from_str(&json).map_err(QueueError::from)
    })
    .collect()
}

fn load_step_states_tx(
    transaction: &Transaction<'_>,
    plan: &PlanSpec,
) -> Result<BTreeMap<PlanStepId, PlanStepState>, QueueError> {
    let mut statement = transaction.prepare(
        "SELECT step_id, state FROM plan_steps
         WHERE plan_id = ?1 AND revision = ?2 ORDER BY step_id ASC",
    )?;
    let rows = statement.query_map(params![plan.plan_id.0.to_string(), plan.revision], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.map(|row| {
        let (step_id, state) = row?;
        Ok((PlanStepId::new(step_id)?, parse_step_state(&state)?))
    })
    .collect::<Result<BTreeMap<_, _>, QueueError>>()
}

fn append_plan_transition_tx(
    transaction: &Transaction<'_>,
    tenant_id: &Principal,
    plan: &PlanSpec,
    from: PlanState,
    to: PlanState,
    idempotency_key: &str,
    now: DateTime<Utc>,
) -> Result<(), QueueError> {
    let last = last_transition_tx(transaction, &plan.plan_id, plan.revision)?;
    let sequence = last.as_ref().map_or(1, |row| row.0 + 1);
    let previous_hash = last.map(|row| row.1);
    let transition = PlanStoreTransition::new(
        plan.plan_id.clone(),
        plan.revision,
        sequence,
        PlanTransitionEntity::Plan,
        None,
        PlanTransitionState::Plan(from),
        PlanTransitionState::Plan(to),
        idempotency_key,
        previous_hash,
        now,
    )?;
    apply_transition_tx(transaction, &tenant_id.tenant_id, &transition)?;
    insert_transition_tx(transaction, &transition)
}

fn append_step_transition_tx(
    transaction: &Transaction<'_>,
    plan: &PlanSpec,
    step_id: &PlanStepId,
    from: PlanStepState,
    to: PlanStepState,
    idempotency_key: &str,
    now: DateTime<Utc>,
) -> Result<(), QueueError> {
    let last = last_transition_tx(transaction, &plan.plan_id, plan.revision)?;
    let sequence = last.as_ref().map_or(1, |row| row.0 + 1);
    let previous_hash = last.map(|row| row.1);
    let transition = PlanStoreTransition::new(
        plan.plan_id.clone(),
        plan.revision,
        sequence,
        PlanTransitionEntity::Step,
        Some(step_id.clone()),
        PlanTransitionState::Step(from),
        PlanTransitionState::Step(to),
        idempotency_key,
        previous_hash,
        now,
    )?;
    apply_transition_tx(transaction, &plan.tenant_id, &transition)?;
    insert_transition_tx(transaction, &transition)
}

#[allow(clippy::too_many_arguments)]
fn insert_checkpoint_tx(
    transaction: &Transaction<'_>,
    plan_id: &PlanId,
    revision: u32,
    step_id: Option<&PlanStepId>,
    phase: PlanCheckpointPhase,
    status: PlanCheckpointStatus,
    state_digest: Option<&str>,
    created_at: DateTime<Utc>,
) -> Result<(), QueueError> {
    if let Some(digest) = state_digest {
        validate_sha256(digest, "state_digest")?;
    }
    let sequence = next_checkpoint_sequence_tx(transaction, plan_id, revision)?;
    transaction.execute(
        "INSERT INTO plan_checkpoints
         (plan_id, revision, sequence, step_id, phase, status, state_digest, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            plan_id.0.to_string(),
            revision,
            sequence,
            step_id.map(|id| id.0.clone()),
            phase.as_str(),
            status.as_str(),
            state_digest,
            created_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub(crate) fn task_reference(task: &TaskRecord) -> Result<PlanTaskReference, QueueError> {
    match (
        &task.plan_id,
        task.plan_revision,
        task.plan_digest.as_deref(),
    ) {
        (Some(plan_id), Some(revision), Some(digest)) => {
            PlanTaskReference::new(plan_id.clone(), revision, digest)
        }
        _ => Err(QueueError::InvalidInput(
            "task planejada sem referência completa".to_owned(),
        )),
    }
}

fn apply_transition_tx(
    transaction: &Transaction<'_>,
    tenant_id: &TenantId,
    transition: &PlanStoreTransition,
) -> Result<(), QueueError> {
    match (
        transition.entity,
        transition.from_state,
        transition.to_state,
    ) {
        (
            PlanTransitionEntity::Plan,
            PlanTransitionState::Plan(from),
            PlanTransitionState::Plan(to),
        ) => {
            let current = transaction.query_row(
                "SELECT state FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
                params![
                    transition.plan_id.0.to_string(),
                    transition.revision,
                    tenant_id.0
                ],
                |row| row.get::<_, String>(0),
            )?;
            let current = parse_plan_state(&current)?;
            if current != from {
                return Err(QueueError::InvalidInput(
                    "estado atual do plano não corresponde à transição".to_owned(),
                ));
            }
            from.validate_transition(to)?;
            transaction.execute(
                "UPDATE plans SET state = ?1, updated_at = ?2
                 WHERE plan_id = ?3 AND revision = ?4 AND tenant_id = ?5",
                params![
                    plan_state_str(to),
                    Utc::now().to_rfc3339(),
                    transition.plan_id.0.to_string(),
                    transition.revision,
                    tenant_id.0
                ],
            )?;
        }
        (
            PlanTransitionEntity::Step,
            PlanTransitionState::Step(from),
            PlanTransitionState::Step(to),
        ) => {
            let step_id = transition.entity_id.as_ref().ok_or_else(|| {
                QueueError::InvalidInput("transição de etapa sem step_id".to_owned())
            })?;
            let current = transaction.query_row(
                "SELECT state FROM plan_steps
                 WHERE plan_id = ?1 AND revision = ?2 AND step_id = ?3",
                params![
                    transition.plan_id.0.to_string(),
                    transition.revision,
                    step_id.0
                ],
                |row| row.get::<_, String>(0),
            )?;
            let current = parse_step_state(&current)?;
            if current != from {
                return Err(QueueError::InvalidInput(
                    "estado atual da etapa não corresponde à transição".to_owned(),
                ));
            }
            from.validate_transition(to)?;
            transaction.execute(
                "UPDATE plan_steps SET state = ?1
                 WHERE plan_id = ?2 AND revision = ?3 AND step_id = ?4",
                params![
                    step_state_str(to),
                    transition.plan_id.0.to_string(),
                    transition.revision,
                    step_id.0
                ],
            )?;
        }
        _ => {
            return Err(QueueError::InvalidInput(
                "entidade e estados da transição não correspondem".to_owned(),
            ));
        }
    }
    Ok(())
}

fn insert_transition_tx(
    transaction: &Transaction<'_>,
    transition: &PlanStoreTransition,
) -> Result<(), QueueError> {
    transition.verify_hash()?;
    transaction.execute(
        "INSERT INTO plan_transitions
         (transition_id, plan_id, revision, sequence, entity, entity_id, transition_json,
          idempotency_key, previous_hash, event_hash, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            transition.transition_id.to_string(),
            transition.plan_id.0.to_string(),
            transition.revision,
            transition.sequence,
            transition.entity.as_str(),
            transition.entity_id.as_ref().map(|id| id.0.clone()),
            serde_json::to_string(transition)?,
            transition.idempotency_key,
            transition.previous_hash,
            transition.event_hash,
            transition.created_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn apply_transition_state(
    plan_state: &mut PlanState,
    step_states: &mut BTreeMap<PlanStepId, PlanStepState>,
    transition: &PlanStoreTransition,
) -> Result<(), QueueError> {
    match (
        transition.entity,
        transition.from_state,
        transition.to_state,
    ) {
        (
            PlanTransitionEntity::Plan,
            PlanTransitionState::Plan(from),
            PlanTransitionState::Plan(to),
        ) if *plan_state == from => {
            from.validate_transition(to)?;
            *plan_state = to;
        }
        (
            PlanTransitionEntity::Step,
            PlanTransitionState::Step(from),
            PlanTransitionState::Step(to),
        ) => {
            let step_id = transition.entity_id.as_ref().ok_or_else(|| {
                QueueError::InvalidInput("transição de etapa sem step_id".to_owned())
            })?;
            let current = step_states
                .get(step_id)
                .copied()
                .ok_or_else(|| QueueError::NotFound(format!("step {}", step_id.0)))?;
            if current != from {
                return Err(QueueError::InvalidInput(
                    "cadeia de etapas diverge no estado de origem".to_owned(),
                ));
            }
            from.validate_transition(to)?;
            step_states.insert(step_id.clone(), to);
        }
        _ => {
            return Err(QueueError::InvalidInput(
                "transição incompatível com o reducer".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_transition_shape(
    entity: PlanTransitionEntity,
    entity_id: Option<&PlanStepId>,
    from_state: PlanTransitionState,
    to_state: PlanTransitionState,
) -> Result<(), QueueError> {
    match (entity, entity_id, from_state, to_state) {
        (
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(_),
            PlanTransitionState::Plan(_),
        )
        | (
            PlanTransitionEntity::Step,
            Some(_),
            PlanTransitionState::Step(_),
            PlanTransitionState::Step(_),
        ) => Ok(()),
        _ => Err(QueueError::InvalidInput(
            "entidade, identificador e estados incompatíveis".to_owned(),
        )),
    }
}

fn last_transition_tx(
    transaction: &Transaction<'_>,
    plan_id: &PlanId,
    revision: u32,
) -> Result<Option<(u64, String)>, QueueError> {
    Ok(transaction
        .query_row(
            "SELECT sequence, event_hash FROM plan_transitions
             WHERE plan_id = ?1 AND revision = ?2 ORDER BY sequence DESC LIMIT 1",
            params![plan_id.0.to_string(), revision],
            |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?)
}

fn next_checkpoint_sequence_tx(
    transaction: &Transaction<'_>,
    plan_id: &PlanId,
    revision: u32,
) -> Result<u64, QueueError> {
    let last = transaction
        .query_row(
            "SELECT sequence FROM plan_checkpoints
             WHERE plan_id = ?1 AND revision = ?2 ORDER BY sequence DESC LIMIT 1",
            params![plan_id.0.to_string(), revision],
            |row| row.get::<_, u64>(0),
        )
        .optional()?;
    Ok(last.map_or(1, |sequence| sequence + 1))
}

fn check_checkpoint_sequence(checkpoints: &[PlanCheckpoint]) -> bool {
    checkpoints
        .iter()
        .enumerate()
        .all(|(index, checkpoint)| checkpoint.sequence == index as u64 + 1)
}

fn plan_state_str(state: PlanState) -> &'static str {
    match state {
        PlanState::Draft => "draft",
        PlanState::Proposed => "proposed",
        PlanState::Validated => "validated",
        PlanState::AwaitingApproval => "awaiting_approval",
        PlanState::Approved => "approved",
        PlanState::Running => "running",
        PlanState::Paused => "paused",
        PlanState::Succeeded => "succeeded",
        PlanState::Failed => "failed",
        PlanState::CancelRequested => "cancel_requested",
        PlanState::Cancelled => "cancelled",
        PlanState::Compensating => "compensating",
        PlanState::Compensated => "compensated",
        PlanState::Unknown => "unknown",
        PlanState::Rejected => "rejected",
    }
}

fn parse_plan_state(value: &str) -> Result<PlanState, QueueError> {
    match value {
        "draft" => Ok(PlanState::Draft),
        "proposed" => Ok(PlanState::Proposed),
        "validated" => Ok(PlanState::Validated),
        "awaiting_approval" => Ok(PlanState::AwaitingApproval),
        "approved" => Ok(PlanState::Approved),
        "running" => Ok(PlanState::Running),
        "paused" => Ok(PlanState::Paused),
        "succeeded" => Ok(PlanState::Succeeded),
        "failed" => Ok(PlanState::Failed),
        "cancel_requested" => Ok(PlanState::CancelRequested),
        "cancelled" => Ok(PlanState::Cancelled),
        "compensating" => Ok(PlanState::Compensating),
        "compensated" => Ok(PlanState::Compensated),
        "unknown" => Ok(PlanState::Unknown),
        "rejected" => Ok(PlanState::Rejected),
        other => Err(QueueError::InvalidInput(format!(
            "estado de plano desconhecido: {other}"
        ))),
    }
}

fn step_state_str(state: PlanStepState) -> &'static str {
    match state {
        PlanStepState::Pending => "pending",
        PlanStepState::Ready => "ready",
        PlanStepState::Running => "running",
        PlanStepState::Succeeded => "succeeded",
        PlanStepState::Failed => "failed",
        PlanStepState::Blocked => "blocked",
        PlanStepState::AwaitingApproval => "awaiting_approval",
        PlanStepState::CancelRequested => "cancel_requested",
        PlanStepState::Cancelled => "cancelled",
        PlanStepState::Compensating => "compensating",
        PlanStepState::Compensated => "compensated",
        PlanStepState::Unknown => "unknown",
    }
}

fn parse_step_state(value: &str) -> Result<PlanStepState, QueueError> {
    match value {
        "pending" => Ok(PlanStepState::Pending),
        "ready" => Ok(PlanStepState::Ready),
        "running" => Ok(PlanStepState::Running),
        "succeeded" => Ok(PlanStepState::Succeeded),
        "failed" => Ok(PlanStepState::Failed),
        "blocked" => Ok(PlanStepState::Blocked),
        "awaiting_approval" => Ok(PlanStepState::AwaitingApproval),
        "cancel_requested" => Ok(PlanStepState::CancelRequested),
        "cancelled" => Ok(PlanStepState::Cancelled),
        "compensating" => Ok(PlanStepState::Compensating),
        "compensated" => Ok(PlanStepState::Compensated),
        "unknown" => Ok(PlanStepState::Unknown),
        other => Err(QueueError::InvalidInput(format!(
            "estado de etapa desconhecido: {other}"
        ))),
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

fn validate_sha256(value: &str, field: &str) -> Result<(), QueueError> {
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(QueueError::InvalidInput(format!(
            "{field} deve ser SHA-256 hexadecimal"
        )));
    }
    Ok(())
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>, QueueError> {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.with_timezone(&Utc))
        .map_err(|error| QueueError::InvalidInput(format!("timestamp inválido: {error}")))
}

fn safe_error(error: &QueueError) -> String {
    match error {
        QueueError::InvalidInput(detail) | QueueError::NotFound(detail) => {
            detail.chars().take(160).collect()
        }
        _ => "falha na reconstrução do reducer".to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::SubmitOutcome;
    use chrono::Duration;
    use shaka_core::{
        ExecutionBudget, OperatorId, PlanAction, PlanApprovalRequirement, PlanMode, PlanRisk,
        PlanSpecInput, Role, TaskId,
    };
    use std::{
        sync::{Arc, Barrier, mpsc},
        thread,
        time::Duration as StdDuration,
    };

    fn test_plan() -> PlanSpec {
        PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant-plan").unwrap(),
            operator_id: OperatorId::new("operator-plan").unwrap(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget: ExecutionBudget::default(),
            steps: vec![PlanStep {
                step_id: PlanStepId::new("read").unwrap(),
                depends_on: Vec::new(),
                action: PlanAction::ReadOnly {
                    operation: "inspect".to_owned(),
                },
                preconditions: Vec::new(),
                postconditions: Vec::new(),
                risk: PlanRisk::ReadOnly,
                approval: PlanApprovalRequirement::None,
                max_attempts: 1,
                compensation_step_id: None,
            }],
        })
        .unwrap()
    }

    fn dependency_plan() -> PlanSpec {
        let mut plan = test_plan();
        plan.steps.push(PlanStep {
            step_id: PlanStepId::new("write-preview").unwrap(),
            depends_on: vec![PlanStepId::new("read").unwrap()],
            action: PlanAction::ReadOnly {
                operation: "preview".to_owned(),
            },
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            max_attempts: 1,
            compensation_step_id: None,
        });
        plan.digest = plan.calculate_digest().unwrap();
        plan
    }

    fn approval_plan() -> PlanSpec {
        let mut plan = test_plan();
        plan.approval = PlanApprovalRequirement::Reviewer;
        plan.digest = plan.calculate_digest().unwrap();
        plan
    }

    fn admit_plan(store: &QueueStore, plan: &PlanSpec) -> Principal {
        let principal = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        store.save_plan(plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "task planejada de teste",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "stage5-admission",
                &format!("stage5-fingerprint:{}", plan.plan_id.0),
                &envelope,
                1,
                1,
                Some(&reference),
            )
            .unwrap();
        principal
    }

    fn compensation_plan() -> PlanSpec {
        PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant-compensation").unwrap(),
            operator_id: OperatorId::new("operator-compensation").unwrap(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget: ExecutionBudget::default(),
            steps: vec![
                PlanStep {
                    step_id: PlanStepId::new("source").unwrap(),
                    depends_on: Vec::new(),
                    action: PlanAction::ReadOnly {
                        operation: "observe".to_owned(),
                    },
                    preconditions: Vec::new(),
                    postconditions: Vec::new(),
                    risk: PlanRisk::ReadOnly,
                    approval: PlanApprovalRequirement::None,
                    max_attempts: 1,
                    compensation_step_id: Some(PlanStepId::new("undo").unwrap()),
                },
                PlanStep {
                    step_id: PlanStepId::new("undo").unwrap(),
                    depends_on: Vec::new(),
                    action: PlanAction::ReadOnly {
                        operation: "restore-preview".to_owned(),
                    },
                    preconditions: Vec::new(),
                    postconditions: Vec::new(),
                    risk: PlanRisk::ReadOnly,
                    approval: PlanApprovalRequirement::None,
                    max_attempts: 1,
                    compensation_step_id: None,
                },
            ],
        })
        .unwrap()
    }

    #[test]
    fn save_load_is_idempotent_and_tenant_isolated() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        let saved = store.save_plan(&plan).unwrap();
        assert_eq!(saved.plan.digest, plan.digest);
        let loaded = store.load_plan(&plan.plan_id, &plan.tenant_id).unwrap();
        assert_eq!(loaded.plan, plan);
        assert!(matches!(
            store.load_plan(&plan.plan_id, &TenantId::new("other").unwrap()),
            Err(QueueError::NotFound(_))
        ));
        assert_eq!(store.save_plan(&plan).unwrap().plan.digest, plan.digest);
    }

    #[test]
    fn save_existing_plan_returns_persisted_state() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let transition = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            1,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Draft),
            PlanTransitionState::Plan(PlanState::Proposed),
            "save-existing-state",
            None,
            Utc::now(),
        )
        .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &transition)
            .unwrap();
        let persisted = store.save_plan(&plan).unwrap();
        assert_eq!(persisted.plan.state, PlanState::Proposed);
    }

    #[test]
    fn save_plan_waits_for_concurrent_writer_before_read_then_write() {
        let path = std::env::temp_dir().join(format!("shaka-plan-lock-{}.sqlite", Uuid::new_v4()));
        let store = Arc::new(QueueStore::open(&path).unwrap());
        let plan = test_plan();
        let plan_id = plan.plan_id.clone();
        let tenant_id = plan.tenant_id.clone();
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let worker_store = Arc::clone(&store);
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_store.save_plan(&plan)
        });
        started_rx.recv_timeout(StdDuration::from_secs(1)).unwrap();
        thread::sleep(StdDuration::from_millis(100));
        blocker.execute_batch("COMMIT").unwrap();
        assert!(worker.join().unwrap().is_ok());
        assert!(store.load_plan(&plan_id, &tenant_id).is_ok());
        drop(blocker);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn record_plan_transition_waits_for_concurrent_writer_before_read_then_write() {
        let path =
            std::env::temp_dir().join(format!("shaka-transition-lock-{}.sqlite", Uuid::new_v4()));
        let store = Arc::new(QueueStore::open(&path).unwrap());
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let transition = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            1,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Draft),
            PlanTransitionState::Plan(PlanState::Proposed),
            "concurrent-transition",
            None,
            Utc::now(),
        )
        .unwrap();
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let worker_store = Arc::clone(&store);
        let tenant_id = plan.tenant_id.clone();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            worker_store.record_plan_transition(&tenant_id, &transition)
        });
        started_rx.recv_timeout(StdDuration::from_secs(1)).unwrap();
        thread::sleep(StdDuration::from_millis(100));
        blocker.execute_batch("COMMIT").unwrap();
        assert!(worker.join().unwrap().is_ok());
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Proposed
        );
        drop(blocker);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn transition_updates_reducer_and_is_idempotent() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let transition = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            1,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Draft),
            PlanTransitionState::Plan(PlanState::Proposed),
            "plan-propose",
            None,
            Utc::now(),
        )
        .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &transition)
            .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &transition)
            .unwrap();
        let loaded = store.load_plan(&plan.plan_id, &plan.tenant_id).unwrap();
        assert_eq!(loaded.plan.state, PlanState::Proposed);
        let report = store.resume_plan(&plan.tenant_id, &plan.plan_id).unwrap();
        assert_eq!(report.status, PlanResumeStatus::Stable);
    }

    #[test]
    fn idempotent_planned_admission_does_not_append_checkpoint() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        let principal = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        store.save_plan(&plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "idempotent planned task",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        let (first, _) = store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "same-idempotency-key",
                "same-request-fingerprint",
                &envelope,
                0,
                1,
                Some(&reference),
            )
            .unwrap();
        assert_eq!(first, SubmitOutcome::Created);
        let checkpoints_after_first = store
            .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
            .unwrap();
        let (second, _) = store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "same-idempotency-key",
                "same-request-fingerprint",
                &envelope,
                0,
                1,
                Some(&reference),
            )
            .unwrap();
        assert_eq!(second, SubmitOutcome::Existing);
        let checkpoints_after_second = store
            .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
            .unwrap();
        assert_eq!(checkpoints_after_second, checkpoints_after_first);
    }

    #[test]
    fn active_state_recovers_to_unknown_with_checkpoint() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let proposed = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            1,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Draft),
            PlanTransitionState::Plan(PlanState::Proposed),
            "propose",
            None,
            Utc::now(),
        )
        .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &proposed)
            .unwrap();
        let validated = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            2,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Proposed),
            PlanTransitionState::Plan(PlanState::Validated),
            "validate",
            Some(proposed.event_hash.clone()),
            Utc::now(),
        )
        .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &validated)
            .unwrap();
        let approved = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            3,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Validated),
            PlanTransitionState::Plan(PlanState::Approved),
            "approve",
            Some(validated.event_hash.clone()),
            Utc::now(),
        )
        .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &approved)
            .unwrap();
        let running = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            4,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Approved),
            PlanTransitionState::Plan(PlanState::Running),
            "run",
            Some(approved.event_hash.clone()),
            Utc::now(),
        )
        .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &running)
            .unwrap();
        let report = store.resume_plan(&plan.tenant_id, &plan.plan_id).unwrap();
        assert_eq!(report.status, PlanResumeStatus::RecoveredUnknown);
        assert_eq!(report.plan.state, PlanState::Unknown);
        assert_eq!(report.checkpoints_checked, 1);
    }

    #[test]
    fn checkpoint_sequence_and_digest_are_validated() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let checkpoint = PlanCheckpoint {
            plan_id: plan.plan_id.clone(),
            revision: plan.revision,
            sequence: 1,
            step_id: Some(PlanStepId::new("read").unwrap()),
            phase: PlanCheckpointPhase::BeforeStep,
            status: PlanCheckpointStatus::Pending,
            state_digest: Some("a".repeat(64)),
            created_at: Utc::now(),
        };
        store
            .append_plan_checkpoint(&plan.tenant_id, &checkpoint)
            .unwrap();
        store
            .append_plan_checkpoint(&plan.tenant_id, &checkpoint)
            .unwrap();
        let bad = PlanCheckpoint {
            sequence: 3,
            state_digest: Some("bad".to_owned()),
            ..checkpoint
        };
        assert!(matches!(
            store.append_plan_checkpoint(&plan.tenant_id, &bad),
            Err(QueueError::InvalidInput(_))
        ));
    }

    #[test]
    fn malformed_transition_hash_is_rejected() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let mut transition = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            1,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Draft),
            PlanTransitionState::Plan(PlanState::Proposed),
            "bad-hash",
            None,
            Utc::now(),
        )
        .unwrap();
        transition.event_hash = "0".repeat(64);
        assert!(matches!(
            store.record_plan_transition(&plan.tenant_id, &transition),
            Err(QueueError::InvalidInput(_))
        ));
    }

    #[test]
    fn planned_admission_claim_and_completion_are_atomic() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        let principal = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        store.save_plan(&plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "executar etapa planejada",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        let (_, admitted) = store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "planned-1",
                "planned-fingerprint",
                &envelope,
                10,
                1,
                Some(&reference),
            )
            .unwrap();
        assert_eq!(admitted.plan_id, Some(plan.plan_id.clone()));
        assert_eq!(admitted.plan_step_id, None);
        let claimed = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(claimed.plan_step_id, Some(PlanStepId::new("read").unwrap()));
        assert_eq!(
            claimed
                .plan_execution_scope
                .as_ref()
                .map(|scope| &scope.action),
            Some(&PlanAction::ReadOnly {
                operation: "inspect".to_owned(),
            })
        );
        let outcome = store
            .finish_task_with_plan_context(
                &claimed.task_id,
                &principal.tenant_id,
                Some(Value::Null),
                None,
                false,
                Utc::now(),
                Duration::milliseconds(1),
                Duration::seconds(1),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap();
        assert_eq!(outcome, FinishOutcome::Succeeded);
        assert_eq!(
            store
                .get_task(&claimed.task_id, &principal.tenant_id)
                .unwrap()
                .status,
            TaskStatus::Succeeded
        );
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Succeeded
        );
    }

    #[test]
    fn planned_claim_respects_dependencies_and_advances_deterministically() {
        let store = QueueStore::in_memory().unwrap();
        let plan = dependency_plan();
        let principal = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        store.save_plan(&plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "executar DAG planejado",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "planned-dag",
                "planned-dag-fingerprint",
                &envelope,
                1,
                1,
                Some(&reference),
            )
            .unwrap();
        let first = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(first.plan_step_id, Some(PlanStepId::new("read").unwrap()));
        assert_eq!(
            store
                .finish_task_with_plan_context(
                    &first.task_id,
                    &principal.tenant_id,
                    Some(Value::Null),
                    None,
                    false,
                    Utc::now(),
                    Duration::milliseconds(1),
                    Duration::seconds(1),
                    &PlanClaimContext {
                        circuit_closed: true,
                        ..PlanClaimContext::default()
                    },
                )
                .unwrap(),
            FinishOutcome::PlanStepSucceeded {
                next_attempt_at: store
                    .get_task(&first.task_id, &principal.tenant_id)
                    .unwrap()
                    .next_attempt_at,
            }
        );
        let second = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            second.plan_step_id,
            Some(PlanStepId::new("write-preview").unwrap())
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn expired_planned_lease_becomes_unknown_without_retry() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        let principal = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        store.save_plan(&plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "recuperar etapa planejada",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "planned-recovery",
                "planned-recovery-fingerprint",
                &envelope,
                1,
                1,
                Some(&reference),
            )
            .unwrap();
        let claimed = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(1),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        let recovered = store
            .recover_expired_leases(Utc::now() + Duration::seconds(2))
            .unwrap();
        assert_eq!(recovered, 1);
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Unknown
        );
        assert_eq!(
            store
                .get_task(&claimed.task_id, &principal.tenant_id)
                .unwrap()
                .status,
            TaskStatus::Failed
        );
        assert!(
            store
                .claim_next(Utc::now(), Duration::seconds(1))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            store.resolve_plan_unknown(
                &principal,
                &plan.plan_id,
                PlanResolutionDecision::Resume,
                "resume-after-recovery",
                None,
            ),
            Err(QueueError::InvalidInput(_))
        ));
        assert_eq!(
            store
                .resolve_plan_unknown(
                    &principal,
                    &plan.plan_id,
                    PlanResolutionDecision::Resume,
                    "resume-after-recovery",
                    Some(&"a".repeat(64)),
                )
                .unwrap(),
            PlanResolutionOutcome::Resumed
        );
        let resumed = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(1),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(resumed.plan_step_id, Some(PlanStepId::new("read").unwrap()));
    }

    #[test]
    fn reducer_inconsistency_is_quarantined_once_after_restart() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let proposed = PlanStoreTransition::new(
            plan.plan_id.clone(),
            plan.revision,
            1,
            PlanTransitionEntity::Plan,
            None,
            PlanTransitionState::Plan(PlanState::Draft),
            PlanTransitionState::Plan(PlanState::Proposed),
            "propose-before-corruption",
            None,
            Utc::now(),
        )
        .unwrap();
        store
            .record_plan_transition(&plan.tenant_id, &proposed)
            .unwrap();
        store
            .connection
            .lock()
            .execute(
                "UPDATE plans SET state = 'running' WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
                params![plan.plan_id.0.to_string(), plan.revision, plan.tenant_id.0],
            )
            .unwrap();

        let first = store.resume_plan(&plan.tenant_id, &plan.plan_id).unwrap();
        assert_eq!(first.status, PlanResumeStatus::Inconsistent);
        assert_eq!(first.plan.state, PlanState::Unknown);
        let checkpoints_after_first = store
            .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
            .unwrap();
        assert_eq!(checkpoints_after_first.len(), 1);
        assert_eq!(
            checkpoints_after_first[0].phase,
            PlanCheckpointPhase::Recovery
        );
        assert_eq!(
            checkpoints_after_first[0].status,
            PlanCheckpointStatus::Unknown
        );

        let second = store.resume_plan(&plan.tenant_id, &plan.plan_id).unwrap();
        assert_eq!(second.status, PlanResumeStatus::Inconsistent);
        assert_eq!(second.plan.state, PlanState::Unknown);
        assert_eq!(
            store
                .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
                .unwrap(),
            checkpoints_after_first
        );
    }

    #[test]
    fn expired_planned_lease_recovery_is_idempotent_after_restart() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        let principal = admit_plan(&store, &plan);
        let claim_at = Utc::now();
        let claimed = store
            .claim_next_with_plan_context(
                claim_at,
                Duration::seconds(1),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        let before = store
            .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
            .unwrap();
        assert_eq!(
            before.last().map(|checkpoint| checkpoint.phase),
            Some(PlanCheckpointPhase::BeforeStep)
        );
        assert_eq!(
            before.last().map(|checkpoint| checkpoint.status),
            Some(PlanCheckpointStatus::Pending)
        );

        let recovery_at = claim_at + Duration::seconds(2);
        assert_eq!(store.recover_expired_leases(recovery_at).unwrap(), 1);
        let after = store
            .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
            .unwrap();
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(
            after.last().map(|checkpoint| checkpoint.phase),
            Some(PlanCheckpointPhase::Recovery)
        );
        assert_eq!(
            after.last().map(|checkpoint| checkpoint.status),
            Some(PlanCheckpointStatus::Unknown)
        );
        assert_eq!(
            store
                .get_task(&claimed.task_id, &principal.tenant_id)
                .unwrap()
                .status,
            TaskStatus::Failed
        );

        assert_eq!(
            store
                .recover_expired_leases(recovery_at + Duration::seconds(1))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
                .unwrap(),
            after
        );
        let report = store.resume_plan(&plan.tenant_id, &plan.plan_id).unwrap();
        assert_eq!(report.status, PlanResumeStatus::Stable);
        assert_eq!(report.plan.state, PlanState::Unknown);
        assert_eq!(report.checkpoints_checked, after.len() as u64);
    }

    #[test]
    fn planned_finish_replay_after_commit_is_terminal_and_idempotent() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        let principal = admit_plan(&store, &plan);
        let claimed = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        let finished_at = Utc::now();
        assert_eq!(
            store
                .finish_task_with_plan_context(
                    &claimed.task_id,
                    &principal.tenant_id,
                    Some(Value::String("committed".to_owned())),
                    None,
                    false,
                    finished_at,
                    Duration::milliseconds(1),
                    Duration::seconds(1),
                    &PlanClaimContext {
                        circuit_closed: true,
                        ..PlanClaimContext::default()
                    },
                )
                .unwrap(),
            FinishOutcome::Succeeded
        );
        let checkpoints_after_commit = store
            .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
            .unwrap();

        assert_eq!(
            store
                .finish_task_with_plan_context(
                    &claimed.task_id,
                    &principal.tenant_id,
                    Some(Value::String("replayed".to_owned())),
                    None,
                    false,
                    finished_at + Duration::seconds(1),
                    Duration::milliseconds(1),
                    Duration::seconds(1),
                    &PlanClaimContext {
                        circuit_closed: true,
                        ..PlanClaimContext::default()
                    },
                )
                .unwrap(),
            FinishOutcome::Succeeded
        );
        assert_eq!(
            store
                .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
                .unwrap(),
            checkpoints_after_commit
        );
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Succeeded
        );
        assert_eq!(
            store
                .resume_plan(&plan.tenant_id, &plan.plan_id)
                .unwrap()
                .status,
            PlanResumeStatus::Stable
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn approval_requires_separation_and_is_idempotent() {
        let store = QueueStore::in_memory().unwrap();
        let plan = approval_plan();
        store.save_plan(&plan).unwrap();
        let self_approval = PlanApproval {
            approval_id: Uuid::new_v4(),
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.digest.clone(),
            revision: plan.revision,
            tenant_id: plan.tenant_id.clone(),
            approver: plan.operator_id.clone(),
            approver_role: Role::Administrator,
            step_id: None,
            required: PlanApprovalRequirement::Administrator,
            decision: PlanApprovalDecision::Approved,
            expires_at: Utc::now() + Duration::minutes(5),
            revoked: false,
        };
        let proposer = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        assert!(matches!(
            store.approve_plan(&proposer, &self_approval, "self-approval"),
            Err(QueueError::Core(_))
        ));
        let reviewer = Principal {
            operator_id: OperatorId::new("reviewer").unwrap(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Reviewer,
        };
        let approval = PlanApproval {
            approval_id: Uuid::new_v4(),
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.digest.clone(),
            revision: plan.revision,
            tenant_id: plan.tenant_id.clone(),
            approver: reviewer.operator_id.clone(),
            approver_role: Role::Reviewer,
            step_id: None,
            required: PlanApprovalRequirement::Reviewer,
            decision: PlanApprovalDecision::Approved,
            expires_at: Utc::now() + Duration::minutes(5),
            revoked: false,
        };
        assert_eq!(
            store
                .approve_plan(&reviewer, &approval, "approval-1")
                .unwrap(),
            PlanApprovalOutcome::Approved
        );
        assert_eq!(
            store
                .approve_plan(&reviewer, &approval, "approval-1")
                .unwrap(),
            PlanApprovalOutcome::Existing
        );
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Approved
        );
        let expected_approval_id = approval.approval_id;
        store.bootstrap_principal(&proposer).unwrap();
        let session = store.create_session(proposer.clone(), Value::Null).unwrap();
        let mut envelope = TaskEnvelope::new(
            proposer.tenant_id.clone(),
            proposer.operator_id.clone(),
            "admitir plano aprovado",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        let (_, admitted) = store
            .submit_task_governed_with_plan_and_provenance(
                session.session_id,
                &proposer,
                "approval-admission-1",
                "approval-admission-fingerprint",
                &envelope,
                1,
                1,
                Some(&reference),
                Some("plan-admission-request"),
            )
            .unwrap();
        assert_eq!(
            admitted
                .envelope
                .execution_context
                .provenance
                .admission_approval_id,
            Some(expected_approval_id)
        );
        let bad_digest = PlanApproval {
            approval_id: Uuid::new_v4(),
            plan_digest: "0".repeat(64),
            ..approval
        };
        assert!(matches!(
            store.approve_plan(&reviewer, &bad_digest, "approval-bad-digest"),
            Err(QueueError::InvalidInput(_))
        ));
    }

    #[test]
    fn resume_recovery_updates_persisted_step_snapshot() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        let principal = admit_plan(&store, &plan);
        store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Running
        );
        let report = store
            .resume_plan(&principal.tenant_id, &plan.plan_id)
            .unwrap();
        assert_eq!(report.status, PlanResumeStatus::RecoveredUnknown);
        let inspection = store.inspect_plan(&plan.tenant_id, &plan.plan_id).unwrap();
        assert_eq!(inspection.status, PlanInspectionStatus::Stable);
        assert_eq!(
            inspection
                .step_states
                .get(&PlanStepId::new("read").unwrap()),
            Some(&PlanStepState::Unknown)
        );
    }

    #[test]
    fn concurrent_resume_recovers_once_and_keeps_reducer_consistent() {
        let path =
            std::env::temp_dir().join(format!("shaka-resume-race-{}.sqlite", Uuid::new_v4()));
        let store = Arc::new(QueueStore::open(&path).unwrap());
        let plan = compensation_plan();
        let principal = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        store.save_plan(&plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "recovery concorrente",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "resume-race",
                "resume-race-fingerprint",
                &envelope,
                1,
                1,
                Some(&reference),
            )
            .unwrap();
        let claimed = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert!(claimed.plan_step_id.is_some());
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Running
        );

        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let worker_store = Arc::clone(&store);
            let worker_barrier = Arc::clone(&barrier);
            let tenant_id = plan.tenant_id.clone();
            let plan_id = plan.plan_id.clone();
            workers.push(thread::spawn(move || {
                worker_barrier.wait();
                worker_store.resume_plan(&tenant_id, &plan_id)
            }));
        }
        barrier.wait();
        thread::sleep(StdDuration::from_millis(200));
        blocker.execute_batch("COMMIT").unwrap();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }

        let inspection = store.inspect_plan(&plan.tenant_id, &plan.plan_id).unwrap();
        assert_eq!(inspection.status, PlanInspectionStatus::Stable);
        let checkpoints = store
            .list_plan_checkpoints(&plan.tenant_id, &plan.plan_id)
            .unwrap();
        assert_eq!(
            checkpoints
                .iter()
                .filter(|checkpoint| checkpoint.phase == PlanCheckpointPhase::Recovery)
                .count(),
            1
        );
        drop(blocker);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn unknown_resolution_runs_only_declared_compensation() {
        let store = QueueStore::in_memory().unwrap();
        let plan = compensation_plan();
        let principal = Principal {
            operator_id: plan.operator_id.clone(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Administrator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        store.save_plan(&plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "executar operação com rollback declarado",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "compensation-1",
                "compensation-fingerprint",
                &envelope,
                1,
                1,
                Some(&reference),
            )
            .unwrap();
        let original = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            original.plan_step_id,
            Some(PlanStepId::new("source").unwrap())
        );
        assert_eq!(
            store
                .finish_task_with_plan_context(
                    &original.task_id,
                    &principal.tenant_id,
                    Some(Value::Null),
                    None,
                    false,
                    Utc::now(),
                    Duration::milliseconds(1),
                    Duration::seconds(1),
                    &PlanClaimContext {
                        circuit_closed: true,
                        ..PlanClaimContext::default()
                    },
                )
                .unwrap(),
            FinishOutcome::PlanStepSucceeded {
                next_attempt_at: store
                    .get_task(&original.task_id, &principal.tenant_id)
                    .unwrap()
                    .next_attempt_at,
            }
        );
        store
            .mark_plan_unknown(&principal.tenant_id, &plan.plan_id, None, None)
            .unwrap();
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Unknown
        );
        assert_eq!(
            store
                .resolve_plan_unknown(
                    &principal,
                    &plan.plan_id,
                    PlanResolutionDecision::Compensate,
                    "resolve-compensation",
                    None,
                )
                .unwrap(),
            PlanResolutionOutcome::Compensating
        );
        let compensation = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            compensation.plan_step_id,
            Some(PlanStepId::new("undo").unwrap())
        );
        assert_eq!(
            store
                .finish_task_with_plan_context(
                    &compensation.task_id,
                    &principal.tenant_id,
                    Some(Value::Null),
                    None,
                    false,
                    Utc::now(),
                    Duration::milliseconds(1),
                    Duration::seconds(1),
                    &PlanClaimContext {
                        circuit_closed: true,
                        ..PlanClaimContext::default()
                    },
                )
                .unwrap(),
            FinishOutcome::Compensated
        );
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Compensated
        );
        assert_eq!(
            store
                .get_task(&plan.task_id, &plan.tenant_id)
                .unwrap()
                .status,
            TaskStatus::Failed
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn compensation_failure_requires_new_human_resolution() {
        let store = QueueStore::in_memory().unwrap();
        let plan = compensation_plan();
        let principal = admit_plan(&store, &plan);
        let original = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        store
            .finish_task_with_plan_context(
                &original.task_id,
                &principal.tenant_id,
                Some(Value::Null),
                None,
                false,
                Utc::now(),
                Duration::milliseconds(1),
                Duration::seconds(1),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap();
        store
            .mark_plan_unknown(&principal.tenant_id, &plan.plan_id, None, None)
            .unwrap();
        store
            .resolve_plan_unknown(
                &principal,
                &plan.plan_id,
                PlanResolutionDecision::Compensate,
                "comp-failure-resolution-1",
                None,
            )
            .unwrap();
        let compensation = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .finish_task_with_plan_context(
                    &compensation.task_id,
                    &principal.tenant_id,
                    None,
                    Some("rollback inconclusivo"),
                    false,
                    Utc::now(),
                    Duration::milliseconds(1),
                    Duration::seconds(1),
                    &PlanClaimContext {
                        circuit_closed: true,
                        ..PlanClaimContext::default()
                    },
                )
                .unwrap(),
            FinishOutcome::Failed
        );
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Unknown
        );
        assert!(
            store
                .claim_next(Utc::now(), Duration::seconds(1))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .resolve_plan_unknown(
                    &principal,
                    &plan.plan_id,
                    PlanResolutionDecision::Compensate,
                    "comp-failure-resolution-2",
                    None,
                )
                .unwrap(),
            PlanResolutionOutcome::Compensating
        );
        let retry_compensation = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            store
                .finish_task_with_plan_context(
                    &retry_compensation.task_id,
                    &principal.tenant_id,
                    Some(Value::Null),
                    None,
                    false,
                    Utc::now(),
                    Duration::milliseconds(1),
                    Duration::seconds(1),
                    &PlanClaimContext {
                        circuit_closed: true,
                        ..PlanClaimContext::default()
                    },
                )
                .unwrap(),
            FinishOutcome::Compensated
        );
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Compensated
        );
    }

    #[test]
    fn rejected_approval_closes_plan_without_admission() {
        let store = QueueStore::in_memory().unwrap();
        let plan = approval_plan();
        store.save_plan(&plan).unwrap();
        let reviewer = Principal {
            operator_id: OperatorId::new("reviewer").unwrap(),
            tenant_id: plan.tenant_id.clone(),
            role: Role::Reviewer,
        };
        let rejection = PlanApproval {
            approval_id: Uuid::new_v4(),
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.digest.clone(),
            revision: plan.revision,
            tenant_id: plan.tenant_id.clone(),
            approver: reviewer.operator_id.clone(),
            approver_role: Role::Reviewer,
            step_id: None,
            required: PlanApprovalRequirement::Reviewer,
            decision: PlanApprovalDecision::Rejected,
            expires_at: Utc::now() + Duration::minutes(5),
            revoked: false,
        };
        assert_eq!(
            store
                .approve_plan(&reviewer, &rejection, "reject-1")
                .unwrap(),
            PlanApprovalOutcome::Rejected
        );
        assert_eq!(
            store
                .load_plan(&plan.plan_id, &plan.tenant_id)
                .unwrap()
                .plan
                .state,
            PlanState::Rejected
        );
        assert_eq!(
            store
                .approve_plan(&reviewer, &rejection, "reject-1")
                .unwrap(),
            PlanApprovalOutcome::Existing
        );
    }

    #[test]
    fn approval_persistence_preserves_tenant_and_digest_binding() {
        let store = QueueStore::in_memory().unwrap();
        let plan = test_plan();
        store.save_plan(&plan).unwrap();
        let approval = PlanApproval {
            approval_id: Uuid::new_v4(),
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.digest.clone(),
            revision: plan.revision,
            tenant_id: plan.tenant_id.clone(),
            approver: OperatorId::new("reviewer").unwrap(),
            approver_role: Role::Reviewer,
            step_id: None,
            required: PlanApprovalRequirement::None,
            decision: shaka_core::PlanApprovalDecision::Approved,
            expires_at: Utc::now() + Duration::minutes(5),
            revoked: false,
        };
        store
            .save_plan_approval(&plan.tenant_id, &approval)
            .unwrap();
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn planned_finish_uses_task_capabilities_not_worker_global() {
        let store = QueueStore::in_memory().unwrap();
        let principal = Principal {
            operator_id: OperatorId::new("operator-poststep").unwrap(),
            tenant_id: TenantId::new("tenant-poststep").unwrap(),
            role: Role::Operator,
        };
        store.bootstrap_principal(&principal).unwrap();
        let session = store
            .create_session(principal.clone(), Value::Null)
            .unwrap();
        let plan = PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: principal.tenant_id.clone(),
            operator_id: principal.operator_id.clone(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget: ExecutionBudget::default(),
            steps: vec![PlanStep {
                step_id: PlanStepId::new("poststep").unwrap(),
                depends_on: Vec::new(),
                action: PlanAction::ReadOnly {
                    operation: "inspect".to_owned(),
                },
                preconditions: Vec::new(),
                postconditions: vec![shaka_core::PlanCondition::CapabilityGranted {
                    capability: shaka_core::Capability::CodeExecution,
                }],
                risk: PlanRisk::ReadOnly,
                approval: PlanApprovalRequirement::None,
                max_attempts: 1,
                compensation_step_id: None,
            }],
        })
        .unwrap();
        store.save_plan(&plan).unwrap();
        let mut envelope = TaskEnvelope::new(
            principal.tenant_id.clone(),
            principal.operator_id.clone(),
            "pós-condição sem capability",
        )
        .unwrap();
        envelope.task_id = plan.task_id.clone();
        let reference =
            PlanTaskReference::new(plan.plan_id.clone(), plan.revision, plan.digest.clone())
                .unwrap();
        store
            .submit_task_governed_with_plan(
                session.session_id,
                &principal,
                "poststep-capability-admission",
                "poststep-capability-fingerprint",
                &envelope,
                1,
                1,
                Some(&reference),
            )
            .unwrap();
        let claimed = store
            .claim_next_with_plan_context(
                Utc::now(),
                Duration::seconds(30),
                &PlanClaimContext {
                    circuit_closed: true,
                    granted_capabilities: vec![shaka_core::Capability::CodeExecution],
                    ..PlanClaimContext::default()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            claimed.envelope.execution_context.capabilities,
            shaka_core::CapabilitySet::default()
        );
        let outcome = store
            .finish_task_with_plan_context(
                &claimed.task_id,
                &principal.tenant_id,
                Some(Value::Null),
                None,
                false,
                Utc::now(),
                Duration::milliseconds(1),
                Duration::seconds(1),
                &PlanClaimContext {
                    circuit_closed: true,
                    granted_capabilities: vec![shaka_core::Capability::CodeExecution],
                    ..PlanClaimContext::default()
                },
            )
            .unwrap();
        assert_eq!(outcome, FinishOutcome::Failed);
    }
}
