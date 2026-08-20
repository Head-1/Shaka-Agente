# Shaka

MVP de um agente de IA **extensível, auditável e governado pelo operador**, implementado em Rust. O projeto prioriza uma fronteira segura para execução dinâmica, memória persistente, contratos tipados e documentação operável.

> **Estado atual:** produção candidata para operação controlada. O modelo local continua disponível para desenvolvimento; production exige provedor externo HTTPS, chave de API, auditoria habilitada e configuração validada. Mensageria, pesquisa web e autopromoção de skills permanecem deliberadamente desabilitadas.

## Objetivos do MVP

O Shaka já oferece um núcleo modular com as seguintes capacidades:

| Capacidade | Estado |
|---|---|
| Workspace Cargo com crates separados | Implementado |
| Memória de trabalho com TTL | Implementado em SQLite |
| Memória episódica persistente | Implementado em SQLite |
| Memória semântica versionada | Implementado como consolidação explícita; busca vetorial fica para fase posterior |
| Orquestração de uma tarefa | Implementada |
| Provedor local determinístico | Implementado |
| Provedor OpenAI-compatível | Implementado como adaptador opcional |
| Function calling com validação mínima de schema | Implementado |
| Dry-run para efeitos colaterais | Implementado |
| Sandbox WASM com Wasmtime | Implementado com deny-by-default e sem imports de host |
| Catálogo de skills candidatas | Implementado e persistente em JSON |
| Aprovação humana e revogação | Implementado |
| Tracing estruturado | Implementado |
| Auditoria persistente com cadeia de hashes | Implementado por tenant, com verificação CLI |
| Mensageria externa | Não habilitada no MVP |
| Pesquisa autônoma na web | Não habilitada no MVP |
| Subagentes paralelos | Fora do caminho de produção candidata; implementação posterior |
| Autoevolução/autopromoção | **Proibida por decisão de governança** |

## Requisitos locais

O projeto usa Rust stable e Cargo. A versão mínima declarada no workspace é Rust 1.85, enquanto o ambiente de validação utilizado para este MVP foi Rust 1.97.1.

Para compilar e testar:

```bash
cd Shaka
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings -A missing_docs -A clippy::missing_errors_doc
```

O banco SQLite e o catálogo de skills são criados automaticamente quando a CLI é executada.

## Uso da CLI

A execução padrão usa o modelo local e mantém o agente em modo seguro:

```bash
cargo run -- run "Descreva como o Shaka deve tratar uma tarefa"
```

Para usar o endpoint OpenAI-compatível, configure a chave somente no ambiente, nunca no código ou no repositório. Em `production`, a configuração também exige `SHAKA_ENVIRONMENT=production`, endpoint HTTPS e auditoria habilitada:

```bash
export SHAKA_MODEL_PROVIDER=openai-compatible
export SHAKA_MODEL_API_KEY="chave-fornecida-pelo-operador"
export SHAKA_MODEL="gpt-4o-mini"
cargo run -- run "Responda ao objetivo usando as ferramentas disponíveis"
```

O endpoint padrão é `https://api.openai.com/v1/chat/completions`, mas pode ser substituído por `SHAKA_MODEL_ENDPOINT`. O adaptador é genérico e deve ser validado contra o provedor escolhido antes de uso operacional.

Consultar a memória episódica:

```bash
cargo run -- memory recent --limit 10
```

Expurgar episódios antigos do tenant atual:

```bash
cargo run -- memory purge --days 30
```

Executar o exemplo do sandbox:

```bash
cargo run -- sandbox-demo
```

Verificar a configuração e a prontidão operacional:

```bash
cargo run -- config
cargo run -- doctor
cargo run -- verify-audit
```

Criar um backup consistente e restaurar:

```bash
cargo run -- backup --output backups/shaka.db
cargo run -- restore --input backups/shaka.db
```

Criar uma skill candidata:

```bash
cargo run -- skill candidate relatorio "Gera um relatório estruturado" --permissions memory-write
cargo run -- skill list
```

Aprovar uma skill exige papel `reviewer` ou `administrator`, hash SHA-256 completo e justificativa. O caminho recomendado é fornecer o arquivo real para que a CLI calcule o hash; a aprovação manual do hash continua disponível para fluxos externos controlados:

