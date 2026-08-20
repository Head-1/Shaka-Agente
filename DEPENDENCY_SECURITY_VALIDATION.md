# Validação externa de dependências

## Wasmtime

A versão inicialmente usada pelo MVP era Wasmtime 27.0.0. A execução de `cargo audit` em 20 de agosto de 2026 encontrou advisories RustSec associados a essa versão, incluindo riscos de crash, problemas de component model e advisories críticos relacionados a sandbox escape. A decisão foi atualizar para Wasmtime 47.0.3, versão disponível no registro crates.io e com `rust-version` compatível com o toolchain utilizado.

Fontes consultadas:

- [Wasmtime 47.0.3 no docs.rs](https://docs.rs/wasmtime/47.0.3)
- [Wasmtime no crates.io](https://crates.io/crates/wasmtime)
- [RUSTSEC-2026-0095 — Wasmtime com Winch pode permitir acesso de memória fora do sandbox](https://rustsec.org/advisories/RUSTSEC-2026-0095)
- [RUSTSEC-2026-0096 — heap access mal compilado pode permitir escape do sandbox](https://rustsec.org/advisories/RUSTSEC-2026-0096)
- [RUSTSEC-2026-0222 — mistura de índices de tipo entre engines](https://rustsec.org/advisories/RUSTSEC-2026-0222)

## Resultado da auditoria

Depois da atualização do lockfile para Wasmtime 47.0.3, o comando `cargo audit` foi executado contra as dependências do workspace e terminou sem advisories reportados. A CI agora trata `cargo audit` como gate obrigatório, sem `continue-on-error`.

A auditoria não substitui revisão de configuração do host, isolamento de processo, atualização contínua, assinatura de artefatos ou testes adversariais do sandbox.
