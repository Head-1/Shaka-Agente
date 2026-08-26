# Validação repository-first

Este documento define a validação reproduzível do Shaka a partir de um checkout do GitHub. O repositório é a fonte da verdade: o operador deve clonar ou atualizar uma referência conhecida, confirmar o SHA e executar o contrato versionado abaixo. Evidências externas podem complementar a análise, mas não substituem a execução no commit publicado.

## Contrato

O executor `scripts/validate_postmerge.sh` deve ser executado em checkout limpo. Ele falha fechado se o SHA esperado não corresponder, se houver alterações locais ou se uma dependência operacional obrigatória estiver ausente. O executor não instala pacotes, não altera branches, não faz commit, não faz push e não modifica arquivos rastreados.

O contrato executa, nesta ordem, a identificação do checkout, validação de toolchain, sintaxe dos scripts, fmt, check locked, testes all-targets, Clippy com as exceções oficiais, secret scan, workflow policy, testes Python, version preflight, auditoria de dependências, build do CLI, smoke de produção e probe de ciclo de vida.

O executor pode receber `SHAKA_EXPECTED_HEAD` para tornar a validação vinculada a um commit específico. Os scripts usam as portas locais `29143` e `29144` por padrão; em hosts compartilhados, o operador deve fornecer portas livres com `SHAKA_SMOKE_API_PORT` e `SHAKA_LIFECYCLE_API_PORT`.

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

O `scripts/process_lifecycle_probe.sh` sobe `target/debug/shaka` somente em loopback, confirma `healthz`, encerra o processo com `SIGTERM`, verifica que a porta foi liberada, repete o ciclo com o mesmo banco temporário e limpa os artefatos próprios. Ele não acessa bancos, portas ou processos externos ao seu diretório e à porta informada.

Os probes multiprocesso especializados de `QueueStore` e auditoria continuam deliberadamente fora deste executor. As versões exploratórias existentes dependem de paths absolutos e artefatos fora do repositório; incorporá-las sem adaptação criaria uma falsa promessa de portabilidade. Eles devem voltar em uma mudança separada, como harnesses ou crates versionados com dependências relativas e contrato próprio.

## Critérios de aprovação

A execução só deve ser considerada aprovada quando terminar com `postmerge_validation=PASS`, `validation_exit=0`, working tree limpo e sem listener residual nas portas usadas. Um warning de documentação é dívida de qualidade e deve ser registrado, mas não pode ser confundido com erro de execução. Qualquer falha de processo, auditoria, integridade, autenticação ou limpeza deve interromper o fluxo e gerar investigação própria.

## Fluxo de mudanças

Toda alteração nasce de `origin/main` em uma branch de trabalho. A branch passa por CI, é validada em um segundo ambiente pelo SHA publicado e entra em `main` apenas por PR. Após o merge, execute novamente o contrato a partir de um clone novo de `main`. Nunca use `reset --hard` para esconder divergências locais e nunca use force push para contornar proteção.
