# API pública do Shaka v0.8.2

Este documento descreve o contrato operacional público do Shaka v0.8.2 para integração local e revisão de segurança. Ele cobre a API HTTP exposta pelo crate `shaka-api` e os tipos Rust que formam a fronteira entre o host, a fila persistente e o Plan Engine. A implementação continua sendo a fonte normativa para detalhes de serialização e validação; este guia não amplia permissões nem habilita execução `live`.

> **Regra de segurança:** uma proposta de tarefa ou plano não é uma autorização. A autoridade de tenant, operador e papel é resolvida pelo host autenticado, e não por campos fornecidos pelo modelo ou pelo cliente.

## 1. Estado e limites da API

A API foi projetada para operação local controlada. O `ApiConfig` usa `127.0.0.1:8080` por padrão, tarefas usam `dry_run` por padrão e planos submetidos pela API precisam ser criados em `dry_run`. Uma tentativa de execução real somente pode prosseguir se a configuração explícita estiver habilitada, houver confirmação de live e o principal autorizado possuir a capability correspondente; o caminho normal da v0.8.2 permanece bloqueado para efeitos externos.

A exposição fora do loopback exige autenticação por `SHAKA_API_KEY` ou token IAM ativo, HTTPS na borda, armazenamento protegido, backup e revisão específica de ameaça. Não use esta API como um serviço público diretamente exposto à internet.

| Propriedade | Contrato v0.8.2 |
| --- | --- |
| Bind padrão | `127.0.0.1:8080` |
| Transporte | HTTP local; HTTPS deve ser terminado em uma borda administrada quando necessário |
| Autenticação | Loopback sem chave pode usar o principal local; fora do loopback exige API key ou bearer token IAM |
| Identidade | Tenant, operador e papel vêm do principal autenticado no host |
| Execução padrão | `dry_run: true` |
| Idempotência | `Idempotency-Key` é obrigatório para submissões de tarefa, aprovações, retomadas e cancelamentos de planos |
| Persistência | SQLite com isolamento por tenant, leases, transições e checkpoints |
| Recuperação | Fronteiras ambíguas são tratadas como `unknown` e exigem resolução humana |
| Conteúdo sensível | Não enviar segredos, bearer tokens, prompts brutos ou payloads de ferramentas para logs ou evidências |

## 2. Inicialização e autenticação

Crie o estado da API no host com `ApiState::new`. A construção valida limites operacionais, inicializa o principal, registra o circuito persistente e rejeita bind não local sem uma forma de autenticação configurada. O roteador retornado por `ApiState::router` contém os endpoints HTTP descritos nas seções seguintes; `serve` faz o bind e inicia os workers configurados.

As chamadas autenticadas usam o cabeçalho padrão:

```http
Authorization: Bearer <token>
```

O valor bearer nunca é devolvido em respostas. Tokens IAM são armazenados apenas por hash no banco; o valor bruto retornado por emissão deve ser exibido e armazenado pelo operador uma única vez. Em operação local sem `api_key`, o loopback pode resolver o principal local configurado, mas essa exceção não deve ser usada como justificativa para bind em `0.0.0.0`.

### 2.1 Emissão de token IAM

A emissão administrativa exige um prazo explícito. O CLI rejeita a ausência de `--expires-in-seconds` e aceita somente valores entre `1` e `7.776.000` segundos, equivalentes a no máximo 90 dias:

```bash
shaka iam token-issue operator --expires-in-seconds 3600
```

A mesma regra é aplicada novamente no queue, portanto nenhum consumidor Rust pode emitir um token novo sem expiração, com prazo passado ou acima do teto. O campo `expires_at` da resposta de emissão é efetivamente preenchido; o segredo bruto deve ser tratado como credencial de uso único e nunca deve ser copiado para logs, tickets ou evidências.

O host deve rejeitar ou converter em erro qualquer principal inválido. O cliente não pode elevar seu próprio papel enviando `tenant_id`, `operator_id` ou `role` no corpo; esses campos são comparados com o principal autenticado ou ignorados quando não fazem parte do contrato do endpoint.

## 3. Health check

O endpoint de saúde não cria sessão nem executa tarefa:

```http
GET /healthz
```

