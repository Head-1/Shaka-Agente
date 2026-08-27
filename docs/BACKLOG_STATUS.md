# Status consolidado do backlog de confiabilidade

**Data de atualização:** 27 de agosto de 2026
**Fonte de código:** `main` no merge commit [`f1a8fdd`](https://github.com/Head-1/Shaka-Agente/commit/f1a8fdd1040308df682e98ada7f126703500895a), após as PRs #32, #33 e #34
**Estado de validação:** CI, sandbox e VM aprovados

## Visão executiva

Os itens BR-01 a BR-06 foram analisados com o protocolo de engenharia do projeto. Quando havia bug confirmado, a evidência preserva a hipótese, uma reprodução pré-falhando, a correção mínima, a regressão pós-passando e a validação integrada. No estado atual da `main`, os seis itens estão **corrigidos, cobertos e validados**.

> **Regra de interpretação:** cobertura de testes e status verde de CI são necessários, mas não substituem testes específicos para a propriedade de segurança ou integridade em questão. Cada BR abaixo possui uma regressão direcionada.

## Matriz de status

| Item | Risco tratado | Estado atual | Propriedade comprovada |
|---|---|---|---|
| **BR-01** | Token IAM legado com `expires_at = NULL` permanecia aceito | **Corrigido e validado** | Autenticação rejeita token sem expiração; `has_active_tokens()` não o conta como ativo. |
| **BR-02** | Transação deferred podia falhar com `SQLITE_BUSY` ao ser promovida | **Corrigido e validado** | `record_plan_transition` inicia escrita com transação `Immediate` e aguarda writer concorrente dentro do timeout bounded. |
| **BR-03** | Replay idempotente criava checkpoint persistente adicional | **Corrigido e validado** | A chave é verificada antes da admissão planejada; replay compatível retorna `Existing` e não aumenta checkpoints. |
| **BR-04** | Instâncias concorrentes podiam bifurcar a cadeia de auditoria | **Corrigido e validado** | Predecessor, insert e commit usam transação `Immediate`; a cadeia permanece linear entre instâncias. |
| **BR-05** | Leitura de memória corrompida fabricava valores sintéticos | **Corrigido e validado** | UUID, `task_id`, timestamps e `expires_at` inválidos retornam `MemoryError::InvalidRecord`. |
| **BR-06** | Rollback do relógio podia invalidar a cadeia já gravada | **Corrigido e validado** | A ordem estrutural usa ordem de inserção/commit (`rowid`); `occurred_at` permanece apenas metadado temporal. |

## Evidências de integração

A correção de BR-01 foi integrada pela [PR #30](https://github.com/Head-1/Shaka-Agente/pull/30). As correções anteriores de transação, idempotência, leitura fail-closed e ordenação estrutural foram integradas na série de hardening da [PR #26](https://github.com/Head-1/Shaka-Agente/pull/26). Os probes operacionais multiprocesso estão na [PR #29](https://github.com/Head-1/Shaka-Agente/pull/29).

A evidência de validação inicial deste consolidado foi coletada no merge commit `445bc65`, com testes do workspace, smoke de produção, lifecycle do processo, crash/recovery do QueueStore, cadeia de auditoria multiprocesso e validação equivalente na VM. O estado corrente da `main` está no merge commit `f1a8fdd`, que também foi validado em CI, sandbox e VM; o working tree permaneceu limpo e as portas operacionais foram liberadas.

## Limites e decisões residuais

O BR-01 rejeita tokens legados sem expiração, mas não executa migração ou revogação em massa. Operadores que encontrem esses registros devem emitir credenciais com expiração finita e planejar a retirada dos registros antigos como operação separada.

O BR-06 usa `rowid` como ordem estrutural porque o ciclo de vida atual da tabela de auditoria é append-only e não há remoção ou compactação de `audit_events` implementada. Uma sequência semântica explícita deve ser reavaliada antes de introduzir retenção, compactação, exportação/importação ou replicação entre bancos. Essa é uma melhoria arquitetural condicional, não uma falha aberta neste estado.

Os itens seguintes permanecem fora desta matriz: IAM remoto, rotação automática, retenção regulatória, ancoragem WORM, replicação distribuída, métricas exportáveis e demais capacidades explicitamente fora da v0.8.2. Eles não devem ser inferidos como resolvidos pelo fechamento dos BR-01 a BR-06.

## Validação repository-first

Toda alteração futura deve nascer de `origin/main`, ser testada localmente, receber commit assinado, passar pelo CI, ser validada na VM pelo SHA publicado, entrar por PR e ser submetida novamente ao executor pós-merge. O contrato executável está em [`docs/VALIDACAO_REPOSITORY_FIRST.md`](VALIDACAO_REPOSITORY_FIRST.md).

## Referências

[1]: https://github.com/Head-1/Shaka-Agente/pull/26 "PR #26 — hardening do runtime, persistência e governança"

[2]: https://github.com/Head-1/Shaka-Agente/pull/29 "PR #29 — probes operacionais multiprocesso"

[3]: https://github.com/Head-1/Shaka-Agente/pull/30 "PR #30 — correção fail-closed de tokens legados"
