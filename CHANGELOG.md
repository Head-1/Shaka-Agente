# Changelog

Todas as mudanças relevantes do Shaka serão registradas neste arquivo. O formato segue [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e o versionamento segue [Semantic Versioning](https://semver.org/lang/pt-BR/).

## [Unreleased]

### Documentação e validação

- Consolidado o status dos BR-01 a BR-06 em [`docs/BACKLOG_STATUS.md`](docs/BACKLOG_STATUS.md), com evidências pré/pós, limites residuais e referência ao merge commit validado.
- Atualizada a estratégia de testes e o runbook para refletir o executor repository-first, os probes de lifecycle/crash-recovery e a validação equivalente em CI, sandbox e VM.

### Planejado

- Busca semântica com embeddings e recuperação híbrida.
- Subagentes paralelos com DAG, orçamento por filho, cancelamento e falha parcial.
- Build sandbox separado para código gerado e verificação de dependências.
- Adaptadores de mensageria com autenticação de webhook e idempotência.
- Pesquisa web com conteúdo marcado como não confiável e mitigação de SSRF.
- IAM remoto, ABAC, cofre de segredos, multi-tenancy forte e métricas exportáveis.
- Backup remoto automatizado, migrações formais, RPO/RTO e testes de recuperação em infraestrutura-alvo.

## [0.8.2] - 2026-08-22

### Segurança e release

- Adicionado preflight fail-closed para validar tag SemVer, versão do workspace, metadata do Cargo, `Cargo.lock` e entrada correspondente no changelog antes de qualquer build ou upload de release.
- Adicionados testes automatizados para versões válidas, tags divergentes, metadata divergente, lockfile divergente, changelog ausente e validação sem tag em contexto não-release.
- Atualizadas `actions/cache` e `actions/upload-artifact` para versões compatíveis com Node.js 24, mantendo os gates, permissões e triggers existentes.

## [0.8.1] - 2026-08-22

### Manutenção

- README alinhado à release v0.8.0, à CLI atual e aos limites operacionais de `dry_run`, loopback e planos `live` bloqueados.
- `actions/checkout` atualizado para v5 nos workflows de CI, release e fuzzing, removendo o warning evitável de runtime Node.js 20.
- Contratos públicos do `PlanVerifier` documentados, incluindo códigos de violação, relatórios bounded e limites de proteção contra planos patológicos.
- Validação pós-release repetida com formato, compilação, testes, Clippy, secret scan, política de workflows, auditoria de dependências e smoke test de produção.

## [0.8.0] - 2026-08-22


### v0.8.0 — Etapa 6: exposição HTTP/CLI e inspeção operacional de planos

- Exposição dos planos por HTTP com criação, detalhe, validação preflight, aprovação, retomada, cancelamento e consulta de checkpoints.
- Criação de planos restrita ao principal efetivo, tenant derivado da autenticação e modo `dry_run`; planos `live` continuam bloqueados na v0.8.0.
- Relatório de inspeção somente leitura com status `valid`, `requires_approval` ou `invalid`, verificação de digest, cadeia de transições, estado reduzido, checkpoints e limites bounded.
- Idempotência determinística de aprovações por `Idempotency-Key`, com conflito fail-closed para intenção divergente e replay estável mesmo quando a expiração é recalculada.
- Comandos administrativos `shaka plan validate|show|approve|resume|cancel|verify|checkpoints`, com parsing fechado de UUID, decisão, evidência e demais argumentos.
- Auditoria de todas as operações administrativas de plano sem persistir payloads, segredos ou conteúdo livre; respostas HTTP permanecem bounded e tenant-isolated.
- Testes HTTP do ciclo de plano, aprovação/idempotência, submissão planejada, cancelamento e checkpoints; testes de parsing da CLI; workspace, Clippy e smoke de produção aprovados.

### v0.8.0 — Etapa 7: crash/recovery e fronteiras ambíguas

- Recovery de lease planejada permanece fail-closed: a fronteira ativa vira `unknown`, a task não recebe retry cego e a retomada exige resolução humana.
- Recovery repetido após reinicialização é idempotente, sem duplicar checkpoints ou reprocessar uma task já encerrada.
- Snapshot divergente da cadeia do reducer entra em quarentena `unknown`, com checkpoint de recovery único e sem efeitos cumulativos em reinicializações seguintes.
- Replay do worker após commit terminal retorna o mesmo outcome persistido e não duplica transições, checkpoints ou efeitos de plano.
- Três novos testes de crash/recovery e replay; 23 testes do `shaka-queue` aprovados.

### v0.8.0 — Etapa 5: aprovações, compensações e resolução de unknown

- `approve_plan` com autorização RBAC, separação de funções, escopo por plano/etapa, digest/revisão vinculados e idempotência.
- Decisão de rejeição terminal e revalidação de aprovação global em cada claim; expiração/revogação não liberam novas etapas.
- Migração `plan_store` v3 com `idempotency_key` de aprovação e compatibilidade com bancos da Etapa 3.
- Resolução humana tipada de `unknown` para `resume`, `compensate` ou `cancel`, com digest de evidência e checkpoints de governança.
- Cancelamento planejado cooperativo, bloqueado enquanto houver fronteira ativa ou ambígua.
- Compensações limitadas ao subgrafo estático declarado, com claim filtrado, checkpoint próprio e sem loop ou retry autônomo.
- Falha de compensação retorna a `unknown`; nova tentativa exige nova resolução humana; plano compensado nunca reporta sucesso da operação original.
- 20 testes do `shaka-queue`, workspace, Clippy e smoke de produção aprovados.

### v0.8.0 — Etapa 4: integração QueueStore/worker do Plan Engine

- Tasks governadas agora carregam referência imutável ao plano: `plan_id`, revisão, digest e etapa locada.
- Migração idempotente do `api_tasks` para colunas opcionais de plano, com schema `plan_store` v2 e índice de claim planejado.
- Admissão preflight fail-closed com isolamento por tenant, vínculo à task declarada e fingerprint de idempotência incluindo plano e digest.
- Tasks planejadas live são bloqueadas até existir executor tipado; a Etapa 4 mantém o caminho seguro em `dry_run`.
- Claim transacional e bounded com seleção determinística, dependências, condições, aprovação, capabilities, circuit breaker e checkpoint `before_step`.
- Finalização transacional com pós-condições, progresso multi-etapa, retry bounded por etapa e estado terminal do plano.
- Lease planejada expirada não sofre retry cego: plano e etapa são marcados como `unknown` com checkpoint de recuperação.
- Worker expõe ao verificador somente facts host-side e o runtime fornece cópia das capabilities concedidas.
- Testes de admissão, claim, conclusão, recuperação e compatibilidade direta; workspace, Clippy e smoke aprovados.

### v0.8.0 — Etapa 3: persistência SQLite do Plan Engine

- Migração SQLite idempotente e versionada para planos, etapas, checkpoints, aprovações, transições e compensações.
- `plan_store` com persistência append-only por revisão, isolamento por tenant e validação de digest canônico.
- Transições do reducer com sequência, idempotência e cadeia SHA-256 vinculada por `previous_hash`/`event_hash`.
- Checkpoints monotônicos de preflight, execução e recuperação com digest de estado validado.
- Retomada após reinício com reconciliação fail-closed; inconsistências e fronteiras ativas são convertidas em `unknown` sem retry automático.
- Aprovações persistidas somente após revalidação de tenant, revisão, digest, escopo, expiração, revogação e separação de funções.
- 6 testes unitários de persistência, integridade, isolamento e recovery; workspace completo e Clippy aprovados.

### v0.8.0 — Etapa 2: verificador determinístico

- `PlanVerifier` público no `shaka-core` com fases `preflight`, `step_ready` e `post_step`.
- Relatório bounded com estados `valid`, `requires_approval` e `invalid`, códigos de violação estáveis e detalhes sem payload.
- Verificação fail-closed de digest, estrutura, terminalidade, limites, referências, dependências e condições.
- Avaliação allowlisted de tarefa, etapas, capabilities, circuito, orçamento, artefatos, idempotência e digest de estado.
- Aprovação global ou por etapa revalidada com tenant, revisão, digest, papel, expiração e revogação.
- Limite configurável de violações para evitar relatórios patológicos.
- Testes de digest adulterado, dependências, condições, aprovação, pós-condições, contexto ausente, alvo inexistente e bounded report.

### v0.8.0 — Etapa 1: contratos do Plan Engine governado

- `PlanId`, `PlanStepId`, `PlanSpec` e `PlanStep` adicionados ao `shaka-core`.
- Modos `dry_run`/`live`, riscos `read_only`/`mutation`/`external_effect`/`irreversible` e níveis de aprovação humana tipados.
- Estados de plano e etapa com transições fail-closed, incluindo `unknown` para resultados ambíguos após crash ou timeout.
- Condições allowlisted e ações tipadas, sem expressões arbitrárias ou autoridade implícita do modelo.
- Digest SHA-256 canônico do plano e aprovação vinculada a tenant, revisão, etapa, digest, papel, expiração e revogação.
- Separação de funções: o operador proponente não pode aprovar o próprio plano.
- Verificação estrutural de IDs, dependências, ciclos, risco, aprovação, retries e referências de compensação.
- Testes unitários para dry-run padrão, digest verificável, ciclo/dependência inválida, transições e aprovação segura.

### v0.7.0 — Etapa A: observabilidade governada

- Fachada `Telemetry` com schema interno `shaka.observability` v0.7 e perfil `shaka.genai.v0.7`.
- `CorrelationContext` com request ID validado e referências opcionais de trace, span, tarefa, sessão e tenant redacted.
- `Redactor` central para texto, metadata e JSON recursivo, com limites de profundidade/itens/tamanho e captura de conteúdo desabilitada por padrão.
- `AuditLogger` passa a aplicar redaction central antes de persistir metadata textual ou JSON.
- Testes unitários de redaction, correlação, schema, limites e rejeição de captura de conteúdo.
- Middleware HTTP com `x-request-id` validado, geração de UUID para entrada inválida e propagação do ID efetivo na resposta.
- Propagação segura de `traceparent` W3C para spans internos, com rejeição de versões/IDs inválidos e sem efeito sobre autorização.
- Spans de servidor HTTP e spans filhos por operação de health, sessão e tarefa, usando rotas normalizadas e referências técnicas.
- Auditoria de API enriquecida com request/trace correlation IDs redacted, sem payload ou segredo.

### Segurança e governança

- Redactor endurecido contra `Authorization: Bearer`, chaves compostas de conteúdo (`prompt_text`, `model_input`, `response_content`) e truncamento que ultrapassava o limite configurado.
- Falha interna da expressão de redaction agora degrada para marcador seguro, nunca para o texto original.
- Tenant references em `CorrelationContext` são pseudonimizadas com prefixo SHA-256 truncado; tokens, objetivos e conteúdo GenAI não entram nos spans.
- Spans `queue.admission`, `queue.claim`, `queue.finish`, `queue.lease.recover`, `queue.circuit.*` e `worker.task.process` para correlacionar admissão, leases, tentativas, retries e estados do circuito.
- Taxonomias de outcome e erro de baixa cardinalidade para `created`, `existing`, `claimed`, `empty`, `succeeded`, `retry_scheduled`, `failed`, `cancelled`, `rate_limited` e classes equivalentes.
- Registro de worker, attempt, retryability, retry delay, lease state, circuit state e quantidade de leases recuperados sem incluir payloads, mensagens livres ou IDs como labels métricas.

## [0.6.0] - 2026-08-21

### Adicionado

- IAM persistente local com tenants, usuários, papéis, tokens bearer opacos e revogação/expiração.
- Resolução de principal por request na API, sem confiar em `tenant_id`, `operator_id` ou `role` enviados pelo cliente.
- Comandos administrativos `iam tenant`, `iam user`, `iam token`, `iam limits` e `iam list` protegidos por `ManageIam`.
- Quotas persistentes por tenant para tarefas ativas, volume diário e custo diário estimado.
- Rate limits transacionais por tenant e operador, com resposta HTTP `429` e `Retry-After`.
- Isolamento de sessões e tarefas por tenant e operador autenticado, incluindo submissão governada e idempotência.
- Desenho técnico `V0.6_DESIGN.md` com migração, contratos, compatibilidade e critérios de aceite.

### Segurança e governança

- Segredos bearer nunca são persistidos; somente SHA-256, `token_id` e prefixo operacional são armazenados.
- Tokens expirados, revogados, usuários inativos e tenants inativos falham com `401`.
- Ações IAM não possuem endpoint público e exigem papel administrador na CLI.
- A API mantém loopback e dry-run como defaults; OIDC/OAuth2 remoto, mensageria, web e escala distribuída permanecem fora do escopo.
- Auditoria de API e término de workers passa a usar a identidade efetiva do principal/tarefa.

## [0.5.0] - 2026-08-20

### Adicionado

- Crates `shaka-api` e `shaka-queue` para API HTTP/REST persistente, sessões SQLite e fila de tarefas priorizada.
- Endpoints `GET /healthz`, sessões, submissão idempotente por `Idempotency-Key`, consulta de tarefas e cancelamento.
- Persistência de leases, recuperação de tarefas após reinicialização, limites de tentativas e backoff exponencial saturante.
- Circuit breaker persistente com estados `closed`, `open` e `half_open` para impedir falhas em cascata.
- Cancelamento cooperativo integrado ao `AgentRuntime`, com auditoria e redaction mantidas no host.
- Subcomando CLI `serve` com bind local por padrão, workers configuráveis e autenticação Bearer opcional para binds não locais.
- Documento `V0.5_DESIGN.md` com contratos, transições, critérios de aceite, riscos e limites deliberados.

### Segurança e governança

- O cliente não pode escolher tenant, operador ou papel; a API usa o principal local validado pelo `shaka-config`.
- Tarefas permanecem em dry-run por padrão; modo live exige configuração explícita, confirmação e papel administrador.
- Repetição de uma chave de idempotência com payload diferente é rejeitada com conflito, evitando reuso ambíguo.
- Objetivos, resultados e erros de fila são limitados e redacted antes de persistência, logs e respostas.
- A API não aprova skills nem cria autoridade adicional para o modelo; TrustStore e aprovação humana da v0.4.0 permanecem obrigatórios.

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
