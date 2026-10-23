# Rodadas do `qa-agente` contra o jogo (dnd-mestre), 22/10/2026

Sempre no ambiente E2E descartável do próprio jogo (banco `dnd_teste`, narrador falso, sessão de teste), com o LLM
pelo OmniRoute. "Úteis" = passos que mudaram a tela. Uma rodada de cada: o LLM varia, então diferença pequena não prova nada.

| Rodada | O que mudou | Passos | Úteis | Resultado e o que aprendi |
|---|---|---|---|---|
| 1 | só clicava | 16 | n/d | ficou no menu e no Perfil; não criou nada |
| 2 | digitar em campos | 20 | n/d | criou a campanha e entrou na criação de personagem |
| 3 | 80 passos | 80 | **1** | relatório dizia VERDE, mas o agente clicou "nome" 22 vezes sem digitar (campo sem `placeholder`) |
| 4 | anti-repetição, `<label>`, veredito exige progresso | 60 | **30** | o campo de nome passou a ser preenchido; 11 telas; evento WebSocket novo (`imagem:gerando`) |
| 5 | texto da tela como contexto | 60 | 21 | sem melhora clara (ruído de uma rodada); ainda sem "Criar personagem" |
| 6 | `Marcar:` e `Selecionar:` | 0 | n/d | caiu: prompt de 6000+ caracteres recusado pelo gateway, rodada perdida sem relatório |
| 7 | orçamento de prompt | 3 | 2 | parou: o rótulo do `select` vinha com todas as opções (estava dentro de um `<label>`) |
| 8 | rótulo limpo e fallback marcado | 70 | **37** | usou `Marcar` 8 vezes e `Selecionar` 1; "Falta" caiu de 6 para 5 itens; **objetivo não cumprido** |

Rodada 8: 40 requisições HTTP sem erro, WebSocket 53 enviados e 54 recebidos, campanha confirmada no backend,
**183 mil tokens e 21 minutos** (cerca de 2,6 mil tokens e 18 s por passo). Amostra dos insumos em
`amostras/insumos-jogo-rodada-h.jsonl`.

## O que falta para chegar ao turno com o mestre

O assistente de criação exige cerca de 6 decisões coerentes (idiomas, escolhas da espécie e da classe, perícias,
maestria, antecedente, alinhamento) e o agente ainda alterna entre abas e desfaz progresso. Caminhos, do mais barato:

1. Usar a lista "Falta:" como fila de trabalho: pedir ao LLM um plano (`planejar` existe e não é usado) e executar item por item.
2. Reduzir o custo por passo (modelo menor para escolhas simples) e o prompt, que passa de 2 mil tokens.
3. Um roteiro determinístico para fluxos conhecidos, deixando o LLM só para o que for novo.

## Falhas do próprio agente que esta sessão achou e consertou

Rótulo de campo sem `placeholder`, rótulo de `select` dentro de `<label>`, modelo que copia o número da lista ou tira o
prefixo do rótulo, prompt acima do limite do gateway, jornada que perdia tudo num erro grave, e veredito "verde" sem
progresso. Cada um virou teste.
