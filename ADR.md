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

SQLite é copiado pela API de backup online do rusqlite, evitando cópia ingênua do arquivo enquanto o processo está ativo. Restore é ação administrativa, exige arquivo existente, integrity check e schema compatível com as tabelas persistentes conhecidas do Shaka antes de substituir o destino. Um snapshot SQLite válido, mas incompatível, é rejeitado sem mutação do banco de destino. Backups externos criptografados e testes RPO/RTO permanecem responsabilidade da infraestrutura.

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

**Compatibilidade e limites:** Envelopes antigos sem o campo são desserializados com o contexto conservador `operator` sem capabilities e proveniência vazia. `tenant_id`, `operator_id`, orçamento, `dry_run` e referências de plano permanecem no `TaskEnvelope` porque já fazem parte do contrato de identidade, execução e persistência da task. A proveniência nova contém `request_id` opcional e `admission_approval_id` opcional; ambos são derivados pelo host e não pelo payload. Administrator recebe o conjunto de capabilities atualmente declarado pelo host; Operator e Reviewer permanecem deny-by-default nesta política.

**Consequências:** Uma solicitação autenticada com menor privilégio não recebe definições administrativas no prompt do modelo e também é bloqueada se tentar chamar a ferramenta diretamente. A autorização efetiva deixa de depender apenas do catálogo global, preservando o catálogo como gate de registro do processo. O contexto é serializável e acompanha retries, leases e claims persistidos.

**Alternativas consideradas:** Confiar somente nas capabilities globais do worker, rejeitado por permitir herança entre requests; aceitar capabilities do payload, rejeitado por permitir forgery; persistir trace e span IDs, rejeitado porque são identificadores temporários de observabilidade; criar um novo registro de autorização separado, reservado para quando houver decisão de incluir correlação, aprovação e política de orçamento no mesmo objeto.

**Evidência:** Testes de núcleo, registry, fila e API cobrem derivação least-privilege, sobrescrita na admissão, filtragem de ferramentas, bloqueio na execução, round-trip de proveniência e correlação do request ID até o claim da task, além da suíte completa do workspace.


## ADR-018 — Fronteira da submissão de tasks na fila

**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** `QueueStore::submit_task` aceitava chamadas de qualquer crate porque era pública, mas não recebia `Principal` nem aplicava a admissão governada de sessão, contexto de execução, quota, rate limit e plano. A API HTTP já usava a família `submit_task_governed*`, porém a superfície pública permitia que um consumidor Rust externo bypassasse essa fronteira.

**Decisão:** Reduzir `QueueStore::submit_task` para `pub(crate)` sob `cfg(test)`. A primitiva fica disponível exclusivamente aos testes internos do crate `shaka-queue`; consumidores externos e builds de produção não recebem esse método. Toda entrada externa deve usar `submit_task_governed` ou `submit_task_governed_with_plan`, que recebem o principal autenticado e aplicam a política transacional completa.

**Consequências:** O probe externo que compilava e executava uma submissão direta antes do patch passa a falhar com `E0599` porque `QueueStore` não possui `submit_task` no build público. Não há alteração no schema, na máquina de estados, na API HTTP, nas quotas, nos rate limits ou na matriz de capabilities. O fechamento é de superfície de API, não uma substituição das verificações governadas.

**Alternativas consideradas:** Remover ou reescrever a primitiva, rejeitado por ampliar o diff e o risco sobre persistência, retry e testes; manter `pub`, rejeitado porque deixa um caminho de bypass disponível; mover toda a fila para outro módulo, reservado para uma refatoração posterior com contrato de API separado.

**Evidência pré/pós:** `/home/ubuntu/full-audit/option-b-before-run.log` mostra o probe externo executando na base anterior; `/home/ubuntu/full-audit/option-b-after-2.log` mostra a falha pós-patch `E0599`, validada em `/home/ubuntu/full-audit/option-b-after-result.txt`. A suíte interna do queue e a suíte completa do workspace permaneceram verdes nos gates da branch.


## ADR-019 — Proveniência de request e aprovação na task

**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** A API já validava ou gerava `x-request-id` e o incluía nos eventos de auditoria do handler, mas o identificador não era persistido no `TaskEnvelope`. Ao processar a task em background, o worker não conseguia reconstruir de forma determinística a requisição que originou o trabalho. Além disso, o Plan Engine validava aprovações persistidas, mas a task não registrava qual aprovação global sustentou sua admissão.

**Decisão:** Adicionar `ExecutionContext.provenance`, contendo `request_id: Option<String>` e `admission_approval_id: Option<Uuid>`. A API passa o request ID ativo do middleware; a CLI gera um UUID local; e a fila sobrescreve qualquer proveniência presente no envelope com os valores derivados pelo host. Na admissão planejada, o `approval_id` é escolhido dentro da mesma transação entre as aprovações globais válidas para o plano, revisão, digest e tenant. Tasks diretas e planos sem aprovação usam `None`.

O worker reconstrói o `CorrelationContext` a partir do request ID persistido antes de criar spans, usando `task-<task_id>` como fallback estável para envelopes legados sem proveniência. O evento de finalização inclui o `admission_approval_id` quando existente. Trace ID e span ID não são persistidos, pois são identificadores temporários de observabilidade.

