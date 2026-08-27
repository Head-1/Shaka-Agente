//! Fachada de observabilidade governada do Shaka.
//!
//! A Etapa A fornece os contratos locais para correlação, redaction e schema.
//! Exporters e instrumentação dos componentes serão adicionados em etapas
//! posteriores sem permitir que telemetria altere decisões de autorização.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use shaka_core::{AuditEvent, TaskId, TenantId, redact_sensitive};
use shaka_memory::{AuditVerification, MemoryError, MemoryStore};
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

/// Nome do schema interno de telemetria do Shaka.
pub const SCHEMA_NAME: &str = "shaka.observability";
/// Versão do schema interno implementado nesta etapa.
pub const SCHEMA_VERSION: &str = "0.7";
/// Perfil de interoperabilidade `GenAI` adotado pelo Shaka.
pub const GEN_AI_PROFILE: &str = "shaka.genai.v0.7";
const DEFAULT_MAX_TEXT_CHARS: usize = 4_096;
const DEFAULT_MAX_JSON_DEPTH: usize = 16;
const DEFAULT_MAX_JSON_ITEMS: usize = 256;

/// Inicializa o subscriber de logs compatível com a configuração atual do CLI.
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

/// Erros da fachada de observabilidade.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TelemetryError {
    /// Um identificador de correlação não respeitou o formato seguro.
    #[error("identificador de correlação inválido: {0}")]
    InvalidCorrelationId(String),
    /// A captura de conteúdo não faz parte da política estrita da Etapa A.
    #[error("captura de conteúdo GenAI está desabilitada pela política estrita")]
    ContentCaptureDisabled,
    /// A política recebeu limites impossíveis ou excessivos.
    #[error("política de telemetria inválida: {0}")]
    InvalidPolicy(String),
}

/// Descritor público do schema interno usado nos sinais do Shaka.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetrySchema {
    /// Nome estável do schema interno.
    pub name: &'static str,
    /// Versão do schema interno.
    pub version: &'static str,
    /// Perfil `GenAI` de interoperabilidade.
    pub gen_ai_profile: &'static str,
}

impl Default for TelemetrySchema {
    fn default() -> Self {
        Self {
            name: SCHEMA_NAME,
            version: SCHEMA_VERSION,
            gen_ai_profile: GEN_AI_PROFILE,
        }
    }
}

/// Política segura e local da Etapa A.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryPolicy {
    /// Captura de prompts, respostas e argumentos; permanece falsa por padrão.
    pub capture_content: bool,
    /// Limite máximo de caracteres por valor textual redacted.
    pub max_text_chars: usize,
    /// Profundidade máxima de um JSON redacted.
    pub max_json_depth: usize,
    /// Número máximo de itens por objeto ou array redacted.
    pub max_json_items: usize,
}

impl Default for TelemetryPolicy {
    fn default() -> Self {
        Self {
            capture_content: false,
            max_text_chars: DEFAULT_MAX_TEXT_CHARS,
            max_json_depth: DEFAULT_MAX_JSON_DEPTH,
            max_json_items: DEFAULT_MAX_JSON_ITEMS,
        }
    }
}

