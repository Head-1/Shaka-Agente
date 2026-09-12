# Memorando de Handoff e Onboarding

## 1. Finalidade

Este memorando orienta a transferência da operação do Shaka para uma nova pessoa técnica ou operacional. Ele explica o que o sistema faz hoje, onde estão os dados, quais ações exigem aprovação e quais partes ainda não devem ser tratadas como prontas para produção.

## 2. Visão rápida

O Shaka é um agente de IA com runtime Rust. Ele recebe um objetivo, chama um modelo local ou OpenAI-compatível, valida ferramentas no host, registra o resultado em SQLite e mantém um catálogo de skills candidatas em JSON. O sandbox WASM é isolado por padrão e não possui acesso a rede, filesystem ou imports do host.

A regra de governança mais importante é:

> O agente pode sugerir melhorias, mas não pode criar, ativar, promover, modificar o core ou conceder novas permissões a si próprio.

## 3. Mapa de diretórios

| Caminho | Conteúdo | Sensibilidade |
|---|---|---|
| `Cargo.toml` | Workspace e políticas de lint. | Baixa, mas controla build. |
| `crates/` | Código modular do agente. | Alta para integridade do sistema. |
| `data/shaka.db` | Memória episódica, working memory, semântica e auditoria. | Potencialmente alta; pode conter dados de usuários. |
| `data/skills.json` | Catálogo de skills e aprovações. | Alta para governança. |
| `.env.example` | Nomes de variáveis, sem segredos. | Baixa. |
| `docs/` | Material complementar, incluindo ADRs e validações. | Baixa, salvo dados inseridos manualmente. |
| `.github/workflows/` | Pipeline de validação. | Alta para integridade da entrega. |

## 4. Responsabilidades

O operador deve controlar identidade, tenant, aprovação de skills, retenção de memória, revisão de incidentes e rotação de credenciais. A pessoa responsável por segurança deve revisar capabilities, sandbox, dependências, artefatos WASM e logs de auditoria. A pessoa responsável pela implementação deve manter contratos e documentação sincronizados.

O MVP não define ainda uma separação técnica de funções nem aprovação de duas pessoas. Em um ambiente multiusuário, essa lacuna deve ser tratada antes de habilitar efeitos externos.

## 5. Onde ficam os segredos

O Shaka não armazena a chave do modelo no repositório. A variável `SHAKA_MODEL_API_KEY` deve ser injetada por ambiente seguro ou cofre. O arquivo `.env.example` contém apenas nomes e valores fictícios. Nunca copie uma chave real para `data`, logs, issues, commits ou documentos de handoff.

## 6. Como interpretar uma execução

Uma execução válida apresenta `task_id`, resposta textual, resultados de ferramentas e indicador de sucesso. O `task_id` é a chave inicial para correlacionar logs e episódio. O conteúdo da resposta não é prova suficiente de que uma ação externa ocorreu; confirme o `ToolResult`, o modo `dry_run` e os logs.

Quando uma ferramenta retorna `dry_run`, o sistema simulou o efeito e não deve ser descrito como uma ação realizada. Quando uma skill está `Candidate`, ela não deve ser carregada como ativa. Quando uma skill está `Revoked`, qualquer tentativa de execução deve ser bloqueada por uma camada futura de catálogo ativo.

## 7. Procedimento de onboarding

A nova pessoa deve primeiro ler `README.md`, `ARCHITECTURE.md`, `SECURITY.md`, `SKILL_GOVERNANCE.md`, `RUNBOOK_OPERACIONAL.md`, `API_CONTRACTS.md`, `TESTING_STRATEGY.md` e `docs/VALIDACAO_REPOSITORY_FIRST.md`. O guia ampliado de continuidade está em [`documento.md`](documento.md).

Depois, deve confirmar o SHA remoto da `origin/main`, a árvore limpa e o toolchain; executar `cargo fmt --all -- --check`, `cargo check --workspace --locked`, `cargo test --workspace --locked`, `cargo run -p shaka-cli -- sandbox-demo`, `cargo run -p shaka-cli -- config` e `cargo run -p shaka-cli -- doctor`.

Por fim, deve executar uma tarefa local em `dry-run`, inspecionar uma execução em `memory recent`, criar uma skill candidata de teste e confirmar que ela não se torna ativa sem aprovação. A aprovação deve ser exercitada somente com artefato de teste e hash conhecido.

