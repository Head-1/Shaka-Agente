# Progresso de implementação do roadmap

## Marco 1 — plano de controle do runtime

Data: 2026-08-20

Implementado na branch local `roadmap/v0.3.0`:

- Loop multi-turno do `AgentRuntime`, limitado por `max_steps`, `max_tool_calls`, `max_elapsed_ms` e orçamento acumulado.
- Timeout global da tarefa e timeout individual por ferramenta.
- Registro de falhas do modelo, orçamento e deadline em episódio e auditoria.
- Revalidação de capabilities no momento de cada execução de ferramenta.
- Ordenação determinística das definições de ferramentas.
- Redaction e limite de 8 KiB para resultados devolvidos ao próximo ciclo do modelo e ao operador.
- Auditoria encadeada individual de cada tool execution.
- Catálogo de skills com caminho canônico e SHA-256 do artefato quando a aprovação é feita por arquivo real.
- Skills revogadas excluídas do conjunto de artefatos ativos.
- Adaptador `WasmSkillTool` que verifica novamente o hash, rejeita imports via sandbox, aplica policy Wasmtime e valida o schema de saída.
- CLI registra somente skills ativas verificáveis e concede `CodeExecution` apenas ao papel `administrator`.
- Teste do loop multi-turno e teste da aprovação por artefato.

Validação local com Rust stable 1.98.0:

- `cargo fmt --all`: passou.
- `cargo check --workspace`: passou.
- `cargo test --workspace`: passou; 22 testes unitários, além de doc-tests sem falhas.

Limitação ainda aberta: a publicação no GitHub deverá ocorrer somente depois de concluir o endurecimento dos workflows, backup/container e documentação, e exige uma nova tag sobre a `main` corrigida.
