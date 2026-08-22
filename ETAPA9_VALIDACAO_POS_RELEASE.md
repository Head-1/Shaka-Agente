# Etapa 9 — validação pós-release da v0.8.0

Data da validação: 22 de agosto de 2026.

## Escopo

Foi validado o binário Linux publicado na GitHub Release v0.8.0, sempre com banco SQLite temporário, tenant isolado e provider local. Nenhuma fonte do projeto, tag, release ou configuração remota foi alterada. Nenhuma ação externa foi autorizada.

## Artefato validado

| Item | Resultado |
|---|---|
| Binário | `shaka-linux-x86_64` |
| Versão informada | `shaka 0.8.0` |
| Release | `v0.8.0` |
| Commit da release | `73b4ed5c4e87999f993f8b4620842f6520a9cab5` |
| Provider | Local determinístico |
| Tenant de teste | `post-release` e `api-smoke` |
| Banco | Temporário em `/tmp` |
| Docker/Podman local | Não disponível no sandbox |

## Resultados da CLI

| Teste | Resultado | Observação |
|---|---|---|
| `--version` | PASS | Retornou `shaka 0.8.0`. |
| `--help` | PASS | Comandos principais disponíveis. |
| `config` | PASS | Ambiente Development, provider Local, sem API key e sem live. |
| `doctor` | PASS | `config_valid=true`, integridade SQLite verdadeira e `status=ready`. |
| `run` padrão | PASS | Resposta local com `success=true`; episódio persistido. |
| `verify-audit` como operator | BLOQUEADO | Falha esperada: operador comum não pode executar a ação administrativa. |
| `verify-audit` como administrator | PASS | Cadeia válida; 1 evento verificado. |
| `memory recent` | PASS | Episódio da tarefa recuperado. |
| `sandbox-demo` | PASS | `exit_code=42` e `fuel_consumed=2`. |
| `backup` como operator | BLOQUEADO | Falha esperada: backup exige administrator. |
| `backup` como administrator | PASS | Backup SQLite criado com permissões restritas. |
| `restore` em banco separado | PASS | Restore concluído; integridade e auditoria válidas. |
| `run --live` como operator | BLOQUEADO | Falha esperada: `RunExternal` não autorizado. |

## Resultados da API HTTP

O serviço foi iniciado em `127.0.0.1:18081`, com um banco temporário e um worker. Todos os testes abaixo passaram:

| Teste | Resultado |
|---|---|
| `GET /healthz` | PASS; `status=ok`, versão `0.8.0`, circuito `closed`. |
| `POST /v1/sessions` | PASS; sessão isolada criada. |
| `POST /v1/sessions/{id}/tasks` | PASS; tarefa enfileirada com `Idempotency-Key`. |
| Processamento do worker | PASS; tarefa chegou a `succeeded` com `attempts=1`. |
| `GET /v1/tasks/{id}` | PASS; resultado local recuperado. |
| Replay da mesma tarefa | PASS; mesmo `task_id`, sem segunda execução. |

## Integridade e distribuição

Antes desta etapa, os assets da GitHub Release também foram baixados e comparados com `SHA256SUMS`. O manifesto confirmou o binário e o SBOM; tar.gz e zip passaram nos testes estruturais `tar -tzf` e `unzip -t`. O SBOM inicia com `bomFormat: CycloneDX`.

A imagem GHCR foi publicada com as tags `v0.8.0` e `latest`, digest `sha256:d9b28642bbbb3f83c49cf46be306e4281b81ae246c7aaeb1d5953c905d5b694c`. O sandbox não possui Docker nem Podman, portanto a execução local do container não foi possível; a publicação e o build da imagem foram confirmados pelo workflow oficial de release.

## Conclusão

A v0.8.0 está operacionalmente consistente para uso local controlado em `dry-run`. Os controles de autorização, auditoria, sandbox, backup/restauração, API loopback e idempotência responderam conforme o desenho esperado.

Esta validação não constitui autorização para exposição pública nem para execução `live`. Antes de qualquer implantação externa ainda são necessários IAM remoto, cofre de segredos, HTTPS na borda, armazenamento persistente, backup externo, alertas e revisão de ameaça.