**Compatibilidade e segurança:** Os campos novos possuem defaults conservadores para desserializar envelopes antigos. O payload não escolhe request ID nem approval ID efetivo. A fila valida o limite bounded do request ID, mantém o isolamento por tenant e preserva os wrappers de submissão governada existentes. A aprovação por etapa continua sendo reavaliada pelo Plan Engine durante o claim; o campo singular registra somente a aprovação global da admissão.

**Consequências:** Retries, leases, claims e finalização podem ser correlacionados à mesma intenção original sem depender de um contexto task-local do processo HTTP. A prova anti-forgery confirma que valores inseridos no envelope são substituídos pelo host. A prova de aprovação confirma que o ID persistido corresponde ao registro aprovado, e não a um identificador enviado pelo cliente.

**Evidência pré/pós:** `/home/ubuntu/full-audit/request-provenance-before.log` falha antes do patch porque o envelope não contém `origin-request-1` após submissão HTTP. Os testes pós-patch em `request-provenance-core-test-3.log`, `request-provenance-queue-antiforgery.log`, `request-provenance-queue-approval.log` e `request-provenance-api-test-3.log` passam após a implementação. A suíte workspace posterior passou com 93 testes aprovados e os gates finais da fatia passaram com código 0.


## ADR-020 — Autoridade request-scoped na pós-etapa planejada

**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** O claim de uma etapa planejada já substituía as capabilities globais do worker pelas capabilities persistidas em `TaskEnvelope.execution_context`. Entretanto, a finalização da etapa reutilizava `PlanClaimContext.granted_capabilities`, que na API era montado a partir do runtime global. O `PlanVerifier` avalia pós-condições nessa fronteira, incluindo `PlanCondition::CapabilityGranted`; portanto, uma task de operador poderia ter uma pós-condição satisfeita pela capacidade administrativa do processo, apesar de o envelope não concedê-la.

**Decisão:** Em `finish_planned_step_tx`, o campo `PlanVerificationContext.granted_capabilities` deve ser copiado exclusivamente de `task.envelope.execution_context.capabilities`. O `PlanClaimContext` continua sendo a fonte para fatos observados pelo host — circuit breaker, orçamento, digest e demais dados de claim — mas deixa de ser autoridade de capability na pós-etapa.

**Consequências:** A seleção e a finalização de uma etapa usam a mesma autoridade request-scoped. Um worker com catálogo administrativo não pode fazer uma pós-condição parecer autorizada para uma task com menor privilégio. A mudança não concede capabilities, não altera a matriz de papéis, não muda o schema SQLite e não altera a política de aprovação.

**Evidência pré/pós:** O teste `planned_finish_uses_task_capabilities_not_worker_global` falhou no commit B com `left: Succeeded` e `right: Failed`, demonstrando que `CodeExecution` global era aceito indevidamente. Após o patch, o mesmo teste passou porque a capability ausente no envelope faz a pós-etapa falhar. A suíte do queue passou com 25 testes; core passou com 19; orchestrator passou com 7. A suíte workspace final passou com 93 testes aprovados e os gates finais passaram com código 0.

**Fora do escopo:** Não foram alterados commits, branches remotas, PRs, merges, publicação GitHub, validação do segundo ambiente ou regras de capabilities por papel.


## ADR-021 — Atestação da autoridade declarativa de skills

**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** A atestação Ed25519 anterior protegia nome, versão, operador, hash do artefato e justificativa, mas não protegia `permissions`, `input_schema` nem `output_schema` do `SkillManifest`. O registry materializa esses campos no artefato executável; portanto, uma mutação posterior do manifesto podia alterar a autoridade da ferramenta sem invalidar a assinatura do Wasm.

**Decisão:** Introduzir o protocolo `shaka-skill-approval-v2`. A aprovação V2 assina, além dos campos existentes, um digest SHA-256 canônico da autoridade declarativa. O digest representa permissões como conjunto ordenado e deduplicado, e normaliza objetos JSON recursivamente antes da serialização; arrays preservam sua ordem. O digest é persistido em `ApprovalRecord.manifest_authority_sha256`. O registry compara o digest atual com o aprovado antes de materializar a skill e o `WasmSkillTool` repete a validação antes da instanciação.

Atestações V1, mesmo assinadas por chave confiável, não são executáveis. Registros antigos continuam desserializáveis por compatibilidade de armazenamento, mas exigem reaprovação explícita no protocolo V2. A descrição permanece fora do digest por ser informativa; o nome e a versão já são campos assinados separadamente. O hash do artefato Wasm continua sendo verificado de forma independente.

**Consequências:** Alterações posteriores em permissões ou schemas falham fechadas com `ManifestAuthorityMismatch`; atestações sem digest V2 falham com `UnsupportedApprovalProtocol`. Uma skill V2 inalterada continua executável. A mudança exige reaprovação operacional de skills legadas e atualiza o smoke test para esperar V2. Não há migração automática, promoção silenciosa, mudança no sandbox ou concessão adicional de capabilities.

