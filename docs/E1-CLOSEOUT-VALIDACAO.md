# E1 — Relatório de closeout e evidências de validação

**Status:** implementação e validação operacional concluídas; publicação formal pendente de PR e CI
**Commit validado:** [`b6581c72bca5e2a69c7b066b7d525e8be21c73bd`](https://github.com/Head-1/Shaka-Agente/commit/b6581c72bca5e2a69c7b066b7d525e8be21c73bd)
**Branch:** `docs/e1-handoff-rpo-rto`
**Data do closeout:** 2026-11-09

## 1. Escopo

A E1 implementa backup externo durável para SQLite com:

- snapshots consistentes pela Online Backup API;
- manifesto sidecar SHA-256;
- destino externo em filesystem separado;
- disparo híbrido por evento pós-commit e timer de segurança;
- coalescência e execução single-flight;
- integração com o Plan Engine por `PlanBackupSink`;
- worker residente com shutdown gracioso;
- restauração fail-closed antes de qualquer alteração no destino.

A configuração operacional usada na VM foi:

```text
SHAKA_BACKUP_PATH=/mnt/shaka-e1-external/shaka.db
```

## 2. Validação no Sandbox

Antes da publicação, o Sandbox passou:

```text
cargo fmt --all -- --check                         PASS
RUSTFLAGS='-W missing_docs' cargo check ...        PASS
cargo test --workspace --all-targets --locked      PASS
git diff --check                                   PASS
```

Os testes direcionados incluíram manifesto verificável, divergência de checksum, rejeição de restore corrompido, coordenador híbrido, adaptador do Plan Engine e worker residente.

## 3. Validação independente na VM

A VM foi sincronizada diretamente para o commit `b6581c72bca5e2a69c7b066b7d525e8be21c73bd` em worktree limpo. A validação completa passou:

```text
vm_e1_commit_validation=PASS
```

A suíte executou, entre outros, 21 testes de `shaka-api`, 26 de `shaka-memory` e 46 de `shaka-queue`, todos sem falha.

O destino externo foi montado em `/mnt/shaka-e1-external` como `/dev/loop8`, enquanto o banco de trabalho permaneceu em `/dev/mapper/ubuntu--vg-ubuntu--lv`:

```text
external_filesystem_separation=PASS
backup_environment=PASS
```

O serviço iniciou com o worker habilitado e passou por health/readiness:

```text
service_started=PASS
backup_worker=ENABLED
service_health=PASS
```

A transição do Plan Engine foi exercitada com separação de funções: a tentativa de autoaprovação foi rejeitada, e a aprovação por `reviewer-e1` foi aceita. O checkpoint durável foi persistido:

```text
self_approval_rejected=PASS
plan_approved_by_distinct_reviewer=PASS
checkpoint_count=1
durable_plan_transition=PASS
```

O snapshot externo passou no checksum e na integridade SQLite:

```text
sha256_manifest=PASS
sqlite_integrity=PASS
```

O serviço encerrou graciosamente e o backup permaneceu verificável após o shutdown:

```text
graceful_shutdown=PASS
backup_survives_shutdown=PASS
```

## 4. Ensaio fail-closed

O snapshot oficial não foi modificado. Uma cópia foi alterada deliberadamente, mantendo o manifesto original. O restore rejeitou a cópia antes de modificar o destino:

```text
corrupt_restore_exit=1
corrupt_restore_rejected=PASS
target_unchanged_after_rejection=PASS
restore_fail_closed=PASS
```

O snapshot íntegro foi restaurado em banco temporário separado e passou em `PRAGMA integrity_check`:

```text
valid_restore=PASS
restored_sqlite_integrity=PASS
valid_restore_target=PASS
official_snapshot_integrity=PASS
restore_drill=PASS
```

## 5. Limites da evidência

Esta validação comprova o comportamento funcional e operacional da E1, mas não deve ser descrita como medição quantitativa formal de RPO/RTO. Ainda não foi executado um ensaio cronometrado de:

- perda máxima observada em uma janela contínua de cinco minutos sob falha catastrófica;
- tempo desde início de restore até `/readyz` pronto, com medição monotônica;
- recuperação completa incluindo cadeia de auditoria em um cenário de host destruído.

Assim, os valores de RPO de 0–5 minutos e RTO de até 1 minuto permanecem objetivos operacionais aprovados, não garantias quantitativamente medidas por este closeout.

## 6. Conclusão executiva

A E1 está implementada, testada no Sandbox, reproduzida na VM e aprovada para publicação formal por PR. O sistema agora recusa snapshots com checksum divergente, preserva o banco de destino quando a validação falha e restaura snapshots válidos em um destino separado.

A E2 não deve começar antes de a PR da E1 passar pela CI e pelo processo de merge protegido. A medição quantitativa de RPO/RTO permanece backlog explícito, não uma lacuna silenciosa.
