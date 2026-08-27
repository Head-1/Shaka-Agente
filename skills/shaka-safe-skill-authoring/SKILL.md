---
name: shaka-safe-skill-authoring
description: Autoria e revisão segura de skills nativas para o Shaka-Agente. Use ao criar ou alterar SKILL.md, referências, scripts, manifests, artefatos WASM, permissões, schemas, hashes, trust stores ou fluxos de aprovação.
---

# Shaka Safe Skill Authoring

## Objetivo

Criar conhecimento reutilizável para agentes sem transformar documentação em autorização implícita. Uma skill Markdown versionada em `skills/` orienta o agente; ela não é, por si só, um artefato executável nem concede capability, aprovação ou acesso externo.

## Separar as duas superfícies

### Conhecimento versionado

Guardar a skill em `skills/<nome>/SKILL.md`, com frontmatter YAML contendo somente `name` e `description` como metadados de disparo. Manter o corpo conciso, abaixo de 500 linhas, escrito em modo imperativo e organizado para progressive disclosure. Usar `references/` para detalhes que não precisam ser carregados sempre, `scripts/` para automação determinística e `templates/` somente para recursos realmente reutilizados.

O conteúdo deve declarar objetivo, gatilhos, invariantes, workflow, critérios de aceite e limites. Não incluir segredos, tokens, chaves privadas, instruções para burlar aprovação ou afirmações de que a skill está autorizada apenas porque foi versionada.

### Artefato executável

Tratar qualquer WASM ou ferramenta como não confiável até que o host valide manifesto, schema, capabilities, orçamento, modo, caminho, hash SHA-256, identidade e aprovação. O runtime do Shaka persiste manifests em `data/skills.json` e chaves públicas em `data/trusted_keys.json`; o diretório Markdown `skills/` não substitui esses registros.

Para execução verificada, exigir skill no estado `Candidate`, aprovação humana explícita, artefato existente e caminho canônico, hash igual ao aprovado, digest de autoridade do manifesto igual ao atual, atestação Ed25519 V2, chave no `TrustStore` e chave não revogada. Revalidar tudo imediatamente antes de montar a ferramenta.

## Workflow de criação

1. Entender o uso com exemplos concretos e identificar fronteiras de confiança, efeitos externos e dados sensíveis.
2. Definir escopo, gatilhos, invariantes, critérios de aceite e limites. Separar documentação de execução.
3. Planejar recursos: manter o `SKILL.md` pequeno; mover especificações detalhadas para `references/`; adicionar scripts somente quando reduzirem repetição determinística.
4. Inicializar uma skill nova com o `init_skill.py` oficial e remover todos os arquivos de exemplo não utilizados.
5. Escrever o `SKILL.md` com frontmatter correto, instruções imperativas, ausência de segredos e decisão explícita para cada fluxo de criação, edição, aprovação, revogação e falha.
6. Testar scripts e referências. Rodar `quick_validate.py` e corrigir falhas de estrutura, frontmatter, tamanho ou arquivos residuais.
7. Copiar o pacote validado para o diretório versionado `skills/` do repositório apenas dentro de uma branch dedicada. Revisar `git diff --check` e confirmar que nenhum código funcional foi alterado sem escopo.
8. Fazer commit SSH assinado, verificar a assinatura, publicar a branch e registrar SHA, diff, validações e riscos em relatório.

## Regras de segurança do conteúdo

- O modelo pode propor uma skill, mas não pode aprová-la nem alterar o trust store.
- O host deve decidir capabilities, schema, orçamento, tenant, papel, modo e destino.
- Usar `dry-run` para explorar fluxos quando disponível; bloquear efeitos externos sem aprovação correspondente.
- Nunca instruir uma skill a ler ou enviar segredos, escapar do sandbox, ignorar redaction, contornar auditoria ou assumir autoridade de outro componente.
- Manter skills de leitura separadas de skills com escrita ou efeitos externos.
- Para mudança em permissions ou schemas, exigir nova aprovação; o digest de autoridade deve mudar de forma verificável.
- Revogar a skill ou a chave quando o conteúdo, artefato, autoridade ou confiança deixar de ser válido.

## Revisão e aceite

Aceitar uma skill somente quando: o gatilho é específico; o corpo é conciso; recursos não utilizados foram removidos; `quick_validate.py` passa; scripts têm teste representativo; o diff está limitado; não há segredos; a documentação não promete execução automática; e as instruções deixam claro que aprovação e revalidação pertencem ao host.

Para integração executável, adicionar testes de rejeição de hash, digest, schema, capability, tenant, orçamento, chave revogada, aprovação ausente e artefato ausente. Testar também replay, crash, revalidação pós-alteração e ausência de vazamento em logs. Não declarar uma skill executável antes desses gates.

## Entrega

Anexar o `SKILL.md` validado, registrar o caminho no repositório, informar se a skill é somente conhecimento ou também artefato, listar aprovações necessárias e fornecer comandos para validar no segundo ambiente. Nunca solicitar ou anexar chave privada, token bruto ou credencial.
