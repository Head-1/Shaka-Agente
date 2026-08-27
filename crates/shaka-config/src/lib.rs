//! Configuração validada do runtime Shaka.

use serde::{Deserialize, Serialize};
use shaka_core::{Action, CoreError, OperatorId, Principal, Role, TenantId};
use std::{path::PathBuf, str::FromStr};
use thiserror::Error;
use url::Url;

/// Ambiente operacional em que o runtime será executado.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Environment {
    /// Ambiente local sem requisitos de produção.
    Development,
    /// Ambiente intermediário com controles de execução externa.
    Staging,
    /// Ambiente lockado com modelo externo, chave e endpoint HTTPS.
    Production,
}

impl FromStr for Environment {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "staging" | "stage" => Ok(Self::Staging),
            "production" | "prod" => Ok(Self::Production),
            other => Err(ConfigError::InvalidEnvironment(other.to_owned())),
        }
    }
}

/// Tipo de provedor de modelo aceito pela configuração.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelProvider {
    /// Provedor local para desenvolvimento ou testes.
    Local,
    /// Endpoint externo compatível com a API `OpenAI`.
    OpenAiCompatible,
}

impl FromStr for ModelProvider {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "openai-compatible" | "openai" => Ok(Self::OpenAiCompatible),
            other => Err(ConfigError::InvalidProvider(other.to_owned())),
        }
    }
}

/// Configuração completa validada antes de iniciar o runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    /// Ambiente que determina os requisitos de segurança aplicados.
    pub environment: Environment,
    /// Caminho do banco SQLite operacional.
    pub database: PathBuf,
    /// Caminho do manifesto de skills confiável.
    pub skills_file: PathBuf,
    /// Tenant padrão do processo.
    pub tenant_id: TenantId,
    /// Principal host-side usado pelo processo.
    pub principal: Principal,
    /// Provedor de modelo selecionado.
    pub model_provider: ModelProvider,
    /// Endpoint do provedor de modelo.
    pub model_endpoint: Url,
    /// Nome do modelo solicitado.
    pub model_name: String,
    /// Chave de API; não deve ser incluída em sumários públicos ou logs.
    pub api_key: Option<String>,
    /// Indica que uma execução live foi solicitada.
    pub live_requested: bool,
    /// Confirmação explícita exigida para execução live em produção.
    pub live_confirmation: bool,
    /// Indica que a auditoria é obrigatória para o runtime.
    pub audit_required: bool,
}

impl AppConfig {
    /// Valida invariantes de ambiente, modelo, endpoint, auditoria e execução live.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.model_name.trim().is_empty() || self.model_name.len() > 256 {
            return Err(ConfigError::InvalidValue("model_name".to_owned()));
        }
        if !self.audit_required {
            return Err(ConfigError::InvalidValue(
                "audit_required deve permanecer habilitado".to_owned(),
            ));
        }
        match self.environment {
            Environment::Production => {
                if self.model_provider == ModelProvider::Local {
                    return Err(ConfigError::ProductionRequiresExternalModel);
                }
                if self
                    .api_key
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    return Err(ConfigError::ProductionRequiresApiKey);
                }
                if self.model_endpoint.scheme() != "https" {
                    return Err(ConfigError::ProductionRequiresHttps);
                }
                if self.live_requested && !self.live_confirmation {
                    return Err(ConfigError::LiveModeRequiresExplicitDeployment);
                }
            }
            Environment::Staging => {
                if self.live_requested && !self.principal.allows(&Action::RunExternal) {
                    return Err(ConfigError::Unauthorized(Action::RunExternal));
                }
            }
            Environment::Development => {}
        }
        if self.live_requested && !self.principal.allows(&Action::RunExternal) {
            return Err(ConfigError::Unauthorized(Action::RunExternal));
        }
        Ok(())
    }

    /// Constrói e valida uma configuração a partir de valores textuais externos.
    #[allow(clippy::too_many_arguments)]
    pub fn from_values(
        environment: &str,
        database: PathBuf,
        skills_file: PathBuf,
        tenant: &str,
        operator: &str,
        role: &str,
        provider: &str,
        endpoint: &str,
        model_name: &str,
        api_key: Option<String>,
        live_requested: bool,
        live_confirmation: bool,
        audit_required: bool,
    ) -> Result<Self, ConfigError> {
        let environment = Environment::from_str(environment)?;
        let model_provider = ModelProvider::from_str(provider)?;
        let tenant_id = TenantId::new(tenant)?;
        let operator_id = OperatorId::new(operator)?;
        let role = parse_role(role)?;
        let principal = Principal {
            operator_id,
            tenant_id: tenant_id.clone(),
            role,
        };
        let model_endpoint = Url::parse(endpoint)
            .map_err(|error| ConfigError::InvalidEndpoint(error.to_string()))?;
        let config = Self {
            environment,
            database,
            skills_file,
            tenant_id,
            principal,
            model_provider,
            model_endpoint,
            model_name: model_name.to_owned(),
            api_key,
            live_requested,
            live_confirmation,
            audit_required,
        };
        config.validate()?;
        Ok(config)
    }

    /// Produz um resumo operacional que informa apenas se a chave está configurada.
    #[must_use]
    pub fn public_summary(&self) -> ConfigSummary {
        ConfigSummary {
            environment: self.environment.clone(),
            tenant_id: self.tenant_id.clone(),
            operator_id: self.principal.operator_id.clone(),
            role: self.principal.role.clone(),
            model_provider: self.model_provider.clone(),
            model_endpoint: self.model_endpoint.clone(),
            model_name: self.model_name.clone(),
            api_key_configured: self.api_key.is_some(),
            live_requested: self.live_requested,
            live_confirmation: self.live_confirmation,
            audit_required: self.audit_required,
        }
    }
}

