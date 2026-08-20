# Relatório de validação — Shaka 0.2.0

## Resultado executivo

A release 0.2.0 foi validada como **produção candidata para operação controlada**. O workspace compila, os testes passam, o lint estrito passa, a auditoria de dependências passa, o smoke test operacional passa e o binário otimizado foi gerado.

A validação foi executada no ambiente local em 20 de agosto de 2026, com Rust/Cargo 1.97.1.

## Gates executados

| Gate | Resultado | Evidência |
|---|---:|---|
| `cargo fmt --all -- --check` | Aprovado | Nenhuma alteração de formatação pendente. |
| `cargo check --workspace` | Aprovado | Todos os crates integrados. |
| `cargo test --workspace` | Aprovado | 20 testes unitários distribuídos no workspace, incluindo JSON Schema, RBAC, redaction, backup, auditoria, isolamento, sandbox e orquestração. |
| `cargo clippy --workspace --all-targets -- -D warnings -A missing_docs -A clippy::missing_errors_doc` | Aprovado | Lint estrito sem erros. |
| `cargo audit` | Aprovado | Nenhum advisory reportado após Wasmtime 47.0.3. |
| `./scripts/production_smoke.sh` | Aprovado | Tarefa local, memória, candidata, aprovação por arquivo, revogação, backup, restore, auditoria, doctor e sandbox. |
| `cargo build --release -p shaka-cli` | Aprovado | Binário otimizado gerado em `target/release/shaka`. |

## Smoke test

O smoke test confirmou que uma execução local produz JSON parseável em stdout, enquanto tracing permanece em stderr. Também confirmou que a cadeia de auditoria registra pelo menos a execução e as transições de skill, que `doctor` retorna `ready`, que backup/restore preservam o banco e que o sandbox executa o módulo puro esperado.

## Controles verificados

A configuração production bloqueia modelo local, endpoint sem HTTPS, ausência de API key e auditoria desabilitada. O modo live exige papel administrador e confirmação explícita. O host valida JSON Schema, capabilities, budget e dry-run; a resposta é submetida a redaction antes de persistência e retorno. O catálogo de skills não possui autopromoção, grava atomicamente e pode calcular SHA-256 do artefato real. Wasmtime não habilita WASI, rede, filesystem ou imports de host.

## Limites da evidência

A validação local não prova IAM remoto, alta disponibilidade, backup externo, RPO/RTO, rotação real de segredos, métricas distribuídas, assinatura de imagem, multi-tenancy distribuído, mensageria, pesquisa web ou operação pública 24/7. Esses itens permanecem gates de infraestrutura e de uma próxima fase.