Resposta conceitual:

```json
{
  "status": "ok",
  "version": "0.8.2",
  "queued_tasks": 0,
  "circuit": {
    "name": "agent-runtime",
    "state": "closed",
    "failure_count": 0,
    "opened_at": null,
    "next_probe_at": null
  }
}
```

O campo `version` é derivado da versão do pacote compilado. O circuito pode aparecer como `closed`, `open` ou `half_open`; `open` significa que novas chamadas ao runtime devem permanecer bloqueadas até a política de recuperação permitir uma sonda.

### Readiness operacional protegido

O endpoint de prontidão separa processo vivo de serviço apto a operar. Ele reutiliza a autenticação bearer já existente e executa apenas verificações read-only do banco da fila, do store de auditoria, da cadeia de auditoria do tenant autenticado e do circuito:

```http
GET /readyz
Authorization: Bearer <token>
```

Em modo local de loopback sem `SHAKA_API_KEY`, a política existente aceita o principal local sem header. Em bind não local, é obrigatório fornecer uma chave estática ou token IAM válido. Um bearer inválido é rejeitado com `401 Unauthorized`.

Resposta pronta:

```json
{
  "status": "ready",
  "version": "0.8.2",
  "database_integrity": true,
  "audit_chain": {
    "valid": true,
    "checked_events": 0,
    "failure_at": null
  },
  "queued_tasks": 0,
  "circuit": {
    "name": "agent-runtime",
    "state": "closed",
    "failure_count": 0,
    "opened_at": null,
    "next_probe_at": null
  }
}
```

O endpoint retorna `200 OK` somente quando a integridade, a cadeia de auditoria e o circuito estão prontos. Se qualquer sinal verificável não estiver pronto, retorna `503 Service Unavailable` com `status: "failed"`; falhas de acesso ao store retornam o erro interno sanitizado. O readiness não cria sessão, não enfileira tarefa, não executa ferramenta e não expõe conteúdo de negócio.

## 4. Sessões

Sessões agrupam tarefas para um principal e tenant. A sessão não concede autoridade adicional; ela apenas fornece um contexto persistente para submissões e consultas.

| Método | Endpoint | Sucesso | Comportamento |
| --- | --- | --- | --- |
| `POST` | `/v1/sessions` | `201 Created` | Cria uma sessão para o principal autenticado. O campo opcional `metadata` é limitado e persistido como JSON. |
| `GET` | `/v1/sessions/{session_id}` | `200 OK` | Consulta uma sessão do tenant autenticado e atualiza `last_seen_at`. |

Exemplo de criação:

```bash
curl --fail --silent -X POST \
  http://127.0.0.1:8080/v1/sessions \
  -H 'content-type: application/json' \
  -d '{"metadata":{"source":"manual"}}'
```

O identificador de sessão não permite atravessar tenants. Uma sessão inexistente ou pertencente a outro tenant deve resultar em `404 Not Found` sem revelar dados de outra identidade.

## 5. Tarefas

### 5.1 Submissão

```http
POST /v1/sessions/{session_id}/tasks
```

O corpo aceita `objective`, `priority`, `max_attempts`, `dry_run`, `budget` e, quando aplicável, a referência imutável a um plano:

```json
{
  "objective": "Descreva a política deny-by-default do Shaka",
  "priority": 5,
  "max_attempts": 3,
  "dry_run": true
}
```

A submissão exige `Idempotency-Key` não vazio. A chave é vinculada ao tenant e à impressão digital do payload. Repetir a mesma chave com o mesmo payload devolve a tarefa existente; reutilizar a chave com payload diferente produz conflito e não cria uma segunda execução.

A validação do host rejeita objetivo vazio ou maior que 32.000 caracteres, prioridade fora do limite, sessão de outro tenant e `task_id` sem referência planejada. O valor omitido de `dry_run` é `true`. O limite de tentativas padrão é `3`.

Quando `budget` é informado, o host aceita somente os seguintes intervalos: `max_steps` de `1` a `256`, `max_tool_calls` de `0` a `512`, `max_elapsed_ms` de `1` a `300000` e `max_cost_microunits` de `0` a `10000000`, sempre incluindo os limites superiores. Um campo fora desses intervalos é rejeitado com `400 Bad Request` antes da admissão na fila; os valores omitidos usam `ExecutionBudget::default()`.

