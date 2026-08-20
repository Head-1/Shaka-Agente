# Shaka — Avaliação de prontidão e roadmap técnico

## Conclusão executiva

O Shaka **não está totalmente pronto para qualquer cenário**, mas está em uma posição sólida como **produção candidata para operação local ou privada, controlada por operador humano**. A release já possui uma base acima de um MVP comum: workspace Rust modular, memória SQLite, RBAC local, validação de contratos, redaction, auditoria encadeada, backup/restore, sandbox Wasmtime deny-by-default, catálogo de skills com aprovação explícita, `doctor`, smoke test e CI/CD.

A própria documentação classifica a versão como produção candidata e registra que IAM remoto, cofre de segredos, backup externo, métricas remotas, mensageria, pesquisa web, subagentes distribuídos e multi-tenancy forte ainda não estão implementados.[1] Essa classificação está correta e deve ser preservada; promover o sistema agora como agente público 24/7 criaria uma falsa sensação de prontidão.

> A próxima evolução mais importante não é tornar o Shaka mais autônomo. É tornar o **plano de controle, autorização, operação e recuperação** mais forte, sem remover a governança humana.

## O que já está pronto

| Área | Avaliação |
|---|---|
| Núcleo Rust modular | Pronto para desenvolvimento e operação controlada. |
| Execução local | Funcional com o `LocalModel`, memória persistente e CLI. |
| Segurança básica de ferramentas | Schema, capability, dry-run e orçamento já existem. |
| Governança de skills | Candidata → aprovação humana → ativa → revogada, com SHA-256. |
| Sandbox | Wasmtime sem imports/WASI, com fuel e interrupção por epoch. |
| Auditoria | Cadeia de hashes por tenant e verificação operacional. |
| Continuidade local | Backup, restore e integrity check implementados. |
| CI | A execução #23 passou format, check, test, Clippy, audit e smoke test.[2] |
| Agente generalista com ferramentas reais | Ainda não. A CLI registra atualmente apenas `EchoTool`; skills aprovadas ainda não são executadas pelo runtime. |
| Serviço remoto 24/7 | Ainda não. Não há IAM remoto, fila, workers, observabilidade remota ou adaptadores externos. |

## Lacunas técnicas mais relevantes

### 1. O runtime ainda é um núcleo conservador, não um agente completo

O runtime executa uma única conclusão do modelo e processa as chamadas de ferramentas retornadas. Ele não possui ainda um ciclo completo de planejamento, chamada de ferramenta, retorno do resultado ao modelo e nova conclusão. A CLI registra apenas uma ferramenta `echo`; o `OutboundMessageTool` existe como placeholder, mas não é um adaptador real e retorna `not_configured`.

A melhoria recomendada é implementar um **loop de execução governado**, com no máximo N ciclos, deadline global, limite de chamadas, cancelamento, resultado de ferramenta redigido, reavaliação de autorização a cada ciclo e auditoria de cada decisão. O loop não deve permitir que o modelo conceda permissões a si próprio.

### 2. Skills aprovadas ainda não entram no caminho de execução

A governança do catálogo está bem posicionada, mas aprovação não equivale a execução. O próximo estágio deve ser um pipeline explícito para skills humanas ou geradas sob supervisão: registro do artefato, build isolado sem segredos, testes adversariais, verificação de hash, assinatura, aprovação, ativação e rollback.

Essa capacidade deve continuar **sem autopromoção e sem autoaperfeiçoamento**. O agente pode sugerir uma skill candidata, mas somente um operador autorizado pode aprovar, ativar, revogar ou alterar permissões.

### 3. A identidade atual não é autenticação forte

O papel e o operador ainda são fornecidos por configuração local. Isso é aceitável para uma CLI em uma máquina controlada, mas não para exposição remota. Antes de servir usuários reais, o Shaka precisa de autenticação forte, sessões assinadas, autorização no host e separação entre identidade autenticada e parâmetros enviados pelo cliente.

Para ambientes multi-tenant, também serão necessários isolamento de dados, quotas por tenant, políticas ABAC quando apropriado, criptografia, testes negativos e auditoria de acesso.

### 4. Limites de tempo e falhas precisam cobrir a execução inteira

A chamada ao modelo possui timeout, mas a execução de ferramentas precisa de deadline e cancelamento próprios. O runtime deve aplicar um orçamento global da tarefa, limites individuais por ferramenta, limite de payload, limite de memória e política de falha. Falhas de modelo, ferramentas ou persistência devem produzir um evento de auditoria de falha sempre que possível, evitando lacunas silenciosas na evidência.

Também são recomendados backoff, circuit breaker, idempotência e prevenção de retries duplicados antes de conectar serviços externos.

### 5. Auditoria local não é evidência imutável de produção

A cadeia de hashes melhora a detecção de alteração, mas continua armazenada no mesmo ambiente operacional do SQLite. Em produção, a auditoria deve ser exportada para armazenamento append-only ou WORM, com retenção definida, controle de acesso, sincronização e alerta quando houver quebra de cadeia.

### 6. Backup local não é recuperação de desastre

Backup/restore está implementado, mas o arquivo local não protege contra perda da máquina, ransomware ou corrupção do filesystem. A evolução mínima inclui backup criptografado externo, política de retenção, teste periódico de restauração, identificação da versão do schema e procedimento documentado de recuperação.

### 7. Supply chain e artefatos de release precisam de endurecimento

Os próximos gates devem incluir SBOM, assinatura e verificação dos artefatos, imagem Docker referenciada por digest, dependências e actions pinadas por commit, política de licenças, secret scanning e build reproduzível. O workflow deve publicar evidência de hashes e anexar os resultados dos testes à release.

