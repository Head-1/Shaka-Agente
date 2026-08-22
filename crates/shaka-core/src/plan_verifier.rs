use super::{
    Capability, CoreError, ExecutionBudget, PlanApproval, PlanApprovalRequirement, PlanCondition,
    PlanId, PlanSpec, PlanState, PlanStep, PlanStepId, PlanTaskState,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Fase determinística em que um plano está sendo verificado.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanVerificationPhase {
    /// Valida estrutura, referências e limites sem executar uma etapa.
    #[default]
    Preflight,
    /// Valida pré-condições e aprovação da etapa pronta para execução.
    StepReady,
    /// Valida pós-condições depois de uma etapa concluída.
    PostStep,
}

/// Fatos observáveis fornecidos pelo host ao verificador.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanVerificationContext {
    /// Fase da verificação.
    pub phase: PlanVerificationPhase,
    /// Etapa alvo quando a fase exige uma etapa específica.
    pub target_step: Option<PlanStepId>,
    /// Estado atual da tarefa, quando conhecido.
    pub task_state: Option<PlanTaskState>,
    /// Etapas cuja conclusão foi comprovada pelo host.
    pub succeeded_steps: BTreeSet<PlanStepId>,
    /// Capabilities concedidas ao principal efetivo.
    pub granted_capabilities: Vec<Capability>,
    /// Estado atual do circuit breaker relevante.
    pub circuit_closed: bool,
    /// Orçamento ainda disponível para a execução.
    pub remaining_budget: ExecutionBudget,
    /// Digests de artefatos conhecidos pelo host.
    pub artifacts: BTreeMap<String, String>,
    /// Referências de idempotência já utilizadas.
    pub used_idempotency_keys: BTreeSet<String>,
    /// Digest do estado externo observado, quando disponível.
    pub state_digest: Option<String>,
    /// Aprovações disponíveis para a revisão do plano.
    pub approvals: Vec<PlanApproval>,
    /// Instante usado para avaliar expiração de aprovações.
    pub now: DateTime<Utc>,
}

impl PlanVerificationContext {
    /// Cria um contexto vazio e conservador para uma fase específica.
    #[must_use]
    pub fn new(phase: PlanVerificationPhase) -> Self {
        Self {
            phase,
            target_step: None,
            task_state: None,
            succeeded_steps: BTreeSet::new(),
            granted_capabilities: Vec::new(),
            circuit_closed: false,
            remaining_budget: ExecutionBudget::default(),
            artifacts: BTreeMap::new(),
            used_idempotency_keys: BTreeSet::new(),
            state_digest: None,
            approvals: Vec::new(),
            now: Utc::now(),
        }
    }

    /// Define a etapa que será verificada sem modificar os demais fatos.
    #[must_use]
    pub fn for_step(mut self, step_id: PlanStepId) -> Self {
        self.target_step = Some(step_id);
        self
    }
}

/// Resultado de uma verificação de plano.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanVerificationStatus {
    /// O plano ou a etapa satisfaz todos os fatos fornecidos.
    Valid,
    /// A estrutura é válida, mas falta aprovação humana exigida.
    RequiresApproval,
    /// Existe uma violação estrutural, de condição, estado ou governança.
    Invalid,
}

/// Classe estável de uma violação, sem mensagens livres de payload.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanViolationCode {
    StructureInvalid,
    DigestInvalid,
    PlanStateInvalid,
    LimitExceeded,
    MissingReference,
    DependencyNotSatisfied,
    ConditionContextMissing,
    ConditionUnsatisfied,
    ApprovalRequired,
    ApprovalInvalid,
}

/// Violação redacted produzida pelo verificador.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanViolation {
    pub code: PlanViolationCode,
    pub step_id: Option<PlanStepId>,
    pub condition_index: Option<usize>,
    pub detail: String,
}

/// Relatório bounded e serializável da verificação.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanVerificationReport {
    pub plan_id: PlanId,
    pub plan_digest: String,
    pub phase: PlanVerificationPhase,
    pub status: PlanVerificationStatus,
    pub violations: Vec<PlanViolation>,
}

impl PlanVerificationReport {
    /// Retorna verdadeiro somente quando não há nenhuma barreira à execução.
    #[must_use]
    pub const fn is_executable(&self) -> bool {
        matches!(self.status, PlanVerificationStatus::Valid)
    }