**Alternativas consideradas:** Manter V1 e confiar na integridade do arquivo, rejeitado porque o arquivo contém autoridade executável não assinada; assinar somente permissões, rejeitado porque schemas também controlam a fronteira do host; incluir a descrição no digest, não necessário para autoridade e com custo operacional de reaprovação por mudança informativa; migrar V1 automaticamente, rejeitado porque transformaria dados não vinculados em autoridade aprovada sem decisão humana.

**Evidência pré/pós:** `/home/ubuntu/full-audit/skill-authority-before.log` mostra o teste falhando antes do patch porque a permissão adulterada ainda era aceita. `/home/ubuntu/full-audit/skill-attestation-regression.log` mostra as regressões de permissões, schema e V1 rejeitada passando depois. `/home/ubuntu/full-audit/skill-attestation-gates-final3/summary.log` registra todos os gates oficiais com código 0, incluindo smoke em porta 29126.

**Limites:** A validação do alvo de fuzzing continua condicionada ao `Cargo.lock` próprio e às dependências disponíveis. A portabilidade no segundo ambiente, commits assinados, publicação GitHub e revisão/merge desta decisão já foram concluídas; o fuzzing permanece uma frente separada.

## ADR-022 — Revogação efetiva de skills residentes
**Status:** Integrada em `main` e validada no CI, sandbox e VM.
**Contexto:** O processo carregava uma `WasmSkillTool` validada no startup. A revogação posterior da skill ou da chave no registry/trust store persistido não era consultada pelo executor residente, portanto uma chamada nova continuava executando após a revogação.
**Decisão:** A CLI passa a construir skills com uma fonte de revalidação persistida. Antes de cada nova execução, o `WasmSkillTool` recarrega o `SkillRegistry` e o `TrustStore`, exige que a skill permaneça ativa, compara o artefato aprovado com o objeto residente e repete as verificações de hash, autoridade e assinatura V2. Qualquer falha de leitura, revogação, mudança ou ausência de registro falha fechado.
**Semântica operacional:** A revogação impede novas execuções depois do gate de autorização. Uma chamada que já passou pelo gate não é interrompida por esta fatia, evitando uma operação parcialmente executada e mantendo a decisão de execução determinística.
**Consequências:** Revogar uma skill ou chave passa a ter efeito dentro de um processo já iniciado. Há custo de I/O e desserialização antes de cada chamada de skill; esse custo foi escolhido em favor da segurança e pode ser otimizado futuramente apenas com uma prova equivalente de invalidação. A definição da ferramenta pode continuar aparecendo até uma nova chamada, mas a execução não atravessa o gate revogado.
**Alternativas consideradas:** Confiar em restart operacional, rejeitado porque deixa uma janela dependente de disciplina externa; interromper chamadas em andamento, rejeitado por risco de estado parcial; manter somente a validação de startup, rejeitado pela prova pré-patch; usar cache de epoch sem fonte transacional equivalente, adiado até haver necessidade de otimização mensurada.
**Evidência pré/pós:** `/home/ubuntu/full-audit/skill-revocation-before-rerun.log` mostra a segunda execução aceitando a revogação no código anterior, com `assertion failed: result.is_err()`. As regressões `revoked_skill_blocks_future_execution_of_resident_tool` e `revoked_key_blocks_future_execution_of_resident_tool` passam após o patch. Os gates completos estão em `/home/ubuntu/full-audit/skill-revocation-gates-final/summary.log`, com código 0.
**Limites:** O patch não adiciona endpoint administrativo, não interrompe chamadas em andamento, não conecta efeitos externos reais, não altera a matriz de capabilities e não resolve a inconsistência preexistente do lockfile do fuzz. A portabilidade e a publicação desta decisão já foram concluídas; os limites técnicos permanecem.

## ADR-023 — Bind não local sem efeitos persistentes em falha de inicialização
**Status:** Integrada em `main` e validada no CI, sandbox e VM.
**Contexto:** `ApiState::new` verificava a exigência de autenticação para bind não local somente depois de `QueueStore::bootstrap_principal`. Assim, uma inicialização sem `api_key` e sem token IAM ativo era rejeitada, mas podia deixar tenant, usuário e limites persistidos no SQLite.
**Decisão:** Validar a exigência de autenticação do bind não local imediatamente após `ApiConfig::validate` e antes de qualquer chamada que altere o `QueueStore`. Somente depois dessa validação o bootstrap do principal e a criação do circuit breaker podem ocorrer.
**Consequências:** Uma configuração de rede inválida falha sem efeitos persistentes. Bind loopback sem API key, bind não local com API key não vazia e bind não local com token IAM ativo preservam o comportamento permitido. Não há rollback compensatório nem limpeza posterior: a operação inválida deixa de iniciar qualquer mutação.
**Evidência pré/pós:** `/home/ubuntu/full-audit/api-startup-side-effect-before.log` mostra a prova falhando no commit B porque `list_tenants()` não permanecia vazio após a rejeição. `/home/ubuntu/full-audit/api-startup-side-effect-after.log` mostra a mesma regressão passando; `/home/ubuntu/full-audit/api-startup-side-effect-targeted.log` mostra também o caminho com API key passando. Os gates completos estão em `/home/ubuntu/full-audit/api-startup-side-effect-gates-final2/summary.log`, com código 0.
**Fora do escopo:** Não foram alterados autenticação por request, fallback loopback, tokens, schema SQLite, listener, capabilities, commits, publicação, portabilidade ou fuzzing.

