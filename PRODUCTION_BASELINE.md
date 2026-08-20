# Baseline de produção do Shaka

## Diagnóstico do MVP

O MVP atual compila e possui testes funcionais para memória SQLite, governança básica de skills, dry-run, orquestração local e sandbox Wasmtime. Ele ainda não deve ser chamado de produção porque faltam controles de identidade, verificação da cadeia de auditoria, backup/restauração operáveis, configuração validada, política explícita de ambiente, redaction de segredos, health checks, testes de isolamento e uma separação rigorosa entre artefato candidato e artefato executável.

A validação inicial foi executada em 20 de agosto de 2026 com Rust 1.97.1. O workspace passou por `cargo check --workspace` e `cargo test --workspace`; o resultado foi de 12 testes unitários, todos aprovados no baseline anterior.

## Escopo da release de produção candidata

Esta evolução implementará no próprio repositório:

| Área | Entrega |
|---|---|
| Configuração | Configuração tipada, validação de ambiente, modo development/staging/production e proibição de defaults inseguros em produção. |
| Identidade e autorização | RBAC mínimo para operador, revisor e administrador; aprovação de skill vinculada a identidade e capacidade. |
| Auditoria | Cadeia de hashes por tenant, verificação de integridade e comandos de diagnóstico. |
| Memória | Backup consistente, restauração para novo arquivo, integridade SQLite, retenção segura e testes de isolamento por tenant. |
| Skills | Manifesto com hash, estado persistente, aprovação revogável e separação explícita entre candidata e ativa. |
| Runtime | Limites de tarefas, redaction de dados sensíveis, validação de respostas e política de efeitos colaterais. |
| Operação | Comandos `doctor`, `backup`, `restore`, `verify-audit`, health/readiness e encerramento previsível. |
| Supply chain | Lockfile obrigatório, cargo-deny/audit quando disponível, imagem não-root, SBOM/inventário e CI com gates de segurança. |
| Documentação | Atualização de arquitetura, threat model, runbook, handoff, contratos, testes e critérios de release. |

## Fora do alcance desta release

Não serão habilitados sem infraestrutura e validação específicas: WhatsApp/Telegram/Discord/Slack reais, pesquisa web, multi-tenancy distribuído, cofre externo obrigatório, Postgres gerenciado, subagentes paralelos, execução WASI com rede ou filesystem, geração automática de código de skill em produção e deploy público 24/7.

Essas capacidades podem ser adicionadas depois que os contratos, provedores, quotas, webhooks, proteção de dados e procedimentos de incidentes forem definidos. O resultado desta fase será uma **release de produção candidata para operação controlada**, não uma alegação de que qualquer implantação pública está automaticamente pronta sem configuração e revisão humana.

## Gates de release

A release só poderá ser considerada aprovada quando `fmt`, `check`, testes, Clippy, verificação de auditoria, backup/restore, testes adversariais e validação de configuração passarem. Qualquer caminho de auto-promoção de skill, execução de código no processo principal, default de segredo em produção ou falha de isolamento por tenant bloqueará a release independentemente da pontuação restante.
