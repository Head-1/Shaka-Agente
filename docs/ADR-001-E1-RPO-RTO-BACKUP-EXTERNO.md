# ADR-001 — E1: RPO/RTO e backup externo

**Status:** Decisão aprovada para implementação incremental
**Data:** 2026-09-11
**Escopo:** Contrato operacional, arquitetura e critérios de teste
**Base técnica:** `origin/main` em `239f6b65d31accd718d70fac5a62f687f9eb2412`

## Contexto

O Shaka já possui persistência SQLite, WAL, verificação de integridade, auditoria append-only no modelo atual e comandos administrativos de backup e restore. Também há testes locais de backup, restore válido e rejeição de fonte corrompida. Esses fatos demonstram capacidade local, mas não demonstram recuperação externa dentro de um objetivo operacional definido.

A quota SQLite corrente, com default de 256 MiB, não é um orçamento físico completo: WAL, SHM, logs, temporários, backups e demais processos podem consumir espaço fora dessa quota. Uma cópia informal do arquivo principal também não deve ser tratada como backup válido sem considerar o modo WAL e a consistência do snapshot.

A documentação oficial do SQLite descreve a Online Backup API como mecanismo para copiar um banco ativo [1]. A estratégia E1 usará essa API nativa, exposta pelo `rusqlite` com o recurso `backup`, como mecanismo primário para produzir snapshots consistentes. `VACUUM INTO` permanece uma alternativa operacional a ser avaliada separadamente, não uma segunda implementação concorrente nesta fase.

## Decisão aprovada

Os objetivos de recuperação da implantação controlada são:

| Decisão | Valor aprovado | Interpretação operacional |
|---|---:|---|
| RPO | **0 a 5 minutos** | Em perda catastrófica do host, o máximo aceitável é perder os últimos cinco minutos de transações ou histórico ainda não replicado. O alvo operacional é o menor intervalo possível; cinco minutos é o limite superior, não uma garantia de perda zero. |
| RTO | **≤ 1 minuto** | Do início do procedimento de recuperação até o serviço estar novamente online e pronto para requisições HTTP/CLI, incluindo obtenção do snapshot, checksum, restore, integridade, auditoria e inicialização. |

O escopo do backup externo inclui estritamente:

| Incluído | Excluído |
|---|---|
| Banco SQLite governado por `SHAKA_DATABASE_MAX_BYTES`, incluindo o snapshot consistente de seu estado transacional | Chaves privadas, API keys, tokens e segredos de cofre/ambiente |
| Metadados necessários para identificar versão, tenant, timestamp, sequência, checksum e origem | Caches, temporários, `target/`, artefatos de build e logs descartáveis |
| Configuração não secreta necessária para abrir e validar a restauração, quando não puder ser reconstruída por versão | Arquivos que não tenham contrato explícito de restauração |
| Inventário de skills necessário para preservar a governança, sem material secreto | Credenciais ou material criptográfico privado |

O destino externo deverá estar isolado do filesystem primário. São opções permitidas um volume dedicado montado separadamente, NFS/SMB administrado para esse fim ou object storage local/nuvem compatível com a política de segurança. A escolha concreta do provedor e das credenciais exige configuração operacional própria e não será embutida no código nesta etapa.

## Estratégia de disparo

A estratégia será **híbrida**, com um único coordenador de backup e execução single-flight:

1. **Evento:** após um checkpoint/redução de plano e commit durável bem-sucedidos, o Plan Engine sinaliza uma solicitação de backup. A solicitação é coalescida, não executada em paralelo, e não pode atrasar nem reabrir a transação que já foi confirmada.
2. **Tempo:** um timer de segurança dispara no máximo a cada cinco minutos, com jitter pequeno e controlado para evitar sincronização acidental entre instâncias. Esse timer cobre períodos sem finalização de step e impede que o RPO dependa de haver eventos de plano.
3. **Coalescência:** se já houver backup em execução, novas solicitações marcam apenas que existe estado mais recente a capturar. Ao terminar, o coordenador agenda nova execução conforme o intervalo e o estado observados.
4. **Falha:** uma falha não é convertida em sucesso. O sistema registra erro, mantém o último backup confirmado e emite sinal operacional; retries devem respeitar backoff e nunca ultrapassar o contrato de RPO silenciosamente.
5. **Confirmação:** o backup só passa a ser considerado replicado depois de snapshot, checksum, transporte e validação do artefato no destino externo.

Essa combinação é preferível a apenas eventos porque o sistema pode permanecer ativo sem concluir steps por mais de cinco minutos. É preferível a apenas timer porque um checkpoint importante pode ser protegido imediatamente sem aguardar o próximo ciclo periódico.

## Mecanismo de snapshot e fail-closed

