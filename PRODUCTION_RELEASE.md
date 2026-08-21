# Shaka — Release de produção candidata

## Status

Esta release eleva o Shaka para a **v0.6.0, produção candidata para operação controlada e multiusuário local**. Além da configuração tipada, RBAC mínimo, validação JSON Schema real, redaction de credenciais, auditoria encadeada, backup/restore, verificação de integridade, catálogo de skills com escrita atômica, hash calculado de artefato e pipeline de dependências com `cargo audit` obrigatório, o código possui API REST persistente, sessões SQLite, fila priorizada, retry, cancelamento, idempotência e circuit breaker.

> O termo “produção candidata” é intencional. O repositório está endurecido e validado, mas uma implantação pública ainda depende de identidade externa, cofre de segredos, política de dados, provisionamento, alertas, backup fora da máquina e revisão humana do ambiente.

## Funcionalidades implementadas

| Área | Implementação |
|---|---|
| Configuração | `shaka-config` valida ambiente, provedor, endpoint, papel, credencial, auditoria e modo ao vivo. |
| RBAC | `Operator`, `Reviewer` e `Administrator`, com ações separadas para execução, skills, backup, restore e auditoria. |
| Produção | Ambiente `production` exige provedor externo, chave, HTTPS e confirmação explícita para live. |
| Contratos | `jsonschema` valida entradas de ferramentas, incluindo campos obrigatórios e tipos. |
| Redaction | Respostas, objetivos e registros de execução removem padrões de API key, token, senha e Bearer. |
| Auditoria | Eventos são encadeados por hash por tenant; tool calls e falhas de execução também são registrados e podem ser verificados com `verify-audit`. |
| Dados | SQLite usa WAL e busy timeout; há integrity check, backup online, restore com verificação da origem e backup local com permissão restrita. |
| Skills | Catálogo salvo atomicamente; arquivo recebe permissão restrita; aprovação por artefato calcula SHA-256 real; somente skills ativas e hash-verificadas entram no runtime WASM. |
| Sandbox | Wasmtime 47.0.3, sem imports de host, sem WASI, com fuel e interrupção por epoch. |
| Supply chain | `Cargo.lock` atualizado; `cargo audit`, secret scan e SBOM CycloneDX fazem parte da validação/release. Assinatura de artefatos e pinagem de actions ainda são pendências. |
| Operação | CLI inclui `doctor`, `backup`, `restore`, `verify-audit`, `config`, `serve` e comandos IAM administrativos; API possui workers, leases, retry, cancelamento cooperativo e autenticação por request. |
| IAM | Tenants, usuários, tokens bearer hash-only, revogação, expiração e principal resolvido por request. |
| Quotas/rate limit | Quotas persistentes por tenant e rate limits por tenant e operador, com `429` e `Retry-After`. |
| Fila/API | `shaka-queue` persiste sessões, tarefas, idempotência, prioridade, leases e circuit breaker; `shaka-api` expõe REST v1. |
| Imagem | Dockerfile compila com lockfile, executa como usuário não-root, expõe 8080, inicia `serve` e possui healthcheck. |

## Papéis

| Papel | Permissões principais |
|---|---|
| `operator` | Tarefas somente leitura e criação de candidata. |
| `reviewer` | Tarefas somente leitura, aprovação de skills, backup e verificação de auditoria. |
| `administrator` | Todas as ações do host, incluindo live, revoke e restore. |

A v0.6.0 adiciona identidade persistente local e tokens bearer opacos; a compatibilidade por configuração local permanece disponível para loopback e API key estática. Antes de exposição pública, substituir ou complementar esse mecanismo por OIDC/OAuth2, rotação de chaves, sessão assinada e integração com um provedor IAM.

## Configuração segura

Iniciar a API local, limitada a loopback:

```bash
cargo run -- serve --bind 127.0.0.1:8080 --workers 2
curl -fsS http://127.0.0.1:8080/healthz
```

Em desenvolvimento:

```bash
export SHAKA_ENVIRONMENT=development
export SHAKA_ROLE=operator
cargo run -- config
cargo run -- doctor
cargo run -- run "validar o agente em modo local"
```

Em staging, o provedor externo e o endpoint podem ser usados, mas efeitos ao vivo exigem administrador e `SHAKA_CONFIRM_LIVE=true`.

Em production, a configuração precisa conter `SHAKA_MODEL_PROVIDER=openai-compatible`, `SHAKA_MODEL_API_KEY`, endpoint HTTPS e `SHAKA_AUDIT_REQUIRED=true`. O modo live ainda exige `--live`, `SHAKA_CONFIRM_LIVE=true` e papel administrador. A release não registra adaptador de mensageria externa, portanto confirmar live não cria por si só um canal de envio.

## Operação de dados

Verificar prontidão:

```bash
cargo run -- doctor
```

Criar backup consistente:

```bash
cargo run -- backup --output backups/shaka-$(date -u +%Y%m%dT%H%M%SZ).db
```

Restaurar para o banco configurado:

```bash
cargo run -- restore --input backups/shaka-arquivo.db
```

Verificar a cadeia de auditoria:

```bash
cargo run -- verify-audit
```

O backup deve ser transferido para armazenamento externo criptografado. O arquivo local não é uma estratégia suficiente de recuperação contra perda da máquina, ransomware ou corrupção de filesystem.

## Gates de publicação

A publicação exige que os comandos abaixo passem sem exceções:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings -A missing_docs -A clippy::missing_errors_doc
cargo audit
```

Além desses comandos, o responsável precisa validar no ambiente-alvo: injeção de segredos, permissões do container, rotação de chave, restauração real, alertas, política de retenção, classificação de dados e acesso de operadores.

## Limitações que continuam bloqueando produção pública

A release ainda não implementa IAM remoto OIDC/OAuth2, multi-tenancy distribuído, quotas e rate limits distribuídos, subagentes, webhooks, mensageria, pesquisa web, controles SSRF de crawler, cofre externo, métricas Prometheus, tracing OTLP, backup remoto automatizado ou geração automática de skills com pipeline completo de build/teste sandbox. A fila local, o SBOM, o secret scan, a assinatura de skills e a proveniência do container fazem parte da v0.5.0/v0.4.0; a operação pública ainda exige infraestrutura de borda e políticas adicionais.

Esses itens são próximos incrementos, não devem ser simulados por configuração. A ausência deliberada desses adaptadores reduz a superfície de ataque da release atual.
