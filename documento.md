# Documento de Continuidade do Shaka-Agente

**Finalidade:** transferir o entendimento técnico e operacional do Shaka-Agente para o próximo desenvolvedor, engenheiro de confiabilidade ou agente de IA que precise manter, validar ou evoluir o sistema.

**Data de elaboração:** 2026-08-28
**Estado de referência:** `origin/main` em `4bf4fe75aa31a29fec71cc43d3c168a54da5e577`
**Release funcional:** `v0.8.2`
**Autor:** Manus AI

> **Regra central:** este documento ensina como continuar o sistema sem transformar uma execução verde em uma afirmação sem prova. Toda mudança relevante deve ter hipótese, reprodução, correção mínima, teste direcionado, validação completa e evidência vinculada ao SHA exato.

## 1. Como usar este documento

Leia este material na ordem apresentada na primeira transferência. A pessoa ou agente que assumir o trabalho deve começar confirmando o SHA remoto, a limpeza do checkout e a versão dos toolchains. Depois deve ler os contratos do repositório, executar uma validação mínima e somente então escolher um novo escopo.

Este guia é um mapa de continuidade, não substitui os contratos mais detalhados do projeto. Quando houver diferença entre uma memória informal e o conteúdo versionado, o repositório publicado e o SHA conferido no GitHub são a fonte da verdade. O operador nunca deve presumir que o checkout local está em `main` apenas porque a pasta possui esse nome.

## 2. Estado executivo do sistema

O Shaka é um agente de IA em Rust para execução governada de tarefas. Ele recebe um objetivo, usa um provedor de modelo local ou compatível com OpenAI, valida ferramentas no host, persiste memória e auditoria em SQLite e executa módulos WASM em sandbox restrito. O agente pode sugerir melhorias, mas não pode promover suas próprias skills, alterar políticas ou conceder autoridade a si mesmo.

A versão atual é uma **base operacional local controlada**, não uma implantação pública irrestrita. O padrão é loopback, provedor local, tarefas em `dry-run` e rejeição de efeitos externos. Planos `live`, mensageria externa, pesquisa autônoma na web, autopromoção de skills e subagentes irrestritos continuam bloqueados ou fora do escopo.

| Área | Estado atual comprovado |
|---|---|
| Workspace Rust | 11 crates modulares, edition 2024, MSRV declarado 1.85 |
| Toolchain de validação | Rust/Cargo 1.98.0 no CI e nos ciclos de validação registrados |
| Memória | Working, episódica e semântica persistentes em SQLite |
| Auditoria | Cadeia de hashes por tenant, com redaction e verificação administrativa |
| Fila | Sessões, idempotência, prioridade, leases, retry, cancelamento e circuit breaker persistentes |
| API | HTTP local com health/readiness, sessões, tarefas e cancelamento |
| Sandbox | Wasmtime deny-by-default, sem WASI, rede, filesystem ou imports do host |
| Skills | Catálogo com estados, hash, aprovação humana e revogação |
| Quota SQLite | Configurável; default 256 MiB; mínimo 1 MiB; aplicada em `QueueStore` e `MemoryStore` |
| P0-A a P0-E | Concluídos, mesclados e validados em sandbox e VM |
| Produção pública | Não autorizada pela versão atual |

## 3. Fonte de verdade e divergências documentais

O SHA de referência atual é:

```text
4bf4fe75aa31a29fec71cc43d3c168a54da5e577
```

Esse commit é o estado corrente da `main`, após o merge da PR #47, que incorporou o guia de continuidade e handoff. A quota total do SQLite permanece integrada no histórico pela PR #46. A branch de feature da quota continua preservada em:

```text
chore/p0e-sqlite-quota -> 38eea16fcbe7bd9599be65a4ed7ecdcba9540a36
```

