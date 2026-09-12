# ADR-001 — E1: RPO/RTO e backup externo

**Status:** Proposta para decisão do proprietário da implantação
**Data:** 2026-09-11
**Escopo:** Planejamento e critérios de aceitação; nenhuma alteração funcional nesta etapa
**Base técnica:** `origin/main` em `239f6b65d31accd718d70fac5a62f687f9eb2412`

## Contexto

O Shaka já possui persistência SQLite, WAL, verificação de integridade, auditoria append-only no modelo atual e comandos administrativos de backup e restore. Também há testes locais de backup, restore válido e rejeição de fonte corrompida. Esses fatos demonstram capacidade local, mas não demonstram que o sistema consegue recuperar-se de uma perda do host dentro de um objetivo operacional definido.

A quota SQLite corrente, com default de 256 MiB, também não é um orçamento físico completo: WAL, SHM, logs, temporários, backups e demais processos podem consumir espaço fora dessa quota. O documento de continuidade registra essa limitação e impede que uma cópia informal do arquivo seja tratada como estratégia de recuperação.

A documentação oficial do SQLite descreve a Online Backup API como mecanismo para copiar um banco ativo [1]. O desenho da frente E1 deve usar um mecanismo que preserve um snapshot consistente e deve validar a restauração em destino separado, em vez de copiar somente o arquivo principal sem considerar o modo WAL.

## Decisão proposta

Antes de implementar qualquer mecanismo adicional, o proprietário da implantação deve aprovar valores explícitos para:

| Decisão | Valor a preencher | Critério de aceitação |
|---|---|---|
| RPO | `a definir` | A perda máxima de dados é mensurável e testável. |
| RTO | `a definir` | O tempo máximo de restauração inclui verificação de integridade e auditoria. |
| Dados incluídos | `a definir` | Banco, metadados necessários, configuração não secreta e inventário de skills são enumerados. |
| Dados excluídos | `a definir` | Segredos, caches e temporários têm tratamento explícito. |
| Destino externo | `a definir` | O destino não depende do mesmo filesystem do banco operacional. |
| Criptografia | `a definir` | Chaves ficam fora do repositório e do backup não protegido. |
| Retenção | `a definir` | A retenção não apaga evidência necessária à auditoria ou ao RPO. |
| Frequência | `a definir` | A periodicidade é compatível com o RPO aprovado. |
| Teste periódico | `a definir` | Há restauração automática ou operacionalmente repetível em ambiente separado. |

A implementação deverá manter o princípio de fail-closed: se o snapshot não puder ser verificado, se o destino externo não estiver disponível ou se a restauração falhar na integridade do banco ou da auditoria, o artefato não será marcado como backup válido.

## Hipótese testável

> Se o Shaka produzir um snapshot consistente por mecanismo suportado pelo SQLite, transportar esse artefato para um destino externo e restaurá-lo em um banco novo, então o banco restaurado deverá passar na verificação de integridade, preservar os registros esperados e validar a cadeia de auditoria, dentro do RTO aprovado e com perda inferior ou igual ao RPO aprovado.

A hipótese será considerada refutada se qualquer uma das seguintes condições ocorrer: o snapshot não puder ser aberto; a integridade falhar; a cadeia de auditoria divergir; registros confirmados antes do limite de RPO desaparecerem; o restore alterar o banco operacional durante uma falha; ou o tempo total exceder o RTO.

## Reprodução obrigatória antes da implementação

O primeiro teste deve ser criado para falhar por ausência da garantia E1, não por erro de compilação ou ambiente. O cenário mínimo é:

1. Criar uma base temporária governada pelo mesmo código de persistência usado pelo agente.
2. Persistir um conjunto conhecido de episódios e eventos de auditoria em ordem determinística.
3. Produzir o artefato de backup por uma função ou comando explicitamente identificado.
4. Simular transporte para um segundo caminho que represente o destino externo.
5. Restaurar em um terceiro caminho, sem substituir a base de origem.
6. Verificar integridade SQLite, quantidade e conteúdo dos registros, cadeia de auditoria e metadados do backup.
7. Medir o tempo do ponto de início do restore até a validação final.
8. Confirmar que uma falha no artefato deixa o destino inválido e não modifica a origem.

No baseline atual, o teste deve permanecer marcado como **não implementado** ou falhar com uma asserção funcional clara sobre a ausência do contrato E1. Não é aceitável mascarar a lacuna com `#[ignore]`, trocar a asserção por `true`, aceitar `exit 0` ou considerar uma cópia de arquivo não verificada como sucesso.

## Critérios de aceitação da implementação futura

A futura mudança somente será aprovada quando cumprir todos os critérios abaixo:

| Categoria | Evidência exigida |
|---|---|
| Consistência | Snapshot abre e passa em `PRAGMA integrity_check` ou equivalente do contrato do projeto. |
| Auditoria | A cadeia restaurada é válida e contém os eventos esperados. |
| Isolamento | Restore é feito em destino separado e não corrompe nem substitui a origem em caso de erro. |
| Transporte | O artefato possui checksum e metadados suficientes para detectar alteração. |
| Segurança | Segredos não são gravados no artefato ou nos logs; destino e chaves têm controle de acesso. |
| RPO | A perda observada fica dentro do limite aprovado em teste controlado. |
| RTO | A restauração completa fica dentro do limite aprovado em Sandbox e VM. |
| Regressão | O teste novo falha no baseline e passa após a implementação. |
| Portabilidade | O mesmo procedimento é reproduzível nos dois ambientes. |
| Operação | Runbook contém diagnóstico, abortamento, retenção e rollback. |

## Fora de escopo nesta etapa

Este ADR não escolhe provedor de armazenamento, não instala agente de backup, não altera o schema SQLite, não modifica a política de retenção, não habilita live, não expõe endpoints públicos e não executa upload de dados. Essas decisões dependem dos valores de RPO/RTO e do threat model aprovado.

## Consequências

A consequência imediata é reconhecer formalmente que o Shaka possui backup e restore locais, mas ainda não possui uma garantia operacional de recuperação externa. A consequência positiva é que a próxima implementação terá uma hipótese, uma prova reproduzível e limites mensuráveis. A consequência operacional é que a produção pública continua bloqueada até que a decisão seja aprovada e validada nos dois ambientes.

## Referências

[1]: https://www.sqlite.org/backup.html "SQLite Online Backup API"
[2]: https://www.sqlite.org/wal.html "SQLite Write-Ahead Logging"
