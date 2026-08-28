# Shaka

O Shaka é um agente de IA **extensível, auditável e governado pelo operador**, implementado em Rust. A arquitetura prioriza contratos tipados, execução mediada pelo host, memória persistente, rastreabilidade e uma fronteira segura para capacidades dinâmicas.

> **Estado atual:** a release estável mais recente é a [**v0.8.2**](https://github.com/Head-1/Shaka-Agente/releases/tag/v0.8.2), validada para operação local controlada em `dry-run`. A `main` corrente está no merge commit [`48eeed2`](https://github.com/Head-1/Shaka-Agente/commit/48eeed2d2ba9a9caa975d0e376d24efc467ea380), após a série de PRs #31–#38. BR-01 a BR-06 permanecem documentados como corrigidos, cobertos e validados; a série também consolidou a documentação pública dos contratos de probes, sandbox, fila, memória, configuração, runtime, skills, identidade e governança de planos. Os ciclos repository-first tiveram CI, sandbox e VM aprovados, com branches de feature preservadas. A API HTTP usa loopback por padrão; planos `live`, mensageria externa, pesquisa autônoma na web, autopromoção de skills e controle irrestrito de subagentes permanecem fora dos limites desta versão.

## O que a v0.8.2 entrega

A v0.8.2 mantém a base do **Plan Engine governado** e adiciona uma cadeia de publicação mais verificável: planos possuem contratos tipados, digest canônico, verificação determinística, persistência SQLite, aprovações humanas, compensações declaradas, exposição HTTP/CLI e recuperação fail-closed; a release também valida a correspondência entre tag, workspace Cargo, `Cargo.lock` e changelog antes de empacotar.

| Capacidade | Estado acumulado na v0.8.2 |
|---|---|
| Workspace Cargo com crates separados | Implementado |
| Memória de trabalho e episódica | Persistente em SQLite |
| Quota do arquivo SQLite | Configurável por `SHAKA_DATABASE_MAX_BYTES`; default de 256 MiB e aplicação por `max_page_count` |
| Provedor local determinístico | Implementado |
| Provedor OpenAI-compatível | Adaptador opcional; credencial somente pelo ambiente/cofre |
| Function calling | Mediado pelo host e validado por schema |
| Dry-run | Padrão para tarefas e caminho seguro de execução |
| Sandbox WASM | Wasmtime, deny-by-default, sem WASI, rede, filesystem ou imports do host |
| Catálogo de skills | Persistente, com estados candidata, ativa e revogada |
| Aprovação de skills | Humana, vinculada a hash SHA-256 e justificativa |
| API HTTP | Sessões, fila, workers, health check, cancelamento e idempotência |
| IAM local | Tenants, usuários, papéis, tokens hash-only, revogação e expiração |
| Quotas e rate limits | Persistentes por tenant e operador |
| Auditoria | Cadeia de hashes por tenant, com redaction e verificação administrativa |
| Plan Engine | Contratos, verifier, reducer SQLite, checkpoints, aprovações e compensações |
| Recovery | Idempotente e fail-closed; fronteiras ambíguas tornam-se `unknown` |
| Mensageria externa | Não habilitada |
| Pesquisa autônoma na web | Não habilitada |
| Autopromoção e autoevolução | Proibidas por governança |
| Subagentes irrestritos | Fora do escopo |

O histórico detalhado está em [`CHANGELOG.md`](CHANGELOG.md). O status corrente de confiabilidade está em [`docs/BACKLOG_STATUS.md`](docs/BACKLOG_STATUS.md). O procedimento de operação sem conhecimento interno de Rust está em [`RUNBOOK_OPERACIONAL.md`](RUNBOOK_OPERACIONAL.md), a validação reproduzível do repositório está em [`docs/VALIDACAO_REPOSITORY_FIRST.md`](docs/VALIDACAO_REPOSITORY_FIRST.md), o contrato HTTP e Rust público está em [`docs/API_PUBLICA.md`](docs/API_PUBLICA.md), e as evidências da validação pós-release estão em [`ETAPA9_VALIDACAO_POS_RELEASE.md`](ETAPA9_VALIDACAO_POS_RELEASE.md).

## Estado repository-first e closeout documental

O estado corrente verificável do repositório é o merge commit [`48eeed2`](https://github.com/Head-1/Shaka-Agente/commit/48eeed2d2ba9a9caa975d0e376d24efc467ea380). A cadeia integrada de PRs #31–#38 é:

| PR | Escopo integrado | Merge commit |
|---|---|---|
| [#31](https://github.com/Head-1/Shaka-Agente/pull/31) | Consolidação documental do backlog de confiabilidade | `eb5a6a5` |
| [#32](https://github.com/Head-1/Shaka-Agente/pull/32) | Correção de replay idempotente sob rate limit | `8c92dca` |
| [#33](https://github.com/Head-1/Shaka-Agente/pull/33) | Documentação dos probes de crash/recovery | `2e546fe` |
| [#34](https://github.com/Head-1/Shaka-Agente/pull/34) | Documentação de sandbox e fila | `f1a8fdd` |
| [#35](https://github.com/Head-1/Shaka-Agente/pull/35) | Documentação de memória e configuração | `d46b058` |
| [#36](https://github.com/Head-1/Shaka-Agente/pull/36) | Documentação de orchestrator e skills | `38b7a8c` |
| [#37](https://github.com/Head-1/Shaka-Agente/pull/37) | Documentação de estados e identidade em `shaka-core` | `d78a32a` |
| [#38](https://github.com/Head-1/Shaka-Agente/pull/38) | Documentação de governança de planos em `shaka-core` | `48eeed2` |

Os ciclos das PRs #31–#38 foram conduzidos com branch baseada no repositório, commit de trabalho assinado quando aplicável, CI remoto, validação em sandbox e confirmação independente na VM. As branches de feature permanecem disponíveis para auditoria; nenhum efeito externo novo foi habilitado pelo closeout.

No `main` final, `cargo check --workspace --locked` concluiu com sucesso. A medição documental registrou 100 avisos `missing_docs`, todos em `shaka-core`: 1 módulo, 7 structs, 53 campos de struct, 2 itens associados, 6 métodos, 4 enums e 27 variantes. Esses avisos são dívida documental conhecida e não representam falha de compilação ou de operação; qualquer redução futura deve ocorrer em lotes isolados, com escopo e validação próprios.

## Modelo de segurança

O Shaka trata conteúdo externo, objetivos do modelo e resultados de ferramentas como dados não confiáveis. O modelo não recebe autoridade implícita para alterar políticas, promover skills, modificar o system prompt ou ignorar aprovações.

A regra operacional é falhar de forma explícita diante de ambiguidade, inconsistência, timeout ou evidência incompleta. No Plan Engine, transições são verificadas por digest, dependências, limites, condições, aprovações, capabilities e estado persistido. Uma fronteira ativa ou ambígua após crash não recebe retry cego: o estado é convertido em `unknown` e exige resolução humana.

A release v0.8.2 não é uma implantação pública pronta por si só. Exposição externa exige, em um ciclo separado, IAM remoto forte, cofre de segredos, HTTPS na borda, armazenamento persistente, backup externo, métricas, alertas, política de dados e revisão de ameaça.

## Requisitos locais

O workspace usa Rust edition 2024 e declara Rust `1.85` como versão mínima. O CI e o workflow de release utilizam Rust/Cargo `1.98.0` para validação reprodutível.

Para validar o código-fonte, execute:

```bash
cd Shaka
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version
cargo --version
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- \
  -D warnings -A missing_docs -A clippy::missing_errors_doc
```

O banco SQLite, o catálogo de skills e outros dados locais devem permanecer fora do controle de versão. Não armazene API keys, tokens IAM, backups ou dados de usuários no repositório.

A quota operacional do arquivo SQLite é configurável por `--database-max-bytes` ou `SHAKA_DATABASE_MAX_BYTES`; o default é `268435456` bytes (256 MiB) e valores abaixo de 1 MiB são rejeitados antes da abertura. `QueueStore` e `MemoryStore` aplicam e verificam o mesmo limite no arquivo compartilhado. A contenção usa páginas SQLite, portanto o limite efetivo é arredondado para baixo ao tamanho de página; ele não é uma quota física de todo o filesystem, nem elimina o crescimento temporário de arquivos auxiliares do WAL durante checkpoints [1]. Uma escrita que atinge o limite falha como armazenamento cheio e não deve ser repetida cegamente [2].

## Usar a CLI a partir do código-fonte

Os exemplos abaixo usam o binário do crate explicitamente para evitar ambiguidade no workspace:

```bash
cargo run -p shaka-cli -- --help
cargo run -p shaka-cli -- --version
cargo run -p shaka-cli -- config
cargo run -p shaka-cli -- doctor
```

A execução padrão usa o provedor local e mantém a tarefa em modo seguro:

```bash
cargo run -p shaka-cli -- run \
  "Descreva em uma frase a política deny-by-default do Shaka"
```

A tentativa de execução real não deve ser usada como atalho operacional. Tarefas começam em `dry-run` e planos `live` permanecem bloqueados na v0.8.2. Efeitos externos, quando futuramente autorizados, exigirão mudança formal de governança e novo ciclo de validação.

Para executar a demonstração do sandbox:

```bash
cargo run -p shaka-cli -- sandbox-demo
```

O resultado esperado contém `exit_code: 42` e consumo positivo de fuel. O sandbox rejeita imports do host e não fornece WASI, rede ou filesystem.

## API HTTP local

Inicie o servidor somente em loopback durante a operação local:

```bash
cargo run -p shaka-cli -- serve \
  --bind 127.0.0.1:8080 \
  --workers 2
```

Consulte o health check:

```bash
curl --fail --silent http://127.0.0.1:8080/healthz
```

A resposta esperada apresenta status operacional `ok`, versão `0.8.2` e circuito `closed`. Antes de aceitar operação, consulte também `/readyz`, que verifica integridade, cadeia de auditoria e circuito. Os endpoints principais e os endpoints de planos estão descritos em [`docs/API_PUBLICA.md`](docs/API_PUBLICA.md).

```bash
curl --fail --silent http://127.0.0.1:8080/readyz
```

Em loopback sem `SHAKA_API_KEY`, a política local usa o principal local; um bind não local exige bearer válido. O readiness retorna `200` com `status: "ready"` somente quando os sinais estão prontos e `503` com `status: "failed"` quando o processo está vivo, mas não apto a operar.

| Método | Endpoint | Finalidade |
|---|---|---|
| `GET` | `/healthz` | Saúde pública mínima: versão, fila e circuito |
| `GET` | `/readyz` | Readiness protegido: integridade, auditoria, fila e circuito |
| `POST` | `/v1/sessions` | Criar sessão |
| `GET` | `/v1/sessions/{session_id}` | Consultar sessão |
| `POST` | `/v1/sessions/{session_id}/tasks` | Enfileirar tarefa |
| `GET` | `/v1/tasks/{task_id}` | Consultar estado e resultado |
| `DELETE` | `/v1/tasks/{task_id}` | Solicitar cancelamento |

Toda submissão de tarefa deve possuir `Idempotency-Key`. Um exemplo seguro é:

```bash
SESSION=$(curl --fail --silent -X POST \
  http://127.0.0.1:8080/v1/sessions \
  -H 'content-type: application/json' \
  -d '{"metadata":{"source":"manual"}}' \
  | sed -n 's/.*"session_id":"\([^" ]*\)".*/\1/p')

curl --fail --silent -X POST \
  "http://127.0.0.1:8080/v1/sessions/$SESSION/tasks" \
  -H 'content-type: application/json' \
  -H 'Idempotency-Key: manual-task-1' \
  -d '{"objective":"Descreva a política de execução segura","priority":5}'
```

Repetir a mesma chave com o mesmo payload deve retornar a tarefa existente. Reutilizar a chave com intenção divergente deve ser rejeitado. Não faça bind em `0.0.0.0` sem autenticação adequada, HTTPS na borda e revisão específica de exposição.

## Configurar um provedor OpenAI-compatível

O adaptador é opcional. A chave deve ser fornecida somente por variável de ambiente ou cofre externo, nunca no código, em prompts, logs, banco ou catálogo:

```bash
export SHAKA_MODEL_PROVIDER=openai-compatible
export SHAKA_MODEL_API_KEY="chave-fornecida-pelo-operador"
export SHAKA_MODEL_ENDPOINT="https://provedor.example/v1/chat/completions"
export SHAKA_MODEL="modelo-aprovado"
cargo run -p shaka-cli -- run "Objetivo controlado para validação"
```

Em ambiente de produção, use endpoint HTTPS, auditoria habilitada, credencial válida e configuração validada. Isso não libera automaticamente efeitos externos nem planos `live`.

## IAM, memória, auditoria e backup

As operações IAM e de manutenção exigem o papel apropriado, em especial `administrator` para auditoria, backup e restauração. Exemplos:

```bash
cargo run -p shaka-cli -- iam tenant-create acme "Acme"
cargo run -p shaka-cli -- iam user-create alice --tenant acme --role operator
cargo run -p shaka-cli -- iam token-issue alice --expires-in-seconds 86400
cargo run -p shaka-cli -- memory recent --limit 10
cargo run -p shaka-cli -- verify-audit
cargo run -p shaka-cli -- --role administrator \
  backup --output backups/shaka.db
```

O token bruto é exibido somente durante a emissão. O banco persiste apenas o hash SHA-256, o `token_id` e referências operacionais redacted.

Skills WASM continuam sem WASI, imports de host, rede ou filesystem por padrão e são limitadas por fuel e tempo. A política também limita a memória linear do guest a `16 MiB` por default, com teto host-side de `64 MiB`; esse limite não representa RSS total do processo nem substitui isolamento de processo, cgroup ou uma sandbox de infraestrutura.

Restaure sempre primeiro em um banco separado:

```bash
cargo run -p shaka-cli -- --role administrator \
  --database data/restore-test.db \
  restore --input backups/shaka.db
cargo run -p shaka-cli -- --role administrator \
  --database data/restore-test.db \
  doctor
cargo run -p shaka-cli -- --role administrator \
  --database data/restore-test.db \
  verify-audit
```

Consulte o [runbook operacional](RUNBOOK_OPERACIONAL.md) para retenção, recuperação de incidentes, skills e operação do container.

### Referências da quota SQLite

[1]: https://sqlite.org/wal.html "SQLite Write-Ahead Logging"
[2]: https://sqlite.org/rescode.html "SQLite Result and Error Codes"

## Skills e governança humana

Criar uma skill registra uma candidata; não gera nem executa código automaticamente:

```bash
cargo run -p shaka-cli -- skill candidate relatorio \
  "Gera um relatório estruturado" \
  --permissions memory-write
cargo run -p shaka-cli -- skill list
```

A promoção exige revisão humana, papel autorizado, hash SHA-256 completo e justificativa. Uma skill revogada não pode ser executada mesmo que permaneça no histórico. Não edite manualmente o catálogo para contornar uma transição.

## Estrutura do workspace

```text
Shaka/
├── Cargo.toml
├── Cargo.lock
├── crates/
│   ├── shaka-core/            # tipos, contratos, políticas e Plan Engine
│   ├── shaka-memory/          # SQLite, memória e auditoria persistente
│   ├── shaka-skills/          # catálogo e governança de skills
│   ├── shaka-sandbox/         # execução WASM deny-by-default
│   ├── shaka-orchestrator/    # modelo, ferramentas e runtime
│   ├── shaka-observability/   # tracing, redaction e correlação
│   ├── shaka-config/          # configuração, identidade e políticas
│   ├── shaka-queue/           # sessões, fila, leases e resiliência
│   ├── shaka-api/             # HTTP REST, workers e autenticação
│   └── shaka-cli/              # interface de operação e servidor
├── data/                      # dados locais; não versionar segredos
├── docs/                      # material complementar
└── .github/workflows/         # CI e release
```

## CI/CD no GitHub

O workflow [`.github/workflows/ci.yml`](.github/workflows/ci.yml) executa, em cada push e Pull Request, formatação, compilação, testes, Clippy, secret scan, política de workflows, auditoria de dependências e smoke test de produção.

O workflow [`.github/workflows/release.yml`](.github/workflows/release.yml) é acionado por tags SemVer no formato `vX.Y.Z`. Ele valida a correspondência entre tag e `Cargo.toml`, gera o binário otimizado, SBOM CycloneDX, checksums, tarball e ZIP, cria a GitHub Release e publica opcionalmente a imagem no GHCR. A publicação de attestations é condicionada à visibilidade pública do repositório.

As actions de checkout usam a série compatível com Node.js 24. A action `actions/checkout@v5` requer runner Actions `v2.327.1` ou superior; os workflows deste projeto usam `ubuntu-latest`.[1] [2]

Para contribuir, crie uma branch curta a partir da `main` atual e mantenha a alteração limitada ao escopo revisado:

```bash
git switch main
git pull --ff-only origin main
git switch -c docs/minha-alteracao
# implementar uma alteração pequena e verificável
git diff --check
```

Commits devem ser assinados conforme a chave de assinatura configurada no repositório e verificados antes da publicação. A criação de tags, releases e merges deve ocorrer somente após revisão dos gates e confirmação apropriada.

## Produção candidata e limitações

A v0.8.2 deve ser entendida como uma base operacional local governada, não como autorização para implantação pública irrestrita. Permanecem fora do escopo IAM remoto, cofre de segredos integrado, backup remoto automatizado, métricas exportáveis, mensageria, pesquisa web, escala horizontal com PostgreSQL/row-level security e subagentes distribuídos.

O Shaka deve preservar as seguintes propriedades: autoridade de tenant e papel derivada no host; tarefas iniciando em `dry-run`; planos `live` bloqueados nesta release; auditoria sem payloads ou segredos; recuperação idempotente; inconsistências convertidas em `unknown`; e decisão humana para aprovação, compensação ou cancelamento de fronteiras ambíguas.

## Licença

Apache-2.0. Consulte [`CHANGELOG.md`](CHANGELOG.md) para o histórico completo.

## Referências

[1]: https://github.com/actions/checkout "actions/checkout — documentação oficial"
[2]: https://github.blog/changelog/2025-09-19-deprecation-of-node-20-on-github-actions-runners/ "GitHub — depreciação do Node.js 20 nos runners"