Alguns documentos históricos do repositório ainda apontam para o estado anterior `48eeed2` e para a série de PRs #31–#38. Isso é uma **referência histórica**, não uma indicação de que os merges P0-E ou #47 não ocorreram. O inventário histórico de 100 avisos `missing_docs` pertence ao estado `48eeed2`; a coleta explícita no estado corrente `4bf4fe75` registrou zero avisos. Antes de qualquer nova atualização, confirme `git rev-parse origin/main`, compare o conteúdo real e faça uma alteração documental isolada. Não reescreva históricos para apagar referências antigas.

## 4. Arquitetura em linguagem simples

O sistema é dividido para que o modelo não seja a autoridade final. O modelo produz uma intenção; o host decide se a intenção possui ferramenta conhecida, schema válido, capability concedida, orçamento disponível e modo de execução permitido.

| Crate | Responsabilidade | O que o próximo mantenedor deve proteger |
|---|---|---|
| `shaka-core` | Tipos, identidade, políticas, contratos e Plan Engine | Estados, invariantes, digest canônico e autorização no host |
| `shaka-config` | Configuração tipada, ambiente, papéis e validações | Fail-closed para credenciais, HTTPS, live e quotas inválidas |
| `shaka-memory` | SQLite, memória, episódios, semântica e auditoria | Integridade, isolamento por tenant, transações e limites de escrita |
| `shaka-queue` | Fila, sessões, leases, retry, cancelamento e circuito | Idempotência, claim transacional e recuperação após crash |
| `shaka-api` | Rotas HTTP, autenticação, workers e integração | Sanitização, status estável, correlação e não vazamento de segredos |
| `shaka-orchestrator` | Modelo, ferramentas, orçamento e runtime | Validação antes da execução e ausência de autoridade implícita |
| `shaka-sandbox` | Wasmtime e política WASM | Deny-by-default, fuel, tempo e imports proibidos |
| `shaka-skills` | Catálogo, estados, aprovação e revogação | Hash completo, revisão humana e máquina de estados |
| `shaka-observability` | Tracing, redaction e auditoria | Correlação útil sem payloads sensíveis |
| `shaka-cli` | Interface operacional e composição dos serviços | Propagação uniforme da configuração e dos papéis |

O fluxo típico é: o operador cria uma sessão; uma tarefa com `Idempotency-Key` entra na fila; um worker obtém um lease; o runtime executa em `dry-run` por padrão; ferramentas são validadas no host; o resultado é persistido; auditoria e tracing permitem reconstruir a operação. Após uma interrupção, leases podem ser recuperados, mas fronteiras ambíguas não devem receber retry cego.

## 5. Invariantes que não podem ser quebrados

O próximo mantenedor deve tratar estas propriedades como contratos de segurança, não como preferências de implementação.

| Invariante | Consequência prática |
|---|---|
| Autoridade pertence ao host | Saída do modelo nunca executa diretamente e nunca muda policy |
| `dry-run` é o padrão | Não descrever simulação como ação externa realizada |
| `live` exige governança explícita | Não habilitar `--live` por conveniência ou teste local |
| Conteúdo externo é dado não confiável | Prompt, web, skill e resultado de ferramenta não são instruções de sistema |
| Skill candidata não é skill ativa | Criar candidata não pode carregar código automaticamente |
| WASM é deny-by-default | Não adicionar WASI, rede, filesystem ou import de host sem novo threat model |
| Tenant e papel vêm do host | Não confiar em tenant, role ou capability fornecidos pelo modelo |
| Escritas críticas são transacionais | Não trocar `Immediate` por transação deferred sem reprodução concorrente |
| Ambiguidade após crash é `unknown` | Não presumir sucesso nem repetir uma operação externa |
| Auditoria é append-only no modelo atual | Não editar ou apagar eventos manualmente para “corrigir” a cadeia |
| Segredos não entram em logs ou commits | API keys, tokens, bancos e backups ficam fora do versionamento |
| Merge só ocorre por PR protegida | Não usar push direto, `--admin`, bypass, force push ou exclusão automática |