## ADR-024 — Fencing transacional de leases de worker
**Status:** Integrada em `main` e validada no CI, sandbox e VM.
**Contexto:** A fila permitia que um worker atrasado finalizasse uma tarefa depois de a lease expirar, inclusive após recovery e novo claim por outro worker. O schema não persistia identidade de lease, e a finalização consultava apenas `task_id`, tenant e estado.
**Decisão:** Cada claim emite um `LeaseToken` opaco, persistido somente durante a lease ativa. As APIs de produção de finalização exigem esse token; antes de qualquer escrita, a transação confirma token correspondente e `lease_until` ainda futuro. Recovery no instante de expiração limpa o token, e toda transição que libera ou reencaminha a tarefa também o limpa. Registros legados recebem a coluna via migração automática, mas não recebem token até novo claim.
**Semântica operacional:** Uma chamada que já começou não é cancelada por esta fatia. Se ela terminar depois da expiração, sua escrita de sucesso, falha, cancelamento ou retry é rejeitada com `LeaseLost`; o worker seguinte permanece dono do estado. Replay de tarefa já terminal continua idempotente e não reabre nem sobrescreve o resultado.
**Consequências:** O token de fence não é serializado no `TaskRecord`, evitando vazamento da autoridade interna para contratos externos. O custo é uma coluna e uma comparação adicional por finalização, além da migração automática de bancos existentes. O código de produção não expõe mais finalização sem token; wrappers sem token existem somente sob `cfg(test)` para preservar fixtures internos.
**Alternativas consideradas:** Confiar apenas no recovery periódico, rejeitado porque mantém janela entre expiração e recovery; comparar somente `lease_until`, rejeitado porque não distingue o worker antigo do novo; interromper runtime em andamento, rejeitado pela política de não produzir efeitos parciais; invalidar por restart, rejeitado por depender de operação externa.
**Evidência pré/pós:** A prova equivalente no estado anterior aceitou a finalização do worker antigo depois do segundo claim e falhou na asserção de rejeição, código 101. As regressões pós-patch cobrem token antigo após recovery, expiração antes de recovery, owner atual, migração de schema e ausência do token na serialização. A rodada definitiva `/home/ubuntu/full-audit/shaka-lease-fencing-gates-final4/summary.log` registra 106 testes aprovados, 0 falhas, todos os gates com código 0 e smoke na porta 29133.
**Fora do escopo:** Não foram alterados o algoritmo de seleção de tarefas, a política de retry, a interrupção cooperativa do runtime, a portabilidade entre ambientes, commits assinados, publicação GitHub ou merge.


## ADR-025 — Shutdown graceful com cancelamento cooperativo de tasks
**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** O container declara `SIGTERM` como sinal de parada, mas o servidor aguardava somente `Ctrl-C` e não coordenava workers em voo. A prova pré-patch mostrou que o processo terminava com `SIGTERM`, enquanto uma task em execução permanecia `running` após restart no mesmo SQLite.

**Decisão:** Tratar `SIGTERM` e `SIGINT` como eventos de shutdown. Após o listener terminar, o host cancela todos os tokens de execução registrados, marca como cancelada a task interrompida pelo shutdown, aguarda cada worker por cinco segundos e aborta somente o `JoinHandle` próprio que exceder essa janela. A finalização continua protegida pelo `LeaseToken` e pela transação existente.

**Consequências:** Tasks interrompidas por parada coordenada alcançam estado terminal `cancelled`, evitando depender de expiração de lease após reinício. A política não interrompe efeitos externos já iniciados nem presume que um processo arbitrário possa ser encerrado; o timeout final é apenas uma barreira de disponibilidade do próprio servidor.

**Evidência pré/pós:** `/home/ubuntu/full-audit/shutdown-prepatch-run-final.log` registra `persisted_status_after_restart=running`. `/home/ubuntu/full-audit/shutdown-postpatch-worker-blocking.log` registra `persisted_status_after_restart=cancelled` com o mesmo cenário controlado.

## ADR-026 — Operações bloqueantes fora das futures Tokio
**Status:** Integrada em `main` e validada no CI, sandbox e VM; o escopo continua limitado ao caminho do worker.

**Contexto:** `WasmExecutor::execute` é síncrono por natureza e a fila usa `rusqlite` síncrono. Chamá-los diretamente dentro de uma future do worker pode bloquear uma thread Tokio e atrasar outras tasks, sinais de cancelamento e health checks.

**Decisão:** Expor `WasmExecutor::execute_async`, que move a execução para `spawn_blocking`, e usar essa API em `WasmSkillTool`. No worker da API, recovery, circuit breaker, claim, cancelamento, finish e auditoria de finish usam um wrapper `queue_blocking` baseado em `spawn_blocking`. As APIs síncronas permanecem disponíveis para CLI, testes e consumidores explicitamente bloqueantes.