/// Resumo seguro da configuração para diagnóstico e observabilidade.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConfigSummary {
    /// Ambiente ativo.
    pub environment: Environment,
    /// Tenant selecionado.
    pub tenant_id: TenantId,
    /// Operador selecionado.
    pub operator_id: OperatorId,
    /// Papel resolvido do operador.
    pub role: Role,
    /// Provedor de modelo selecionado.
    pub model_provider: ModelProvider,
    /// Endpoint configurado, sem o segredo da chave.
    pub model_endpoint: Url,
    /// Nome do modelo configurado.
    pub model_name: String,
    /// Indica apenas a presença de uma chave, sem expor seu valor.
    pub api_key_configured: bool,
    /// Indica se o modo live foi solicitado.
    pub live_requested: bool,
    /// Indica se a confirmação live foi fornecida.
    pub live_confirmation: bool,
    /// Indica se a auditoria permanece obrigatória.
    pub audit_required: bool,
}

/// Converte a representação textual de um papel em um papel host-side.
pub fn parse_role(value: &str) -> Result<Role, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "operator" => Ok(Role::Operator),
        "reviewer" => Ok(Role::Reviewer),
        "administrator" | "admin" => Ok(Role::Administrator),
        other => Err(ConfigError::InvalidRole(other.to_owned())),
    }
}

/// Falhas de parsing e de validação de configuração.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Ambiente textual desconhecido.
    #[error("ambiente inválido: {0}")]
    InvalidEnvironment(String),
    /// Provedor textual desconhecido.
    #[error("provedor inválido: {0}")]
    InvalidProvider(String),
    /// Papel textual desconhecido.
    #[error("papel inválido: {0}")]
    InvalidRole(String),
    /// Endpoint que não pôde ser interpretado como URL.
    #[error("endpoint inválido: {0}")]
    InvalidEndpoint(String),
    /// Campo de configuração vazio, excessivo ou incompatível.
    #[error("valor inválido: {0}")]
    InvalidValue(String),
    /// Produção não pode usar o provedor local.
    #[error("configuração de produção exige um provedor externo")]
    ProductionRequiresExternalModel,
    /// Produção exige uma chave de API não vazia.
    #[error("configuração de produção exige chave de API")]
    ProductionRequiresApiKey,
    /// Produção exige endpoint HTTPS.
    #[error("configuração de produção exige endpoint HTTPS")]
    ProductionRequiresHttps,
    /// O modo live exige confirmação explícita de implantação.
    #[error("modo live exige uma implantação explícita fora do ambiente production lockado")]
    LiveModeRequiresExplicitDeployment,
    /// O principal não possui a ação requerida.
    #[error("principal não autorizado para a ação: {0:?}")]
    Unauthorized(Action),
    /// Violação de contrato compartilhado do núcleo.
    #[error("erro de núcleo: {0}")]
    Core(#[from] CoreError),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn base(environment: &str) -> AppConfig {
        AppConfig::from_values(
            environment,
            PathBuf::from("data.db"),
            PathBuf::from("skills.json"),
            "tenant",
            "operator",
            "operator",
            "local",
            "http://localhost:1",
            "local",
            None,
            false,
            false,
            true,
        )
        .unwrap()
    }

    #[test]
    fn production_rejects_local_model() {
        assert!(matches!(
            AppConfig::from_values(
                "production",
                PathBuf::from("data.db"),
                PathBuf::from("skills.json"),
                "tenant",
                "operator",
                "administrator",
                "local",
                "https://localhost/api",
                "model",
                None,
                false,
                false,
                true,
            ),
            Err(ConfigError::ProductionRequiresExternalModel)
        ));
    }

    #[test]
    fn development_summary_does_not_expose_secret() {
        let mut config = base("development");
        config.api_key = Some("secret".to_owned());
        assert!(config.public_summary().api_key_configured);
    }
}
