# Branch Gates para Rust e GitHub

## Pré-condições

Executar no checkout limpo e a partir de uma branch curta:

```bash
git status --short --branch
git fetch origin main
git diff --check
rustc --version
cargo --version
```

Fixar o toolchain declarado pelo projeto. No Shaka, a CI usa Rust `1.98.0`:

```bash
rustup toolchain install 1.98.0
rustup component add rustfmt clippy --toolchain 1.98.0
. "$HOME/.cargo/env"
```

## Gates padrão

Executar nesta ordem e salvar a saída em arquivos fora do checkout quando for necessária auditoria:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- \
  -D warnings -A missing_docs -A clippy::missing_errors_doc
cargo audit
python3 scripts/secret_scan.py
python3 scripts/workflow_policy_check.py
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 scripts/version_preflight.py --allow-no-tag --allow-unreleased
bash scripts/production_smoke.sh
```

Para um gate de bugfix, preservar quatro evidências: teste que falha antes, patch mínimo, mesmo teste passando depois e suíte completa passando. Um código de saída não-zero por ambiente deve ser corrigido no ambiente e repetido; não mascarar a falha.

## Gates de persistência e concorrência

Para memória, fila ou Plan Engine, adicionar testes que:

1. corrompam UUID, timestamp, JSON, hash ou sequência e exijam erro explícito;
2. repitam a mesma operação com a mesma chave de idempotência;
3. repitam `recover_unknown`, `force_unknown`, retry e cancelamento;
4. simulem dois workers reclamando o mesmo trabalho;
5. injetem falha entre transition, checkpoint, snapshot e atualização da task;
6. reconstruam o estado a partir do histórico e comparem com o snapshot;
7. verifiquem isolamento por tenant e rejeição de digest divergente.

## Gates de segurança de agente

Verificar que o modelo não decide autoridade, que o contexto por request contém tenant/operador/papel/capabilities/orçamento, que ferramentas validam schema e modo, que `dry-run` é o padrão e que efeitos externos são bloqueados por ausência de aprovação. Validar redaction de cabeçalhos com espaços, Bearer, JSON aninhado, mensagens de erro e resultados de ferramentas.

## Commit e assinatura

Configurar a assinatura somente no repositório, usando chave privada local ao ambiente:

```bash
git config gpg.format ssh
git config user.signingkey "$HOME/.ssh/shaka-agente-signing.pub"
git config commit.gpgsign true
git commit -S -m "tipo(escopo): descrição curta"
git verify-commit HEAD
git show -s --format='%H%n%G? %GS %GK%n%s' HEAD
```

Cadastrar no GitHub apenas a chave pública como Signing Key. Após publicar, comparar SHA local e remoto e confirmar a API/página do commit com `verified: true`. Nunca copiar a chave privada para o segundo ambiente; gerar outra chave e cadastrar sua parte pública.

## Publicação

```bash
git diff --check
git status --short --branch
git push --set-upstream origin nome-da-branch
git ls-remote origin refs/heads/nome-da-branch
```

Não abrir PR, fazer merge, criar tag ou publicar release sem autorização separada. Se um commit remoto precisar ser substituído por correção de identidade ou assinatura, usar `git push --force-with-lease` e registrar o novo SHA e a razão.

## Relatório

Registrar branch, base, SHA, diff, teste pré-patch, teste pós-patch, contagens da suíte, gates, assinatura local, verificação remota, limitações, riscos residuais e comandos para o segundo ambiente. Nunca afirmar portabilidade antes da execução no segundo ambiente.
