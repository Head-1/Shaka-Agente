# Validação repository-first

Este documento define a validação reproduzível do Shaka a partir de um checkout do GitHub. O repositório é a fonte da verdade: o operador deve clonar ou atualizar uma referência conhecida, confirmar o SHA e executar o contrato versionado abaixo. Evidências externas podem complementar a análise, mas não substituem a execução no commit publicado.

## Contrato

O executor `scripts/validate_postmerge.sh` deve ser executado em checkout limpo. Ele falha fechado se o SHA esperado não corresponder, se houver alterações locais ou se uma dependência operacional obrigatória estiver ausente. O executor não instala pacotes, não altera branches, não faz commit, não faz push e não modifica arquivos rastreados.

O contrato executa, nesta ordem, a identificação do checkout, validação de toolchain, sintaxe dos scripts, fmt, check locked, testes all-targets, Clippy com as exceções oficiais, secret scan, workflow policy, testes Python, version preflight, auditoria de dependências, build do CLI, build dos probes, smoke de produção, probe de ciclo de vida e probes multiprocesso de QueueStore/auditoria.

O executor pode receber `SHAKA_EXPECTED_HEAD` para tornar a validação vinculada a um commit específico. Os scripts usam as portas locais `29143` e `29144` por padrão; em hosts compartilhados, o operador deve fornecer portas livres com `SHAKA_SMOKE_API_PORT` e `SHAKA_LIFECYCLE_API_PORT`. O CI busca o banco de advisories do `cargo-audit` por padrão; `SHAKA_CARGO_AUDIT_NO_FETCH=1` habilita somente o modo offline quando esse banco já estiver instalado.

## Uso após clone

```bash
git clone --branch main --single-branch \
  git@github.com:Head-1/Shaka-Agente.git Shaka-Agente
cd Shaka-Agente
SHAKA_EXPECTED_HEAD="$(git rev-parse HEAD)" \
  SHAKA_VALIDATION_LOG="$HOME/shaka-validation.log" \
  bash scripts/validate_postmerge.sh
```

Para validar uma branch antes do merge, use a branch publicada e substitua o SHA pelo valor conferido no GitHub:

```bash
git clone --branch feat/<branch> --single-branch \
  git@github.com:Head-1/Shaka-Agente.git Shaka-Agente
cd Shaka-Agente
SHAKA_EXPECTED_HEAD="<sha-publicado>" \
  SHAKA_VALIDATION_LOG="$HOME/shaka-validation.log" \
  bash scripts/validate_postmerge.sh
```

## Probe de ciclo de vida

O `scripts/process_lifecycle_probe.sh` sobe `target/debug/shaka` somente em loopback, confirma `healthz`, encerra o processo com `SIGTERM`, verifica que a porta foi liberada, repete o ciclo com o mesmo banco temporário e limpa os artefatos próprios. O `scripts/process_crash_probes.sh` executa diretamente os binários `queue-process-crash-probe` e `audit-process-crash-probe`, propagando qualquer falha. Os probes não acessam bancos, portas ou processos externos ao seu diretório e aos seus artefatos temporários.

Os probes multiprocesso de `QueueStore` e auditoria agora são crates/binários versionados com dependências relativas. O cenário de QueueStore comprova recuperação de lease após abort do filho; o cenário de auditoria comprova oito appends concorrentes no mesmo SQLite, um filho abortado após persistir e cadeia válida após reabertura. Ambos usam somente processos, bancos e marcadores temporários próprios.

## Critérios de aprovação

A execução só deve ser considerada aprovada quando terminar com `postmerge_validation=PASS`, `validation_exit=0`, working tree limpo e sem listener residual nas portas usadas. Um warning de documentação é dívida de qualidade e deve ser registrado, mas não pode ser confundido com erro de execução. Qualquer falha de processo, auditoria, integridade, autenticação ou limpeza deve interromper o fluxo e gerar investigação própria.

## Fluxo de mudanças

Toda alteração nasce de `origin/main` em uma branch de trabalho. A branch passa por CI, é validada em um segundo ambiente pelo SHA publicado e entra em `main` apenas por PR. Após o merge, execute novamente o contrato a partir de um clone novo de `main`. Nunca use `reset --hard` para esconder divergências locais e nunca use force push para contornar proteção.
