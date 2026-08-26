# Architecture Decision Records

## ADR-001 — Workspace Cargo por responsabilidade

**Status:** Aceita
**Contexto:** O agente combina contratos, memória, execução WASM, orquestração e CLI. Um único crate facilitaria o início, mas aumentaria o acoplamento e dificultaria testes de fronteira.
**Decisão:** Usar workspace Cargo com `shaka-core`, `shaka-memory`, `shaka-skills`, `shaka-sandbox`, `shaka-orchestrator`, `shaka-observability` e `shaka-cli`.
**Consequências:** As interfaces ficam explícitas e os crates podem ser testados isoladamente. Há maior custo inicial de configuração e versionamento coordenado.
**Alternativas consideradas:** Um crate único, rejeitado por acoplamento; microserviços, rejeitados no MVP por custo operacional prematuro.

## ADR-002 — Tokio para runtime assíncrono

**Status:** Aceita
**Contexto:** O agente precisa chamar modelo, ferramentas e futuras integrações sem bloquear o processo.
**Decisão:** Usar Tokio com runtime multi-thread, timers e sincronização.
**Consequências:** Boa integração com `reqwest`, canais e timeouts. O código precisa respeitar `Send`/`Sync` e evitar locks durante chamadas externas.
**Alternativas consideradas:** `async-std` e `smol`, descartados no MVP por reduzirem a convergência com o ecossistema escolhido para HTTP, tracing e orquestração; execução síncrona, rejeitada por não representar o perfil de I/O do agente.

## ADR-003 — SQLite para o primeiro backend de memória

**Status:** Aceita para MVP
**Contexto:** O MVP é local, single-tenant por padrão e precisa ser reproduzível sem serviço externo.
**Decisão:** Usar SQLite embutido via `rusqlite`, com migração inicial idempotente, índices por tenant/data e conexão protegida por `parking_lot::Mutex`.
**Consequências:** Instalação simples, baixo custo e bons testes locais. Não é uma decisão de produção para alta concorrência, replicação ou multi-tenancy distribuído.
**Alternativas consideradas:** Postgres, reservado para implantação multiusuário; RocksDB, descartado por oferecer menor ergonomia relacional para auditoria e retenção; Qdrant, reservado para a futura busca vetorial.

## ADR-004 — Wasmtime sem WASI no MVP

**Status:** Aceita
**Contexto:** Código de skill é não confiável e não pode compartilhar a fronteira do processo principal.
**Decisão:** Compilar/receber WASM e executar via Wasmtime com fuel, sem imports e sem WASI. Módulos precisam exportar `run() -> i32`.
**Consequências:** Fronteira mínima e fácil de testar. Skills reais ainda não podem acessar filesystem ou rede; uma futura concessão deve usar interfaces pequenas e capacidades explícitas, não acesso amplo ao host.
**Alternativas consideradas:** Execução nativa em subprocesso, rejeitada como primeira fronteira por ampliar o risco de syscall e filesystem; gVisor/Firecracker, reservados para uma camada adicional quando houver necessidade de processos ou workloads não adequados a WASM; WASI Preview 2, reservado após definir interfaces de capacidade.

## ADR-005 — Skills candidatas e promoção humana

**Status:** Aceita e obrigatória
**Contexto:** O agente pode identificar oportunidades de automação, mas autoalteração e autopromoção criariam uma mudança de capacidade não revisada.
**Decisão:** O agente pode sugerir uma skill. A criação, testes, candidatura, aprovação e ativação são estados distintos. A transição para ativa requer comando de operador, hash SHA-256 e justificativa.
**Consequências:** Maior governança e auditabilidade; menor velocidade de adaptação. A equipe precisa operar um processo explícito de revisão.
**Alternativas consideradas:** Auto-promoção por taxa de sucesso, rejeitada por permitir que o agente amplie privilégios sem aprovação; revisão manual sem estado formal, rejeitada por dificultar rollback e auditoria.

## ADR-006 — Memória episódica antes da busca vetorial

**Status:** Aceita para MVP
**Contexto:** Busca semântica sem proveniência, retenção e controle de tenant pode recuperar informação errada ou sensível.
**Decisão:** Registrar episódios primeiro e permitir consolidação semântica explícita associada a uma origem. Embeddings e ranking híbrido entram depois.
**Consequências:** Menor capacidade de recuperação no MVP, mas melhor controle de origem, retenção e migração.
**Alternativas consideradas:** Qdrant desde o primeiro commit, rejeitado por introduzir operação externa antes de definir o modelo de dados; busca textual SQLite, reservada como evolução intermediária.

## ADR-007 — Function calling mediado pelo host