    /// Retorna verdadeiro quando a estrutura e as regras verificadas são válidas.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        !matches!(self.status, PlanVerificationStatus::Invalid)
    }
}

/// Verificador determinístico com limites para impedir planos patológicos.
#[derive(Debug, Clone, Copy)]
pub struct PlanVerifier {
    pub max_steps: usize,
    pub max_dependencies_per_step: usize,
    pub max_conditions_per_step: usize,
    pub max_violations: usize,
}

impl Default for PlanVerifier {
    fn default() -> Self {
        Self {
            max_steps: 128,
            max_dependencies_per_step: 32,
            max_conditions_per_step: 64,
            max_violations: 64,
        }
    }
}

impl PlanVerifier {
    /// Verifica estrutura, digest, condições contextualizadas e aprovação sem efeitos.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn verify(
        &self,
        plan: &PlanSpec,
        context: &PlanVerificationContext,
    ) -> PlanVerificationReport {
        let mut report = PlanVerificationReport {
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.digest.clone(),
            phase: context.phase,
            status: PlanVerificationStatus::Valid,
            violations: Vec::new(),
        };

        if let Err(error) = plan.validate_structure() {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::StructureInvalid,
                    step_id: None,
                    condition_index: None,
                    detail: safe_error_detail(&error),
                },
            );
            report.status = PlanVerificationStatus::Invalid;
            return report;
        }
        if let Err(error) = plan.verify_digest() {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::DigestInvalid,
                    step_id: None,
                    condition_index: None,
                    detail: safe_error_detail(&error),
                },
            );
        }
        if plan.steps.len() > self.max_steps {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::LimitExceeded,
                    step_id: None,
                    condition_index: None,
                    detail: "quantidade de etapas excede o limite do verificador".to_owned(),
                },
            );
        }
        for step in &plan.steps {
            self.verify_step_shape(plan, step, &mut report);
        }
        if matches!(
            plan.state,
            PlanState::Succeeded
                | PlanState::Failed
                | PlanState::Cancelled
                | PlanState::Compensated
                | PlanState::Rejected
        ) {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::PlanStateInvalid,
                    step_id: None,
                    condition_index: None,
                    detail: "plano está em estado terminal".to_owned(),
                },
            );
        }

        if report.has_blocking_violation() {
            report.status = PlanVerificationStatus::Invalid;
            return report;
        }

        match context.phase {
            PlanVerificationPhase::Preflight => {
                self.verify_preflight_approval(plan, context, &mut report);
            }
            PlanVerificationPhase::StepReady => {
                let Some(step_id) = context.target_step.as_ref() else {
                    report.push(
                        self.max_violations,
                        PlanViolation {
                            code: PlanViolationCode::MissingReference,
                            step_id: None,
                            condition_index: None,
                            detail: "step-ready exige uma etapa alvo".to_owned(),
                        },
                    );
                    report.status = PlanVerificationStatus::Invalid;
                    return report;
                };
                let Some(step) = plan.steps.iter().find(|step| &step.step_id == step_id) else {
                    report.push(
                        self.max_violations,
                        PlanViolation {
                            code: PlanViolationCode::MissingReference,
                            step_id: Some(step_id.clone()),
                            condition_index: None,
                            detail: "etapa alvo não existe no plano".to_owned(),
                        },
                    );
                    report.status = PlanVerificationStatus::Invalid;
                    return report;
                };
                self.verify_dependencies(step, context, &mut report);
                self.verify_conditions(plan, step, &step.preconditions, context, &mut report);
                self.verify_step_approval(plan, step, context, &mut report);
            }
            PlanVerificationPhase::PostStep => {
                let Some(step_id) = context.target_step.as_ref() else {
                    report.push(
                        self.max_violations,
                        PlanViolation {
                            code: PlanViolationCode::MissingReference,
                            step_id: None,
                            condition_index: None,
                            detail: "post-step exige uma etapa alvo".to_owned(),
                        },
                    );
                    report.status = PlanVerificationStatus::Invalid;
                    return report;
                };
                let Some(step) = plan.steps.iter().find(|step| &step.step_id == step_id) else {
                    report.push(
                        self.max_violations,
                        PlanViolation {
                            code: PlanViolationCode::MissingReference,
                            step_id: Some(step_id.clone()),
                            condition_index: None,
                            detail: "etapa alvo não existe no plano".to_owned(),
                        },
                    );
                    report.status = PlanVerificationStatus::Invalid;
                    return report;
                };
                self.verify_conditions(plan, step, &step.postconditions, context, &mut report);
            }
        }

        if report.has_blocking_violation() {
            report.status = PlanVerificationStatus::Invalid;
        } else if report
            .violations
            .iter()
            .any(|violation| matches!(violation.code, PlanViolationCode::ApprovalRequired))
        {
            report.status = PlanVerificationStatus::RequiresApproval;
        }
        report
    }

    fn verify_step_shape(
        &self,
        plan: &PlanSpec,
        step: &PlanStep,
        report: &mut PlanVerificationReport,
    ) {
        if step.depends_on.len() > self.max_dependencies_per_step {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::LimitExceeded,
                    step_id: Some(step.step_id.clone()),
                    condition_index: None,
                    detail: "quantidade de dependências excede o limite".to_owned(),
                },
            );
        }
        if step.preconditions.len() > self.max_conditions_per_step
            || step.postconditions.len() > self.max_conditions_per_step
        {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::LimitExceeded,
                    step_id: Some(step.step_id.clone()),
                    condition_index: None,
                    detail: "quantidade de condições excede o limite".to_owned(),
                },
            );
        }
        for (index, condition) in step
            .preconditions
            .iter()
            .chain(&step.postconditions)
            .enumerate()
        {
            let referenced_step = match condition {
                PlanCondition::StepSucceeded { step_id }
                | PlanCondition::ApprovalExists {
                    step_id: Some(step_id),
                } => Some(step_id),
                _ => None,
            };
            if let Some(referenced_step) = referenced_step
                && !plan
                    .steps
                    .iter()
                    .any(|candidate| &candidate.step_id == referenced_step)
            {
                report.push(
                    self.max_violations,
                    PlanViolation {
                        code: PlanViolationCode::MissingReference,
                        step_id: Some(step.step_id.clone()),
                        condition_index: Some(index),
                        detail: "condição referencia etapa inexistente".to_owned(),
                    },
                );
            }
        }
    }

    fn verify_preflight_approval(
        &self,
        plan: &PlanSpec,
        context: &PlanVerificationContext,
        report: &mut PlanVerificationReport,
    ) {
        if plan.required_approval() == PlanApprovalRequirement::None {
            return;
        }
        if !Self::has_valid_approval(plan, None, context) {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::ApprovalRequired,
                    step_id: None,
                    condition_index: None,
                    detail: "aprovação humana global ainda não foi comprovada".to_owned(),
                },
            );
        }
    }

    fn verify_dependencies(
        &self,
        step: &PlanStep,
        context: &PlanVerificationContext,
        report: &mut PlanVerificationReport,
    ) {
        for dependency in &step.depends_on {
            if !context.succeeded_steps.contains(dependency) {
                report.push(
                    self.max_violations,
                    PlanViolation {
                        code: PlanViolationCode::DependencyNotSatisfied,
                        step_id: Some(step.step_id.clone()),
                        condition_index: None,
                        detail: "dependência ainda não foi comprovada como concluída".to_owned(),
                    },
                );
            }
        }
    }

    fn verify_conditions(
        &self,
        plan: &PlanSpec,
        step: &PlanStep,
        conditions: &[PlanCondition],
        context: &PlanVerificationContext,
        report: &mut PlanVerificationReport,
    ) {
        for (index, condition) in conditions.iter().enumerate() {
            match Self::evaluate_condition(plan, condition, context) {
                ConditionResult::Satisfied => {}
                ConditionResult::ApprovalRequired => report.push(
                    self.max_violations,
                    PlanViolation {
                        code: PlanViolationCode::ApprovalRequired,
                        step_id: Some(step.step_id.clone()),
                        condition_index: Some(index),
                        detail: "condição de aprovação ainda não foi satisfeita".to_owned(),
                    },
                ),
                ConditionResult::MissingContext => report.push(
                    self.max_violations,
                    PlanViolation {
                        code: PlanViolationCode::ConditionContextMissing,
                        step_id: Some(step.step_id.clone()),
                        condition_index: Some(index),
                        detail: "fato necessário não foi fornecido pelo host".to_owned(),
                    },
                ),
                ConditionResult::Unsatisfied => report.push(
                    self.max_violations,
                    PlanViolation {
                        code: PlanViolationCode::ConditionUnsatisfied,
                        step_id: Some(step.step_id.clone()),
                        condition_index: Some(index),
                        detail: "condição não satisfeita".to_owned(),
                    },
                ),
            }
        }
    }

    fn evaluate_condition(
        plan: &PlanSpec,
        condition: &PlanCondition,
        context: &PlanVerificationContext,
    ) -> ConditionResult {
        match condition {
            PlanCondition::TaskStateIs { state } => {
                context
                    .task_state
                    .map_or(ConditionResult::MissingContext, |actual| {
                        if actual == *state {
                            ConditionResult::Satisfied
                        } else {
                            ConditionResult::Unsatisfied
                        }
                    })
            }
            PlanCondition::StepSucceeded { step_id } => {
                if context.succeeded_steps.contains(step_id) {
                    ConditionResult::Satisfied
                } else {
                    ConditionResult::Unsatisfied
                }
            }
            PlanCondition::ApprovalExists { step_id } => {
                if Self::has_valid_approval(plan, step_id.as_ref(), context) {
                    ConditionResult::Satisfied
                } else {
                    ConditionResult::ApprovalRequired
                }
            }
            PlanCondition::CapabilityGranted { capability } => {
                if context.granted_capabilities.contains(capability) {
                    ConditionResult::Satisfied
                } else {
                    ConditionResult::Unsatisfied
                }
            }
            PlanCondition::CircuitClosed => {
                if context.circuit_closed {
                    ConditionResult::Satisfied
                } else {
                    ConditionResult::Unsatisfied
                }
            }
            PlanCondition::BudgetRemainingAtLeast { budget } => {
                if budget_at_least(&context.remaining_budget, budget) {
                    ConditionResult::Satisfied
                } else {
                    ConditionResult::Unsatisfied
                }
            }
            PlanCondition::ArtifactDigestMatches { artifact, sha256 } => context
                .artifacts
                .get(artifact)
                .map_or(ConditionResult::MissingContext, |actual| {
                    if actual == sha256 {
                        ConditionResult::Satisfied
                    } else {
                        ConditionResult::Unsatisfied
                    }
                }),
            PlanCondition::IdempotencyKeyUnused { key_ref } => {
                if context.used_idempotency_keys.contains(key_ref) {
                    ConditionResult::Unsatisfied
                } else {
                    ConditionResult::Satisfied
                }
            }
            PlanCondition::StateDigestMatches { sha256 } => {
                context
                    .state_digest
                    .as_ref()
                    .map_or(ConditionResult::MissingContext, |actual| {
                        if actual == sha256 {
                            ConditionResult::Satisfied
                        } else {
                            ConditionResult::Unsatisfied
                        }
                    })
            }
        }
    }

    fn verify_step_approval(
        &self,
        plan: &PlanSpec,
        step: &PlanStep,
        context: &PlanVerificationContext,
        report: &mut PlanVerificationReport,
    ) {
        let required = step.approval.max(step.risk.minimum_approval());
        if required == PlanApprovalRequirement::None {
            return;
        }
        if !Self::has_valid_approval(plan, Some(&step.step_id), context)
            && !Self::has_valid_approval(plan, None, context)
        {
            report.push(
                self.max_violations,
                PlanViolation {
                    code: PlanViolationCode::ApprovalRequired,
                    step_id: Some(step.step_id.clone()),
                    condition_index: None,
                    detail: "aprovação humana da etapa ainda não foi comprovada".to_owned(),
                },
            );
        }
    }

    fn has_valid_approval(
        plan: &PlanSpec,
        step_id: Option<&PlanStepId>,
        context: &PlanVerificationContext,
    ) -> bool {
        context
            .approvals
            .iter()
            .any(|approval| approval.validate_for(plan, step_id, context.now).is_ok())
    }
}

