//! Interface operacional do Shaka.

use anyhow::{Context, Result, bail};
use chrono::Duration;
use clap::{Args, Parser, Subcommand};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use serde_json::{Value, json};
use shaka_api::{ApiConfig, ApiState, serve as serve_api};
use shaka_config::{AppConfig, ModelProvider};
use shaka_core::{
    Action, AuditEvent, Capability, CapabilitySet, Principal, Role, SkillManifest, SkillStatus,
    TaskEnvelope,
};
use shaka_memory::MemoryStore;
use shaka_observability::{AuditLogger, init_tracing};
use shaka_orchestrator::{
    AgentRuntime, EchoTool, LocalModel, OpenAiCompatibleModel, ToolRegistry, WasmSkillTool,
};
use shaka_queue::{QueueStore, TenantLimits};
use shaka_sandbox::{SandboxPolicy, WasmExecutor};
use shaka_skills::{SkillRegistry, TrustStore, load_signing_key, public_key_hex, save_signing_key};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Parser)]
#[command(
    name = "shaka",
    version,
    about = "Agente de IA governado, auditável e extensível"
)]
struct Cli {
    #[arg(long, default_value = "data/shaka.db", env = "SHAKA_DATABASE")]
    database: PathBuf,
    #[arg(long, default_value = "data/skills.json", env = "SHAKA_SKILLS_FILE")]
    skills_file: PathBuf,
    #[arg(
        long,
        default_value = "data/trusted_keys.json",
        env = "SHAKA_TRUST_FILE"
    )]
    trust_file: PathBuf,
    #[arg(long, default_value = "demo", env = "SHAKA_TENANT")]
    tenant: String,
    #[arg(long, default_value = "operator", env = "SHAKA_OPERATOR")]
    operator: String,
    #[arg(long, default_value = "operator", env = "SHAKA_ROLE")]
    role: String,
    #[arg(long, default_value = "development", env = "SHAKA_ENVIRONMENT")]
    environment: String,
    #[arg(long, default_value_t = false, env = "SHAKA_JSON_LOGS")]
    json_logs: bool,
    #[arg(
        long,
        default_value = "local",
        env = "SHAKA_MODEL_PROVIDER",
        help = "local ou openai-compatible"
    )]
    provider: String,
    #[arg(
        long,
        env = "SHAKA_MODEL_ENDPOINT",
        default_value = "https://api.openai.com/v1/chat/completions"
    )]
    endpoint: String,
    #[arg(long, env = "SHAKA_MODEL_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    #[arg(long, env = "SHAKA_MODEL", default_value = "gpt-4o-mini")]
    model: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run(RunArgs),
    Serve(ServeArgs),
    Memory(MemoryArgs),
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    SandboxDemo,
    Doctor,
    Backup {
        #[arg(long)]
        output: PathBuf,
    },
    Restore {
        #[arg(long)]
        input: PathBuf,
    },
    VerifyAudit,
    Config,
    Iam {
        #[command(subcommand)]
        command: IamCommand,
    },
}

#[derive(Debug, Subcommand)]
enum IamCommand {
    TenantCreate {
        tenant_id: String,
        display_name: String,
    },
    UserCreate {
        operator_id: String,
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        role: String,
    },
    TokenIssue {
        operator_id: String,
        #[arg(long)]
        expires_in_seconds: Option<i64>,
    },
    TokenRevoke {
        token_id: String,
    },
    LimitsSet {
        tenant_id: String,
        #[arg(long)]
        max_active: u32,
        #[arg(long)]
        max_daily: u32,
        #[arg(long)]
        max_cost_microunits: u64,
        #[arg(long)]
        requests: u32,
        #[arg(long)]
        window_seconds: u32,
    },
    List,
}

