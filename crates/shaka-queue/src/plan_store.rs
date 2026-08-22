use super::{QueueError, QueueStore};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shaka_core::{PlanApproval, PlanId, PlanSpec, PlanState, PlanStepId, PlanStepState, TenantId};
use std::collections::BTreeMap;
use uuid::Uuid;

#[cfg(test)]
use shaka_core::PlanStep;

/// Snapshot persistente de um plano pertencente a um tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedPlan {
    pub plan: PlanSpec,
    pub updated_at: DateTime<Utc>,
}

/// Fase de um checkpoint persistido.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanCheckpointPhase {
    Preflight,
    BeforeStep,
    AfterStep,
    Recovery,
}

impl PlanCheckpointPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::BeforeStep => "before_step",
            Self::AfterStep => "after_step",
            Self::Recovery => "recovery",
        }
    }

    fn parse(value: &str) -> Result<Self, QueueError> {
        match value {
            "preflight" => Ok(Self::Preflight),
            "before_step" => Ok(Self::BeforeStep),
            "after_step" => Ok(Self::AfterStep),
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
    Pending,
    Succeeded,
    Failed,
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
    Plan,
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
    Plan(PlanState),
    Step(PlanStepState),
}

/// Transição append-only com encadeamento SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStoreTransition {
    pub transition_id: Uuid,
    pub plan_id: PlanId,
    pub revision: u32,
    pub sequence: u64,
    pub entity: PlanTransitionEntity,
    pub entity_id: Option<PlanStepId>,
    pub from_state: PlanTransitionState,
    pub to_state: PlanTransitionState,
    pub idempotency_key: String,
    pub previous_hash: Option<String>,
    pub event_hash: String,
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
    pub plan_id: PlanId,
    pub revision: u32,
    pub sequence: u64,
    pub step_id: Option<PlanStepId>,
    pub phase: PlanCheckpointPhase,
    pub status: PlanCheckpointStatus,
    pub state_digest: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Resultado da reconstrução do reducer após reinício.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanResumeStatus {
    Stable,
    RecoveredUnknown,
    Inconsistent,
}

/// Relatório bounded da retomada de um plano.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanResumeReport {
    pub plan: PlanSpec,
    pub step_states: BTreeMap<PlanStepId, PlanStepState>,
    pub status: PlanResumeStatus,
    pub checkpoints_checked: u64,
    pub transitions_checked: u64,
    pub inconsistency: Option<String>,
}

type PlanReducerRows = (
    Vec<PlanStoreTransition>,
    Vec<PlanCheckpoint>,
    PlanState,
    BTreeMap<PlanStepId, PlanStepState>,
);

impl QueueStore {
    /// Persiste uma revisão de plano e suas etapas de forma append-only e idempotente.
    pub fn save_plan(&self, plan: &PlanSpec) -> Result<PersistedPlan, QueueError> {
        plan.validate_structure()?;
        plan.verify_digest()?;
        let plan_json = serde_json::to_string(plan)?;
        let mode = serde_json::to_string(&plan.mode)?;
        let risk = serde_json::to_string(&plan.risk)?;
        let now = Utc::now();
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT digest, updated_at FROM plans WHERE plan_id = ?1 AND revision = ?2 AND tenant_id = ?3",
                params![plan.plan_id.0.to_string(), plan.revision, plan.tenant_id.0],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((digest, updated_at)) = existing {
            if digest != plan.digest {
                return Err(QueueError::InvalidInput(
                    "revisão de plano existente é append-only".to_owned(),
                ));
            }
            transaction.commit()?;
            return Ok(PersistedPlan {
                plan: plan.clone(),
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
        let transaction = connection.transaction()?;
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
        let transaction_db = connection.transaction()?;
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

    /// Persiste uma aprovação somente depois de revalidar seu vínculo ao plano.
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
             (approval_id, plan_id, revision, tenant_id, step_id, approval_json, revoked, expires_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        )?;
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
        let transaction = connection.transaction()?;
        let last = last_transition_tx(&transaction, plan_id, revision)?;
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
            insert_transition_tx(&transaction, &transition)?;
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
            insert_transition_tx(&transaction, &transition)?;
            previous_hash = Some(transition.event_hash);
            sequence += 1;
            states.insert(step_id.clone(), PlanStepState::Unknown);
        }
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

    fn force_unknown(
        &self,
        tenant_id: &TenantId,
        plan_id: &PlanId,
        revision: u32,
    ) -> Result<(PlanSpec, BTreeMap<PlanStepId, PlanStepState>), QueueError> {
        let mut connection = self.connection.lock();
        let transaction = connection.transaction()?;
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
        transaction.commit()?;
        drop(connection);
        let persisted = self.load_plan(plan_id, tenant_id)?;
        let (_, _, _, states) = self.load_reducer_rows(plan_id, revision, tenant_id)?;
        Ok((persisted.plan, states))
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
    use chrono::Duration;
    use shaka_core::{
        ExecutionBudget, OperatorId, PlanAction, PlanApprovalRequirement, PlanMode, PlanRisk,
        PlanSpecInput, Role, TaskId,
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
}