impl TelemetryPolicy {
    fn validate(self) -> Result<Self, TelemetryError> {
        if self.capture_content {
            return Err(TelemetryError::ContentCaptureDisabled);
        }
        if self.max_text_chars == 0 || self.max_text_chars > 64 * 1024 {
            return Err(TelemetryError::InvalidPolicy(
                "max_text_chars deve estar entre 1 e 65536".to_owned(),
            ));
        }
        if self.max_json_depth == 0 || self.max_json_depth > 64 {
            return Err(TelemetryError::InvalidPolicy(
                "max_json_depth deve estar entre 1 e 64".to_owned(),
            ));
        }
        if self.max_json_items == 0 || self.max_json_items > 4_096 {
            return Err(TelemetryError::InvalidPolicy(
                "max_json_items deve estar entre 1 e 4096".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// Contexto de correlação propagado entre HTTP, fila, worker e runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrelationContext {
    request_id: String,
    trace_id: Option<String>,
    span_id: Option<String>,
    task_id: Option<String>,
    session_id: Option<String>,
    tenant_ref: Option<String>,
}

impl Default for CorrelationContext {
    fn default() -> Self {
        Self::new()
    }
}

impl CorrelationContext {
    /// Cria um contexto com um request ID aleatório e não secreto.
    #[must_use]
    pub fn new() -> Self {
        Self {
            request_id: Uuid::new_v4().to_string(),
            trace_id: None,
            span_id: None,
            task_id: None,
            session_id: None,
            tenant_ref: None,
        }
    }

    /// Cria um contexto usando um request ID validado e fornecido pelo chamador.
    pub fn with_request_id(request_id: impl Into<String>) -> Result<Self, TelemetryError> {
        let request_id = validate_correlation_id("request_id", request_id.into())?;
        Ok(Self {
            request_id,
            ..Self::new()
        })
    }

    /// Anexa trace ID e span ID validados ao contexto.
    pub fn with_trace_ids(
        mut self,
        trace_id: Option<impl Into<String>>,
        span_id: Option<impl Into<String>>,
    ) -> Result<Self, TelemetryError> {
        self.trace_id = validate_optional_id("trace_id", trace_id)?;
        self.span_id = validate_optional_id("span_id", span_id)?;
        Ok(self)
    }

    /// Anexa a referência de tarefa ao contexto.
    pub fn with_task_id(
        mut self,
        task_id: Option<impl Into<String>>,
    ) -> Result<Self, TelemetryError> {
        self.task_id = validate_optional_id("task_id", task_id)?;
        Ok(self)
    }

    /// Anexa a referência de sessão ao contexto.
    pub fn with_session_id(
        mut self,
        session_id: Option<impl Into<String>>,
    ) -> Result<Self, TelemetryError> {
        self.session_id = validate_optional_id("session_id", session_id)?;
        Ok(self)
    }

    /// Anexa uma referência de tenant já redacted/pseudonimizada.
    pub fn with_tenant_ref(
        mut self,
        tenant_ref: Option<impl Into<String>>,
    ) -> Result<Self, TelemetryError> {
        self.tenant_ref = tenant_ref
            .map(|value| {
                let value = validate_correlation_id("tenant_ref", value.into())?;
                let digest = hex::encode(Sha256::digest(value.as_bytes()));
                Ok(format!("sha256:{}", &digest[..16]))
            })
            .transpose()?;
        Ok(self)
    }

    /// Retorna o request ID correlacionável.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Retorna o trace ID, quando propagado.
    #[must_use]
    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
    }

    /// Retorna o span ID, quando propagado.
    #[must_use]
    pub fn span_id(&self) -> Option<&str> {
        self.span_id.as_deref()
    }

    /// Retorna a referência de tarefa, quando disponível.
    #[must_use]
    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    /// Retorna a referência de sessão, quando disponível.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Retorna a referência de tenant redacted, quando disponível.
    #[must_use]
    pub fn tenant_ref(&self) -> Option<&str> {
        self.tenant_ref.as_deref()
    }
}

fn validate_optional_id<T>(field: &str, value: Option<T>) -> Result<Option<String>, TelemetryError>
where
    T: Into<String>,
{
    value
        .map(|value| validate_correlation_id(field, value.into()))
        .transpose()
}

fn validate_correlation_id(field: &str, value: String) -> Result<String, TelemetryError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '~')
        });
    if valid {
        Ok(value)
    } else {
        Err(TelemetryError::InvalidCorrelationId(field.to_owned()))
    }
}

fn normalized_key(key: &str) -> String {
    key.to_ascii_lowercase().replace('-', "_")
}

fn sensitive_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    matches!(
        normalized.as_str(),
        "api_key"
            | "apikey"
            | "access_token"
            | "authorization"
            | "bearer"
            | "cookie"
            | "set_cookie"
            | "secret"
            | "password"
            | "private_key"
            | "token"
    ) || normalized.ends_with("_secret")
        || normalized.ends_with("_password")
        || normalized.ends_with("_api_key")
        || normalized.ends_with("_access_token")
}

