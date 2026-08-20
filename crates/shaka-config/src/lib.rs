//! Configuração validada do runtime Shaka.

use serde::{Deserialize, Serialize};
use shaka_core::{Action, CoreError, OperatorId, Principal, Role, TenantId};
use std::{path::PathBuf, str::FromStr};
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Environment {
    Development,
    Staging,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelProvider {
    Local,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppConfig {
    pub environment: Environment,
    pub database: PathBuf,
    pub skills_file: PathBuf,
    pub tenant_id: TenantId,
    pub principal: Principal,
    pub model_provider: ModelProvider,
    pub model_endpoint: Url,
    pub model_name: String,
    pub api_key: Option<String>,
    pub live_requested: bool,
    pub live_confirmation: bool,
    pub audit_required: bool,
}

impl AppConfig {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConfigSummary {
    pub environment: Environment,
    pub tenant_id: TenantId,
    pub operator_id: OperatorId,
    pub role: Role,
    pub model_provider: ModelProvider,
    pub model_endpoint: Url,
    pub model_name: String,
    pub api_key_configured: bool,
    pub live_requested: bool,
    pub live_confirmation: bool,
    pub audit_required: bool,
}

pub fn parse_role(value: &str) -> Result<Role, ConfigError> {
    match value.to_ascii_lowercase().as_str() {
        "operator" => Ok(Role::Operator),
        "reviewer" => Ok(Role::Reviewer),
        "administrator" | "admin" => Ok(Role::Administrator),
        other => Err(ConfigError::InvalidRole(other.to_owned())),
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("ambiente inválido: {0}")]
    InvalidEnvironment(String),
    #[error("provedor inválido: {0}")]
    InvalidProvider(String),
    #[error("papel inválido: {0}")]
    InvalidRole(String),
    #[error("endpoint inválido: {0}")]
    InvalidEndpoint(String),
    #[error("valor inválido: {0}")]
    InvalidValue(String),
    #[error("configuração de produção exige um provedor externo")]
    ProductionRequiresExternalModel,
    #[error("configuração de produção exige chave de API")]
    ProductionRequiresApiKey,
    #[error("configuração de produção exige endpoint HTTPS")]
    ProductionRequiresHttps,
    #[error("modo live exige uma implantação explícita fora do ambiente production lockado")]
    LiveModeRequiresExplicitDeployment,
    #[error("principal não autorizado para a ação: {0:?}")]
    Unauthorized(Action),
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