Exemplo explícito:

```json
{
  "objective": "Executar uma leitura limitada",
  "dry_run": true,
  "budget": {
    "max_steps": 32,
    "max_tool_calls": 16,
    "max_elapsed_ms": 30000,
    "max_cost_microunits": 1000000
  }
}
```

Uma submissão nova retorna `202 Accepted`; uma submissão idempotente que encontra o registro existente retorna `200 OK`. O objeto de resposta contém o estado persistido da tarefa, incluindo `task_id`, `status`, tentativas, lease, resultado, erro e, quando aplicável, `plan_id`, `plan_revision`, `plan_digest` e `plan_step_id`.

### 5.2 Tarefas planejadas

Uma tarefa planejada precisa carregar todos os quatro elementos da referência: `task_id`, `plan_id`, `plan_revision` e `plan_digest`. A referência é validada contra o plano persistido e contra o tenant autenticado. A tarefa planejada deve permanecer em `dry_run` na v0.8.2; não existe atalho HTTP para transformar um plano em execução externa.

Exemplo de submissão planejada, depois que o plano e sua etapa foram criados pelo fluxo apropriado:

```json
{
  "objective": "Executar a etapa somente para validação",
  "dry_run": true,
  "task_id": "00000000-0000-0000-0000-000000000000",
  "plan_id": "00000000-0000-0000-0000-000000000000",
  "plan_revision": 1,
  "plan_digest": "sha256:..."
}
```

O exemplo usa identificadores ilustrativos. Não substitua o digest pelo valor de outro plano nem fabrique aprovação no cliente.

### 5.3 Consulta e cancelamento

| Método | Endpoint | Sucesso | Comportamento |
| --- | --- | --- | --- |
| `GET` | `/v1/tasks/{task_id}` | `200 OK` | Retorna a tarefa se ela pertencer ao tenant autenticado. |
| `DELETE` | `/v1/tasks/{task_id}` | `202 Accepted` ou `200 OK` | Registra cancelamento cooperativo e sinaliza o worker quando a tarefa está em execução. |

O cancelamento não apaga a tarefa nem desfaz efeitos já confirmados. Se a tarefa ainda estiver em processamento, o estado pode ser `cancel_requested` até que o worker alcance um ponto seguro; o cliente não deve interpretar esse estado como sucesso ou cancelamento terminal.

## 6. Plan Engine HTTP

A API expõe o Plan Engine por endpoints aditivos. Planos possuem revisão, digest canônico, estados tipados, dependências, orçamento, capabilities, aprovações e checkpoints persistidos. A API não aceita um plano como autoridade quando o corpo contiver ação desconhecida, condição não permitida, digest inconsistente ou aprovação forjada.

| Método | Endpoint | Sucesso | Idempotência |
| --- | --- | --- | --- |
| `POST` | `/v1/plans` | `201 Created` | A criação é persistida como plano `dry_run`; a autoridade de tenant e operador deve coincidir com o principal autenticado. |
| `GET` | `/v1/plans/{plan_id}` | `200 OK` | Inspeciona plano, estados de etapa, integridade, contagens verificadas e tarefa associada. |
| `POST` | `/v1/plans/{plan_id}/validate` | `200 OK` | Executa verificação determinística; relatório bloqueado não é convertido em execução. |
| `POST` | `/v1/plans/{plan_id}/approve` | `200 OK` | Exige `Idempotency-Key`; decisão fica vinculada ao digest e à revisão persistidos. |
| `POST` | `/v1/plans/{plan_id}/resume` | `200 OK` | Exige `Idempotency-Key` e `evidence_digest`; resolve `unknown` somente para `resume`. |
| `POST` | `/v1/plans/{plan_id}/cancel` | `200 OK` ou `202 Accepted` | Exige `Idempotency-Key`; solicita cancelamento normal ou resolve `unknown` para `cancel`. |
| `GET` | `/v1/plans/{plan_id}/checkpoints` | `200 OK` | Retorna checkpoints persistidos em ordem de sequência. |

### 6.1 Criar e validar um plano

