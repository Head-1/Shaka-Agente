//! Contratos centrais e tipos compartilhados do agente Shaka.

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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
                Action::RunReadOnly | Action::ApproveSkill | Action::VerifyAudit | Action::Backup
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

    #[test]
    fn sensitive_values_are_redacted() {
        let value = "api_key=secret-value Authorization: Bearer abc.def";
        let result = redact_sensitive(value);
        assert!(!result.contains("secret-value"));
        assert!(!result.contains("abc.def"));
    }
}