impl PlanVerificationReport {
    fn push(&mut self, max_violations: usize, violation: PlanViolation) {
        if self.violations.len() < max_violations {
            self.violations.push(violation);
        }
    }

    fn has_blocking_violation(&self) -> bool {
        self.violations
            .iter()
            .any(|violation| !matches!(violation.code, PlanViolationCode::ApprovalRequired))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionResult {
    Satisfied,
    ApprovalRequired,
    MissingContext,
    Unsatisfied,
}

fn budget_at_least(available: &ExecutionBudget, required: &ExecutionBudget) -> bool {
    available.max_steps >= required.max_steps
        && available.max_tool_calls >= required.max_tool_calls
        && available.max_elapsed_ms >= required.max_elapsed_ms
        && available.max_cost_microunits >= required.max_cost_microunits
}

fn safe_error_detail(error: &CoreError) -> String {
    match error {
        CoreError::PlanInvalid(detail) | CoreError::PlanApprovalInvalid(detail) => {
            detail.chars().take(160).collect()
        }
        _ => "falha de validação do contrato do plano".to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        PlanAction, PlanApprovalDecision, PlanMode, PlanRisk, PlanSpecInput, Role, TaskId, TenantId,
    };

    fn step(
        id: &str,
        depends_on: &[&str],
        action: PlanAction,
        risk: PlanRisk,
        approval: PlanApprovalRequirement,
        preconditions: Vec<PlanCondition>,
        postconditions: Vec<PlanCondition>,
    ) -> PlanStep {
        PlanStep {
            step_id: PlanStepId::new(id).unwrap(),
            depends_on: depends_on
                .iter()
                .map(|value| PlanStepId::new(*value).unwrap())
                .collect(),
            action,
            preconditions,
            postconditions,
            risk,
            approval,
            max_attempts: 1,
            compensation_step_id: None,
        }
    }

    fn read_only_plan(steps: Vec<PlanStep>) -> PlanSpec {
        PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: crate::OperatorId::new("operator").unwrap(),
            mode: PlanMode::DryRun,
            risk: PlanRisk::ReadOnly,
            approval: PlanApprovalRequirement::None,
            budget: ExecutionBudget::default(),
            steps,
        })
        .unwrap()
    }