**Status:** Aceita
**Contexto:** Saída do modelo pode ser malformada, exceder orçamento ou solicitar uma ferramenta não autorizada.
**Decisão:** O host mantém catálogo de definições, valida entrada, compara capabilities, aplica dry-run e só então chama a implementação.
**Consequências:** O modelo não tem autoridade direta. A validação inicial de JSON Schema do MVP é mínima e deve evoluir para validação completa antes de expor contratos complexos.
**Alternativas consideradas:** Executar diretamente o JSON do modelo, rejeitado; confiar apenas em instruções textuais, rejeitado por não ser controle determinístico.

## ADR-008 — Modelo local e adaptador OpenAI-compatível

**Status:** Aceita
**Contexto:** O MVP deve ser testável sem credenciais, mas precisa permitir conexão a um modelo real.
**Decisão:** Fornecer `LocalModel` determinístico para testes e `OpenAiCompatibleModel` opcional, com endpoint, modelo e chave vindos do ambiente.
**Consequências:** Execução local reproduzível e integração flexível. O contrato do provedor deve ser testado separadamente, e nenhum segredo deve entrar no repositório.
**Alternativas consideradas:** Acoplar diretamente a um único SDK, rejeitado por reduzir portabilidade; exigir modelo real para todos os testes, rejeitado por custo e não determinismo.

## ADR-009 — Dry-run como padrão

**Status:** Aceita
**Contexto:** Ações externas, mensagens e mutações são efeitos colaterais que não devem ocorrer por interpretação ambígua do modelo.
**Decisão:** `TaskEnvelope::dry_run` inicia como `true`; efeitos não somente leitura retornam uma simulação. O MVP não registra adaptador externo de mensageria.
**Consequências:** Operação segura por padrão, com necessidade de um fluxo de aprovação quando integrações reais forem adicionadas.
**Alternativas consideradas:** Execução imediata após schema válido, rejeitada por não incorporar intenção, identidade e confirmação.

## ADR-010 — Auditoria de eventos, não armazenamento obrigatório do raciocínio bruto

**Status:** Aceita
**Contexto:** Auditoria precisa explicar ações, versões, evidências e políticas sem exigir o armazenamento indiscriminado do raciocínio interno do modelo.
**Decisão:** Registrar eventos, chamadas de ferramenta, resultado, política aplicada, hashes e resumo operacional. Não persistir raciocínio interno bruto como requisito do sistema.
**Consequências:** Melhor minimização de dados e menor risco de exposição. O resumo precisa ser suficiente para reconstruir decisões relevantes.
**Alternativas consideradas:** Persistir toda a cadeia de raciocínio, rejeitado por privacidade e necessidade desnecessária; registrar apenas resposta final, rejeitado por não permitir auditoria operacional.

## ADR-011 — Atualizar Wasmtime antes da release candidata

**Status:** Aceita.

A versão 27.0.0 foi substituída por Wasmtime 47.0.3 depois que `cargo audit` reportou advisories críticos e médios ligados ao runtime de sandbox. A release usa features explícitas de runtime, component model e Cranelift, sem WASI. A versão deve continuar sendo revisada pelo pipeline de dependências.

## ADR-012 — RBAC mínimo no host

**Status:** Aceita.

O host separa `operator`, `reviewer` e `administrator`. Cada ação sensível recebe uma variante de `Action`, e a autorização ocorre antes de acessar memória, catálogo ou modo live. A autorização local não é considerada IAM de produção pública; uma integração remota deve preservar o mesmo contrato.

## ADR-013 — JSON Schema como fronteira de ferramenta

**Status:** Aceita.

A validação estrutural de entradas foi substituída por compilação e validação de JSON Schema. Uma entrada incompatível falha antes da ferramenta e não pode ser corrigida pela ferramenta executora. Schemas inválidos bloqueiam a execução e devem ser corrigidos por revisão de código.

## ADR-014 — Cadeia de auditoria por tenant

**Status:** Aceita.

O `MemoryStore` calcula o elo anterior por tenant e reprocessa o hash do evento antes de persistir. O comando `verify-audit` valida tenant, elo anterior e hash do conteúdo. Isso oferece detecção de alteração acidental ou silenciosa, mas não substitui armazenamento WORM, assinatura externa ou exportação remota.

## ADR-015 — Backup online e restore explícito

**Status:** Aceita.

SQLite é copiado pela API de backup online do rusqlite, evitando cópia ingênua do arquivo enquanto o processo está ativo. Restore é ação administrativa, exige arquivo existente e termina com integrity check. Backups externos criptografados e testes RPO/RTO permanecem responsabilidade da infraestrutura.

## ADR-016 — Catálogo de skills com escrita atômica

**Status:** Aceita.

O catálogo é gravado em arquivo temporário sincronizado, renomeado atomicamente e com permissões restritas em sistemas Unix. O fluxo de aprovação pode calcular SHA-256 do artefato real; nenhum caminho de autopromoção foi adicionado.

## ADR-017 — Contexto de execução por request

