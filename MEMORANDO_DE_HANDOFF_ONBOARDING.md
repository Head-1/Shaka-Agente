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
| `docs/` | Material complementar. | Baixa, salvo dados inseridos manualmente. |
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

A nova pessoa deve primeiro ler `README.md`, `ARCHITECTURE.md`, `SECURITY.md`, `SKILL_GOVERNANCE.md` e `RUNBOOK_OPERACIONAL.md`. Em seguida deve executar `cargo test --workspace`, `cargo run -- sandbox-demo` e uma tarefa local em dry-run.

Depois, deve inspecionar uma execução em `memory recent`, criar uma skill candidata de teste e confirmar que ela não se torna ativa sem aprovação. A aprovação deve ser exercitada somente com artefato de teste e hash conhecido.

## 8. Procedimento de handoff

A transferência deve registrar a versão do commit, versão de Rust, localização do banco, política de retenção, operador autorizado, responsável de segurança, estado das skills, endpoint do modelo e pendências. O novo responsável deve confirmar acesso mínimo necessário, nunca receber segredos por texto livre e assinar a confirmação de entendimento do runbook.

## 9. Contatos e lacunas

Os nomes, contatos e responsáveis reais não são conhecidos pelo repositório e devem ser preenchidos pelo proprietário da implantação. Não invente contatos em documentação operacional. As lacunas atuais incluem IAM remoto, alertas remotos, backup externo automatizado, suporte a multi-tenancy forte, mensageria e processo formal de incidentes.

## 10. Critério de transferência concluída

O handoff está concluído quando a nova pessoa consegue compilar, testar, executar uma tarefa local, consultar memória, verificar o sandbox, criar uma skill candidata, explicar por que ela não está ativa e seguir o procedimento de revogação sem alterar arquivos de controle manualmente.

## 11. Notas da release 0.2.0

A release atual é uma produção candidata para operação controlada. O novo crate `shaka-config` valida ambiente, provedor, endpoint, credencial, papel, auditoria e modo live. Os papéis são `operator`, `reviewer` e `administrator`; a autorização acontece no host antes da ação.

Antes de assumir a operação, execute:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo run -- config
cargo run -- doctor
cargo run -- verify-audit
```

A rotina administrativa de backup e restore é:

```bash
cargo run -- backup --output backups/shaka.db
cargo run -- restore --input backups/shaka.db
```

Restore exige administrador e deve ser validado em cópia de trabalho. Backups locais devem ser enviados a armazenamento externo criptografado. O catálogo de skills agora usa escrita atômica e pode calcular SHA-256 do arquivo real com `--artifact`.

A atualização de Wasmtime para 47.0.3 é parte do gate de segurança. `cargo audit` deve passar sem advisories reportados. Os detalhes estão em `DEPENDENCY_SECURITY_VALIDATION.md`.

As lacunas restantes não são bugs ocultos: IAM remoto, cofre de segredos, backup externo automatizado, métricas remotas, mensageria, pesquisa web, subagentes distribuídos, assinatura de artefatos e multi-tenancy forte exigem nova fase com infraestrutura e threat model próprios.