**Consequências:** O caminho assíncrono deixa de compartilhar a thread Tokio com compilação/execução WASM ou I/O SQLite. O custo é usar a pool de blocking do Tokio e exigir que novas chamadas síncronas adicionadas ao worker sigam o mesmo padrão. O uso de `spawn_blocking` não transforma automaticamente operações externas em canceláveis; o cancelamento cooperativo continua sendo responsabilidade do runtime e do sandbox.

**Evidência pré/pós:** `/home/ubuntu/full-audit/sandbox-blocking-prepatch.log` mostra ticker de 10 ms atrasado para 173 ms pelo executor síncrono. `/home/ubuntu/full-audit/sandbox-blocking-postpatch.log` mostra o ticker em 11 ms com timeout WASM preservado. A regressão `async_execution_does_not_block_executor` passou, assim como os testes de API e orchestrator.


## ADR-027 — Budgets bounded em todas as fronteiras de execução
**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** A representação anterior aceitava valores máximos do tipo (`u32::MAX`/`u64::MAX`) quando a quota do tenant permitia, permitindo que uma tarefa chegasse à fila com passos, chamadas de ferramenta, duração ou custo patológicos. Uma suíte verde não cobria esse caminho de admissão.

**Decisão:** O host impõe e valida `ExecutionBudget` no core, na criação de `PlanSpec`, na submissão governada da fila e antes do runtime. Os limites máximos são `max_steps=256`, `max_tool_calls=512`, `max_elapsed_ms=300000` e `max_cost_microunits=10000000`. `max_steps` e `max_elapsed_ms` exigem valor mínimo `1`; `max_tool_calls` e `max_cost_microunits` podem ser `0`, significando nenhuma chamada ou nenhum custo permitido. Os valores exatamente iguais aos tetos são aceitos.

**Consequências:** Requests HTTP acima dos limites retornam `400 Bad Request` antes da admissão; consumidores Rust governados recebem erro de entrada; o runtime não executa um envelope inválido. O contrato mantém os campos serializáveis e não altera quotas de tenant nem a contabilidade de custo. Alterar os tetos exige atualizar os testes de fronteira, a documentação pública e esta decisão.

**Evidência pré/pós:** `/home/ubuntu/full-audit/budget-prepatch.log` demonstra que a fila anterior aceitava limites representáveis patológicos, incluindo `u32::MAX`, `u64::MAX` e custo `100000000` quando a quota era suficiente. `/home/ubuntu/full-audit/budget-postpatch.log` demonstra a rejeição pós-patch de `max_steps` acima de `256`. Os testes permanentes `execution_budget_accepts_defaults_and_exact_host_limits`, `execution_budget_rejects_each_invalid_boundary`, `execution_budget_allows_zero_tool_calls_and_zero_cost` e `api_rejects_budget_above_host_maximum` passaram no gate `/home/ubuntu/full-audit/phase6-targeted3.log`.

**Fora do escopo:** Não foi criado um sistema de preços, cobrança ou quota distribuída; não há alteração de política para `0` além da semântica já implementada; limites adaptativos por modelo e orçamento de rede permanecem decisões futuras.

## ADR-028 — Emissão IAM com expiração finita obrigatória
**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** A API de emissão de token aceitava `expires_at=None` e persistia um bearer sem expiração, criando credencial potencialmente eterna. O fato de a autenticação funcionar não era evidência suficiente de uma política segura; a falha foi reproduzida diretamente contra a base anterior.

**Decisão:** `QueueStore::issue_token` exige expiração explícita, futura e não superior a `7776000` segundos (90 dias). A emissão retorna `TokenIssue.expires_at` preenchido, embora o tipo `Option<DateTime<Utc>>` seja preservado nesta versão para compatibilidade de serialização; nenhum novo token pode ser emitido com `None`. O CLI torna `--expires-in-seconds` obrigatório e valida `1..=7776000` antes da emissão. O segredo bruto continua sendo devolvido somente no comando de emissão e não é retido nos logs de prova.

**Consequências:** Tokens existentes sem expiração, se houverem em um banco legado, não são migrados nem revogados automaticamente; a autenticação rejeita registros com `expires_at = NULL` e eles não contam como tokens ativos. Toda nova emissão é finita. A mudança de `Option` para tipo não opcional foi deliberadamente adiada para evitar quebra pública e deverá ser uma decisão separada se a compatibilidade puder ser removida.

**Evidência pré/pós:** `/home/ubuntu/full-audit/iam-token-prepatch.log` registra a base anterior emitindo `TokenIssue` com `expires_at: None` e `regression_status=FAIL_EXPECTED_ETERNAL_TOKEN_ACCEPTED`. `/home/ubuntu/full-audit/iam-token-postpatch.log` registra `none_status=REJECTED_EXPLICIT_EXPIRY_REQUIRED` e emissão válida autenticada com `expires_present=true`, omitindo o segredo bruto. `/home/ubuntu/full-audit/iam-cli-postpatch.log` registra ausência do argumento rejeitada e emissão com `3600` segundos aceita. O teste permanente `iam_token_issue_requires_finite_future_expiry` passou no gate `/home/ubuntu/full-audit/phase6-targeted3.log`.