O cliente envia um `PlanSpecInput` em `POST /v1/plans`. O host exige que `tenant_id` e `operator_id` do corpo correspondam ao principal autenticado e rejeita `PlanMode::Live`. Depois da criação, use:

```bash
curl --fail --silent -X POST \
  "http://127.0.0.1:8080/v1/plans/$PLAN_ID/validate"
```

O `PlanVerificationReport` diferencia um plano executável de um plano bloqueado. A presença de um relatório `valid` não substitui aprovação exigida pelo risco, nem libera efeitos externos quando o modo ou a configuração não permitem live.

### 6.2 Aprovação humana

O corpo de aprovação é:

```json
{
  "step_id": null,
  "decision": "approve",
  "expires_in_seconds": 3600
}
```

`step_id` pode limitar a aprovação a uma etapa. A expiração precisa estar entre 1 segundo e 604.800 segundos. O host calcula o `approval_id` a partir do plano, revisão e `Idempotency-Key`, vincula a aprovação ao digest e revalida tenant, papel, escopo, expiração e revogação antes de persistir a transição.

A resposta contém o resultado da aprovação e uma nova inspeção do plano. Uma rejeição é terminal para o escopo correspondente; repetir a mesma chave é idempotente, mas uma chave usada com outra decisão ou payload não deve ser reutilizada.

### 6.3 Recovery e `unknown`

Quando a recuperação encontra uma fronteira ativa, lease expirada ou inconsistência que não pode ser resolvida de modo determinístico, o plano permanece em `unknown`. Não faça retry cego e não edite o SQLite manualmente.

Para retomar após análise humana:

```bash
curl --fail --silent -X POST \
  "http://127.0.0.1:8080/v1/plans/$PLAN_ID/resume" \
  -H 'content-type: application/json' \
  -H 'Idempotency-Key: recovery-resume-1' \
  -d '{"evidence_digest":"sha256:..."}'
```

A retomada exige evidência vinculada ao incidente e não inventa sucesso para uma etapa cuja fronteira permaneceu ambígua. A alternativa de cancelamento usa o endpoint `/cancel` com `Idempotency-Key`; a compensação, quando aplicável, continua limitada ao subgrafo declarado no plano e nunca reporta a operação original como sucesso.

## 7. Respostas e erros

Os erros HTTP são serializados com uma mensagem sanitizada e o `request_id` de correlação. O corpo não deve conter bearer token, API key, prompt bruto, argumentos de ferramenta ou resultado confidencial.

| Status | Variante | Significado operacional |
| --- | --- | --- |
| `400` | `BadRequest` | Corpo, header, identificador, modo, limite ou referência inválida. |
| `401` | `Unauthorized` | Credencial ausente ou inválida para o bind atual. |
| `403` | `Forbidden` | Principal autenticado não possui tenant, papel ou capability necessários. |
| `404` | `NotFound` | Sessão, tarefa ou plano não existe no tenant autenticado. |
| `409` | `Conflict` | `Idempotency-Key` foi reutilizada com impressão digital diferente. |
| `429` | `RateLimited` ou `QuotaExceeded` | Política de rate limit ou quota do tenant foi atingida; respeite `Retry-After` quando presente. |
| `500` | `Internal` | Falha persistente ou de serialização; preserve `request_id` e interrompa retries indiscriminados. |

## 8. Tipos Rust públicos

Os tipos públicos abaixo compõem a integração host-side. Eles são contratos de dados e políticas; não conferem autoridade por si mesmos.