## 8. Procedimento de handoff

A transferência deve registrar o SHA exato da `origin/main`, versão de Rust, localização do banco, política de retenção, operador autorizado, responsável de segurança, estado das skills, endpoint do modelo e pendências. O novo responsável deve confirmar acesso mínimo necessário, nunca receber segredos por texto livre e assinar a confirmação de entendimento do runbook.

A referência corrente do repositório é o merge da PR #49, SHA `239f6b65d31accd718d70fac5a62f687f9eb2412`, com Wasmtime `47.0.4`. A PR #50, de reconciliação documental, deve ser considerada parte do estado corrente somente depois de seu merge protegido e da CI pós-merge; enquanto estiver aberta, seu SHA `1b7a25fd58d540a2f1e311048fc9ba10051d6a9c` é apenas candidato documental.

## 9. Contatos e lacunas

Os nomes, contatos e responsáveis reais não são conhecidos pelo repositório e devem ser preenchidos pelo proprietário da implantação. Não invente contatos em documentação operacional. As lacunas atuais incluem RPO/RTO formal, cópia externa criptografada com restauração testada, orçamento físico global, retenção e compactação, métricas e alertas remotos, IAM remoto, mensageria, suporte a multi-tenancy forte e processo formal de incidentes.

## 10. Critério de transferência concluída

O handoff está concluído quando a nova pessoa consegue compilar, testar, executar uma tarefa local, consultar memória, verificar o sandbox, criar uma skill candidata, explicar por que ela não está ativa, seguir o procedimento de revogação sem alterar arquivos de controle manualmente e explicar por que uma cópia local do SQLite não constitui, sozinha, uma estratégia de recuperação validada.

## 11. Estado técnico atual

A release funcional é `v0.8.2`, em base operacional local controlada. O workspace usa Rust edition 2024, declara MSRV `1.85` e é validado com Rust/Cargo `1.98.0` nos ciclos registrados. A persistência usa SQLite com WAL, transações imediatas onde há necessidade de serialização e quota configurável; o default da quota é `268435456` bytes, ou 256 MiB, com mínimo de 1 MiB.

O Wasmtime corrente é `47.0.4`. A versão `47.0.3` não deve ser reintroduzida: ela foi a origem dos advisories reproduzidos no ciclo de segurança anterior. A política `missing_docs` permanece como warning no manifesto, e o check forçado corrente foi validado sem warnings.

Os comandos administrativos de backup e restore são:

```bash
cargo run -p shaka-cli -- backup --output backups/shaka.db
cargo run -p shaka-cli -- restore --input backups/shaka.db
```

O restore exige administrador e deve ser validado em cópia de trabalho, nunca substituindo diretamente o único banco operacional. A existência desses comandos não prova RPO/RTO nem backup externo; essa garantia será implementada na frente E1.

## 12. Próxima frente autorizada para planejamento

A frente **E1 — RPO/RTO e backup externo** foi definida com RPO de **0 a 5 minutos** e RTO de **até 1 minuto**. O desenho aprovado é híbrido: solicitações por evento após checkpoint durável, coalescidas e single-flight, mais um timer de segurança de no máximo cinco minutos. A Online Backup API do SQLite será o mecanismo primário; o snapshot deverá ter checksum, metadados e validação antes da confirmação no destino externo isolado. O restore será fail-closed diante de checksum, integridade, schema ou cadeia de auditoria inválidos. O contrato completo está em [`docs/ADR-001-E1-RPO-RTO-BACKUP-EXTERNO.md`](docs/ADR-001-E1-RPO-RTO-BACKUP-EXTERNO.md).

O teste funcional de E1 já foi criado na branch de trabalho e falha no baseline por ausência do manifesto de checksum; isso comprova a lacuna sem afirmar que a implementação está pronta. A implementação do runtime não deve começar enquanto a PR #50 não estiver mesclada, a CI pós-merge não estiver verde e a VM não tiver sido sincronizada para o novo SHA.

## 13. Referência principal

O [`documento.md`](documento.md) é o guia ampliado para continuidade técnica, operação, segurança, persistência e preparação de novas mudanças. Este memorando permanece como resumo de onboarding e handoff. Quando houver divergência, o SHA publicado no GitHub e o conteúdo versionado correspondente são a fonte da verdade.