**Status:** Aceita para a fatia de hardening atual.

**Contexto:** O catálogo de ferramentas é construído no processo do worker, mas a autorização não pode ser herdada desse estado global quando a fila contém solicitações com papéis diferentes. Um envelope enviado pelo cliente também não pode escolher suas próprias capabilities.

**Decisão:** Persistir em `TaskEnvelope.execution_context` o papel e o conjunto efetivo de capabilities da execução. A API HTTP, a CLI e a submissão governada da fila derivam esse contexto do principal autenticado; a fila sobrescreve qualquer contexto fornecido pelo caller antes da transação de admissão. O runtime filtra as definições anunciadas ao modelo e repete a verificação na execução da ferramenta usando o contexto persistido. No claim de planos, as capabilities do envelope substituem as capabilities globais do worker. A matriz canônica desta fatia é:

| Capability | Operator | Reviewer | Administrator |
|---|---:|---:|---:|
| `Network` | Não | Não | Sim |
| `FilesystemRead` | Não | Não | Sim |
| `FilesystemWrite` | Não | Não | Sim |
| `CodeExecution` | Não | Não | Sim |
| `ExternalMessaging` | Não | Não | Sim |
| `MemoryWrite` | Não | Não | Sim |

A matriz é fornecida por `CapabilitySet::for_role`; não existe wildcard implícito e qualquer alteração deve atualizar os testes exatos de todos os papéis.

**Compatibilidade e limites:** Envelopes antigos sem o campo são desserializados com o contexto conservador `operator` sem capabilities. `tenant_id`, `operator_id`, orçamento, `dry_run` e referências de plano permanecem no `TaskEnvelope` porque já fazem parte do contrato de identidade, execução e persistência da task; esta fatia não adiciona request-id, correlação ou referência de aprovação ao novo tipo. Administrator recebe o conjunto de capabilities atualmente declarado pelo host; Operator e Reviewer permanecem deny-by-default nesta política.

**Consequências:** Uma solicitação autenticada com menor privilégio não recebe definições administrativas no prompt do modelo e também é bloqueada se tentar chamar a ferramenta diretamente. A autorização efetiva deixa de depender apenas do catálogo global, preservando o catálogo como gate de registro do processo. O contexto é serializável e acompanha retries, leases e claims persistidos.

**Alternativas consideradas:** Confiar somente nas capabilities globais do worker, rejeitado por permitir herança entre requests; aceitar capabilities do payload, rejeitado por permitir forgery; criar um novo registro de autorização separado nesta etapa, reservado para quando houver decisão de incluir correlação, aprovação e política de orçamento no mesmo objeto.

**Evidência:** Testes de núcleo, registry, fila e API cobrem derivação least-privilege, sobrescrita na admissão, filtragem de ferramentas e bloqueio na execução, além da suíte completa do workspace.


## ADR-018 — Fronteira da submissão de tasks na fila

**Status:** Aceita localmente para a opção B; publicação pendente de autenticação GitHub.

**Contexto:** `QueueStore::submit_task` aceitava chamadas de qualquer crate porque era pública, mas não recebia `Principal` nem aplicava a admissão governada de sessão, contexto de execução, quota, rate limit e plano. A API HTTP já usava a família `submit_task_governed*`, porém a superfície pública permitia que um consumidor Rust externo bypassasse essa fronteira.

**Decisão:** Reduzir `QueueStore::submit_task` para `pub(crate)` sob `cfg(test)`. A primitiva fica disponível exclusivamente aos testes internos do crate `shaka-queue`; consumidores externos e builds de produção não recebem esse método. Toda entrada externa deve usar `submit_task_governed` ou `submit_task_governed_with_plan`, que recebem o principal autenticado e aplicam a política transacional completa.

**Consequências:** O probe externo que compilava e executava uma submissão direta antes do patch passa a falhar com `E0599` porque `QueueStore` não possui `submit_task` no build público. Não há alteração no schema, na máquina de estados, na API HTTP, nas quotas, nos rate limits ou na matriz de capabilities. O fechamento é de superfície de API, não uma substituição das verificações governadas.

**Alternativas consideradas:** Remover ou reescrever a primitiva, rejeitado por ampliar o diff e o risco sobre persistência, retry e testes; manter `pub`, rejeitado porque deixa um caminho de bypass disponível; mover toda a fila para outro módulo, reservado para uma refatoração posterior com contrato de API separado.

**Evidência pré/pós:** `/home/ubuntu/full-audit/option-b-before-run.log` mostra o probe externo executando na base anterior; `/home/ubuntu/full-audit/option-b-after-2.log` mostra a falha pós-patch `E0599`, validada em `/home/ubuntu/full-audit/option-b-after-result.txt`. A suíte interna do queue e a suíte completa do workspace permaneceram verdes nos gates da branch.