fn content_key(key: &str) -> bool {
    let normalized = normalized_key(key);
    matches!(
        normalized.as_str(),
        "argument"
            | "arguments"
            | "completion"
            | "content"
            | "input"
            | "message"
            | "messages"
            | "objective"
            | "output"
            | "prompt"
            | "response"
            | "result"
            | "system"
            | "system_instructions"
            | "system_prompt"
            | "tool_arguments"
            | "tool_result"
            | "user_message"
    ) || [
        "argument",
        "completion",
        "content",
        "input",
        "message",
        "objective",
        "output",
        "prompt",
        "response",
        "result",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let suffix = "…[TRUNCATED]";
    let suffix_len = suffix.chars().count();
    if max_chars <= suffix_len {
        return suffix.chars().take(max_chars).collect();
    }
    let prefix: String = value.chars().take(max_chars - suffix_len).collect();
    format!("{prefix}{suffix}")
}

/// Redactor central para texto, metadata e JSON estruturado.
#[derive(Debug, Clone)]
pub struct Redactor {
    text: usize,
    json_depth: usize,
    json_items: usize,
}

impl Redactor {
    /// Cria e valida um redactor a partir de uma política.
    pub fn new(policy: TelemetryPolicy) -> Result<Self, TelemetryError> {
        let policy = policy.validate()?;
        Ok(Self::from_policy(policy))
    }

    fn from_policy(policy: TelemetryPolicy) -> Self {
        Self {
            text: policy.max_text_chars,
            json_depth: policy.max_json_depth,
            json_items: policy.max_json_items,
        }
    }

    /// Redacta padrões conhecidos de segredo e limita o tamanho do texto.
    #[must_use]
    pub fn redact_text(&self, input: &str) -> String {
        truncate_text(&redact_sensitive(input), self.text)
    }

    /// Redacta recursivamente um valor JSON, removendo valores de chaves sensíveis.
    #[must_use]
    pub fn redact_json(&self, value: &Value) -> Value {
        self.redact_json_at_depth(value, 0)
    }

    fn redact_json_at_depth(&self, value: &Value, depth: usize) -> Value {
        if depth >= self.json_depth {
            return Value::String("[REDACTED_DEPTH_LIMIT]".to_owned());
        }
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
            Value::String(text) => Value::String(self.redact_text(text)),
            Value::Array(values) => {
                let mut redacted = values
                    .iter()
                    .take(self.json_items)
                    .map(|item| self.redact_json_at_depth(item, depth + 1))
                    .collect::<Vec<_>>();
                if values.len() > self.json_items {
                    redacted.push(Value::String("[TRUNCATED_ITEMS]".to_owned()));
                }
                Value::Array(redacted)
            }
            Value::Object(object) => {
                let mut redacted = Map::new();
                for (index, (key, item)) in object.iter().enumerate() {
                    if index >= self.json_items {
                        redacted.insert(
                            "_truncated".to_owned(),
                            Value::String("[TRUNCATED_ITEMS]".to_owned()),
                        );
                        break;
                    }
                    if sensitive_key(key) {
                        redacted.insert(key.clone(), Value::String("[REDACTED]".to_owned()));
                    } else if content_key(key) {
                        redacted
                            .insert(key.clone(), Value::String("[REDACTED_CONTENT]".to_owned()));
                    } else {
                        redacted.insert(key.clone(), self.redact_json_at_depth(item, depth + 1));
                    }
                }
                Value::Object(redacted)
            }
        }
    }

    fn redact_field(&self, key: &str, value: &str) -> (String, String) {
        let normalized = normalized_key(key);
        let safe_value = if sensitive_key(key) {
            "[REDACTED]".to_owned()
        } else if content_key(key) {
            "[REDACTED_CONTENT]".to_owned()
        } else if normalized.ends_with("_json") {
            serde_json::from_str::<Value>(value).map_or_else(
                |_| self.redact_text(value),
                |json| self.redact_json(&json).to_string(),
            )
        } else {
            self.redact_text(value)
        };
        (key.to_owned(), safe_value)
    }

    /// Redacta metadata textual preservando as chaves permitidas.
    #[must_use]
    pub fn redact_fields(&self, fields: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        fields
            .iter()
            .map(|(key, value)| self.redact_field(key, value))
            .collect()
    }

    fn redact_owned_fields(&self, fields: BTreeMap<String, String>) -> BTreeMap<String, String> {
        fields
            .into_iter()
            .map(|(key, value)| self.redact_field(&key, &value))
            .collect()
    }
}

