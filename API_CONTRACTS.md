# Contratos de API do Shaka

## 1. Convenções

Os contratos abaixo representam o núcleo do Shaka e a API REST persistente da release 0.5.0. Campos de identificação são string no JSON, embora alguns sejam wrappers tipados em Rust. Datas usam RFC 3339. O host deve rejeitar campos desconhecidos quando o contrato de integração exigir compatibilidade estrita.

## 2. TaskEnvelope

```json
{
  "task_id": "uuid",
  "tenant_id": "demo",
  "operator_id": "operator",
  "objective": "objetivo da tarefa",
  "budget": {
    "max_steps": 32,
    "max_tool_calls": 16,
    "max_elapsed_ms": 30000,
    "max_cost_microunits": 1000000
  },
  "dry_run": true,
  "created_at": "2026-08-20T00:00:00Z"
}
```

`objective` não pode ser vazio nem exceder 32.000 caracteres. `dry_run` deve ser verdadeiro por padrão para tarefas iniciadas pela CLI.

## 3. ToolDefinition

```json
{
  "name": "echo",
  "description": "Repete uma mensagem sem efeito colateral.",
  "input_schema": {
    "type": "object",
    "properties": {
      "message": {"type": "string"}
    },
    "required": ["message"]
  },
  "required_capabilities": [],
  "side_effect": "ReadOnly"
}
```

`side_effect` pode ser `ReadOnly`, `ExternalEffect` ou `Mutation`. A entrada é compilada e validada pelo JSON Schema antes da execução; schema inválido ou entrada incompatível gera `schema_violation`.

## 4. ToolCall e ToolResult

```json
{
  "task_id": "uuid",
  "tool_name": "echo",
  "input": {"message": "olá"},
  "requested_at": "2026-08-20T00:00:00Z"
}
```

```json
{
  "tool_name": "echo",
  "output": {"message": "olá"},
  "success": true,
  "error_code": null
}
```

Uma chamada proposta pelo modelo não é uma autorização. O host verifica catálogo, schema, capability, budget, identidade e `dry_run`.

## 5. ModelRequest e ModelResponse

```json
{
  "system": "política do host",
  "user": "objetivo da tarefa",
  "tools": [],
  "prior_tool_results": [],
  "max_output_tokens": 1024
}
```

```json
{
  "content": "resposta textual",
  "tool_calls": [
    {
      "tool_name": "echo",
      "arguments": {"message": "olá"}
    }
  ],
  "estimated_cost_microunits": 0
}
```

Argumentos malformados devem produzir erro antes da execução. O modelo não pode indicar capabilities na resposta como forma de concedê-las. O runtime pode repetir o ciclo até `max_steps`; em cada ciclo, `max_tool_calls`, `max_elapsed_ms` e `max_cost_microunits` são reavaliados cumulativamente. `prior_tool_results` contém somente resultados redigidos e limitados pelo host.

## 6. SkillManifest

```json
{
  "name": "relatorio",
  "version": "0.1.0",
  "description": "Gera um relatório",
  "permissions": ["MemoryWrite"],
  "input_schema": {"type": "object"},
  "output_schema": {"type": "object"},
  "status": "Candidate",
  "artifact_sha256": null
}
```

`status` pode ser `Specified`, `Generated`, `Tested`, `Candidate`, `Active`, `Deprecated` ou `Revoked`. Somente `Candidate` pode ser promovido mediante aprovação. A aprovação pode calcular o SHA-256 diretamente do arquivo do artefato. Uma skill só entra no runtime quando está `Active`, possui caminho canônico persistido por aprovação de artefato e o SHA-256 é recalculado antes do registro. Skills revogadas, hashes manuais sem arquivo verificável e artefatos alterados ficam fora do registro executável.

## 7. Aprovação de skill

```json
{
  "operator_id": "operator",
  "approved_at": "2026-08-20T00:00:00Z",
  "artifact_sha256": "64-caracteres-hexadecimais",
  "artifact_path": "/caminho/canonico/skill.wasm",
  "reason": "justificativa da aprovação"
}
```

A string de hash precisa conter exatamente 64 caracteres hexadecimais. A justificativa não pode ser vazia. `artifact_path` é opcional para aprovações legadas por hash; sem um caminho verificável, a skill permanece no catálogo, mas não é registrada para execução.

## 8. EpisodicRecord

```json
{
  "id": "uuid",
  "tenant_id": "demo",
  "task_id": "uuid",
  "kind": "agent_run",
  "content": "resumo da execução",
  "outcome": "success",
  "cost_microunits": 0,
  "elapsed_ms": 0,
  "created_at": "2026-08-20T00:00:00Z"
}
```

O conteúdo deve ser resumo operacional e não deve incluir segredos. Uma implantação com dados pessoais precisa aplicar classificação, redaction, retenção e exclusão por titular.