#[derive(Debug, Args)]
struct RunArgs {
    objective: String,
    #[arg(
        long,
        default_value_t = false,
        help = "Solicita efeitos externos; exige administrador e confirmação explícita"
    )]
    live: bool,
    #[arg(long, env = "SHAKA_CONFIRM_LIVE", default_value_t = false, hide = true)]
    confirm_live: bool,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:8080", env = "SHAKA_API_BIND")]
    bind: String,
    #[arg(long, default_value_t = 2, env = "SHAKA_API_WORKERS")]
    workers: usize,
    #[arg(long, env = "SHAKA_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    #[arg(long, env = "SHAKA_API_LIVE", default_value_t = false)]
    live: bool,
    #[arg(long, env = "SHAKA_CONFIRM_LIVE", default_value_t = false, hide = true)]
    confirm_live: bool,
}

#[derive(Debug, Args)]
struct MemoryArgs {
    #[command(subcommand)]
    command: MemoryCommand,
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    Recent {
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    Purge {
        #[arg(long, default_value_t = 30)]
        days: i64,
    },
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    List,
    Candidate {
        name: String,
        description: String,
        #[arg(long, value_delimiter = ',')]
        permissions: Vec<String>,
    },
    Approve {
        name: String,
        #[arg(required_unless_present = "artifact", conflicts_with = "artifact")]
        sha256: Option<String>,
        #[arg(long)]
        reason: String,
        #[arg(long, conflicts_with = "sha256")]
        artifact: Option<PathBuf>,
        #[arg(long, requires = "artifact")]
        key_id: Option<String>,
        #[arg(long, requires = "artifact")]
        signing_key_file: Option<PathBuf>,
    },
    TrustGenerate {
        key_id: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        description: String,
    },
    TrustAdd {
        key_id: String,
        public_key: String,
        #[arg(long)]
        description: String,
    },
    TrustRevoke {
        key_id: String,
    },
    TrustList,
    Revoke {
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.json_logs);
    match &cli.command {
        Command::Run(args) => run_agent(&cli, args).await,
        Command::Serve(args) => serve_agent(&cli, args).await,
        Command::Memory(args) => memory_command(&cli, args),
        Command::Skill { command } => skill_command(&cli, command),
        Command::SandboxDemo => sandbox_demo(),
        Command::Doctor => doctor(&cli),
        Command::Backup { output } => backup_command(&cli, output),
        Command::Restore { input } => restore_command(&cli, input),
        Command::VerifyAudit => verify_audit_command(&cli),
        Command::Config => config_command(&cli),
        Command::Iam { command } => iam_command(&cli, command),
    }
}

fn build_config(cli: &Cli, live: bool, live_confirmation: bool) -> Result<AppConfig> {
    Ok(AppConfig::from_values(
        &cli.environment,
        cli.database.clone(),
        cli.skills_file.clone(),
        &cli.tenant,
        &cli.operator,
        &cli.role,
        &cli.provider,
        &cli.endpoint,
        &cli.model,
        cli.api_key.clone(),
        live,
        live_confirmation,
        true,
    )?)
}

fn build_model(config: &AppConfig) -> Result<Arc<dyn shaka_orchestrator::AgentModel>> {
    let model: Arc<dyn shaka_orchestrator::AgentModel> = match config.model_provider {
        ModelProvider::Local => Arc::new(LocalModel),
        ModelProvider::OpenAiCompatible => {
            let key = config
                .api_key
                .clone()
                .context("SHAKA_MODEL_API_KEY é obrigatório para openai-compatible")?;
            Arc::new(OpenAiCompatibleModel::new(
                config.model_endpoint.clone(),
                key,
                config.model_name.clone(),
            )?)
        }
    };
    Ok(model)
}

fn authorize(config: &AppConfig, action: &Action) -> Result<()> {
    if config.principal.allows(action) {
        Ok(())
    } else {
        anyhow::bail!("não autorizado: papel não pode executar {action:?}")
    }
}

async fn run_agent(cli: &Cli, args: &RunArgs) -> Result<()> {
    let config = build_config(cli, args.live, args.confirm_live)?;
    let action = if args.live {
        Action::RunExternal
    } else {
        Action::RunReadOnly
    };
    authorize(&config, &action)?;
    let memory = Arc::new(open_memory(&config.database)?);
    let mut envelope = TaskEnvelope::new(
        config.tenant_id.clone(),
        config.principal.operator_id.clone(),
        args.objective.clone(),
    )?;
    envelope.dry_run = !config.live_requested;
    let model = build_model(&config)?;
    let mut tools = build_tool_registry(cli, &config)?;
    tools.register(Arc::new(EchoTool))?;
    let runtime = AgentRuntime::new(model, memory, tools);
    let result = runtime.run(envelope).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn serve_agent(cli: &Cli, args: &ServeArgs) -> Result<()> {
    let config = build_config(cli, args.live, args.confirm_live)?;
    authorize(&config, &Action::RunReadOnly)?;
    let memory = Arc::new(open_memory(&config.database)?);
    let model = build_model(&config)?;
    let mut tools = build_tool_registry(cli, &config)?;
    tools.register(Arc::new(EchoTool))?;
    let runtime = Arc::new(AgentRuntime::new(model, Arc::clone(&memory), tools));
    let audit = Arc::new(AuditLogger::new(memory));
    let queue = Arc::new(QueueStore::open(&config.database)?);
    let bind_addr: SocketAddr = args
        .bind
        .parse()
        .with_context(|| format!("bind HTTP inválido: {}", args.bind))?;
    let api_config = ApiConfig {
        bind_addr,
        worker_count: args.workers,
        api_key: args.api_key.clone(),
        live_enabled: config.live_requested,
        live_confirmation: config.live_confirmation,
        ..ApiConfig::default()
    };
    let state = ApiState::new(queue, runtime, audit, config.principal, api_config)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    serve_api(state)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn build_tool_registry(cli: &Cli, config: &AppConfig) -> Result<ToolRegistry> {
    let mut capabilities = vec![Capability::MemoryWrite];
    if matches!(&config.principal.role, Role::Administrator) {
        capabilities.push(Capability::CodeExecution);
    }
    let mut tools = ToolRegistry::with_capabilities(CapabilitySet(capabilities));
    let registry = SkillRegistry::load(&config.skills_file)?;
    let trust_store = TrustStore::load(&cli.trust_file)?;
    for artifact in registry
        .active_verified_artifacts(&trust_store)
        .with_context(|| "nenhuma skill ativa legada ou não confiável pode ser carregada")?
    {
        let skill_name = artifact.name.clone();
        let tool = WasmSkillTool::from_approved_artifact(artifact, &trust_store)
            .with_context(|| format!("carregando skill ativa {skill_name}"))?;
        tools.register(Arc::new(tool))?;
    }
    Ok(tools)
}

fn memory_command(cli: &Cli, args: &MemoryArgs) -> Result<()> {
    let config = build_config(cli, false, false)?;
    let store = open_memory(&config.database)?;
    match &args.command {
        MemoryCommand::Recent { limit } => {
            authorize(&config, &Action::RunReadOnly)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&store.recent_episodes(&config.tenant_id, *limit)?)?
            );
        }
        MemoryCommand::Purge { days } => {
            authorize(&config, &Action::PurgeMemory)?;
            let deleted = store.purge_older_than(&config.tenant_id, Duration::days(*days))?;
            println!("{{\"deleted\": {deleted}}}");
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn skill_command(cli: &Cli, command: &SkillCommand) -> Result<()> {
    let config = build_config(cli, false, false)?;
    let mut registry = SkillRegistry::load(&config.skills_file)?;
    let mut trust_store = TrustStore::load(&cli.trust_file)?;
    match command {
        SkillCommand::List => {
            authorize(&config, &Action::RunReadOnly)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&registry.active_skills())?
            );
        }
        SkillCommand::Candidate {
            name,
            description,
            permissions,
        } => {
            authorize(&config, &Action::CreateSkill)?;
            let manifest = SkillManifest {
                name: name.clone(),
                version: "0.1.0".to_owned(),
                description: description.clone(),
                permissions: permissions
                    .iter()
                    .map(String::as_str)
                    .filter_map(parse_capability)
                    .collect(),
                input_schema: json!({"type":"object"}),
                output_schema: json!({"type":"object"}),
                status: SkillStatus::Candidate,
                artifact_sha256: None,
            };
            registry.register_candidate(manifest)?;
            registry.save(&config.skills_file)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "skill.candidate",
                "success",
                BTreeMap::from([(String::from("skill"), name.clone())]),
            )?;
            println!("skill registrada como candidata; nenhuma execução foi ativada");
        }
        SkillCommand::Approve {
            name,
            sha256,
            reason,
            artifact,
            key_id,
            signing_key_file,
        } => {
            authorize(&config, &Action::ApproveSkill)?;
            let record = if let Some(artifact) = artifact {
                let key_id = key_id
                    .clone()
                    .context("--key-id é obrigatório ao aprovar um artifact")?;
                let signing_key_file = signing_key_file
                    .clone()
                    .context("--signing-key-file é obrigatório ao aprovar um artifact")?;
                let signing_key = load_signing_key(&signing_key_file).with_context(|| {
                    format!(
                        "carregando chave de assinatura {}",
                        signing_key_file.display()
                    )
                })?;
                registry.approve_signed_artifact(
                    name,
                    config.principal.operator_id.clone(),
                    artifact,
                    key_id,
                    &signing_key,
                    reason.clone(),
                )?
            } else {
                if key_id.is_some() || signing_key_file.is_some() {
                    bail!("--key-id e --signing-key-file exigem --artifact");
                }
                registry.approve(
                    name,
                    config.principal.operator_id.clone(),
                    sha256
                        .clone()
                        .context("sha256 é obrigatório quando artifact não é informado")?,
                    reason.clone(),
                )?
            };
            registry.save(&config.skills_file)?;
            let mut metadata = BTreeMap::from([
                (String::from("skill"), name.clone()),
                (
                    String::from("sha256"),
                    record
                        .approval
                        .as_ref()
                        .map(|approval| approval.artifact_sha256.clone())
                        .unwrap_or_default(),
                ),
            ]);
            if let Some(attestation) = record
                .approval
                .as_ref()
                .and_then(|approval| approval.attestation.as_ref())
            {
                metadata.insert(String::from("key_id"), attestation.key_id.clone());
                metadata.insert(String::from("protocol"), attestation.protocol.clone());
            }
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "skill.approve",
                "success",
                metadata,
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        SkillCommand::TrustGenerate {
            key_id,
            output,
            description,
        } => {
            authorize(&config, &Action::ApproveSkill)?;
            let signing_key = SigningKey::generate(&mut OsRng);
            save_signing_key(output, &signing_key)?;
            let trusted = trust_store.add_key(
                key_id,
                public_key_hex(&signing_key),
                description,
                config.principal.operator_id.clone(),
            )?;
            trust_store.save(&cli.trust_file)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "skill.trust_generate",
                "success",
                BTreeMap::from([(String::from("key_id"), trusted.key_id.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&trusted)?);
        }
        SkillCommand::TrustAdd {
            key_id,
            public_key,
            description,
        } => {
            authorize(&config, &Action::ApproveSkill)?;
            let trusted = trust_store.add_key(
                key_id,
                public_key,
                description,
                config.principal.operator_id.clone(),
            )?;
            trust_store.save(&cli.trust_file)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "skill.trust_add",
                "success",
                BTreeMap::from([(String::from("key_id"), trusted.key_id.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&trusted)?);
        }
        SkillCommand::TrustRevoke { key_id } => {
            authorize(&config, &Action::ApproveSkill)?;
            let trusted = trust_store.revoke_key(key_id)?;
            trust_store.save(&cli.trust_file)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "skill.trust_revoke",
                "success",
                BTreeMap::from([(String::from("key_id"), key_id.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&trusted)?);
        }
        SkillCommand::TrustList => {
            authorize(&config, &Action::RunReadOnly)?;
            println!("{}", serde_json::to_string_pretty(&trust_store.list())?);
        }
        SkillCommand::Revoke { name } => {
            authorize(&config, &Action::RevokeSkill)?;
            let record = registry.revoke(name)?;
            registry.save(&config.skills_file)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "skill.revoke",
                "success",
                BTreeMap::from([(String::from("skill"), name.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
    }
    Ok(())
}

fn iam_command(cli: &Cli, command: &IamCommand) -> Result<()> {
    let config = build_config(cli, false, false)?;
    authorize(&config, &Action::ManageIam)?;
    let queue = QueueStore::open(&config.database)?;
    queue.bootstrap_principal(&config.principal)?;
    match command {
        IamCommand::TenantCreate {
            tenant_id,
            display_name,
        } => {
            let tenant_id = shaka_core::TenantId::new(tenant_id.clone())?;
            let record = queue.create_tenant(&tenant_id, display_name)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "iam.tenant.create",
                "success",
                BTreeMap::from([(String::from("tenant_id"), tenant_id.0.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        IamCommand::UserCreate {
            operator_id,
            tenant,
            role,
        } => {
            let operator_id = shaka_core::OperatorId::new(operator_id.clone())?;
            let tenant_id = shaka_core::TenantId::new(tenant.clone())?;
            let role =
                parse_role(role).context("role deve ser operator, reviewer ou administrator")?;
            let record = queue.create_user(&operator_id, &tenant_id, &role)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "iam.user.create",
                "success",
                BTreeMap::from([(String::from("operator_id"), operator_id.0.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        IamCommand::TokenIssue {
            operator_id,
            expires_in_seconds,
        } => {
            let operator_id = shaka_core::OperatorId::new(operator_id.clone())?;
            let expires_at =
                expires_in_seconds.map(|seconds| chrono::Utc::now() + Duration::seconds(seconds));
            let issue = queue.issue_token(&operator_id, expires_at)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "iam.token.issue",
                "success",
                BTreeMap::from([(String::from("token_id"), issue.token_id.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&issue)?);
        }
        IamCommand::TokenRevoke { token_id } => {
            queue.revoke_token(token_id)?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "iam.token.revoke",
                "success",
                BTreeMap::from([(String::from("token_id"), token_id.clone())]),
            )?;
            println!("{{\"revoked\":true}}");
        }
        IamCommand::LimitsSet {
            tenant_id,
            max_active,
            max_daily,
            max_cost_microunits,
            requests,
            window_seconds,
        } => {
            let tenant_id = shaka_core::TenantId::new(tenant_id.clone())?;
            let limits = queue.set_limits(TenantLimits {
                tenant_id,
                max_active_tasks: *max_active,
                max_daily_tasks: *max_daily,
                max_daily_cost_microunits: *max_cost_microunits,
                requests_per_window: *requests,
                window_seconds: *window_seconds,
            })?;
            append_control_audit(
                &open_memory(&config.database)?,
                &config.principal,
                "iam.limits.set",
                "success",
                BTreeMap::from([(String::from("tenant_id"), limits.tenant_id.0.clone())]),
            )?;
            println!("{}", serde_json::to_string_pretty(&limits)?);
        }
        IamCommand::List => {
            println!("{}", serde_json::to_string_pretty(&queue.list_tenants()?)?);
        }
    }
    Ok(())
}

fn doctor(cli: &Cli) -> Result<()> {
    let config = build_config(cli, false, false);
    let mut report = serde_json::Map::new();
    match config {
        Ok(config) => {
            let store = open_memory(&config.database)?;
            let integrity = store.verify_integrity()?;
            let audit = store.verify_audit_chain(&config.tenant_id)?;
            report.insert("config_valid".to_owned(), Value::Bool(true));
            report.insert("database_integrity".to_owned(), Value::Bool(integrity));
            report.insert("audit_chain".to_owned(), serde_json::to_value(audit)?);
            report.insert(
                "skills_file_exists".to_owned(),
                Value::Bool(config.skills_file.exists()),
            );
            report.insert(
                "status".to_owned(),
                Value::String(if integrity { "ready" } else { "failed" }.to_owned()),
            );
        }
        Err(error) => {
            report.insert("config_valid".to_owned(), Value::Bool(false));
            report.insert("status".to_owned(), Value::String("failed".to_owned()));
            report.insert("error".to_owned(), Value::String(error.to_string()));
        }
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn backup_command(cli: &Cli, output: &Path) -> Result<()> {
    let config = build_config(cli, false, false)?;
    authorize(&config, &Action::Backup)?;
    let store = open_memory(&config.database)?;
    if !store.verify_integrity()? {
        anyhow::bail!("a integridade do banco de origem falhou; backup bloqueado")
    }
    store.backup_to(output)?;
    println!("backup criado em {}", output.display());
    Ok(())
}

fn restore_command(cli: &Cli, input: &Path) -> Result<()> {
    let config = build_config(cli, false, false)?;
    authorize(&config, &Action::Restore)?;
    if !input.exists() {
        anyhow::bail!("arquivo de restore não encontrado: {}", input.display())
    }
    let store = open_memory(&config.database)?;
    store.restore_from(input)?;
    if !store.verify_integrity()? {
        anyhow::bail!("restore concluído, mas a integridade do banco falhou")
    }
    println!("restore concluído a partir de {}", input.display());
    Ok(())
}

fn verify_audit_command(cli: &Cli) -> Result<()> {
    let config = build_config(cli, false, false)?;
    authorize(&config, &Action::VerifyAudit)?;
    let store = open_memory(&config.database)?;
    let verification = store.verify_audit_chain(&config.tenant_id)?;
    println!("{}", serde_json::to_string_pretty(&verification)?);
    if !verification.valid {
        anyhow::bail!("cadeia de auditoria inválida")
    }
    Ok(())
}

fn config_command(cli: &Cli) -> Result<()> {
    let config = build_config(cli, false, false)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&config.public_summary())?
    );
    Ok(())
}

fn append_control_audit(
    store: &MemoryStore,
    principal: &Principal,
    action: &str,
    outcome: &str,
    metadata: BTreeMap<String, String>,
) -> Result<()> {
    let event = AuditEvent::new(
        None,
        principal.tenant_id.clone(),
        principal.operator_id.0.clone(),
        action,
        outcome,
        metadata,
        None,
    );
    store.append_audit_event(&event)?;
    Ok(())
}

fn sandbox_demo() -> Result<()> {
    let executor = WasmExecutor::new()?;
    let wasm = wat::parse_str(r#"(module (func (export "run") (result i32) i32.const 42))"#)?;
    let result = executor.execute(&wasm, &[], &SandboxPolicy::default())?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn open_memory(path: &Path) -> Result<MemoryStore> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("criando {}", parent.display()))?;
    }
    Ok(MemoryStore::open(path)?)
}

fn parse_role(value: &str) -> Option<Role> {
    match value {
        "operator" => Some(Role::Operator),
        "reviewer" => Some(Role::Reviewer),
        "administrator" => Some(Role::Administrator),
        _ => None,
    }
}

fn parse_capability(value: &str) -> Option<Capability> {
    match value {
        "network" => Some(Capability::Network),
        "filesystem-read" => Some(Capability::FilesystemRead),
        "filesystem-write" => Some(Capability::FilesystemWrite),
        "code-execution" => Some(Capability::CodeExecution),
        "external-messaging" => Some(Capability::ExternalMessaging),
        "memory-write" => Some(Capability::MemoryWrite),
        _ => None,
    }
}
