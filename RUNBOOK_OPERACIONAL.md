# Runbook Operacional do Shaka

## 1. Escopo

Este runbook descreve a operação da release candidata para produção controlada. Ele foi escrito para permitir que uma pessoa que não participou da implementação consiga iniciar o agente, verificar o estado, executar uma tarefa em modo seguro, investigar falhas, operar backups e revogar uma skill.

A release é um processo de linha de comando, não um serviço 24/7. Ela não recebe webhooks, não mantém workers permanentes e não envia mensagens externas. Antes de exposição remota, substituir a identidade local por IAM forte.

## 2. Pré-condições

A máquina deve possuir Rust stable, Cargo e acesso ao filesystem do projeto. O operador precisa conhecer o tenant e usar uma identidade própria na variável `SHAKA_OPERATOR`. Chaves de modelo devem existir somente no ambiente ou em um cofre externo; nunca devem ser adicionadas a arquivos versionados.

Verificar o ambiente:

```bash
cd Shaka
rustc --version
cargo --version
cargo test --workspace
```

## 3. Inicialização segura

A primeira execução deve usar o modelo local e dry-run. O papel padrão é `operator`, que não pode executar efeitos externos, aprovar/revogar skills, restaurar banco ou operar dados de outro tenant:

```bash
cargo run -- run "Faça um diagnóstico resumido do objetivo X"
```

Uma execução correta produz JSON com `task_id`, `answer`, `tool_results` e `success`. Também grava um episódio em `data/shaka.db`.

Se o diretório `data` não existir, a CLI o cria. Os arquivos `data/shaka.db` e `data/skills.json` são dados operacionais e devem ser tratados como sensíveis quando contiverem conteúdo de usuários.

## 4. Diagnóstico de configuração e prontidão

Executar antes de uma mudança ou release:

```bash
cargo run -- config
cargo run -- doctor
cargo run -- verify-audit
```

`doctor` verifica configuração, integridade SQLite, existência do catálogo e cadeia de auditoria. Uma resposta com `status=failed` bloqueia a promoção. Em `production`, a configuração exige provedor externo, API key, endpoint HTTPS e auditoria habilitada. Modo live exige administrador e `SHAKA_CONFIRM_LIVE=true`.

## 5. Backup e restore

Criar backup online:

```bash
cargo run -- backup --output backups/shaka-$(date -u +%Y%m%dT%H%M%SZ).db
```

Restaurar exige administrador e valida a integridade após a operação:

```bash
cargo run -- restore --input backups/shaka-arquivo.db
```

O backup deve ser transferido para armazenamento externo criptografado e a restauração deve ser testada periodicamente em banco separado.

## 6. Verificação do sandbox

Executar:

```bash
cargo run -- sandbox-demo
```

O resultado esperado é um JSON com `exit_code` igual a `42` e um valor positivo de `fuel_consumed`.

Os testes adversariais básicos podem ser repetidos com:

```bash
cargo test -p shaka-sandbox
```

O comportamento esperado é: módulo puro executa; módulo que importa função do host é rejeitado; capability de rede é negada por padrão.

## 7. Diagnóstico de falha de execução

Quando uma tarefa falhar, primeiro capture o `task_id` e repita em modo local:

```bash
RUST_LOG=shaka=debug cargo run -- run "mesmo objetivo"
```

Depois consulte o episódio:

```bash
cargo run -- memory recent --limit 20
```

Classifique a falha antes de tentar novamente:

