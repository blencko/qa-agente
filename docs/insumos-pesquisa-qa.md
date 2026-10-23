# Insumos de pesquisa de QA (Vigia, ciclos 220 a 225)

Fonte: relatórios dos ciclos `qa_*` do Vigia em 22/10/2026, cada um no teto de 8 rodadas com 900 a 1.500 itens úteis.
**Cautela:** são sínteses geradas por LLM sobre posts, artigos e repositórios; vários trechos vêm cortados e nada foi
verificado na fonte. Servem para orientar decisões, não como prova. O tema `qa_grafo_de_estados_ui` falhou (bug do
Vigia, ver o fim) e não tem relatório.

## O que a pesquisa diz e onde entra no `qa-agente` e no motor

| Achado | Onde entra | Estado |
|---|---|---|
| Objetivo de alto nível, laço com LLM observando a página (snapshot de acessibilidade) e **asserção determinística** para validar: é o desenho que já usamos | agente + `avaliacao.rs` | confirma o desenho |
| **Pular um teste que falharia é o "pecado capital"** da seleção de testes. Um benchmark mostrou 73% de testes pulados, mas era enganoso (a ferramenta de cobertura perdia acertos); medido direito deu cerca de 21% | motor, decisão `pular` | **implementado:** auditoria do pular (todo N-ésimo pular vira executar, para medir falso-pular) |
| "Self-healing" adapta-se a mudança legítima da interface; não é consertar teste quebrado | `VersionedStateGraph` por versão | já separa transição nova de transição herdada |
| Um chatbot tirou 0,92 na avaliação e mentia aos clientes; **LLM-as-judge só vale se o juiz for confiável** | `avaliacao.rs`, gate humano | o agente navega e não julga; o gabarito é determinístico e o GO/NO-GO é humano |
| Modelos alucinam em vez de se abster quando a resposta não existe | motor, gate humano | `HumanoSemBase` quando o motor não tem veredito |
| Harness para agente tem de lidar com não-determinismo, injeção de prompt, limite de janela de contexto e latência | agente | texto da tela entra como dado, e a ação é validada contra a lista de opções |
| **Fingerprint de modelo:** verificar o que o gateway realmente serve, porque há troca em fallback | `insumos.jsonl` | **implementado:** o modelo que respondeu cada passo é gravado |
| **Regressão de custo de inferência de LLM** em CI e roteamento de modelos em camadas por custo e latência | `insumos.jsonl` | **implementado:** tokens por passo e por execução |
| Divisão planejar, gerar, executar, inspecionar falha e reparar | o desenho de referencia (fluxo de agente de QA autonomo) | o `qa-agente` cobre planejar e executar; inspeção de falha e reparo ficam para depois |
| Navegador real versus MCP de navegador headless: a escolha depende do caso | `navegador_chrome.rs` | usamos Chrome headless por CDP |

## Bug encontrado no Vigia (não corrigido)

`qa_grafo_de_estados_ui` quebrou no ciclo 222: `ProgramLimitExceededError: string is too long for tsvector
(1623646 bytes, max 1048575 bytes)`. Uma página de 1,6 MB estourou o limite do índice de texto do Postgres. O ciclo
inteiro foi perdido por isso. Correção possível: truncar o texto antes de gerar o `tsvector`. Fica para ordem sua,
porque o Vigia é projeto pronto em Python.