/// Fachada de contratos de observabilidade da Etapa A.
#[derive(Debug, Clone)]
pub struct Telemetry {
    policy: TelemetryPolicy,
    schema: TelemetrySchema,
    redactor: Redactor,
}

impl Default for Telemetry {
    fn default() -> Self {
        let policy = TelemetryPolicy::default();
        Self {
            redactor: Redactor::from_policy(policy),
            policy,
            schema: TelemetrySchema::default(),
        }
    }
}

impl Telemetry {
    /// Cria a fachada a partir de uma política segura.
    pub fn new(policy: TelemetryPolicy) -> Result<Self, TelemetryError> {
        let policy = policy.validate()?;
        Ok(Self {
            redactor: Redactor::from_policy(policy),
            policy,
            schema: TelemetrySchema::default(),
        })
    }

    /// Retorna a política efetiva.
    #[must_use]
    pub const fn policy(&self) -> TelemetryPolicy {
        self.policy
    }

    /// Retorna o descritor do schema interno.
    #[must_use]
    pub const fn schema(&self) -> TelemetrySchema {
        self.schema
    }

    /// Retorna o redactor compartilhável da fachada.
    #[must_use]
    pub const fn redactor(&self) -> &Redactor {
        &self.redactor
    }

    /// Cria um novo contexto de correlação.
    #[must_use]
    pub fn new_correlation_context(&self) -> CorrelationContext {
        CorrelationContext::new()
    }

    /// Cria um span inicial com campos de schema e correlação redacted.
    #[must_use]
    pub fn operation_span(&self, operation: &str, context: &CorrelationContext) -> tracing::Span {
        let operation = self.redactor.redact_text(operation);
        tracing::span!(
            target: "shaka.telemetry",
            tracing::Level::INFO,
            "shaka.operation",
            schema = SCHEMA_NAME,
            schema_version = SCHEMA_VERSION,
            gen_ai_profile = GEN_AI_PROFILE,
            operation = %operation,
            request_id = %context.request_id(),
            trace_id = ?context.trace_id(),
            span_id = ?context.span_id(),
            task_id = ?context.task_id(),
            session_id = ?context.session_id(),
            tenant_ref = ?context.tenant_ref(),
            http_method = tracing::field::Empty,
            http_route = tracing::field::Empty,
            http_status_code = tracing::field::Empty,
            http_status_class = tracing::field::Empty,
            outcome = tracing::field::Empty,
            admission = tracing::field::Empty,
            retryable = tracing::field::Empty,
            attempt = tracing::field::Empty,
            lease_state = tracing::field::Empty,
            circuit_state = tracing::field::Empty,
            error_type = tracing::field::Empty,
            worker_id = tracing::field::Empty,
            queue_depth = tracing::field::Empty,
            retry_delay_ms = tracing::field::Empty,
            lease_recovered = tracing::field::Empty,
        )
    }

    /// Cria um span de servidor HTTP sem registrar a URI bruta.
    #[must_use]
    pub fn http_server_span(
        &self,
        context: &CorrelationContext,
        method: &str,
        route_template: &str,
    ) -> tracing::Span {
        let span = self.operation_span("http.server", context);
        let method = self.redactor.redact_text(method);
        let route = self.redactor.redact_text(route_template);
        span.record("http_method", method.as_str());
        span.record("http_route", route.as_str());
        span
    }

    /// Impede explicitamente a captura de conteúdo na política estrita.
    pub fn ensure_content_capture_disabled(&self, requested: bool) -> Result<(), TelemetryError> {
        if requested {
            Err(TelemetryError::ContentCaptureDisabled)
        } else {
            Ok(())
        }
    }
}