## 6. Persistência SQLite e limites atuais

O arquivo SQLite é compartilhado por componentes de memória, fila, planos e auditoria. As conexões utilizam WAL, `busy_timeout` de 5 segundos e transações imediatas onde a operação exige serialização. O P0-D adicionou limite de 65.536 bytes para `EpisodicRecord.content`; valores de 65.537 bytes são rejeitados antes da escrita.

O P0-E adicionou quota total configurável por `--database-max-bytes` ou `SHAKA_DATABASE_MAX_BYTES`. O default é `268435456` bytes (256 MiB) e valores abaixo de `1048576` bytes (1 MiB) são rejeitados antes da abertura. O limite usa `PRAGMA max_page_count` em cada conexão governada e é verificado pela própria conexão.

> **Atenção:** a quota do SQLite não é uma quota física completa do sistema operacional. Ela não substitui a medição do filesystem e não transforma automaticamente WAL, SHM, logs, temporários ou outros processos em parte de um orçamento global. Também não existe ainda retenção automática ou compactação segura.

Quando ocorrer `DatabaseFull`, a conduta correta é parar o retry cego, preservar logs e banco, verificar o espaço físico, fazer backup consistente se possível e decidir entre retenção autorizada, aumento explícito da quota ou migração. Nunca contorne a quota escrevendo diretamente no banco ou alterando PRAGMAs manualmente sem uma decisão registrada.

## 7. Segurança, identidades e skills

A configuração local usa `operator`, `reviewer` e `administrator`. Operações de auditoria, backup e restore exigem papel administrativo. Aprovação ou revogação de skills exige papel autorizado, justificativa e hash SHA-256 do artefato real.

O catálogo de skills possui máquina de estados. Uma candidata ainda não está ativa; uma skill revogada não deve voltar a ser executada apenas porque o arquivo continua no filesystem. Não edite `skills.json` ou arquivos de confiança manualmente para contornar transições. Use a CLI e preserve a evidência da operação.

```bash
cargo run -p shaka-cli -- skill candidate relatorio \
  "Gera um relatório estruturado" \
  --permissions memory-write

cargo run -p shaka-cli -- skill list

cargo run -p shaka-cli -- --role reviewer skill approve relatorio \
  --artifact artefato.wasm \
  --reason "Aprovada após revisão manual e testes locais"
```

Chaves de modelo e tokens IAM devem ser injetados por ambiente seguro ou cofre externo. Nunca coloque segredos em issues, prompts, `data/`, logs, fixtures ou este documento. O token bruto só deve aparecer durante a emissão; o banco persiste hash e referências redacted.

## 8. Onboarding técnico do próximo Dev ou IA

O onboarding deve ser executado em etapas pequenas. Primeiro, o responsável deve clonar ou atualizar o repositório sem assumir que o checkout atual está correto.

```bash
gh repo clone Head-1/Shaka-Agente
cd Shaka-Agente
git fetch origin main
test "$(git rev-parse origin/main)" = \
  "4bf4fe75aa31a29fec71cc43d3c168a54da5e577"
git switch --detach origin/main
test -z "$(git status --porcelain)"
```

Em seguida, leia pelo menos `README.md`, `ARCHITECTURE.md`, `SECURITY.md`, `SKILL_GOVERNANCE.md`, `RUNBOOK_OPERACIONAL.md`, `API_CONTRACTS.md`, `TESTING_STRATEGY.md` e `docs/VALIDACAO_REPOSITORY_FIRST.md`. O arquivo `MEMORANDO_DE_HANDOFF_ONBOARDING.md` fornece um resumo anterior; este documento acrescenta o estado pós-P0-E.