## 9. SandboxPolicy e SandboxResult

```json
{
  "max_fuel": 100000,
  "max_elapsed_ms": 1000,
  "allow_network": false,
  "allow_filesystem": false
}
```

```json
{
  "exit_code": 42,
  "fuel_consumed": 12
}
```

No MVP o WASM não pode importar funções do host. Uma tentativa de import retorna `HostImportsDenied`; capability não autorizada retorna `CapabilityDenied`.

## 10. Configuração e autorização

A configuração pública informa ambiente, tenant, operador, papel, provedor, endpoint, nome do modelo, presença de chave, modo live e auditoria habilitada. A chave nunca aparece no resumo público; somente `api_key_configured: true/false` é exposto.

Os papéis são `operator`, `reviewer` e `administrator`. A autorização é decidida pelo host por ação: execução somente leitura, execução externa, criação/aprovação/revogação de skill, backup, restore, expurgo e verificação de auditoria. O modelo não pode conceder capabilities ou papel por texto.

## 11. Auditoria, backup e operação

`AuditEvent` inclui `tenant_id`, ator, ação, outcome, metadados, `previous_hash` e `event_hash`. Cada execução de ferramenta gera um evento `tool.execute`; falhas de modelo, orçamento e deadline geram um episódio e um evento `agent.run` com outcome `failure`. O `MemoryStore` reencadeia o evento por tenant e `verify_audit_chain` valida elo anterior, tenant e hash do conteúdo.

A CLI oferece `doctor`, `backup`, `restore`, `verify-audit` e `config`. Backup usa a API online do SQLite; restore deve ser executado por administrador e seguido de integrity check.

## 12. Erros e semântica

| Código lógico | Significado | Retry automático |
|---|---|---:|
| `invalid_input` | Objetivo ou entrada inválida | Não |
| `schema_violation` | Entrada incompatível com ferramenta | Não sem correção explícita |
| `capability_denied` | Capability não concedida | Não |
| `budget_exceeded` | Limite de chamadas/custo/tempo | Não |
| `deadline_exceeded` | Deadline da tarefa vencido | Somente nova tarefa controlada |
| `tool_execution_failed` | Ferramenta falhou | Depende de idempotência |
| `host_imports_denied` | WASM tentou importar host | Não |
| `approval_required` | Efeito exige aprovação | Não até aprovação |

## 13. API REST persistente v1

A API usa JSON UTF-8, prefixo `/v1` e o principal local configurado pelo operador. O cliente não escolhe `tenant_id`, `operator_id` ou `role`. O bind padrão é `127.0.0.1:8080`; binds não locais exigem chave configurada e o header `Authorization: Bearer <chave>`.

| Método e rota | Entrada | Semântica |
|---|---|---|
| `GET /healthz` | Nenhuma | Retorna `status`, versão, quantidade de tarefas ativas e snapshot do circuito. |
| `POST /v1/sessions` | `metadata` opcional, limitado | Cria uma sessão persistente e retorna `201`. |
| `GET /v1/sessions/{session_id}` | UUID da sessão | Retorna a sessão somente no tenant do principal. |
| `POST /v1/sessions/{session_id}/tasks` | `objective`, `priority`, `max_attempts`, `dry_run` e `budget` opcionais; `Idempotency-Key` obrigatório | Retorna `202` para tarefa nova ou `200` para repetição idempotente. |
| `GET /v1/tasks/{task_id}` | UUID da tarefa | Retorna estado, tentativas, lease, erro redacted e resultado quando disponível. |
| `DELETE /v1/tasks/{task_id}` | UUID da tarefa | Solicita cancelamento cooperativo; retorna `202` enquanto o worker ainda encerra. |

Uma tarefa pode estar em `queued`, `running`, `succeeded`, `failed`, `cancel_requested` ou `cancelled`. A fila ordena prioridade descrescente e criação crescente. Um lease expirado volta para `queued` na recuperação do processo, preservando tentativas e auditoria.

A chave de idempotência é única por tenant. O mesmo valor com o mesmo fingerprint devolve a tarefa existente; o mesmo valor com outro payload produz `409 Conflict`. Falhas de transporte do modelo e deadlines podem ser repetidas até `max_attempts`, com backoff exponencial limitado. Entradas inválidas, autorização, schema, capability e orçamento não são retryable.

O circuit breaker persistido usa `closed`, `open` e `half_open`. Quando aberto, workers deixam tarefas em `queued` até a janela de recuperação. O cancelamento é cooperativo e é observado antes de cada passo, ferramenta e chamada assíncrona do runtime.

## 14. Compatibilidade

Novos campos opcionais podem ser adicionados em uma versão minor. Remoção ou alteração semântica exige versão major e ADR. Skills devem versionar separadamente de schemas de tools e do runtime. Migrações de memória devem registrar versão do schema e permitir restauração.
