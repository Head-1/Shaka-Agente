# Runbook Operacional do Shaka v0.8.2

## 1. Escopo e modelo de segurança

Este runbook descreve a operação controlada do **Shaka v0.8.2**, release publicada com binário Linux, SBOM CycloneDX, checksums e imagem no GHCR. Ele foi escrito para que um operador consiga instalar, verificar, executar, diagnosticar e recuperar o agente sem precisar conhecer a implementação interna em Rust.

A operação padrão é local e segura. A API vincula-se a `127.0.0.1`, o provedor padrão é local, tarefas começam em `dry-run` e nenhuma ação externa deve ser liberada por padrão. Os planos `live` permanecem bloqueados na v0.8.2. Mensageria externa, pesquisa autônoma na web, autopromoção de skills e controle irrestrito de subagentes não fazem parte desta release.

> **Princípio operacional:** diante de ambiguidade, falha ou inconsistência, o Shaka deve bloquear a transição, preservar evidências e exigir decisão humana.

A release não deve ser exposta diretamente à internet. Uma implantação pública exigirá, em ciclo posterior, IAM remoto forte, cofre de segredos, HTTPS na borda, armazenamento persistente, backup externo, métricas e revisão de segurança específica.

## 2. Artefatos e pré-condições

Os artefatos oficiais estão na [GitHub Release v0.8.2](https://github.com/Head-1/Shaka-Agente/releases/tag/v0.8.2). Os downloads recomendados são `shaka-linux-x86_64`, `shaka-v0.8.2-linux-x86_64.tar.gz`, `shaka-v0.8.2-linux-x86_64.zip`, `shaka.cdx.json` e `SHA256SUMS`.

Antes de instalar, valide os hashes publicados. O manifesto referencia os arquivos dentro de `dist/`:

```bash
mkdir -p dist
cp shaka-linux-x86_64 shaka.cdx.json dist/
sha256sum -c SHA256SUMS
```

O arquivo `SHA256SUMS` deve responder `OK` para o binário e para o SBOM. Os pacotes compactados devem passar nos testes de integridade:

```bash
tar -tzf shaka-v0.8.2-linux-x86_64.tar.gz >/dev/null
unzip -t shaka-v0.8.2-linux-x86_64.zip
```

Dê permissão de execução somente ao binário baixado e mantenha os arquivos de dados separados do código:

```bash
chmod 0555 ./shaka-linux-x86_64
./shaka-linux-x86_64 --version
```

A saída esperada para a release é `shaka 0.8.2`. Se a execução for feita a partir do código-fonte, use Rust/Cargo 1.98.0 ou toolchain compatível com edition 2024:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version
cargo --version
cargo test --workspace --locked
```

Não armazene chaves de modelo, tokens IAM, bancos de dados de usuários ou backups em arquivos versionados. O arquivo SQLite e o catálogo de skills podem conter dados sensíveis.

## 3. Configuração local segura

Defina um diretório de dados restrito ao operador. Os parâmetros abaixo também podem ser passados diretamente à CLI; variáveis de ambiente são preferíveis para configuração repetível.

```bash
export SHAKA_DATABASE="$PWD/data/shaka.db"
export SHAKA_SKILLS_FILE="$PWD/data/skills.json"
export SHAKA_TRUST_FILE="$PWD/data/trusted_keys.json"
export SHAKA_TENANT="demo"
export SHAKA_OPERATOR="operator"
export SHAKA_ROLE="operator"
export SHAKA_ENVIRONMENT="development"
```

Verifique a configuração sem fornecer chave de modelo:

```bash
./shaka-linux-x86_64 config
./shaka-linux-x86_64 doctor
```

Uma resposta operacionalmente pronta deve apresentar `config_valid: true`, `database_integrity: true` e `status: "ready"`. O campo `api_key_configured` deve permanecer `false` quando o provedor local estiver sendo usado.

Para usar um provedor OpenAI-compatível em ambiente controlado, injete a chave somente pelo ambiente ou por um cofre externo. Nunca registre a chave em shell history, logs, prompts, banco, catálogo ou auditoria:

```bash
export SHAKA_MODEL_PROVIDER=openai-compatible
export SHAKA_MODEL_API_KEY="chave-fornecida-pelo-operador"
export SHAKA_MODEL_ENDPOINT="https://provedor.example/v1/chat/completions"
export SHAKA_MODEL="modelo-aprovado"
```

Em produção, a configuração exige provedor externo HTTPS, chave válida, auditoria habilitada e validação explícita. Isso não libera automaticamente efeitos externos.

## 4. Execução de uma tarefa em dry-run

A execução padrão usa o provedor local e não solicita efeitos externos:

```bash
./shaka-linux-x86_64 run "Descreva em uma frase a política deny-by-default do Shaka"
```

Uma execução bem-sucedida retorna JSON com `task_id`, `answer`, `tool_results` e `success: true`. O episódio é persistido no SQLite do tenant atual.

O operador não deve usar `--live`. O teste de segurança da release confirmou que uma tentativa de `run --live` por um operador comum é bloqueada por autorização. Qualquer proposta futura de execução real exige mudança de governança, aprovação explícita, revisão de permissões e novo ciclo de release.

## 5. Health check e servidor HTTP local

Inicie o servidor somente em loopback:

```bash
./shaka-linux-x86_64 serve --bind 127.0.0.1:8080 --workers 2
```

O health check pode ser consultado com:

```bash
curl --fail --silent http://127.0.0.1:8080/healthz
```

A resposta esperada contém `status: "ok"`, `version: "0.8.2"` e circuito `closed`. A lista completa dos endpoints, incluindo o Plan Engine, está em [`docs/API_PUBLICA.md`](docs/API_PUBLICA.md).

Use também o readiness operacional antes de encaminhar tráfego ou aceitar uma operação administrativa:

```bash
curl --fail --silent http://127.0.0.1:8080/readyz
```

Em loopback sem `SHAKA_API_KEY`, a política local usa o principal local. Em bind não local, envie `Authorization: Bearer <token>` com uma credencial válida. O readiness verifica a integridade dos stores, a cadeia de auditoria do tenant autenticado, a fila e o circuito. `200 OK` com `status: "ready"` significa que os sinais estão prontos; `503 Service Unavailable` com `status: "failed"` significa que o processo está vivo, mas não deve receber operação até a causa ser investigada. Um bearer inválido retorna `401 Unauthorized`.

Os endpoints principais são:

| Endpoint | Finalidade |
|---|---|
| `GET /healthz` | Saúde pública mínima: versão, fila e circuito |
| `GET /readyz` | Readiness protegido: integridade, auditoria, fila e circuito |
| `POST /v1/sessions` | Criar sessão local |
| `GET /v1/sessions/{session_id}` | Consultar sessão |
| `POST /v1/sessions/{session_id}/tasks` | Enfileirar tarefa |
| `GET /v1/tasks/{task_id}` | Consultar estado e resultado |
| `DELETE /v1/tasks/{task_id}` | Solicitar cancelamento |

Toda submissão de tarefa exige um `Idempotency-Key`. Exemplo seguro:

```bash
SESSION=$(curl --fail --silent -X POST http://127.0.0.1:8080/v1/sessions \
  -H 'content-type: application/json' \
  -d '{"metadata":{"source":"manual"}}' \
  | sed -n 's/.*"session_id":"\([^"]*\)".*/\1/p')

curl --fail --silent -X POST "http://127.0.0.1:8080/v1/sessions/$SESSION/tasks" \
  -H 'content-type: application/json' \
  -H 'Idempotency-Key: manual-task-1' \
  -d '{"objective":"Descreva a política de execução segura","priority":5}'
```

O mesmo `Idempotency-Key` e o mesmo payload devem retornar a tarefa já existente, sem criar uma segunda execução ou checkpoint de admissão adicional. Não faça bind em `0.0.0.0` em ambiente real sem autenticação, HTTPS na borda e revisão de exposição.

A imagem GHCR usa `shaka serve` como comando padrão, expõe a porta 8080 e possui `doctor` como healthcheck. Um host com Docker pode executar a imagem privada depois de autenticar no GHCR:

```bash
docker pull ghcr.io/head-1/shaka-agente:v0.8.2
docker run --rm \
  -p 127.0.0.1:8080:8080 \
  -v "$PWD/data:/app/data" \
  ghcr.io/head-1/shaka-agente:v0.8.2
```

A validação da release v0.8.2 confirmou a publicação de `v0.8.2` e `latest` no GHCR, com digest `sha256:7e1c0f36cbe2643f5271c7a4dceb59cff0f105b9959b85eb0acf0f410eea5dd`. Não foi executado Docker localmente no sandbox de validação porque Docker/Podman não estão instalados nesse ambiente.

## 6. Diagnóstico e auditoria

Antes de qualquer alteração operacional, execute:

```bash
./shaka-linux-x86_64 doctor
./shaka-linux-x86_64 verify-audit
```

`verify-audit` exige papel `administrator`. Uma cadeia inválida bloqueia a promoção e deve ser tratada como incidente. Para consultar episódios recentes:

```bash
./shaka-linux-x86_64 memory recent --limit 20
```

Classifique a falha antes de repetir uma tarefa:

| Sintoma | Conduta |
|---|---|
| `DeadlineExceeded` | Reduzir escopo, revisar timeout e validar o provedor. |
| `tool_not_found` | Não adicionar a ferramenta automaticamente; revisar contrato e catálogo. |
| `schema` | Corrigir schema ou adapter; nunca executar JSON diretamente. |
| `capability denied` | Manter o bloqueio até demonstrar necessidade e autorização. |
| `unknown` | Exigir resolução humana; não presumir sucesso nem repetir cegamente. |
| `HostImportsDenied` | Preservar o artefato e revisar sandbox, threat model e imports. |
| erro HTTP do modelo | Verificar endpoint, credencial, limite e contrato sem registrar segredo. |
| inconsistência de reducer | Parar novas transições, preservar evidências e abrir incidente. |

## 7. Backup, restauração e integridade

Backup e restauração exigem papel `administrator`. Faça backup antes de mudanças relevantes:

```bash
./shaka-linux-x86_64 --role administrator \
  backup --output "backups/shaka-$(date -u +%Y%m%dT%H%M%SZ).db"
```

Restaure primeiro em uma cópia de trabalho, nunca diretamente sobre o único banco operacional:

```bash
./shaka-linux-x86_64 --role administrator \
  --database data/restore-test.db \
  restore --input backups/shaka-arquivo.db

./shaka-linux-x86_64 --role administrator \
  --database data/restore-test.db \
  doctor

./shaka-linux-x86_64 --role administrator \
  --database data/restore-test.db \
  verify-audit
```

O backup deve ser transferido para armazenamento externo criptografado. Defina RPO/RTO, retenção, criptografia, rotação e teste periódico de restauração antes de uma implantação pública.

## 8. Skills e aprovações humanas

A criação de skill registra uma candidata; não gera nem executa código automaticamente:

```bash
./shaka-linux-x86_64 skill candidate relatorio \
  "Gera um relatório estruturado" --permissions memory-write
./shaka-linux-x86_64 skill list
```

A aprovação exige papel `reviewer` ou `administrator`, hash SHA-256 completo, justificativa e revisão independente do artefato:

```bash
./shaka-linux-x86_64 --role reviewer skill approve relatorio \
  --artifact artefato.wasm \
  --reason "Aprovada após revisão manual e testes locais"
```

Para revogar uma skill ativa:

```bash
./shaka-linux-x86_64 --role reviewer skill revoke relatorio
./shaka-linux-x86_64 skill list
```

Não edite manualmente o catálogo para contornar uma transição. Registre horário, operador, versão, hash, motivo e evidências.

## 9. Rotação de credenciais e retenção

Novos tokens IAM exigem expiração futura e finita, limitada a 90 dias. Registros legados com `expires_at = NULL` são rejeitados na autenticação e não contam como tokens ativos. O sistema não executa migração ou revogação em massa automaticamente; substitua essas credenciais por tokens novos em uma operação controlada.

Para rotacionar uma credencial, remova o valor antigo do ambiente, injete o novo por mecanismo seguro e execute um teste sem registrar a chave:

```bash
unset SHAKA_MODEL_API_KEY
export SHAKA_MODEL_API_KEY="nova-chave"
./shaka-linux-x86_64 run "teste de conectividade controlado"
```

A memória episódica deve seguir a política do tenant:

```bash
./shaka-linux-x86_64 memory purge --days 30
```

Antes do expurgo, confirme backup, retenção e eventuais obrigações de preservação. O parâmetro `--days` deve ser zero ou positivo; valores negativos são rejeitados para impedir que a operação transforme o cutoff em uma data futura e apague memória recente. O expurgo do MVP não substitui processo formal de privacidade ou restauração.

## 10. Sandbox WASM

Execute o exemplo seguro:

```bash
./shaka-linux-x86_64 sandbox-demo
```

O resultado esperado contém `exit_code: 42` e `fuel_consumed` positivo. O comportamento de segurança esperado é: módulo puro executa; módulo que importa função do host é rejeitado; rede, filesystem, WASI e imports do host permanecem negados por padrão. A memória linear do guest é limitada a `16 MiB` por default e `64 MiB` no máximo da política; isso não limita o RSS total do processo e não substitui isolamento de processo ou cgroup.

## 11. Resposta a incidentes

Trate como incidente crítico qualquer execução fora do sandbox, autopromoção de skill, exposição de segredo, vazamento entre tenants, envio externo não autorizado ou alteração de logs. A primeira resposta é conter: interromper novas execuções, revogar a skill afetada, preservar evidências, fazer backup e rotacionar credenciais potencialmente expostas.

O MVP não fornece contenção remota automática. O responsável técnico deve preservar o `task_id`, versão, hash do artefato, logs, tenant, operador, horário e sequência de comandos. Não apague o banco original nem force transições diretamente no SQLite.

## 12. Validação pós-release registrada

A validação da release v0.8.2 em 22 de agosto de 2026 confirmou versão, configuração, `doctor`, execução local em dry-run, sandbox, backup, restauração, auditoria, health check HTTP, criação de sessão, execução de tarefa e replay idempotente. O workflow também executou preflight de versão, auditoria de dependências, smoke test de produção, geração de SBOM, checksums e publicação no GHCR. A tentativa de `live` por operador comum foi bloqueada conforme esperado.

O relatório operacional detalhado está em `ETAPA9_VALIDACAO_POS_RELEASE.md`; as evidências da publicação e dos hashes estão registradas no relatório local `V0.8.2_RELEASE_RESULT.md`. As evidências não alteram a tag `v0.8.2`. Para a referência normativa da API, consulte [`docs/API_PUBLICA.md`](docs/API_PUBLICA.md). O status de confiabilidade dos BR-01 a BR-06 está em [`docs/BACKLOG_STATUS.md`](docs/BACKLOG_STATUS.md).

## 13. Validação repository-first

Para validar um checkout limpo diretamente do GitHub, incluindo testes, auditoria, smoke, ciclo de vida do processo e probes multiprocesso de QueueStore/auditoria, consulte [`docs/VALIDACAO_REPOSITORY_FIRST.md`](docs/VALIDACAO_REPOSITORY_FIRST.md) e o status consolidado em [`docs/BACKLOG_STATUS.md`](docs/BACKLOG_STATUS.md), e execute:

```bash
SHAKA_EXPECTED_HEAD="$(git rev-parse HEAD)" \
  SHAKA_VALIDATION_LOG="$HOME/shaka-validation.log" \
  bash scripts/validate_postmerge.sh
```

O executor falha fechado diante de SHA divergente, working tree sujo, dependência obrigatória ausente, falha de teste, erro de smoke, falha de crash/recovery ou listener residual. Ele não instala dependências, não altera branches e não publica alterações. A cadeia de auditoria usa ordem estrutural de commit, não `occurred_at`; esse campo é metadado temporal e pode sofrer ajuste de relógio sem reordenar os eventos.