Confirme o ambiente e execute uma validação inicial:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version
cargo --version
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo run -p shaka-cli -- sandbox-demo
cargo run -p shaka-cli -- config
cargo run -p shaka-cli -- doctor
```

O onboarding só está completo quando a pessoa consegue explicar a diferença entre `/healthz` e `/readyz`, executar uma tarefa local em `dry-run`, consultar `memory recent`, verificar auditoria, criar uma skill candidata sem ativá-la e descrever por que um restore nunca deve substituir diretamente o único banco operacional.

## 9. Desenvolvimento repository-first

Toda mudança deve nascer de uma branch criada a partir do `origin/main` atual. A branch de trabalho P0-E já está concluída; para um novo escopo, use outro nome e não reaproveite a branch histórica.

```bash
git fetch origin main
git switch --detach origin/main
git switch -c feat/<escopo-curto>

# implementar apenas o escopo aprovado
git diff --check
git status --short
```

Antes de editar, escreva a hipótese do problema. Para bugs, o teste deve falhar no baseline por uma razão funcional clara. Uma tentativa que executa zero testes, falha no linker, falha por lock ou falha por espaço em disco não conta como reprodução do bug.

Depois da correção, execute a mesma regressão. Um teste verde isolado não é suficiente. Rode a suíte incremental, a suíte completa, Clippy, verificações de segredo, políticas de workflow e o contrato repository-first. Registre comandos, SHA, saída relevante e qualquer falha ambiental separadamente.

## 10. Commits assinados, CI e PR

A configuração esperada para commits é SSH signing. A chave privada nunca deve ser lida, copiada ou colocada em logs.

```bash
git config --get gpg.format
git config --get commit.gpgsign
git config --get user.signingkey
git commit -S -m "tipo(escopo): descrição curta"
git log -1 --show-signature --format=fuller
```

Antes de abrir a PR, confirme que o commit assinado possui a base correta e que só arquivos do escopo foram alterados. Publique apenas a branch:

```bash
git push -u origin feat/<escopo-curto>
```

A PR deve incluir hipótese, red→green, correção, testes, limites, SHA e evidências. O CI deve estar concluído no SHA exato. Não use auto-merge para contornar uma confirmação específica. Cada merge exige nova confirmação explícita do proprietário.

## 11. Contrato de validação completo

O executor oficial é `scripts/validate_postmerge.sh`. Ele deve rodar em checkout limpo, com `SHAKA_EXPECTED_HEAD` apontando para o SHA que está sendo validado.

```bash
SHAKA_EXPECTED_HEAD="$(git rev-parse HEAD)" \
SHAKA_SMOKE_API_PORT=29143 \
SHAKA_LIFECYCLE_API_PORT=29144 \
SHAKA_VALIDATION_LOG="$HOME/shaka-validation.log" \
bash scripts/validate_postmerge.sh
```

O contrato verifica toolchain, sintaxe, fmt, `check --locked`, testes all-targets, Clippy, secret scan, workflow policy, testes Python, version preflight, cargo audit, build da CLI, build dos probes, smoke, lifecycle e crash probes de fila e auditoria.

A execução só é aprovada quando terminar com todos os seguintes sinais:

```text
postmerge_validation=PASS
validation_exit=0
process_crash_probes=PASS
working_tree=clean
port_cleanup=PASS
```

Warnings, especialmente `missing_docs`, devem ser registrados como dívida de qualidade. Eles não podem ser promovidos silenciosamente a “falhas críticas”, mas também não devem ser esquecidos. O saldo de 100 avisos foi medido historicamente no SHA `48eeed2`; no SHA corrente `4bf4fe75`, a coleta explícita com `-W missing_docs` registrou zero avisos. Toda medição deve identificar o SHA validado e ser repetida após cada lote documental. Se a execução parar em linker, falta de espaço, dependência ausente, porta ocupada ou processo residual, classifique a causa e corrija o ambiente de forma não destrutiva antes de concluir.

## 12. Validação no segundo ambiente

A VM é um ambiente de validação independente, não apenas uma repetição informal do resultado do sandbox. O operador deve conferir o SHA remoto, usar checkout detached e preservar a árvore limpa.

```bash
MERGE_OR_FEATURE='<sha-publicado>'
cd "$HOME/TESTE_IA/Shaka-Agente/Shaka-Agente"
git fetch origin main
test "$(git rev-parse HEAD)" = "$MERGE_OR_FEATURE"
test -z "$(git status --porcelain)"
```

Não aceite como prova uma saída que contenha somente um prompt, um SHA antigo ou um marcador de outro bloco. A evidência deve mostrar o SHA exato, o resultado do contrato, `validation_exit=0`, crash probes, working tree limpo e liberação das portas dedicadas.

## 13. Operação cotidiana

Para configuração e diagnóstico locais:

```bash
cargo run -p shaka-cli -- config
cargo run -p shaka-cli -- doctor
cargo run -p shaka-cli -- memory recent --limit 20
cargo run -p shaka-cli -- --role administrator verify-audit
```

Para iniciar a API de forma segura:

```bash
cargo run -p shaka-cli -- serve \
  --bind 127.0.0.1:8080 \
  --workers 2