/// Logger de auditoria persistente compatível com a v0.6.0.
#[derive(Debug, Clone)]
pub struct AuditLogger {
    memory: Arc<MemoryStore>,
    redactor: Redactor,
}

impl AuditLogger {
    /// Cria um logger ligado ao armazenamento de memória/auditoria.
    #[must_use]
    pub fn new(memory: Arc<MemoryStore>) -> Self {
        Self {
            memory,
            redactor: Redactor::from_policy(TelemetryPolicy::default()),
        }
    }

    /// Cria um logger usando um redactor fornecido pela política do runtime.
    #[must_use]
    pub fn with_redactor(memory: Arc<MemoryStore>, redactor: Redactor) -> Self {
        Self { memory, redactor }
    }

    /// Registra um evento de auditoria com metadata textual.
    pub fn record(
        &self,
        task_id: Option<TaskId>,
        tenant_id: TenantId,
        actor: impl Into<String>,
        action: impl Into<String>,
        outcome: impl Into<String>,
        metadata: BTreeMap<String, String>,
    ) -> Result<AuditEvent, MemoryError> {
        let metadata = self.redactor.redact_owned_fields(metadata);
        let event = AuditEvent::new(task_id, tenant_id, actor, action, outcome, metadata, None);
        let chained = self.memory.append_audit_event(&event)?;
        Ok(chained)
    }

    /// Verifica a integridade do banco de memória atualmente aberto.
    pub fn verify_integrity(&self) -> Result<bool, MemoryError> {
        self.memory.verify_integrity()
    }

    /// Verifica a cadeia de auditoria do tenant sem alterar o estado persistido.
    pub fn verify_audit_chain(
        &self,
        tenant_id: &TenantId,
    ) -> Result<AuditVerification, MemoryError> {
        self.memory.verify_audit_chain(tenant_id)
    }

    /// Registra metadata JSON legada como campo textual.
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
        let redacted = self.redactor.redact_json(metadata);
        fields.insert("metadata_json".to_owned(), redacted.to_string());
        self.record(task_id, tenant_id, actor, action, outcome, fields)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_policy_is_strict_and_content_capture_is_rejected() {
        let telemetry = Telemetry::default();
        assert!(!telemetry.policy().capture_content);
        assert_eq!(telemetry.schema().version, "0.7");
        assert!(matches!(
            Telemetry::new(TelemetryPolicy {
                capture_content: true,
                ..TelemetryPolicy::default()
            }),
            Err(TelemetryError::ContentCaptureDisabled)
        ));
        assert!(telemetry.ensure_content_capture_disabled(false).is_ok());
        assert!(matches!(
            telemetry.ensure_content_capture_disabled(true),
            Err(TelemetryError::ContentCaptureDisabled)
        ));
    }

    #[test]
    fn text_redaction_removes_common_secrets_and_bounds_output() {
        let telemetry = Telemetry::default();
        let result = telemetry
            .redactor()
            .redact_text("api_key=secret Authorization: Bearer abc.def password='hidden'");
        assert!(!result.contains("secret"));
        assert!(!result.contains("abc.def"));
        assert!(!result.contains("Bearer"));
        assert!(!result.contains("hidden"));
        assert!(result.contains("REDACTED"));

        let bounded = Telemetry::new(TelemetryPolicy {
            max_text_chars: 24,
            ..TelemetryPolicy::default()
        })
        .expect("policy")
        .redactor()
        .redact_text("prefix with a very long safe value");
        assert!(bounded.chars().count() <= 24);
    }

    #[test]
    fn json_redaction_is_recursive_and_key_aware() {
        let telemetry = Telemetry::default();
        let value = json!({
            "safe": "api_key=embedded-secret",
            "api_key": "top-secret",
            "objective": "private objective",
            "prompt_text": "private prompt",
            "model_input": "private model input",
            "nested": {"authorization": "Bearer abc.def", "value": "ok"},
            "items": ["password=hidden", {"private_key": "pem-secret"}]
        });
        let redacted = telemetry.redactor().redact_json(&value);
        let serialized = redacted.to_string();
        assert!(!serialized.contains("top-secret"));
        assert!(!serialized.contains("abc.def"));
        assert!(!serialized.contains("pem-secret"));
        assert!(!serialized.contains("embedded-secret"));
        assert!(!serialized.contains("private prompt"));
        assert!(!serialized.contains("private model input"));
        assert!(serialized.contains("REDACTED"));
        assert_eq!(redacted["nested"]["value"], "ok");
    }