| Crate | Tipos/funções principais | Papel |
| --- | --- | --- |
| `shaka-core` | `Principal`, `Role`, `TenantId`, `TaskId`, `PlanSpec`, `PlanStep`, `PlanState`, `PlanStepState`, `PlanVerifier` e relatórios de verificação | Identidade, contratos de plano, estados e verificação determinística. |
| `shaka-api` | `ApiConfig`, `ApiState`, `ApiError`, `CreateSessionRequest`, `SubmitTaskRequest`, `PlanCreateRequest`, `PlanApprovalRequest`, `PlanResumeRequest`, `serve` | Configuração e fronteira HTTP. |
| `shaka-queue` | `QueueStore`, `TaskRecord`, `TaskStatus`, `SessionRecord`, `SubmitOutcome`, `FinishOutcome`, `AuthenticatedPrincipal`, `CircuitSnapshot` | Persistência, IAM host-side, idempotência, leases e fila. |
| `shaka-queue::plan_store` | `PersistedPlan`, `PlanTaskReference`, `PlanCheckpoint`, `PlanStoreTransition`, `PlanInspectionReport`, `PlanResolutionOutcome` | Persistência, inspeção, transições, checkpoints e recovery do Plan Engine. |
| `shaka-observability` | `Telemetry`, `CorrelationContext`, `AuditLogger`, `Redactor` | Correlação e auditoria redacted; telemetria não decide autorização. |
| `shaka-sandbox` | `SandboxPolicy`, `SandboxResult`, `SandboxError`, `WasmExecutor` | Execução WASM deny-by-default, sem WASI, rede ou filesystem. |

### Invariantes que os consumidores devem preservar

A identidade efetiva deve ser obtida do principal autenticado e passada ao `QueueStore`; não confie em tenant ou papel provenientes do modelo. Chaves de idempotência devem ser estáveis para uma intenção e nunca reutilizadas com payload divergente. Digests de planos e artefatos devem ser comparados em forma canônica antes de qualquer transição. Estados `unknown`, `inconsistent`, `open` ou `blocked` devem permanecer bloqueados até a decisão prevista pelo contrato.

O consumidor não deve acessar diretamente a conexão SQLite para forçar estados, apagar checkpoints, reescrever transições ou contornar aprovação. Use as operações tipadas do host e registre a correlação da solicitação sem incluir conteúdo de negócio sensível.

## 9. Exemplos seguros e não destrutivos

O fluxo mínimo recomendado para uma integração local é:

```bash
# 1. Iniciar apenas em loopback.
cargo run -p shaka-cli -- serve --bind 127.0.0.1:8080 --workers 2

# 2. Verificar saúde.
curl --fail --silent http://127.0.0.1:8080/healthz

# 3. Criar sessão e enviar tarefa explícita em dry-run.
SESSION=$(curl --fail --silent -X POST \
  http://127.0.0.1:8080/v1/sessions \
  -H 'content-type: application/json' \
  -d '{"metadata":{"source":"api-doc"}}' \
  | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')

curl --fail --silent -X POST \
  "http://127.0.0.1:8080/v1/sessions/$SESSION/tasks" \
  -H 'content-type: application/json' \
  -H 'Idempotency-Key: api-doc-task-1' \
  -d '{"objective":"Verificar uma política sem produzir efeitos externos","dry_run":true}'
```

Antes de repetir qualquer chamada, consulte o status e a correlação. Em caso de timeout, erro de provedor, circuit breaker aberto ou estado `unknown`, preserve o `request_id`, `task_id`, `plan_id`, versão e evidência e siga o runbook; não faça várias tentativas em paralelo.

## 10. Compatibilidade e mudanças

A v0.8.2 é uma release de manutenção. Mudanças que alterem endpoints, campos obrigatórios, semântica de idempotência, autoridade, capabilities, transições do Plan Engine ou recuperação exigem revisão de segurança, testes de compatibilidade, atualização deste documento e novo ciclo de release. Uma atualização de documentação não pode ser usada para prometer capacidades que o código ainda bloqueia.

Para os detalhes da operação, consulte o [`RUNBOOK_OPERACIONAL.md`](../RUNBOOK_OPERACIONAL.md). Para o estado geral do projeto e do pipeline, consulte o [`README.md`](../README.md) e o [`CHANGELOG.md`](../CHANGELOG.md).

## Referências

[1]: ../crates/shaka-api/src/lib.rs "Contrato HTTP e handlers da API"
[2]: ../crates/shaka-queue/src/lib.rs "Fila, IAM, idempotência e persistência host-side"
[3]: ../crates/shaka-queue/src/plan_store.rs "Persistência e recovery do Plan Engine"
[4]: ../crates/shaka-core/src/lib.rs "Contratos centrais, estados e verificação"
[5]: ../RUNBOOK_OPERACIONAL.md "Runbook operacional do Shaka"
[6]: ../README.md "Visão geral e operação local do Shaka"