curl --fail --silent http://127.0.0.1:8080/healthz
curl --fail --silent http://127.0.0.1:8080/readyz
```

`/healthz` indica que o processo responde e expõe saúde mínima. `/readyz` é mais rigoroso: verifica integridade, auditoria, fila e circuito; `200` com `ready` permite operação, enquanto `503` com `failed` significa que o processo está vivo, mas não apto a receber tráfego operacional.

Toda submissão de tarefa deve conter `Idempotency-Key`. Repetir a mesma chave com payload idêntico deve retornar a tarefa existente. Reutilizar a chave com intenção divergente deve ser rejeitado.

## 14. Backup, restore e recuperação

Backup e restore exigem administrador. Faça backup antes de mudanças relevantes e restaure primeiro em banco separado.

```bash
cargo run -p shaka-cli -- --role administrator \
  backup --output "backups/shaka-$(date -u +%Y%m%dT%H%M%SZ).db"

cargo run -p shaka-cli -- --role administrator \
  --database data/restore-test.db \
  restore --input backups/shaka-arquivo.db

cargo run -p shaka-cli -- --role administrator \
  --database data/restore-test.db doctor

cargo run -p shaka-cli -- --role administrator \
  --database data/restore-test.db verify-audit
```

O restore rejeita snapshot SQLite íntegro, porém incompatível com o schema obrigatório do Shaka, antes de copiar os dados. Se falhar, preserve origem, destino e erro; não substitua tabelas manualmente e não apague o banco original.

## 15. Resposta a incidentes

Trate como incidente crítico uma execução externa não autorizada, autopromoção de skill, vazamento de segredo, violação de tenant, corrupção de auditoria, escape do sandbox ou alteração indevida de logs.

A resposta inicial deve conter o sistema, interromper novas execuções se necessário, preservar banco e logs, verificar auditoria, revogar a skill afetada e rotacionar credenciais potencialmente expostas. Não corrija a evidência apagando registros. Não faça `reset --hard` para esconder o estado do repositório e não mate processos ou listeners que não pertençam ao teste em andamento.

| Sintoma | Primeira ação |
|---|---|
| `unknown` após crash | Parar retry e exigir decisão humana |
| `DatabaseFull` | Preservar evidência, medir filesystem e decidir retenção/quota |
| `payload too large` | Reduzir resumo ou escopo; não escrever direto no SQLite |
| cadeia de auditoria inválida | Bloquear transições e abrir incidente |
| `HostImportsDenied` | Preservar artefato e revisar threat model |
| bearer inválido | Não repetir com credenciais improvisadas; revisar autenticação |
| porta residual de teste | Identificar se pertence ao teste próprio antes de agir |

## 16. Backlog e próximos passos

O P0-A a P0-E está encerrado. Os itens BR-01 a BR-06 também estão documentados como corrigidos e validados. O que segue é trabalho futuro e não deve ser inferido como resolvido:

| Frente futura | Pergunta que precisa ser respondida antes do código |
|---|---|
| Quota física do ambiente | Qual orçamento inclui SQLite, WAL, logs, temporários e outros processos? |
| Retenção e compactação | O que pode ser removido sem violar auditoria, privacidade ou recuperação? |
| RPO/RTO | Quanto de perda de dados e tempo de restauração são aceitáveis? |
| Backup externo | Onde ficam cópias criptografadas e como a restauração será testada? |
| Observabilidade | Quais métricas e alertas operacionais serão obrigatórios? |
| Hardening | Qual supervisor, filesystem, permissão, rotação e isolamento serão usados? |
| Warnings | Quais lotes de documentação reduzem `missing_docs` sem esconder problemas? |
| Produção pública | Qual threat model autoriza IAM remoto, HTTPS e efeitos externos? |

Cada frente deve ter ADR, hipótese, critérios de aceitação e validação em dois ambientes. Não iniciar várias dessas frentes simultaneamente apenas porque parecem relacionadas.

## 17. Checklist de transferência concluída

A transferência para o próximo Dev ou IA só deve ser considerada concluída quando todas as respostas forem “sim”.

| Verificação | Concluída |
|---|---:|
| O responsável confirmou `origin/main` pelo SHA real? | ☐ |
| Leu arquitetura, segurança, runbook, contratos e validação? | ☐ |
| Consegue explicar por que o modelo não tem autoridade implícita? | ☐ |
| Executou testes locais e `sandbox-demo`? | ☐ |
| Consultou `/healthz` e `/readyz`? | ☐ |
| Executou uma tarefa em `dry-run` e distinguiu simulação de efeito externo? | ☐ |
| Criou skill candidata sem promovê-la automaticamente? | ☐ |
| Sabe restaurar um backup em cópia separada? | ☐ |
| Sabe interpretar `DatabaseFull`, `unknown` e `payload too large`? | ☐ |
| Sabe executar o contrato repository-first no SHA exato? | ☐ |
| Sabe que cada merge exige confirmação específica? | ☐ |
| Registrou operador, tenant, retenção, localização do banco e pendências? | ☐ |

## 18. Referências primárias

- [`README.md`](README.md) — visão geral, uso e limitações.
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — arquitetura e responsabilidades dos crates.
- [`SECURITY.md`](SECURITY.md) — threat model e regras não negociáveis.
- [`RUNBOOK_OPERACIONAL.md`](RUNBOOK_OPERACIONAL.md) — operação, diagnóstico e recuperação.
- [`API_CONTRACTS.md`](API_CONTRACTS.md) — contratos tipados e fronteiras HTTP/Rust.
- [`TESTING_STRATEGY.md`](TESTING_STRATEGY.md) — estratégia e critérios de evidência.
- [`docs/VALIDACAO_REPOSITORY_FIRST.md`](docs/VALIDACAO_REPOSITORY_FIRST.md) — protocolo executável de validação.
- [`docs/BACKLOG_STATUS.md`](docs/BACKLOG_STATUS.md) — status histórico BR-01 a BR-06.
- [`MEMORANDO_DE_HANDOFF_ONBOARDING.md`](MEMORANDO_DE_HANDOFF_ONBOARDING.md) — handoff anterior.
- O closeout técnico do P0-E deve ser consultado no arquivo de auditoria externo associado ao ciclo; ele não é requisito para compilar o repositório.

Referências externas:

[1]: https://sqlite.org/pragma.html#pragma_max_page_count "SQLite — PRAGMA max_page_count"
[2]: https://sqlite.org/rescode.html#full "SQLite — SQLITE_FULL"
[3]: https://docs.github.com/en/pull-requests/collaborating-with-pull-requests "GitHub — Pull Requests"