A Online Backup API do SQLite será o mecanismo primário. O snapshot deve ser escrito para um artefato temporário no destino isolado, receber checksum SHA-256 e metadados, ser validado em uma abertura independente e somente então ser promovido atomicamente a `last-known-good` conforme as capacidades do destino.

No restore, o procedimento deve:

1. selecionar o último artefato cuja transferência e checksum estejam confirmados;
2. baixar para um caminho temporário separado do banco operacional;
3. verificar checksum e metadados antes de abrir;
4. executar a verificação de integridade do SQLite;
5. validar a cadeia de hashes de auditoria;
6. iniciar o Shaka apontando para o banco restaurado somente após todas as verificações;
7. recusar a inicialização se qualquer verificação falhar.

O fail-closed aplica-se ao banco restaurado: integridade SQLite inválida, cadeia de auditoria inválida, checksum divergente, metadados incompatíveis ou artefato ausente impedem que o serviço seja considerado pronto. O procedimento não deve substituir o único banco operacional antes de concluir a validação.

## Hipótese testável

> Se o Shaka produzir snapshots consistentes por meio da Online Backup API após commits duráveis e, no máximo, a cada cinco minutos, transportar esses artefatos para um destino externo isolado e restaurar o último artefato válido em um banco novo, então o banco restaurado deverá passar na integridade SQLite, preservar a cadeia de auditoria, manter a perda dentro de cinco minutos e deixar o serviço pronto em até um minuto.

A hipótese será refutada se ocorrer qualquer uma destas condições: snapshot ilegível; checksum divergente; integridade inválida; auditoria divergente; perda superior a cinco minutos; restore que modifica a origem durante falha; serviço declarado pronto antes das verificações; ou tempo total superior a um minuto.

## Reprodução obrigatória antes da implementação

O teste inicial deve demonstrar a lacuna no baseline por uma asserção funcional clara, não por erro de compilação, `#[ignore]`, `true` incondicional ou falha ambiental. O cenário mínimo é:

1. criar uma base temporária governada pelo mesmo código de persistência;
2. persistir episódios e eventos de auditoria em ordem determinística;
3. solicitar um snapshot por evento após commit e confirmar que a solicitação é coalescida;
4. avançar o relógio controlado para verificar o disparo periódico de cinco minutos;
5. produzir o artefato por uma função de backup explicitamente identificada;
6. simular transporte para um segundo caminho isolado e calcular checksum;
7. restaurar em um terceiro caminho sem substituir a origem;
8. verificar integridade SQLite, registros esperados, cadeia de auditoria e metadados;
9. corromper uma cópia do artefato e provar que o restore é rejeitado sem alterar a origem;
10. medir o tempo do início do restore até o readiness restaurado.

O teste deve usar relógio e destino controláveis para não depender de espera real de cinco minutos nem de uma rede externa durante a unidade. Um teste de integração separado deverá cobrir o adaptador de destino externo em Sandbox e VM.

## Critérios de aceitação

| Categoria | Evidência exigida |
|---|---|
| RPO | Cenários com evento e sem evento demonstram que o artefato confirmado mais recente não excede cinco minutos no modelo de teste. |
| RTO | Restore validado, integridade, auditoria e readiness completam em até um minuto em Sandbox e VM. |
| Consistência | Snapshot abre e passa na verificação de integridade. |
| Auditoria | A cadeia restaurada é válida e contém os eventos esperados. |
| Isolamento | Restore ocorre em destino separado e falha sem corromper a origem. |
| Transporte | Artefato possui checksum e metadados verificáveis. |
| Segurança | Nenhum segredo entra no artefato ou logs; chaves permanecem em ambiente/cofre. |
| Concorrência | Não há dois backups concorrentes; eventos são coalescidos e o commit não é bloqueado pelo transporte. |
| Regressão | O teste novo falha no baseline e passa após a implementação. |
| Portabilidade | O mesmo procedimento é reproduzível nos dois ambientes. |
| Operação | Runbook contém diagnóstico, abortamento, retenção, restauração e rollback. |

## Fora de escopo nesta etapa

Este ADR não escolhe provedor concreto, não instala agente de backup, não executa upload de dados reais, não altera o schema SQLite, não define retenção final, não habilita `live`, não expõe endpoints públicos e não armazena chaves privadas. Essas decisões exigem configuração e threat model próprios.

## Consequências

O sistema terá um contrato operacional mensurável, mas a garantia ainda não existe até que o teste novo falhe no baseline, a implementação passe nos testes e a VM reproduza os mesmos resultados. A produção pública continua bloqueada até a validação completa. O destino externo passa a ser uma dependência operacional explícita: indisponibilidade ou falha de validação deve aparecer como estado de recuperação degradada, nunca como sucesso fictício.

## Referências

[1]: https://www.sqlite.org/backup.html "SQLite Online Backup API"
[2]: https://www.sqlite.org/wal.html "SQLite Write-Ahead Logging"
