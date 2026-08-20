# Estratégia de Testes

## 1. Objetivo

A estratégia valida não apenas se o código compila, mas se o agente falha de forma segura quando recebe entradas malformadas, capabilities ausentes, módulos WASM hostis, custos excedentes ou estados inválidos de skill.

## 2. Pirâmide de testes

| Camada | Escopo | Estado do MVP |
|---|---|---|
| Unitário | Tipos, validação, TTL, estados, hashes e políticas | Implementado nos crates centrais |
| Integração | SQLite, runtime, tool registry e CLI | Implementado nos crates e smoke test operacional |
| Contrato | JSON de tools, skills, modelo e erros | Contratos documentados; harness formal planejado |
| Adversarial | Prompt injection, capability denial, imports e efeitos | Sandbox e capabilities cobertos; web ainda não habilitada |
| Regressão | Performance, memória, compatibilidade e rollback | Gates básicos implementados; benchmark e rollback distribuído continuam planejados |
| Recuperação | Backup, restauração, integridade e replay | Backup/restore e integrity check implementados; migrações formais continuam planejadas |
| End-to-end | Modelo real, mensagens e web | Fora do MVP |

## 3. Comandos obrigatórios

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings -A missing_docs -A clippy::missing_errors_doc
cargo audit
```

A CI deve executar os comandos sem credenciais reais. O teste do provedor OpenAI-compatível deve usar mock HTTP ou fixture local. `cargo audit` é gate obrigatório e não deve usar `continue-on-error`.

## 4. Casos de teste críticos

### 4.1 Núcleo

O núcleo deve rejeitar tenant, operador e objetivo vazios, rejeitar schema inválido, validar campos obrigatórios e tipos, negar capability ausente, redigir credenciais e criar eventos de auditoria com hash verificável.

### 4.2 Memória

A memória deve registrar e recuperar episódios por tenant, respeitar TTL da working memory, consolidar somente episódio existente, expurgar dados antigos, impedir vazamento entre tenants, verificar a cadeia de auditoria e preservar episódios em backup/restore.

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

Criar dois tenants com episódios, skills e tarefas distintas. Todas as leituras devem retornar somente dados do tenant solicitado. Testar IDs inexistentes, troca de tenant no mesmo processo e concorrência. O teste deve falhar se qualquer campo de recuperação depender apenas de `task_id` sem verificar tenant.

## 7. Testes de custo e resiliência

Simular modelo que demora, modelo que falha, resposta vazia, JSON malformado, ferramenta que falha e sequência de retries. Verificar deadline, não repetição indevida, custo acumulado, cancelamento e registro de outcome. O runtime contém orçamento de passos, chamadas, custo e tempo. Circuit breaker, backoff e quotas distribuídas para provedores externos continuam planejados.

## 8. Testes de segurança do build

Antes de aceitar geração automática de Rust/WASM, adicionar um ambiente de build sem segredos e testes para dependência maliciosa, `build.rs`, macro procedural, acesso de rede, escrita fora do diretório de trabalho e artefato não reproduzível. O código de teste gerado pela IA não deve ser a única fonte de confiança.

## 9. Cobertura e gates

Cobertura percentual é indicador secundário. Um merge que reduz cobertura de uma política crítica ou remove um teste adversarial deve ser bloqueado. Gates mínimos para ativação futura de skill:

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

Na release 0.2.0, a evidência local inclui `cargo fmt --check`, `cargo check`, `cargo test`, Clippy estrito, `cargo audit`, testes de sandbox, testes de isolamento, testes de backup/restore e smoke test da CLI. O processo de publicação ainda deve anexar logs e hashes do ambiente de CI alvo.

A CI deve guardar logs de build, testes, lint, hashes de artefatos e versão de dependências. A documentação não deve declarar teste executado sem evidência. Falhas intermitentes devem ser registradas como pendência, não escondidas por retry ilimitado.
