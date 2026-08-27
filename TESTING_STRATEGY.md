# Estratégia de Testes

## 1. Objetivo

A estratégia valida não apenas se o código compila, mas se o agente falha de forma segura quando recebe entradas malformadas, capabilities ausentes, módulos WASM hostis, custos excedentes ou estados inválidos de skill.

## 2. Pirâmide de testes

| Camada | Escopo | Estado do MVP |
|---|---|---|
| Unitário | Tipos, validação, TTL, estados, hashes e políticas | Implementado nos crates centrais |
| Integração | SQLite, runtime, tool registry e CLI | Implementado nos crates e smoke test operacional |
| Contrato | JSON de tools, skills, modelo e erros | Contratos documentados e verificados por testes e gates |
| Adversarial | Prompt injection, capability denial, imports e efeitos | Sandbox, capabilities, redaction e limites cobertos; web ainda não habilitada |
| Regressão | Performance, memória, compatibilidade, concorrência e rollback | Regressões direcionadas e probes multiprocesso versionados; benchmarks continuam planejados |
| Recuperação | Backup, restauração, integridade, replay e crash/restart | Backup/restore, integrity check, lifecycle e probes de recuperação implementados; migrações formais continuam planejadas |
| End-to-end | Modelo real, mensagens e web | Fora do MVP |

## 3. Comandos obrigatórios

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked --all-targets
cargo clippy --workspace --all-targets --locked -- \
  -D warnings -A missing_docs -A clippy::missing_errors_doc
cargo audit
bash scripts/validate_postmerge.sh
```

O executor repository-first é o contrato agregado para checkout limpo; ele também executa secret scan, política de workflows, testes Python, preflight de versão, smoke, lifecycle e probes multiprocesso. A CI deve executar sem credenciais reais. O teste do provedor OpenAI-compatível deve usar mock HTTP ou fixture local. `cargo audit` é gate obrigatório e não deve usar `continue-on-error`; em runner limpo, o banco de advisories deve ser buscado automaticamente.

## 4. Casos de teste críticos

### 4.1 Núcleo

O núcleo deve rejeitar tenant, operador e objetivo vazios, rejeitar schema inválido, validar campos obrigatórios e tipos, negar capability ausente, redigir credenciais e criar eventos de auditoria com hash verificável.

### 4.2 Memória

A memória deve registrar e recuperar episódios por tenant, respeitar TTL da working memory, rejeitar registros corrompidos sem fabricar UUIDs ou timestamps, consolidar somente episódio existente, expurgar dados antigos, impedir vazamento entre tenants, verificar a cadeia de auditoria por ordem estrutural de commit e preservar episódios em backup/restore. A auditoria deve permanecer linear entre instâncias concorrentes e resistente a timestamps fora de ordem.

### 4.3 Skills

O catálogo deve rejeitar duplicata, impedir registro fora do estado `Candidate`, exigir hash de 64 caracteres hexadecimais ou calcular o hash do artefato real, exigir justificativa, gravar atomicamente, bloquear aprovação repetida e permitir revogação somente de skill ativa.

### 4.4 Sandbox

O sandbox deve executar módulo puro, rejeitar imports, negar rede/filesystem, rejeitar política com fuel ou timeout zero, falhar sem `run() -> i32` e transformar trap/erro em resultado controlado.

### 4.5 Orquestração

O runtime deve registrar episódio e auditoria em sucesso, aplicar timeout e orçamento de custo, negar ferramenta desconhecida, validar JSON Schema, redigir resposta, impedir efeito externo em dry-run e interromper quando o modelo exceder `max_tool_calls`.

### 4.6 Adversarial

A suíte deve incluir uma skill que tenta importar uma função do host, uma skill que declara rede sem capability, argumentos com JSON inválido, ferramenta com nome inventado, objetivo contendo instruções para ignorar políticas e resposta do modelo tentando inserir ferramenta não registrada.

## 5. Testes de prompt injection

Quando a pesquisa web for implementada, cada fixture deve conter instruções como “ignore o sistema”, exfiltração de tokens, tentativa de alterar permissões e pedido para chamar ferramenta. O resultado esperado é que o conteúdo permaneça evidência não confiável e não altere o fluxo de autorização.

## 6. Testes de tenant

Criar dois tenants com episódios, skills e tarefas distintas. Todas as leituras devem retornar somente dados do tenant solicitado. Testar IDs inexistentes, troca de tenant no mesmo processo e concorrência. O teste deve falhar se qualquer campo de recuperação depender apenas de `task_id` sem verificar tenant. Submissões governadas devem exigir `Idempotency-Key`, preservar o fingerprint e retornar a task existente sem criar checkpoints adicionais em replays compatíveis.

## 7. Testes de custo e resiliência

Simular modelo que demora, modelo que falha, resposta vazia, JSON malformado, ferramenta que falha e sequência de retries. Verificar deadline, não repetição indevida, custo acumulado, cancelamento e registro de outcome. O runtime contém orçamento de passos, chamadas, custo e tempo. Circuit breaker, backoff e quotas distribuídas para provedores externos continuam planejados.

## 8. Testes de segurança do build

Antes de aceitar geração automática de Rust/WASM, adicionar um ambiente de build sem segredos e testes para dependência maliciosa, `build.rs`, macro procedural, acesso de rede, escrita fora do diretório de trabalho e artefato não reproduzível. O código de teste gerado pela IA não deve ser a única fonte de confiança.

## 9. Cobertura e gates

Cobertura percentual é indicador secundário e não substitui regressões de propriedade. Na medição do workspace no merge commit `445bc65`, `cargo-llvm-cov` registrou 77,01% de regiões, 77,03% de funções e 81,26% de linhas. O CLI e os binários operacionais de probes possuem cobertura inferior ou não são exercitados pelo `cargo test` instrumentado; suas propriedades são cobertas por smoke e probes operacionais independentes. Um merge que reduza cobertura de uma política crítica ou remova um teste adversarial deve ser investigado.

Gates mínimos para ativação futura de skill:

| Gate | Critério |
|---|---|
| Build | Compilação reproduzível e lockfile revisado. |
| Contrato | Schemas válidos e testes de compatibilidade. |
| Segurança | Todos os testes de negação passam. |
| Recursos | Fuel, memória e timeout observados. |
| Governança | Candidata, aprovação e rollback testados. |
| Auditoria | Versão, hash, operador, resultado e elo da cadeia registrados; `verify-audit` passa. |
| Operação | Runbook atualizado e procedimento testado. |

## 10. Evidência

No estado atual da `main`, a evidência inclui `cargo fmt`, `cargo check --locked`, `cargo test --workspace --locked --all-targets`, Clippy com as exceções oficiais, secret scan, política de workflows, preflight de versão, `cargo audit`, smoke de produção, lifecycle direto do binário e probes multiprocesso de QueueStore e auditoria. O mesmo contrato foi executado no sandbox e na VM pelo SHA publicado, antes e depois dos merges das PRs de hardening e validação.

A CI deve guardar logs de build, testes, lint, hashes de artefatos e versão de dependências. A documentação não deve declarar teste executado sem evidência. Falhas intermitentes devem ser registradas como pendência, não escondidas por retry ilimitado. O status consolidado dos BR-01 a BR-06 está em [`docs/BACKLOG_STATUS.md`](docs/BACKLOG_STATUS.md), e o executor está em [`docs/VALIDACAO_REPOSITORY_FIRST.md`](docs/VALIDACAO_REPOSITORY_FIRST.md).