| Sintoma | Causa provável | Ação |
|---|---|---|
| `DeadlineExceeded` | Modelo ou ferramenta demorou além do orçamento | Reduzir escopo, revisar timeout e verificar endpoint. |
| `tool_not_found` | O modelo propôs ferramenta não registrada | Não adicionar automaticamente; revisar contrato e catálogo. |
| `schema` | Argumentos não são um objeto válido | Corrigir schema/adapter; nunca executar o JSON diretamente. |
| `capability denied` | Ferramenta exige permissão não concedida | Revisar se a permissão é necessária; manter deny-by-default. |
| `dry_run` | Ação tem efeito externo | Confirmar intenção e implementar aprovação antes de habilitar. |
| `HostImportsDenied` | WASM solicita função do host | Inspecionar o artefato; não liberar import sem ADR e threat model. |
| `fuel` ou timeout do sandbox | Código excedeu limite | Reproduzir com fixture; não aumentar o limite indiscriminadamente. |
| erro HTTP do modelo | Endpoint, credencial, rate limit ou schema do provedor | Validar variáveis, endpoint e contrato do provedor sem registrar a chave. |

## 8. Revogação de skill

Se uma skill ativa apresentar comportamento inesperado, revogue-a imediatamente:

```bash
cargo run -- skill revoke NOME_DA_SKILL
```

A revogação deve ser registrada em uma ocorrência operacional com horário, operador, versão, hash do artefato, motivo e evidências. Não substitua o arquivo manualmente sem preservar o catálogo e a trilha de auditoria.

Após a revogação, confirme que ela não aparece na lista de ativas:

```bash
cargo run -- skill list
```

Se a revogação falhar porque a skill não está no estado `Active`, preserve a saída e trate como divergência de estado. Não force a alteração diretamente no JSON sem backup.

## 9. Criação e aprovação de skill

A criação de skill no MVP registra uma candidata; ela não gera nem executa código automaticamente. O fluxo operacional atual é:

```bash
cargo run -- skill candidate NOME "Descrição" --permissions memory-write
cargo run -- skill list
```

Antes da aprovação, o revisor deve revisar a interface, permissões, código/artefato produzido fora do catálogo e resultado dos testes. O caminho recomendado calcula o SHA-256 do arquivo real:

```bash
cargo run -- skill approve NOME --artifact artefato.wasm --reason "Justificativa completa"
```

Também é possível informar um hash completo calculado em fluxo externo:

```bash
cargo run -- skill approve NOME HASH --reason "Justificativa completa"
```

A ausência de hash, hash inválido ou justificativa vazia deve bloquear a transição.

## 10. Rotação de credenciais

No MVP, credenciais de modelo são lidas de `SHAKA_MODEL_API_KEY`. Para rotacionar uma chave, remova o valor antigo do ambiente, injete o novo valor por um mecanismo seguro e execute uma chamada de teste sem registrar o segredo:

```bash
unset SHAKA_MODEL_API_KEY
export SHAKA_MODEL_API_KEY="nova-chave"
RUST_LOG=shaka=info cargo run -- run "teste de conectividade"
```

Não coloque credenciais em `.env` versionado, argumentos visíveis do shell, logs, episódios, prompts ou campos de auditoria.

## 11. Retenção e expurgo

A memória episódica não deve crescer indefinidamente. O operador deve aplicar a política do tenant:

```bash
cargo run -- memory purge --days 30
```

Antes do expurgo em ambiente real, confirme a política de retenção, o backup e eventuais obrigações de preservação. O comando do MVP não substitui um processo formal de direito ao esquecimento ou restauração.

## 12. Recuperação de dados

O comando de backup/restauração já existe, mas não substitui armazenamento externo criptografado. Preserve o arquivo original, valide a cadeia de auditoria e execute o restore primeiro em cópia de trabalho. Antes de uma implantação pública, definir RPO/RTO, retenção de backups, criptografia, rotação e teste periódico de restauração.

## 13. Escalonamento de incidente

Classifique como incidente crítico qualquer execução de código fora do sandbox, autopromoção de skill, exposição de segredo, vazamento entre tenants, envio externo não autorizado ou alteração de logs. A primeira ação é conter: revogar skill, interromper novas execuções, preservar evidências e rotacionar credenciais afetadas. O MVP não fornece contenção remota automática; esse processo deve ser operado pelo responsável técnico.