    #[test]
    fn fields_redaction_preserves_safe_metadata_and_masks_sensitive_keys() {
        let telemetry = Telemetry::default();
        let fields = BTreeMap::from([
            ("outcome".to_owned(), "success".to_owned()),
            ("api_key".to_owned(), "secret".to_owned()),
            ("objective".to_owned(), "private objective".to_owned()),
            (
                "metadata_json".to_owned(),
                "{\"prompt\":\"private\",\"ok\":\"yes\"}".to_owned(),
            ),
            ("error".to_owned(), "password=hidden".to_owned()),
        ]);
        let redacted = telemetry.redactor().redact_fields(&fields);
        assert_eq!(redacted["outcome"], "success");
        assert_eq!(redacted["api_key"], "[REDACTED]");
        assert_eq!(redacted["objective"], "[REDACTED_CONTENT]");
        assert!(!redacted["metadata_json"].contains("private"));
        assert!(redacted["metadata_json"].contains("REDACTED_CONTENT"));
        assert!(!redacted["error"].contains("hidden"));
    }

    #[test]
    fn correlation_context_validates_ids_and_is_chainable() {
        let context = CorrelationContext::with_request_id("request-1")
            .expect("request")
            .with_trace_ids(Some("trace-1"), Some("span-1"))
            .expect("trace")
            .with_task_id(Some("task-1"))
            .expect("task")
            .with_session_id(Some("session-1"))
            .expect("session")
            .with_tenant_ref(Some("tenant-ref"))
            .expect("tenant");
        assert_eq!(context.request_id(), "request-1");
        assert_eq!(context.trace_id(), Some("trace-1"));
        assert_eq!(context.task_id(), Some("task-1"));
        assert!(
            context
                .tenant_ref()
                .is_some_and(|value| value.starts_with("sha256:"))
        );
        assert!(
            !context
                .tenant_ref()
                .is_some_and(|value| value.contains("tenant-ref"))
        );
        assert_eq!(
            CorrelationContext::with_request_id("request id"),
            Err(TelemetryError::InvalidCorrelationId(
                "request_id".to_owned()
            ))
        );
        assert_eq!(
            CorrelationContext::with_request_id(""),
            Err(TelemetryError::InvalidCorrelationId(
                "request_id".to_owned()
            ))
        );
    }

    #[test]
    fn operation_span_has_stable_name_and_schema_metadata() {
        let telemetry = Telemetry::default();
        let context = telemetry
            .new_correlation_context()
            .with_task_id(Some("task-1"))
            .expect("task");
        let subscriber = tracing_subscriber::registry();
        let _guard = tracing::subscriber::set_default(subscriber);
        let span = telemetry.operation_span("invoke_agent", &context);
        assert_eq!(span.metadata().expect("metadata").name(), "shaka.operation");
        assert!(!context.request_id().is_empty());
    }

    #[test]
    fn json_depth_and_item_limits_are_enforced() {
        let telemetry = Telemetry::new(TelemetryPolicy {
            max_json_depth: 2,
            max_json_items: 1,
            ..TelemetryPolicy::default()
        })
        .expect("policy");
        let nested = json!({"one": {"two": {"three": "secret"}}});
        let redacted_nested = telemetry.redactor().redact_json(&nested);
        assert_eq!(redacted_nested["one"]["two"], "[REDACTED_DEPTH_LIMIT]");

        let items = json!({"first": "value", "second": "value"});
        let redacted_items = telemetry.redactor().redact_json(&items);
        assert_eq!(redacted_items["_truncated"], "[TRUNCATED_ITEMS]");
    }
}
