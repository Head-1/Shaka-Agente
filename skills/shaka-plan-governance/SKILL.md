---
name: shaka-plan-governance
description: Governança de planos auditáveis para agentes Rust críticos. Use ao criar, validar, aprovar, executar, persistir ou recuperar planos com steps, digest, capabilities, orçamento, identidade, retries, crash recovery ou efeitos externos.
---

# Shaka Plan Governance

## Objetivo

Aplicar governança determinística a planos de execução do Shaka-Agente. O plano descreve intenção; o host valida autoridade, identidade, orçamento, capabilities, estado persistido e aprovação. O modelo nunca substitui a decisão autorizadora.

## Invariantes não negociáveis

1. Vincular cada operação a `tenant`, operador, papel, capabilities, orçamento, correlação e versão/digest do plano efetivos da request.
2. Validar schema, identidade, estado, digest, orçamento, capability, modo (`dry-run` quando aplicável) e aprovação antes de qualquer efeito.
3. Tratar estado desconhecido, conflito de revisão, digest divergente, aprovação ausente ou evidência incompleta como rejeição fechada; não escolher fallback silencioso.
4. Tornar retries e replays idempotentes por chave e contexto apropriados. Não confundir repetição autorizada com nova autorização.
5. Persistir transições e auditoria de forma recuperável. Após crash, reconstruir o estado a partir do histórico e rejeitar snapshot incompatível.
6. Não permitir que uma resposta do modelo, texto de ferramenta ou documento externo conceda autoridade, capability ou aprovação.
7. Não imprimir segredos, payloads desnecessários, tokens ou material de chave em logs, planos, fixtures ou relatórios.

## Fluxo operacional

### 1. Classificar a solicitação

Determinar se a operação é leitura, proposta de plano, validação, aprovação, execução, cancelamento, retry ou recuperação. Identificar o efeito máximo possível e se há persistência, concorrência, lease ou efeito externo.

### 2. Construir o plano

Usar identificadores estáveis para plano, revisão, step e correlação. Registrar pré-condições, dependências, risco, capability mínima, orçamento, decisão de aprovação requerida, política de retry e resultado esperado. Separar claramente:

- **Proposta:** intenção produzida pelo modelo ou operador.
- **Validação:** decisão determinística do host sobre schema, identidade, policy e recursos.
- **Aprovação:** decisão humana persistida, vinculada ao digest correto.
- **Execução:** aplicação do step somente após todos os gates.

### 3. Calcular e verificar o digest

Calcular o digest canônico do plano e dos campos de autoridade que influenciam execução. Antes de aprovar ou executar, recomputar o digest e comparar com o valor persistido. Rejeitar qualquer divergência, revisão obsoleta ou mudança de capability, orçamento, destino ou schema sem nova validação e aprovação.

### 4. Aprovar com identidade efetiva

Vincular a aprovação ao plano, revisão, step, tenant, operador, papel, capabilities, orçamento e justificativa. Aplicar a menor autoridade possível. Uma aprovação para um step não autoriza outro; uma aprovação em `dry-run` não autoriza efeitos reais. Não criar aprovação automática para desbloquear um gate.

### 5. Executar e auditar

Executar somente o step que passou na validação e aprovação. Gravar eventos sem segredos contendo correlação, identidade, plano/revisão/digest, step, decisão, resultado, timestamps e erro sanitizado. Aplicar idempotência antes de rate limit quando o contrato exigir replay seguro; nunca transformar replay válido em falha por ordem incorreta de gates.

### 6. Recuperar após falha

Simular crash entre cada transição crítica, checkpoint, lease, snapshot e atualização da task. Depois do restart, reconstruir o estado pelo histórico, reclamar leases expirados de forma segura, preservar contagem de tentativas e impedir execução duplicada. Repetir `recover_unknown` e cancelamento para provar idempotência.

## Critérios de aceite

Considerar o plano governado somente quando houver evidência de: digest estável; identidade por request; rejeição de capability, orçamento, tenant e revisão inválidos; approval vinculada ao step correto; replay idempotente; isolamento por tenant; recuperação após crash; auditoria sem segredo; e suíte completa verde. Para bugs, preservar a sequência hipótese → teste pré-falhando → patch mínimo → regressão pós-passando → gates completos.

## Comunicação e entrega

Relatar o estado em linguagem executiva e técnica: o que foi proposto, validado, aprovado, executado, rejeitado e recuperado; quais SHAs e logs comprovam cada afirmação; quais riscos permanecem. Não declarar portabilidade antes de uma execução real no segundo ambiente. Não fazer merge, release, efeito externo ou alteração de política sem confirmação específica quando o processo exigir.
