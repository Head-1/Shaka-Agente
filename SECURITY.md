# Modelo de Segurança do Shaka

## 1. Escopo e postura

O Shaka combina modelo de linguagem, ferramentas, memória persistente, código gerado e futuras integrações externas. A postura de segurança é **deny-by-default**: uma capacidade não concedida não existe para a skill ou ferramenta.

Este documento descreve o MVP e seus limites. A presença de um controle nesta lista não significa que o sistema esteja pronto para produção; significa que o controle foi desenhado ou parcialmente implementado e precisa de validação no ambiente final.

## 2. Ativos protegidos

| Ativo | Risco principal |
|---|---|
| Chaves de modelo e tokens externos | Exfiltração, uso indevido e custo não autorizado. |
| Conversas e memória episódica | Vazamento de dados pessoais, retenção excessiva e acesso entre tenants. |
| Artefatos e catálogo de skills | Alteração de código, promoção indevida e supply chain. |
| Processo principal | Execução de código não confiável e corrupção de estado. |
| Logs de auditoria | Apagamento ou adulteração de evidências. |
| Identidade do operador | Falsificação de aprovação e execução de efeitos externos. |
| Orçamento de tokens e recursos | Loops, denial of service e custo inesperado. |

## 3. Fronteiras de confiança

O modelo é não confiável para autorização. A web e qualquer conteúdo recuperado são não confiáveis para instruções. Uma skill gerada é não confiável até passar por build, testes e aprovação. O host é responsável por validar schema, capabilities, orçamento e efeitos colaterais.

## 4. Ameaças e mitigação

### 4.1 Prompt injection via web ou conteúdo externo

**Ameaça:** uma página, documento ou mensagem instrui o agente a ignorar políticas, revelar segredos ou chamar uma ferramenta.

**Mitigação atual:** o MVP não habilita pesquisa web. O `AgentRuntime` define que conteúdo externo futuro deve ser tratado como dado e o function calling é mediado pelo host.

**Controle obrigatório antes de habilitar web:** separar mensagens por autoridade, marcar origem e não confiabilidade, aplicar allowlist de ferramentas, impedir escrita direta no system prompt, limitar destinos de rede, bloquear SSRF e exigir aprovação para efeitos externos.

### 4.2 Skill maliciosa ou defeituosa

**Ameaça:** uma skill tenta acessar rede, filesystem, ambiente, segredo ou memória de outro tenant.

**Mitigação atual:** `WasmExecutor` usa Wasmtime 47.0.3, rejeita imports, não habilita WASI, aplica fuel e interrupção por epoch; `SandboxPolicy` nega rede/filesystem por padrão; `SkillRegistry` não permite autopromoção. A versão do runtime é atualizada por causa dos advisories críticos encontrados no Wasmtime 27.

**Risco residual:** o pipeline de build e a verificação independente de comportamento ainda não estão completos. O código gerado e suas dependências não devem ser compilados com credenciais ou acesso amplo ao host.

### 4.3 Fuga do sandbox

**Ameaça:** um módulo WASM explora uma vulnerabilidade do runtime ou obtém uma capability maior que a necessária.

**Mitigação atual:** módulo autocontido, sem imports, fuel e limite de tempo. O processo principal não executa código da skill diretamente.

**Controles obrigatórios:** manter Wasmtime atualizado, aplicar limites de memória, executar como usuário não privilegiado, usar filesystem somente leitura, limitar syscalls no ambiente de implantação, escanear dependências, assinar artefatos e testar módulos adversariais.

### 4.4 Exfiltração de segredos pelo modelo

**Ameaça:** o modelo solicita uma variável de ambiente, inclui uma chave na resposta ou passa segredo a ferramenta.

**Mitigação atual:** o adaptador lê a chave somente do ambiente, a configuração de produção exige chave e HTTPS, a redaction remove padrões de credencial de objetivos, respostas e episódios, e nenhuma ferramenta de mensageria é registrada. O sandbox não recebe ambiente nem imports.