```bash
cargo run -- skill approve relatorio \
  0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --reason "Aprovada após revisão manual e testes locais"

# ou, preferencialmente, calcular o hash do artefato real
cargo run -- skill approve relatorio \
  --artifact artefato.wasm \
  --reason "Aprovada após revisão manual e testes locais"
```

Revogar uma skill ativa:

```bash
cargo run -- skill revoke relatorio
```

## Estrutura do workspace

```text
Shaka/
├── Cargo.toml
├── Cargo.lock
├── crates/
│   ├── shaka-core/            # tipos, contratos e políticas centrais
│   ├── shaka-memory/          # SQLite, memória e auditoria persistente
│   ├── shaka-skills/          # catálogo e governança de skills
│   ├── shaka-sandbox/         # execução WASM deny-by-default
│   ├── shaka-orchestrator/    # modelo, ferramentas e runtime
│   ├── shaka-observability/   # tracing e auditoria
│   └── shaka-cli/             # interface de operação local
├── data/                      # dados locais; não versionar segredos
├── docs/                      # material complementar
└── .github/workflows/         # CI mínimo
```

## Decisões de segurança

O agente não executa código gerado no processo principal. O sandbox do MVP não habilita WASI, rede, filesystem nem funções importadas do host. Um módulo WASM precisa ser autocontido e exportar `run() -> i32`; imports são rejeitados antes da instanciação.

As skills têm estados explícitos e não podem passar diretamente de candidata para ativa. A promoção exige operador, hash do artefato e justificativa. A CLI não oferece um caminho de autopromoção.

Conteúdo externo, quando uma camada futura de pesquisa web for adicionada, deverá ser tratado como dado não confiável. Ele não poderá alterar o system prompt, permissões ou fluxo de execução.

## Produção candidata e limitações

A release atual adiciona configuração tipada, RBAC mínimo, validação JSON Schema, redaction, cadeia de auditoria, backup/restore, integrity check, gravação atômica de skills, Wasmtime atualizado e cargo-audit obrigatório. O documento `PRODUCTION_RELEASE.md` contém os gates e as condições para promoção ao ambiente real.

O sistema ainda não é uma implantação pública pronta sem infraestrutura adicional. Faltam IAM remoto, cofre de segredos, backup externo, métricas remotas, mensageria, pesquisa web, subagentes distribuídos e multi-tenancy forte.

## Limitações operacionais remanescentes

O parâmetro `--live` exige confirmação explícita e papel administrador, mas nenhuma ferramenta externa de mensageria é registrada nesta release. Portanto, ele não deve ser interpretado como autorização para enviar mensagens reais. A produção pública continua dependendo de IAM remoto, cofre de segredos, backup externo, alertas, política de dados, mensageria validada, pesquisa web isolada e multi-tenancy forte.

## Princípios de operação

O Shaka deve preferir falhar de forma explícita a executar uma ação ambígua. O operador deve revisar permissões, hashes, logs e custo antes de ativar capacidades novas. A documentação de segurança e o runbook são parte do sistema, não artefatos opcionais.

## Licença

Apache-2.0. Consulte `CHANGELOG.md` para o histórico de mudanças.

## CI/CD no GitHub

O workflow `CI` executa formatação, compilação, testes, Clippy, auditoria de dependências e o smoke test em cada push e pull request. O workflow `Shaka Release` é acionado por uma tag SemVer, como `v0.2.0`, ou manualmente pela interface do GitHub.

Ao criar uma tag de release, o workflow valida que a tag corresponde à versão do `Cargo.toml`, compila o binário otimizado, gera tarball, ZIP e `SHA256SUMS`, publica os artefatos em uma GitHub Release e constrói a imagem Docker no GitHub Container Registry. A publicação da imagem não inicia um servidor automaticamente; ela produz um artefato pronto para uma VM, serviço de containers ou outra infraestrutura operada pelo proprietário.

Depois que o repositório estiver publicado, o fluxo recomendado é:

```bash
git checkout main
git pull --ff-only origin main
git tag -a v0.2.0 -m "Shaka 0.2.0"
git push origin v0.2.0
```

A tag dispara o workflow de release. Para executar apenas a validação, use o workflow `CI`. Para um deploy futuro em infraestrutura externa, o próximo passo será adicionar um ambiente GitHub protegido, secrets de produção e um job de implantação com aprovação manual; não é seguro embutir credenciais ou um destino de produção diretamente no repositório.