    fn mutation_plan(target_step: PlanStep) -> PlanSpec {
        let mut steps = Vec::new();
        if target_step
            .depends_on
            .iter()
            .any(|dependency| dependency.0 == "read")
        {
            steps.push(step(
                "read",
                &[],
                PlanAction::ReadOnly {
                    operation: "inspect".to_owned(),
                },
                PlanRisk::ReadOnly,
                PlanApprovalRequirement::None,
                Vec::new(),
                Vec::new(),
            ));
        }
        steps.push(target_step);
        PlanSpec::new(PlanSpecInput {
            plan_id: PlanId::new(),
            task_id: TaskId::new(),
            tenant_id: TenantId::new("tenant").unwrap(),
            operator_id: crate::OperatorId::new("operator").unwrap(),
            mode: PlanMode::Live,
            risk: PlanRisk::Mutation,
            approval: PlanApprovalRequirement::Reviewer,
            budget: ExecutionBudget::default(),
            steps,
        })
        .unwrap()
    }

    #[test]
    fn preflight_accepts_structurally_valid_read_only_plan() {
        let plan = read_only_plan(vec![step(
            "read",
            &[],
            PlanAction::ReadOnly {
                operation: "inspect".to_owned(),
            },
            PlanRisk::ReadOnly,
            PlanApprovalRequirement::None,
            Vec::new(),
            Vec::new(),
        )]);
        let report = PlanVerifier::default().verify(
            &plan,
            &PlanVerificationContext::new(PlanVerificationPhase::Preflight),
        );
        assert_eq!(report.status, PlanVerificationStatus::Valid);
        assert!(report.is_executable());
    }