### 8. O container precisa de uma postura operacional mais restrita

O Dockerfile já executa como usuário não-root e possui healthcheck. Antes de uma implantação real, deve-se adicionar filesystem somente leitura quando possível, diretório de dados dedicado, remoção de capabilities Linux, seccomp/AppArmor, limites de CPU/memória, base referenciada por digest, atualização controlada de pacotes e política para não expor a chave do modelo a processos desnecessários.

## Roadmap recomendado

| Prioridade | Entrega | Motivo | Critério de aceite |
|---|---|---|---|
| P0 | Alinhar a próxima tag de release com a `main` corrigida | Evita que a tag antiga represente uma árvore incompleta | Nova tag criada sobre commit validado; CI e release passam a partir dela. |
| P0 | Autenticação forte e autorização no host | Impede que qualquer usuário se autodeclare administrador | Identidade verificável, sessão assinada, RBAC aplicado no servidor e testes negativos. |
| P0 | Deadline global e timeout por ferramenta | Evita travamentos e uso ilimitado de recursos | Cancelamento propagado, limites testados e auditoria de timeout/falha. |
| P0 | Backup externo criptografado e restore drill | Torna a recuperação operacional real | Restauração periódica automatizada em ambiente isolado. |
| P0 | SBOM, assinatura de release, secret scanning e actions pinadas | Reduz risco de supply chain | Artefato verificado antes da promoção; nenhum segredo detectado. |
| P1 | Loop multi-turno de ferramentas com reautorização | Transforma o núcleo em agente funcional sem abrir mão do controle | Cada ciclo tem budget, schema, capability, dry-run, auditoria e limite máximo. |
| P1 | Pipeline de skills aprovadas | Integra governança ao runtime de forma verificável | Build sem segredos, testes adversariais, hash/assinatura, ativação e rollback. |
| P1 | Observabilidade remota | Permite operar com diagnóstico e alertas | Métricas, logs estruturados, tracing, health/readiness e alertas de falha. |
| P1 | Fila e worker único com idempotência | Separa CLI de execução contínua | Jobs persistidos, shutdown seguro, retry limitado e deduplicação. |
| P1 | Política de dados e privacidade | Evita retenção e acesso indevidos | Classificação, retenção, expurgo verificável, criptografia e documentação operacional. |
| P2 | Adaptadores externos graduais | Adiciona utilidade real sem ampliar risco de uma vez | Cada adaptador com threat model, allowlist, dry-run, confirmação e auditoria. |
| P2 | Pesquisa web isolada | Permite informação externa sem transformar páginas em autoridade | SSRF bloqueado, destinos permitidos, conteúdo não confiável e fixtures de injection. |
| P2 | Memória semântica híbrida | Melhora recuperação após governança de dados | Proveniência, filtros por tenant, retenção e avaliação de qualidade. |
| P2 | Subagentes | Aumenta paralelismo e capacidade | Parent task, fan-out limitado, orçamento por filho, cancelamento e auditoria. |

## O que não recomendo implementar agora

Não recomendo ativar autopromoção de skills, autoalteração do prompt, execução arbitrária de código gerado, pesquisa web sem isolamento, mensageria com envio real ou subagentes distribuídos antes dos controles P0 e P1. Essas funcionalidades aumentariam a superfície de ataque e poderiam contradizer o princípio central do Shaka: **o modelo sugere; o host autoriza; o humano aprova capacidades**.

Também não recomendo começar pela interface visual. A prioridade é fechar contratos, autorização, execução, recuperação e observabilidade. Uma interface pode ser adicionada depois sobre APIs estáveis e auditáveis.

## Definição de pronto por ambiente

| Ambiente | Estado |
|---|---|
| Desenvolvimento local | Pronto, desde que Rust/Cargo seja compatível com Rust 1.85+; o provedor local não exige chave de API. |
| Operação privada controlada | Quase pronto, condicionado a revisão do host, backup externo, rotação de credenciais, retenção e monitoramento. |
| Serviço interno multiusuário | Não pronto; exige IAM, isolamento, quotas, fila, observabilidade e política de dados. |
| Serviço público 24/7 | Não pronto; exige todos os controles P0, grande parte dos P1 e uma revisão de segurança independente. |
| Agente generalista com skills reais | Não pronto; aprovação existe, mas execução integrada de skills ainda precisa ser implementada. |

## Recomendação final

A versão atual deve ser tratada como **Shaka v0.2.x — núcleo governado para operação controlada**. O próximo marco técnico deveria ser uma versão `v0.3.0` focada no plano de controle: loop seguro de ferramentas, integração de skills aprovadas, limites de execução completos, autenticação forte, backup externo, observabilidade e supply chain.

Depois desse marco, uma versão `v0.4.0` poderia adicionar adaptadores externos graduais, pesquisa web isolada e workers. Subagentes, memória vetorial e automação mais ampla devem vir somente após evidência operacional e threat models específicos.

### Referências

[1]: https://github.com/Head-1/Shaka-Agente/blob/main/PRODUCTION_RELEASE.md "Critérios da produção candidata"
[2]: https://github.com/Head-1/Shaka-Agente/actions/workflows/ci.yml "Histórico do Shaka CI"
[3]: https://github.com/Head-1/Shaka-Agente/blob/main/SECURITY.md "Modelo de segurança do Shaka"
[4]: https://github.com/Head-1/Shaka-Agente/blob/main/ARCHITECTURE.md "Arquitetura do Shaka"
[5]: https://github.com/Head-1/Shaka-Agente/blob/main/TESTING_STRATEGY.md "Estratégia de testes"
