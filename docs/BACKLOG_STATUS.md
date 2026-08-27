# Status consolidado do backlog de confiabilidade e closeout

**Data de atualização:** 27 de agosto de 2026
**Fonte de código:** `main` no merge commit [`48eeed2`](https://github.com/Head-1/Shaka-Agente/commit/48eeed2d2ba9a9caa975d0e376d24efc467ea380), após as PRs #31–#38 [1] [2] [3] [4] [5] [6] [7] [8]
**Estado de validação:** CI, sandbox e VM aprovados para a série integrada; o closeout não altera código funcional

## Visão executiva

Os itens BR-01 a BR-06 foram analisados com o protocolo de engenharia do projeto. Quando havia bug confirmado, a evidência preserva a hipótese, uma reprodução pré-falhando, a correção mínima, a regressão pós-passando e a validação integrada. No estado atual da `main`, os seis itens permanecem **corrigidos, cobertos e validados**. O closeout documental atualiza a referência de código e o histórico de integração, mas não reclassifica nenhum BR.

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

## Cadeia de integração repository-first

A série integrada que levou a `main=48eeed2` é a seguinte:

| PR | Natureza | Escopo | Merge commit |
|---|---|---|---|
| [#31][1] | Documentação | Consolidação do status do backlog de confiabilidade | `eb5a6a5` |
| [#32][2] | Correção funcional | Preservação de replay idempotente sob rate limit | `8c92dca` |
| [#33][3] | Documentação | Contratos dos probes de crash/recovery | `2e546fe` |
| [#34][4] | Documentação | Contratos de sandbox e fila | `f1a8fdd` |
| [#35][5] | Documentação | Contratos de memória e configuração | `d46b058` |
| [#36][6] | Documentação | Contratos de orchestrator e skills | `38b7a8c` |
| [#37][7] | Documentação | Estados e identidade em `shaka-core` | `d78a32a` |
| [#38][8] | Documentação | Governança de planos em `shaka-core` | `48eeed2` |

As correções funcionais dos BRs foram preservadas como histórico técnico; as PRs documentais posteriores não são apresentadas como correções de comportamento. Cada ciclo foi conduzido a partir do estado publicado do repositório, com validação local, commit assinado quando aplicável, CI remoto, validação independente na VM, merge protegido e nova validação pós-merge. As branches de feature permanecem preservadas para auditoria.

## Evidências de integração

A correção de BR-01 foi integrada na série histórica de hardening e documentada pela [PR #31][1]. A correção funcional de replay idempotente sob rate limit foi integrada pela [PR #32][2]. As PRs #33–#38 consolidaram a documentação pública dos probes, sandbox, fila, memória, configuração, runtime, skills, identidade e governança de planos [3] [4] [5] [6] [7] [8].

A evidência histórica inicial deste consolidado foi coletada no merge commit `445bc65`, com testes do workspace, smoke de produção, lifecycle do processo, crash/recovery do QueueStore, cadeia de auditoria multiprocesso e validação equivalente na VM. O estado final da `main` está no merge commit completo `48eeed2d2ba9a9caa975d0e376d24efc467ea380`, que inclui a série #31–#38 e foi validado em CI, sandbox e VM. O working tree permaneceu limpo e as portas operacionais dedicadas foram liberadas após as execuções.

A validação final usada no closeout executou `cargo check --workspace --locked` com sucesso no SHA `48eeed2`. Os ciclos das PRs #36, #37 e #38 também tiveram validação pré-merge e pós-merge nos dois ambientes, nos SHAs finais `38b7a8c`, `d78a32a` e `48eeed2`, respectivamente. O protocolo executável está em [`docs/VALIDACAO_REPOSITORY_FIRST.md`](VALIDACAO_REPOSITORY_FIRST.md).

## Saldo documental conhecido

No `main` final, o `cargo check --workspace --locked` produziu 100 avisos `missing_docs`, todos em `shaka-core`. A distribuição observada foi:

| Categoria | Quantidade |
|---|---:|
| Módulos | 1 |
| Structs | 7 |
| Campos de struct | 53 |
| Itens associados | 2 |
| Métodos | 6 |
| Enums | 4 |
| Variantes | 27 |
| **Total** | **100** |

Esses avisos são dívida documental conhecida sob a política `missing_docs=warn`; não representam falha de compilação, de CI ou de operação. A medição foi feita no SHA final antes do closeout e deve ser usada como linha de base para lotes futuros. A redução desse saldo deve ocorrer em branches isoladas, com escopo explícito, auditoria de diff e validação nos dois ambientes. O closeout não modifica código para reduzir esse número.

## Limites e decisões residuais

O BR-01 rejeita tokens legados sem expiração, mas não executa migração ou revogação em massa. Operadores que encontrem esses registros devem emitir credenciais com expiração finita e planejar a retirada dos registros antigos como operação separada.

O BR-06 usa `rowid` como ordem estrutural porque o ciclo de vida atual da tabela de auditoria é append-only e não há remoção ou compactação de `audit_events` implementada. Uma sequência semântica explícita deve ser reavaliada antes de introduzir retenção, compactação, exportação/importação ou replicação entre bancos. Essa é uma melhoria arquitetural condicional, não uma falha aberta neste estado.

Os itens seguintes permanecem fora desta matriz: IAM remoto, rotação automática, retenção regulatória, ancoragem WORM, replicação distribuída, métricas exportáveis e demais capacidades explicitamente fora da v0.8.2. Eles não devem ser inferidos como resolvidos pelo fechamento dos BR-01 a BR-06 nem pela série documental #31–#38.

## Validação repository-first

Toda alteração futura deve nascer de `origin/main`, ser testada localmente, receber commit assinado, passar pelo CI, ser validada na VM pelo SHA publicado, entrar por PR e ser submetida novamente ao executor pós-merge. O contrato executável está em [`docs/VALIDACAO_REPOSITORY_FIRST.md`](VALIDACAO_REPOSITORY_FIRST.md).

## Referências

[1]: https://github.com/Head-1/Shaka-Agente/pull/31 "PR #31 — consolidação documental do backlog"
[2]: https://github.com/Head-1/Shaka-Agente/pull/32 "PR #32 — replay idempotente sob rate limit"
[3]: https://github.com/Head-1/Shaka-Agente/pull/33 "PR #33 — probes de crash/recovery"
[4]: https://github.com/Head-1/Shaka-Agente/pull/34 "PR #34 — sandbox e fila"
[5]: https://github.com/Head-1/Shaka-Agente/pull/35 "PR #35 — memória e configuração"
[6]: https://github.com/Head-1/Shaka-Agente/pull/36 "PR #36 — orchestrator e skills"
[7]: https://github.com/Head-1/Shaka-Agente/pull/37 "PR #37 — estados e identidade"
[8]: https://github.com/Head-1/Shaka-Agente/pull/38 "PR #38 — governança de planos"
