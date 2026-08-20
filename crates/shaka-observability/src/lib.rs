//! Observabilidade e auditoria do Shaka.

use serde_json::Value;
use shaka_core::{AuditEvent, TaskId, TenantId};
use shaka_memory::{MemoryError, MemoryStore};
use std::{collections::BTreeMap, sync::Arc};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub fn init_tracing(json_logs: bool) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,shaka=debug"));
    if json_logs {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(std::io::stderr),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    }
}

#[derive(Debug, Clone)]
pub struct AuditLogger {
    memory: Arc<MemoryStore>,
}

impl AuditLogger {
    #[must_use]
    pub fn new(memory: Arc<MemoryStore>) -> Self {
        Self { memory }
    }

    pub fn record(
        &self,
        task_id: Option<TaskId>,
        tenant_id: TenantId,
        actor: impl Into<String>,
        action: impl Into<String>,
        outcome: impl Into<String>,
        metadata: BTreeMap<String, String>,
    ) -> Result<AuditEvent, MemoryError> {
        let event = AuditEvent::new(task_id, tenant_id, actor, action, outcome, metadata, None);
        let chained = self.memory.append_audit_event(&event)?;
        Ok(chained)
    }

    pub fn record_json(
        &self,
        task_id: Option<TaskId>,
        tenant_id: TenantId,
        actor: impl Into<String>,
        action: impl Into<String>,
        outcome: impl Into<String>,
        metadata: &Value,
    ) -> Result<AuditEvent, MemoryError> {
        let mut fields = BTreeMap::new();
        fields.insert("metadata_json".to_owned(), metadata.to_string());
        self.record(task_id, tenant_id, actor, action, outcome, fields)
    }
}