**Fora do escopo:** Não houve rotação automática, revogação em massa, integração com provedor externo, armazenamento WORM, alteração de banco legado ou publicação GitHub.


## ADR-029 — Retenção não-negativa e verificação íntegra da auditoria persistida
**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** `purge_older_than` aceitava duração negativa. Como o cutoff era calculado por subtração, `--days=-1` apontava para o futuro e apagava um episódio recém-criado. Em auditoria, a verificação comparava o `event_hash` recalculado com o hash dentro do JSON, mas não comparava a coluna `event_hash` redundante persistida no SQLite; uma adulteração isolada dessa coluna era reportada como cadeia válida.

**Decisão:** A retenção negativa falha antes de qualquer `DELETE`, com erro tipado `InvalidRetention`; zero e valores positivos preservam a semântica existente. `verify_audit_chain` passa a selecionar `event_json` e `event_hash` e exige que a coluna persistida, o campo serializado e o hash recalculado sejam consistentes, além de validar tenant e elo anterior.

**Consequências:** Um operador não consegue transformar por acidente um expurgo em exclusão de memória recente. Adulterações tanto no JSON quanto na coluna redundante são detectadas. A decisão não cria retenção automática, não altera o escopo do expurgo para memória semântica e não fornece ancoragem externa.

**Evidência pré/pós:** `/home/ubuntu/full-audit/retention-negative-prepatch.log` registra `negative_days_deleted=1 remaining_recent=0` e falha esperada na base anterior. `/home/ubuntu/full-audit/retention-negative-postpatch.log` registra rejeição `InvalidRetention`, `remaining_recent=1` e `PASS_NEGATIVE_RETENTION_BLOCKED`. `/home/ubuntu/full-audit/audit-column-prepatch.log` registra `persisted_hash_tamper_valid=true`, demonstrando a lacuna anterior. `/home/ubuntu/full-audit/audit-column-postpatch.log` registra `persisted_hash_tamper_valid=false` e `failure_present=true`. Os testes permanentes do memory passaram em `phase7-memory-tests2.log` com 9 testes.

**Fora do escopo:** A cadeia continua local ao SQLite. Ancoragem WORM, assinatura externa, exportação remota e retenção regulatória precisam de decisão de infraestrutura, política de dados e operação de chaves; não foram inventadas nesta fase.

## ADR-030 — Limite de memória linear por execução WASM
**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** Fuel e interrupção por época limitavam CPU/tempo, mas `Store::new` não impunha limite explícito à memória linear do guest. Um módulo mínimo com uma página conseguia executar `memory.grow` para uma segunda página sob a política default.

**Decisão:** Cada `Store` WASM recebe `StoreLimits` via `Store::limiter`. A política `SandboxPolicy` ganha `max_memory_bytes`, com default de `16 MiB` e teto host-side de `64 MiB`; valores zero ou acima do teto são rejeitados. O guest pode observar `memory.grow=-1` quando o crescimento excede a política, sem transformar isso em alocação patológica do host.

**Consequências:** A memória linear criada ou expandida pelo módulo fica bounded por execução, somando-se ao fuel, timeout, ausência de imports e capabilities deny-by-default. A documentação deixa explícito que `ResourceLimiter`/`StoreLimits` não medem todas as estruturas internas do Wasmtime nem alocações do embedder; não são limite de RSS, cgroup ou substituto de isolamento de processo.

**Evidência pré/pós:** `/home/ubuntu/full-audit/sandbox-memory-prepatch.log` registra `memory_grow_result=1 policy_memory_limit_field=absent` na base anterior. `/home/ubuntu/full-audit/sandbox-memory-postpatch.log` registra `memory_grow_result=-1 policy_memory_limit_bytes=65536`. A regressão permanente `guest_memory_growth_is_bounded_by_policy` passou na suíte do sandbox, registrada em `phase7-sandbox-tests1.log`.

**Fora do escopo:** Não foi adicionado WASI, import de host, rede, filesystem, limite de RSS, cgroup, subprocesso ou sandbox de infraestrutura. O limite de memória de skills residentes e a cópia de dados por chamada continuam sujeitos ao processo já existente e exigem medição separada antes de otimização.


## ADR-031 — Smoke determinístico, gates alinhados e remoção de artefatos legados
**Status:** Integrada em `main` e validada no CI, sandbox e VM.

**Contexto:** O smoke lançava `serve` através da função shell `run` em background. Nesse formato, `$!` identificava um subshell wrapper, não o binário Shaka que possuía o listener. A prova mínima mostrou que matar somente o PID capturado deixava o binário filho e a porta ativos. O workspace atual também continha quatro artefatos históricos rastreados, mas fora do caminho ativo: `main.rs` e `lib.rs` na raiz, `ci.yml` na raiz e `.github/release.yml` fora de `.github/workflows/`. O metadata do Cargo listava dez pacotes `crates/*` e nenhum pacote raiz.