    #[test]
    fn verifier_rejects_tampered_digest() {
        let mut plan = read_only_plan(vec![step(
            "read",
            &[],
            PlanAction::ReadOnly {
                operation: "inspect".to_owned(),
            },
            PlanRisk::ReadOnly,
            PlanApprovalRequirement::None,
            Vec::new(),
            Vec::new(),
        )]);
        plan.digest = "0".repeat(64);
        let report = PlanVerifier::default().verify(
            &plan,
            &PlanVerificationContext::new(PlanVerificationPhase::Preflight),
        );
        assert_eq!(report.status, PlanVerificationStatus::Invalid);
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.code == PlanViolationCode::DigestInvalid)
        );
    }

    #[test]
    fn step_ready_requires_dependencies_conditions_and_approval() {
        let plan = mutation_plan(step(
            "mutate",
            &["read"],
            PlanAction::Mutation {
                operation: "update".to_owned(),
            },
            PlanRisk::Mutation,
            PlanApprovalRequirement::Reviewer,
            vec![
                PlanCondition::StepSucceeded {
                    step_id: PlanStepId::new("read").unwrap(),
                },
                PlanCondition::CapabilityGranted {
                    capability: Capability::MemoryWrite,
                },
                PlanCondition::CircuitClosed,
            ],
            Vec::new(),
        ));
        let mut context = PlanVerificationContext::new(PlanVerificationPhase::StepReady)
            .for_step(PlanStepId::new("mutate").unwrap());
        let report = PlanVerifier::default().verify(&plan, &context);
        assert_eq!(report.status, PlanVerificationStatus::Invalid);
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.code == PlanViolationCode::DependencyNotSatisfied)
        );

        context
            .succeeded_steps
            .insert(PlanStepId::new("read").unwrap());
        context.granted_capabilities.push(Capability::MemoryWrite);
        context.circuit_closed = true;
        let report = PlanVerifier::default().verify(&plan, &context);
        assert_eq!(report.status, PlanVerificationStatus::RequiresApproval);

        context.approvals.push(PlanApproval {
            approval_id: uuid::Uuid::new_v4(),
            plan_id: plan.plan_id.clone(),
            plan_digest: plan.digest.clone(),
            revision: plan.revision,
            tenant_id: plan.tenant_id.clone(),
            approver: crate::OperatorId::new("reviewer").unwrap(),
            approver_role: Role::Reviewer,
            step_id: None,
            required: PlanApprovalRequirement::Reviewer,
            decision: PlanApprovalDecision::Approved,
            expires_at: Utc::now() + chrono::Duration::minutes(5),
            revoked: false,
        });
        let report = PlanVerifier::default().verify(&plan, &context);
        assert_eq!(report.status, PlanVerificationStatus::Valid);
        assert!(report.is_executable());
    }

    #[test]
    fn post_step_missing_context_is_not_assumed_successful() {
        let plan = read_only_plan(vec![step(
            "read",
            &[],
            PlanAction::ReadOnly {
                operation: "inspect".to_owned(),
            },
            PlanRisk::ReadOnly,
            PlanApprovalRequirement::None,
            Vec::new(),
            vec![PlanCondition::StateDigestMatches {
                sha256: "a".repeat(64),
            }],
        )]);
        let report = PlanVerifier::default().verify(
            &plan,
            &PlanVerificationContext::new(PlanVerificationPhase::PostStep)
                .for_step(PlanStepId::new("read").unwrap()),
        );
        assert_eq!(report.status, PlanVerificationStatus::Invalid);
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.code == PlanViolationCode::ConditionContextMissing)
        );
    }

    #[test]
    fn verifier_rejects_used_idempotency_key_and_unknown_target() {
        let plan = read_only_plan(vec![step(
            "read",
            &[],
            PlanAction::ReadOnly {
                operation: "inspect".to_owned(),
            },
            PlanRisk::ReadOnly,
            PlanApprovalRequirement::None,
            vec![PlanCondition::IdempotencyKeyUnused {
                key_ref: "task.read".to_owned(),
            }],
            Vec::new(),
        )]);
        let mut context = PlanVerificationContext::new(PlanVerificationPhase::StepReady)
            .for_step(PlanStepId::new("missing").unwrap());
        context.used_idempotency_keys.insert("task.read".to_owned());
        let report = PlanVerifier::default().verify(&plan, &context);
        assert_eq!(report.status, PlanVerificationStatus::Invalid);
        assert!(
            report
                .violations
                .iter()
                .any(|violation| violation.code == PlanViolationCode::MissingReference)
        );
    }

    #[test]
    fn verifier_bounds_violation_report() {
        let plan = read_only_plan(vec![step(
            "read",
            &[],
            PlanAction::ReadOnly {
                operation: "inspect".to_owned(),
            },
            PlanRisk::ReadOnly,
            PlanApprovalRequirement::None,
            vec![
                PlanCondition::CapabilityGranted {
                    capability: Capability::Network,
                },
                PlanCondition::CircuitClosed,
            ],
            Vec::new(),
        )]);
        let verifier = PlanVerifier {
            max_violations: 1,
            ..PlanVerifier::default()
        };
        let context = PlanVerificationContext::new(PlanVerificationPhase::StepReady)
            .for_step(PlanStepId::new("read").unwrap());
        let report = verifier.verify(&plan, &context);
        assert_eq!(report.violations.len(), 1);
    }
}