**Controles obrigatórios:** redaction de logs, secret scanning, credenciais delegadas, escopo mínimo, proibição de segredos nos argumentos de ferramenta e revisão de respostas antes de efeitos externos.

### 4.5 Abuso de mensageria

**Ameaça:** uma conta falsificada ou uma skill envia mensagens repetidas, fraudulentas ou para destinatário errado.

**Mitigação atual:** mensageria externa não está habilitada.

**Controles obrigatórios:** verificação de webhook, autenticação de operador, idempotência, rate limit, confirmação de destinatário, modo dry-run, aprovação de alto risco e auditoria do conteúdo enviado.

### 4.6 Prompt ou ferramenta alterada

**Ameaça:** alteração silenciosa do system prompt, schema ou implementação muda o comportamento do agente.

**Mitigação atual:** workspace versionado, lockfile, skill com hash calculado do artefato na aprovação, gravação atômica do catálogo, ADRs e `cargo audit` obrigatório na CI.

**Controles obrigatórios:** versionar prompt e schema, registrar hash na execução, revisão de pull request, CI obrigatória, assinatura de release e bloqueio de artefato não aprovado.

### 4.7 Vazamento entre tenants

**Ameaça:** recuperação ou gravação de memória usa identificador incorreto e expõe dados de outra organização.

**Mitigação atual:** tabelas de memória carregam `tenant_id`, APIs recebem tenant explícito, há teste de isolamento e a auditoria é encadeada separadamente por tenant.

**Risco residual:** a release implementa RBAC local mínimo, mas ainda não possui IAM remoto, ABAC, row-level security distribuído nem isolamento completo de uma implantação multi-tenant.

**Controles adicionais antes de exposição pública:** IAM no host, chave de partição/row-level security, testes negativos, quotas, criptografia e auditoria por tenant.

### 4.8 Denial of service e loops

**Ameaça:** tarefas geram retries infinitos, fan-out excessivo, consultas grandes ou uso excessivo de tokens.

**Mitigação atual:** `ExecutionBudget` tem limites de passos, tool calls, tempo e custo; o runtime usa timeout.

**Controles obrigatórios:** circuit breaker, backoff, cancelamento, limite de fan-out, quotas por tenant, métricas e alertas.

## 5. Regras de segurança não negociáveis

O modelo não autoriza a si próprio. Nenhum conteúdo externo altera regras de sistema. Código gerado não roda no processo principal. Rede, filesystem e segredos não são concedidos por padrão. Uma skill candidata não é ativa. Efeito externo precisa de autorização, idempotência e auditoria. Falhas devem interromper a ação, não degradar silenciosamente para execução irrestrita.

## 6. Dados e privacidade

Memória episódica pode conter conversas, objetivos e resultados. A implantação deve definir classificação, retenção, base legal, direito de exclusão, criptografia, residência e redaction antes de conectar usuários reais. O comando de expurgo do MVP é uma ferramenta operacional inicial, não uma implementação completa de LGPD/GDPR.

## 7. Supply chain

Antes de produção, a CI deve executar lockfile imutável, `cargo audit`, geração de SBOM, revisão de dependências, verificação de origem e assinatura de artefatos. O build de código gerado deve ocorrer sem credenciais, idealmente offline ou com allowlist de dependências e fontes.

## 8. Resposta a incidente

Em suspeita de fuga, autopromoção ou exfiltração: interromper novas execuções, revogar skills, preservar logs e hashes, rotacionar credenciais, identificar tenants afetados e registrar timeline. Não apagar o banco ou sobrescrever o catálogo antes de preservar cópia e evidências.

## 9. Risco residual do MVP

A release é adequada para operação controlada com infraestrutura local ou privada previamente revisada. Não é adequada, sem controles adicionais, para exposição pública, dados pessoais reais sem política formal, mensagens externas, código gerado com dependências arbitrárias, ambiente multi-tenant distribuído ou operação 24/7 sem backup externo e monitoramento.
