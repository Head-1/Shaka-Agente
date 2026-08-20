# Changelog

Todas as mudanças relevantes do Shaka serão registradas neste arquivo. O formato segue [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e o versionamento segue [Semantic Versioning](https://semver.org/lang/pt-BR/).

## [Unreleased]

### Planejado

- Busca semântica com embeddings e recuperação híbrida.
- Subagentes paralelos com DAG, orçamento por filho, cancelamento e falha parcial.
- Build sandbox separado para código gerado e verificação de dependências.
- Adaptadores de mensageria com autenticação de webhook e idempotência.
- Pesquisa web com conteúdo marcado como não confiável e mitigação de SSRF.
- IAM remoto, ABAC, cofre de segredos, multi-tenancy forte e métricas exportáveis.
- Backup remoto automatizado, migrações formais, RPO/RTO e testes de recuperação em infraestrutura-alvo.

## [0.4.0] - 2026-08-20

### Adicionado

- Aprovações de skills WASM assinadas com Ed25519, usando atestação canônica vinculada ao hash SHA-256 exato do artefato.
- `TrustStore` persistente com inclusão, revogação, verificação fail-closed, gravação atômica e permissões restritas no Unix.
- Comandos CLI `skill trust-generate`, `skill trust-add`, `skill trust-revoke` e `skill trust-list`.
- Fluxo `skill approve` com `--key-id` e `--signing-key-file` obrigatórios para aprovações executáveis; aprovações legadas permanecem somente para compatibilidade histórica.
- `WasmSkillTool` revalida hash e assinatura antes de instanciar qualquer módulo WASM.

### Segurança e supply chain

- Todos os gates Cargo da CI usam `--locked`, o toolchain está fixado e a política de workflows valida referências, permissões e invariantes de supply chain.
- Release preparada para attestations OIDC de binários, SBOM e imagens de container por `actions/attest@v4`, com execução condicionada à visibilidade pública do repositório.
- Dependabot configurado para atualizações semanais de dependências Cargo e GitHub Actions.
- Harness de fuzzing para verificação adversarial de atestações Ed25519, executado manualmente com nightly Rust datada e limite de tempo.

## [0.3.0] - 2026-08-20

### Adicionado

- Loop multi-turno limitado por orçamento de passos, deadline global e timeout por ferramenta.
- Auditoria de tool calls, falhas do modelo e resultados sanitizados, com revalidação de capabilities em cada chamada.
- Execução de skills WASM somente após aprovação por hash SHA-256, validação de schema e verificação do artefato ativo.
- Secret scan determinístico, SBOM CycloneDX, checksums de release, smoke test executado via `bash` e container runtime não-root.
- Backup/restore com verificação de integridade e permissões restritas no Unix.

### Segurança

- Skills revogadas são excluídas do conjunto executável mesmo que permaneçam no histórico de aprovação.
- O sandbox continua sem WASI, rede, filesystem ou imports do host; IAM remoto, assinatura criptográfica e observabilidade externa permanecem pendentes.

## [0.2.0] - 2026-08-20

### Adicionado

- Crate `shaka-config` com ambientes development, staging e production, provedor, endpoint, credencial, auditoria e confirmação de modo live.
- RBAC mínimo com `operator`, `reviewer` e `administrator`, aplicado no host para execução, skills, backup, restore e auditoria.
- Validação real de entradas de ferramentas com JSON Schema, incluindo campos obrigatórios e tipos.
- Redaction de padrões comuns de API key, token, senha, segredo e Bearer em objetivos, respostas e memória episódica.
- Cadeia de hashes de auditoria por tenant, com verificação via `verify-audit` e validação de integridade do evento.
- Backup online e restore do SQLite por API de backup do rusqlite, além de `PRAGMA integrity_check`.
- Isolamento testado entre tenants e configuração WAL/busy timeout para operação local concorrente.
- Gravação atômica do catálogo de skills com permissões restritas; aprovação opcional calculada a partir do arquivo real.
- Orçamento de custo, latência medida e auditoria automática de execuções do agente.
- Wasmtime atualizado para 47.0.3 após auditoria RustSec da versão anterior, mantendo fuel, epoch interruption e deny-by-default.
- Comandos CLI `doctor`, `backup`, `restore`, `verify-audit` e `config`.
- Healthcheck no Dockerfile e `cargo audit` como gate obrigatório da CI.
- Documento `PRODUCTION_RELEASE.md` e registro `DEPENDENCY_SECURITY_VALIDATION.md`.

### Segurança

- Nenhuma promoção automática de skill foi adicionada.
- Modo live exige administrador e confirmação explícita; a release não registra ferramenta de mensageria real.
- Ambiente production bloqueia modelo local, endpoint sem HTTPS, ausência de API key e auditoria desabilitada.
- O sandbox continua sem WASI, rede, filesystem ou imports do host.

## [0.1.0] - 2026-08-20

### Adicionado

- Workspace Rust 2024 com crates separados por responsabilidade.
- Contratos centrais de tarefas, tenants, operadores, ferramentas, capabilities, skills e auditoria.
- Memória de trabalho com TTL, memória episódica e registros semânticos em SQLite.
- Expurgo explícito da memória episódica por política de retenção.
- Catálogo persistente de skills com estados candidata, ativa e revogada.
- Aprovação de skill com operador, hash SHA-256 e justificativa obrigatória.
- Sandbox Wasmtime com fuel, sem WASI e com rejeição de imports do host.
- Orquestrador com `LocalModel`, adaptador OpenAI-compatível, function calling mediado pelo host e dry-run.
- CLI para executar tarefas, consultar memória, operar skills e executar demonstração do sandbox.
- Tracing estruturado e persistência de eventos de auditoria.
- Testes unitários para núcleo, memória, skills, sandbox e orquestrador.
- Documentação operacional, arquitetura, ADRs, segurança, contratos e estratégia de testes.

### Segurança

- Efeitos colaterais não são executados por padrão.
- Código WASM não pode importar funções do host no MVP.
- Capacidades são deny-by-default no catálogo de ferramentas.
- Segredos do provedor de modelo são lidos somente de variáveis de ambiente.
- Não existe fluxo de auto-promoção de skills.