**Decisão:** `scripts/production_smoke.sh` inicia o binário `$BIN` diretamente e registra esse PID para `kill`/`wait`. A CI principal e o workflow de release ativo usam `cargo test --workspace --locked --all-targets` e validam `bash -n ./scripts/production_smoke.sh`; os demais gates fixados permanecem. Os quatro artefatos históricos comprovadamente inativos são removidos do checkout.

**Consequências:** O cleanup atua sobre o processo que possui o listener, reduzindo risco de processo órfão em CI. Os workflows de PR e release executam os mesmos targets de teste e detectam erro de sintaxe do smoke antes da execução. A remoção evita que código/CI da era v0.2 seja confundido com a arquitetura workspace v0.8.2. O workflow ativo `.github/workflows/release.yml` foi preservado; somente o YAML fora da pasta reconhecida pelo GitHub Actions foi removido.

**Evidência pré/pós:** `/home/ubuntu/full-audit/smoke-pid-harness-prepatch.log` registra `captured_parent_pid=333404`, filho Shaka distinto e `after_parent_kill parent_alive=false child_alive=true listener_alive=true`. `/home/ubuntu/full-audit/smoke-cleanup-postpatch.log` registra `smoke_exit=0`, execução funcional completa; `/home/ubuntu/full-audit/smoke-cleanup-postpatch-listener.log` registra `listener_after_cleanup=false`. `/home/ubuntu/full-audit/phase8-preproof-inventory.log` registra os dez pacotes do workspace e nenhuma referência operacional aos arquivos legados. `/home/ubuntu/full-audit/phase8-ci-final-validation.log` registra policy, preflight, secret scan, fmt, metadata e diff check aprovados.

**Fora do escopo:** Não foram alterados gatilhos, permissões de release ou mecanismo de daemonização do servidor. A eliminação dos artefatos legados foi integrada à `main` e validada no ciclo de hardening; branch remota e política de publicação seguem governadas pelo fluxo repository-first.

## ADR-032 — Transações SQLite de escrita iniciam imediatamente sob concorrência
**Status:** Integrada em `main` e validada no CI, sandbox e VM.
**Contexto:** A validação integrada reproduziu erro intermitente `SQLITE_BUSY`/`database is locked` em 5 de 10 E2E, alternando entre criação e aprovação de plano. O worker escrevia memória/auditoria enquanto a fila iniciava uma transação `DEFERRED`, lia o snapshot e depois tentava promovê-lo a escritor. O comportamento era particularmente perigoso porque a API convertia a falha persistente em erro HTTP 500.
**Decisão:** Conexões de `QueueStore` e `MemoryStore` recebem `busy_timeout` de 5 segundos explicitamente. As transações de escrita da fila e do Plan Store usam `TransactionBehavior::Immediate`, reservando a escrita antes da leitura que faz parte da mesma operação. A transação puramente de leitura de `validate_plan` permanece deferred.
**Consequências:** A disputa normal por writer aguarda dentro de uma janela bounded; uma promoção de snapshot que poderia falhar no meio da transação é evitada. O timeout não transforma lock persistente em sucesso: após a janela, SQLite continua retornando erro e o host permanece fail-closed. O padrão deve ser reavaliado se a aplicação migrar para múltiplos processos ou um backend transacional diferente.
**Evidência pré/pós:** `/home/ubuntu/full-audit/phase9-e2e-stability-summary.log` registra 5 sucessos e 5 falhas antes da correção; `/home/ubuntu/full-audit/phase9-e2e-debug-failure3.json` e o log preservado correspondente registram `DatabaseBusy` em `plan.create`. Após `busy_timeout`, `BEGIN IMMEDIATE` e a correção do timing assíncrono do harness, `/home/ubuntu/full-audit/phase9-e2e-final-stability-summary.log` registra 10/10 E2E aprovados. As regressões permanentes `append_episode_waits_for_concurrent_writer` e `save_plan_waits_for_concurrent_writer_before_read_then_write` passaram 1/1 cada em `/home/ubuntu/full-audit/phase9-memory-lock-regression-corrected.log` e `/home/ubuntu/full-audit/phase9-plan-lock-regression-corrected.log`.
**Base técnica:** A documentação oficial do SQLite explica que `BEGIN DEFERRED` pode iniciar leitura e falhar ao promover para escrita, enquanto `BEGIN IMMEDIATE` inicia a escrita imediatamente; também define que `busy_timeout` aguarda locks até o limite configurado.[1] [2]
**Fora do escopo:** Não foi introduzido pool de conexões, retry cego de transações, alteração de isolamento, migração de banco ou suporte multi-processo. O listener histórico em 29130–29135 pertence a execuções anteriores e não foi encerrado por não haver autorização para matar processo não identificado como desta rodada.

[1]: https://sqlite.org/lang_transaction.html "SQLite Transaction — DEFERRED, IMMEDIATE e SQLITE_BUSY"
[2]: https://sqlite.org/c3ref/busy_timeout.html "SQLite C Interface — Set A Busy Timeout"

## ADR-033 — Limite de payload episódico sob SQLite

**Status:** Em validação no ciclo P0-D.

