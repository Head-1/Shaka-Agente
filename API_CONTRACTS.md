# Contratos de API do Shaka

## 1. Convenções

Os contratos abaixo representam a release 0.2.0 do núcleo do Shaka. Campos de identificação são string no JSON, embora alguns sejam wrappers tipados em Rust. Datas usam RFC 3339. O host deve rejeitar campos desconhecidos quando o contrato de integração exigir compatibilidade estrita.

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

Argumentos malformados devem produzir erro antes da execução. O modelo não pode indicar capabilities na resposta como forma de concedê-las.

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

`status` pode ser `Specified`, `Generated`, `Tested`, `Candidate`, `Active`, `Deprecated` ou `Revoked`. Somente `Candidate` pode ser promovido mediante aprovação. A aprovação pode calcular o SHA-256 diretamente do arquivo do artefato. O catálogo é persistido atomicamente, mas o runtime não executa automaticamente o artefato associado.

## 7. Aprovação de skill

```json
{
  "operator_id": "operator",
  "approved_at": "2026-08-20T00:00:00Z",
  "artifact_sha256": "64-caracteres-hexadecimais",
  "reason": "justificativa da aprovação"
}
```

A string de hash precisa conter exatamente 64 caracteres hexadecimais. A justificativa não pode ser vazia.

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

`AuditEvent` inclui `tenant_id`, ator, ação, outcome, metadados, `previous_hash` e `event_hash`. O `MemoryStore` reencadeia o evento por tenant e `verify_audit_chain` valida elo anterior, tenant e hash do conteúdo.

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

## 13. Compatibilidade

Novos campos opcionais podem ser adicionados em uma versão minor. Remoção ou alteração semântica exige versão major e ADR. Skills devem versionar separadamente de schemas de tools e do runtime. Migrações de memória devem registrar versão do schema e permitir restauração.