**Contexto:** `MemoryStore::append_episode` aceitava qualquer tamanho de `content` antes de adquirir o lock ou executar a escrita SQLite. A reprodução no baseline inseriu um único conteúdo de 65.537 bytes sem erro; em um caminho interno ou consumidor Rust, isso permitia uma escrita patológica e não havia um contrato de limite por registro. A existência de `busy_timeout` e de transações imediatas não limita o tamanho de uma entrada.

**Decisão:** O host rejeita `EpisodicRecord.content` acima de 65.536 bytes, antes do lock e da escrita SQLite, com o erro tipado `PayloadTooLarge`. O limite exato é aceito. O teste permanente também exercita oito conexões concorrentes escrevendo 256 episódios bounded e verifica que todos os registros sobrevivem com integridade válida.

**Consequências:** Uma única entrada não pode consumir memória e I/O desproporcionais ao contrato episódico. A rejeição não modifica o banco e não é retryable sem reduzir o payload. O limite é por registro; não representa limite total de arquivo, retenção, quota de disco ou política de RPO/RTO.

**Evidência pré/pós:** `/home/ubuntu/full-audit/p0d-oversized-episode-red.log` registra a regressão executada no baseline com `red_test_exit=101` e a escrita indevida aceita. `/home/ubuntu/full-audit/p0d-oversized-episode-green.log` registra a mesma regressão passando após a guarda. `/home/ubuntu/full-audit/p0d-memory-validation.log` registra 19 testes do crate `shaka-memory`, incluindo a fronteira exata e o estresse concorrente.

**Fora do escopo:** Não foram adicionados limite total de banco, retenção automática, pool de conexões, retry cego, multi-instância, cgroup, armazenamento externo ou RPO/RTO de infraestrutura. Qualquer desses itens exige decisão e testes próprios.

## ADR-034 — Quota total configurável do arquivo SQLite

**Status:** Em validação no ciclo P0-E.

**Contexto:** O arquivo SQLite compartilhado por `MemoryStore` e `QueueStore` configurava WAL, `busy_timeout` e o schema, mas não configurava nem verificava uma quota total. A reprodução no baseline `4131979e2ffec174dc0f5085af24ab417bd9af2c` observou `page_size=4096`, `page_count=62` e `max_page_count=4294967294`, permitindo crescimento até o limite padrão do SQLite. O limite por registro de 65.536 bytes do ADR-033 não controla o crescimento acumulado de memória episódica, fila, planos, IAM e auditoria.

**Decisão:** O host aplica uma quota configurável ao arquivo SQLite por `--database-max-bytes` ou `SHAKA_DATABASE_MAX_BYTES`. O default aprovado é `268435456` bytes (256 MiB), e valores abaixo de `1048576` bytes (1 MiB) são rejeitados antes da abertura. `MemoryStore` e `QueueStore` calculam o `max_page_count` a partir do `page_size`, aplicam o limite e leem de volta o valor efetivo; divergência falha fechada. Os métodos `open` e `in_memory` mantêm o default, enquanto variantes explícitas permitem testes e configurações validadas. `SQLITE_FULL` é mapeado para um erro estável de banco cheio, sem retry cego.

**Consequências:** O crescimento do arquivo SQLite fica limitado por uma política explícita e consistente em todos os caminhos da CLI. A granularidade é de página, portanto o limite efetivo é arredondado para baixo ao `page_size`. Uma escrita que atinge a quota não deve ser repetida automaticamente e não deve ser interpretada como falha transitória do modelo. O resumo seguro de configuração passa a informar `database_max_bytes`, sem incluir credenciais.

**Evidência pré/pós:** `/home/ubuntu/full-audit/p0e-quota-red.log` registra a reprodução válida no baseline, com `red_test_exit=101`, porque `max_page_count=4294967294` excedia a expectativa finita do harness. Os testes permanentes adicionados em `shaka-memory`, `shaka-queue`, `shaka-config` e `shaka-cli` cobrem aplicação, reabertura, valor inválido, default, valor explícito, falha de escrita por banco cheio e ausência de linha episódica parcial. A validação verde completa será registrada após os gates locais e o ciclo repository-first.

**Base técnica:** O SQLite documenta `max_page_count` como o limite máximo de páginas do arquivo, `page_count` e `page_size` como as medidas para derivar o tamanho efetivo, `SQLITE_FULL` como falha de escrita quando o banco/disco está cheio e WAL como mecanismo que usa arquivo auxiliar até checkpoints.[1] [2] [3] [4]

**Fora do escopo:** Esta decisão não estabelece quota física de todo o filesystem, não limita o tamanho máximo do arquivo `-wal` durante uma janela de checkpoint, não implementa retenção automática, compactação, cgroup, pool de conexões, backend distribuído, backup externo ou RPO/RTO. Aumentar a quota em uma implantação requer decisão operacional explícita; um processo externo que edite o SQLite diretamente não está sob a fronteira governada do Shaka.

[1]: https://sqlite.org/pragma.html "SQLite PRAGMA statements — max_page_count, page_count e page_size"
[2]: https://sqlite.org/limits.html "SQLite Implementation Limits"
[3]: https://sqlite.org/rescode.html "SQLite Result and Error Codes — SQLITE_FULL"
[4]: https://sqlite.org/wal.html "SQLite Write-Ahead Logging"
