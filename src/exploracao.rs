use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::grafo_estados::VersionedStateGraph;
use crate::portas::Navegador;

/// Passo 7: descobre estados e transicoes da jornada e grava no State Graph (passo 8).
///
/// Busca em largura: para cada estado alcancado, clica em cada acao ainda nao tentada, partindo sempre
/// de uma pagina recarregada e repetindo o caminho ate o estado. `limite_cliques` protege o custo.
pub fn descobrir_estados(
    navegador: &mut dyn Navegador,
    url: &str,
    versao: &str,
    grafo: &mut VersionedStateGraph,
    limite_cliques: usize,
) -> Result<usize, String> {
    let mut cliques = 0;
    let mut vistos: BTreeSet<String> = BTreeSet::new();
    let mut tentadas: BTreeSet<(String, String)> = BTreeSet::new();
    let mut fila: VecDeque<Vec<String>> = VecDeque::from([vec![]]);

    while let Some(caminho) = fila.pop_front() {
        let estado = chegar(navegador, url, &caminho)?;
        vistos.insert(estado.clone());
        for acao in navegador.clicaveis()? {
            if !tentadas.insert((estado.clone(), acao.clone())) {
                continue;
            }
            if cliques >= limite_cliques {
                return Ok(cliques);
            }
            chegar(navegador, url, &caminho)?;
            navegador.clicar(&acao)?;
            cliques += 1;
            let destino = navegador.estado()?;
            grafo.registrar(versao, &estado, &acao, &destino)?;
            if !vistos.contains(&destino) {
                vistos.insert(destino);
                let mut novo = caminho.clone();
                novo.push(acao);
                fila.push_back(novo);
            }
        }
    }
    Ok(cliques)
}

fn chegar(navegador: &mut dyn Navegador, url: &str, caminho: &[String]) -> Result<String, String> {
    navegador.abrir(url)?;
    for acao in caminho {
        navegador.clicar(acao)?;
    }
    navegador.estado()
}

/// Um passo da jornada guiada: onde estava, o que clicou e onde chegou.
#[derive(Debug, Clone, PartialEq)]
pub struct Passo {
    pub origem: String,
    pub acao: String,
    pub destino: String,
    /// O grafo ja tinha outro destino para esta mesma origem e acao (estado escondido da aplicacao).
    pub ambigua: bool,
    /// Modelo do gateway que respondeu esta escolha e tokens gastos nela (None sem LLM ou sem `usage`).
    pub modelo: Option<String>,
    pub tokens: Option<u64>,
    /// O que o passo causou no backend (rotas de API, eventos de WebSocket, erros).
    pub efeito: crate::efeito::Efeito,
    /// Criterios de aceite que ficaram verdadeiros neste passo (borda de subida); vazio quase sempre.
    pub criterios: Vec<String>,
}

/// Passo 4 em acao: o agente escolhe, a cada tela, a proxima opcao rumo ao objetivo. Para quando o agente
/// diz que cumpriu, quando nao ha o que clicar ou no `limite`. Cada transicao vai para o State Graph.
/// Erro do agente (inclusive cota esgotada) interrompe: a jornada nunca segue por inferencia.
pub fn jornada_guiada(
    agente: &dyn crate::portas::AgenteQA,
    navegador: &mut dyn Navegador,
    url: &str,
    objetivo: &str,
    versao: &str,
    grafo: &mut VersionedStateGraph,
    limite: usize,
) -> Result<Vec<Passo>, String> {
    jornada_guiada_com(agente, navegador, url, objetivo, versao, grafo, limite, &mut |_, _| Ok(()))
        .and_then(|j| match j.erro {
            Some(e) => Err(e),
            None => Ok(j.passos),
        })
}

/// Quantas vezes a mesma acao pode ser tentada na mesma tela antes de sair da lista do agente.
pub const LIMITE_REPETICAO: usize = 2;

/// Quantas escolhas malformadas seguidas do modelo a jornada cobre com a escolha deterministica antes de parar.
pub const MAX_FALLBACKS_SEGUIDOS: usize = 3;

/// Nome gravado em `Passo::modelo` quando a escolha nao veio do LLM: o relatorio e os insumos nao a confundem com uma.
pub const MODELO_FALLBACK: &str = "fallback-deterministico";

/// Nome gravado em `Passo::modelo` quando a escolha veio da politica aprendida (UCB), nao do LLM.
pub const MODELO_POLITICA: &str = "politica-ucb";

/// De cada N decisoes em que a politica esta confiante, a N-esima vai ao LLM de qualquer jeito (auditoria).
pub const AUDITORIA_CONFIANTE: usize = 5;

/// Quantas vezes a jornada sobe um Chrome novo quando o navegador cai no meio.
pub const MAX_REINICIOS: usize = 5;

/// Passos seguidos sem a tela mudar que passam a escolha do LLM para a politica.
pub const PARADOS_PARA_POLITICA: usize = 3;

/// Limite de ACOES DE ESCRITA por rodada, valido para QUALQUER escolha (LLM, politica ou fallback): o objetivo so
/// restringe o LLM, e a politica explora o que nunca tentou e ignoraria "no maximo duas decisoes".
#[derive(Debug, Clone, PartialEq)]
pub struct Restricao {
    /// Pedacos (minusculos) que marcam uma opcao como escrita, ex.: `aprovar`, `reprovar`, `salvar`.
    pub padroes: Vec<String>,
    pub max_escritas: usize,
    /// Maximo de escritas de CADA tipo (padrao de `padroes`); 0 = so vale o total. Ex.: 1 aprovacao e 1 reprovacao.
    pub max_por_tipo: usize,
}

impl Restricao {
    /// `padroes` separados por `|`. Vazio ou `max_escritas` 0 = sem restricao (None).
    pub fn nova(padroes: &str, max_escritas: usize) -> Option<Restricao> {
        let padroes: Vec<String> =
            padroes.split('|').map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).collect();
        (!padroes.is_empty() && max_escritas > 0).then_some(Restricao { padroes, max_escritas, max_por_tipo: 0 })
    }

    pub fn eh_escrita(&self, acao: &str) -> bool {
        let a = acao.to_lowercase();
        self.padroes.iter().any(|p| a.contains(p))
    }

    /// Passa a limitar tambem cada TIPO de escrita (cada padrao) a `n` por rodada.
    pub fn com_max_por_tipo(mut self, n: usize) -> Self {
        self.max_por_tipo = n;
        self
    }

    /// Indice do primeiro padrao que a acao casa (o "tipo" da escrita).
    fn tipo_de(&self, acao: &str) -> Option<usize> {
        let a = acao.to_lowercase();
        self.padroes.iter().position(|p| a.contains(p))
    }

    /// A opcao esta bloqueada? `feitas` sao as acoes ja executadas nesta rodada. Bloqueia ao atingir o total de
    /// escritas ou, com `max_por_tipo`, o limite do tipo dela (1 aprovacao e 1 reprovacao nao viram 2 aprovacoes).
    pub fn bloqueia(&self, opcao: &str, feitas: &[&str]) -> bool {
        if !self.eh_escrita(opcao) {
            return false;
        }
        if feitas.iter().filter(|f| self.eh_escrita(f)).count() >= self.max_escritas {
            return true;
        }
        match (self.max_por_tipo, self.tipo_de(opcao)) {
            (0, _) | (_, None) => false,
            (max, Some(tipo)) => feitas.iter().filter(|f| self.tipo_de(f) == Some(tipo)).count() >= max,
        }
    }
}

/// Marca dos criterios de aceite ja cumpridos, anexada ao estado que a POLITICA enxerga: antes e depois de cumprir o
/// CA01 deixam de ser a mesma situacao (senao a politica repete a acao que ja pagou). Termina em `]`, nunca em digito:
/// a chave abstrata arranca contador colado ao fim de um segmento.
fn sufixo_criterios(cumpridos: &[String]) -> String {
    if cumpridos.is_empty() {
        String::new()
    } else {
        format!(" / ✓[{}]", cumpridos.join("+"))
    }
}

/// Codigo curto de um criterio: a primeira palavra do nome ("CA01 aprovacao registrada" -> "CA01").
fn codigo_do_criterio(nome: &str) -> String {
    nome.split_whitespace().next().unwrap_or(nome).to_string()
}

/// Motivo de parada quando o proprio agente diz que cumpriu o objetivo.
pub const OBJETIVO_CUMPRIDO: &str = "o agente considerou o objetivo cumprido";

/// "Preencher: campo = texto" e "Preencher: campo" sao a mesma acao para efeito de repeticao.
fn base_da_acao(acao: &str) -> String {
    acao.split(" = ").next().unwrap_or(acao).to_string()
}

/// O que a jornada fez e por que parou.
#[derive(Debug, Clone, PartialEq)]
pub struct Jornada {
    pub passos: Vec<Passo>,
    pub parou_por: String,
    /// Erro grave do agente (cota, cadeia esgotada, configuracao) que interrompeu a jornada. Os passos ja feitos
    /// ficam em `passos`: o relatorio parcial nunca se perde.
    pub erro: Option<String>,
}

/// Erros em que o modelo respondeu mas nao deu uma escolha utilizavel. Os demais (cota, cadeia esgotada) sao graves.
fn escolha_invalida(erro: &str) -> bool {
    ["acao fora", "faltou o campo", "resposta sem", "a resposta nao", "JSON"].iter().any(|p| erro.contains(p))
}

/// Igual a `jornada_guiada`, chamando `ao_passo(numero, navegador)` depois de cada clique (0 = tela inicial),
/// por exemplo para guardar uma captura por passo.
#[allow(clippy::too_many_arguments)]
pub fn jornada_guiada_com(
    agente: &dyn crate::portas::AgenteQA,
    navegador: &mut dyn Navegador,
    url: &str,
    objetivo: &str,
    versao: &str,
    grafo: &mut VersionedStateGraph,
    limite: usize,
    ao_passo: &mut dyn FnMut(usize, &mut dyn Navegador) -> Result<(), String>,
) -> Result<Jornada, String> {
    jornada_guiada_observada(agente, navegador, url, objetivo, versao, grafo, limite, ao_passo, &mut |_, _| {}, None, None, None, None)
}

/// Igual a `jornada_guiada_com`, chamando tambem `ao_feito(numero, passo)` assim que o passo termina, para
/// registrar o rastro na hora (log, checkpoint, banco) em vez de so no fim.
#[allow(clippy::too_many_arguments)]
pub fn jornada_guiada_observada(
    agente: &dyn crate::portas::AgenteQA,
    navegador: &mut dyn Navegador,
    url: &str,
    objetivo: &str,
    versao: &str,
    grafo: &mut VersionedStateGraph,
    limite: usize,
    ao_passo: &mut dyn FnMut(usize, &mut dyn Navegador) -> Result<(), String>,
    ao_feito: &mut dyn FnMut(usize, &Passo),
    politica: Option<&crate::aprendizado::Politica>,
    restricao: Option<&Restricao>,
    mut fonte_extra: Option<&mut dyn FnMut() -> Vec<String>>,
    mut criterios: Option<&mut dyn FnMut(&Passo) -> Vec<String>>,
) -> Result<Jornada, String> {
    navegador.abrir(url)?;
    ao_passo(0, navegador)?;
    let mut passos: Vec<Passo> = Vec::new();
    let mut historico: Vec<String> = Vec::new();
    let mut parou_por = format!("limite de {limite} passos");
    let mut erro: Option<String> = None;
    // Plano de uma chamada so: o modelo segue os passos em vez de decidir as cegas a cada tela. Plano malformado
    // segue sem plano; erro grave (cota, cadeia esgotada) interrompe como em qualquer outra chamada.
    let plano = match agente.planejar(objetivo) {
        Ok(p) => p,
        Err(e) if escolha_invalida(&e) => Vec::new(),
        Err(e) => {
            return Ok(Jornada { passos, parou_por: format!("erro do agente: {e}"), erro: Some(e) });
        }
    };
    let objetivo_efetivo = if plano.is_empty() {
        objetivo.to_string()
    } else {
        format!("{objetivo}\nPlano (siga na ordem; o texto \"Falta:\" da tela diz o que resta): {}", plano.join("; "))
    };
    let objetivo = objetivo_efetivo.as_str();
    let mut tentadas: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut fallbacks_seguidos = 0usize;
    let mut vistas = crate::aprendizado::Memoria::default();
    let mut reinicios = 0usize;
    // Erro do navegador no meio (Chrome caiu, conexao fechada): sobe um novo e recomeca da URL inicial, ate
    // MAX_REINICIOS vezes; passado disso, para com os passos ja feitos em vez de perder a rodada.
    macro_rules! nav {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => {
                    if reinicios < MAX_REINICIOS && navegador.reiniciar().is_ok() && navegador.abrir(url).is_ok() {
                        reinicios += 1;
                        historico.push(format!("(o navegador caiu e foi reiniciado; recomecou da tela inicial: {e})"));
                        continue;
                    }
                    parou_por = format!("erro do navegador: {e}");
                    erro = Some(e);
                    break;
                }
            }
        };
    }
    let mut decisoes_confiantes = 0usize;
    let mut recentes: Vec<crate::aprendizado::Transicao> = Vec::new();
    let mut cumpridos_cod: Vec<String> = Vec::new();
    while passos.len() < limite {
        let mut via_politica = false;
        let estado = nav!(navegador.estado());
        let chave_da_tela = crate::aprendizado::chave_estado(&estado);
        let estado_pol = format!("{estado}{}", sufixo_criterios(&cumpridos_cod));
        let opcoes = nav!(navegador.clicaveis());
        if opcoes.is_empty() {
            parou_por = "nada clicavel na tela".into();
            break;
        }
        // Anti-repeticao: a opcao ja tentada LIMITE_REPETICAO vezes nesta mesma tela some da lista do agente.
        // Sem isso o modelo reclica o mesmo atalho (ja vimos 22 cliques em "nome" sem nunca digitar).
        // Restricao de escrita: ao atingir o maximo de acoes de escrita da rodada, elas somem da lista para TODOS os
        // escolhedores (LLM, politica e fallback).
        let feitas: Vec<&str> = passos.iter().map(|p| p.acao.as_str()).collect();
        let disponiveis: Vec<String> = opcoes
            .iter()
            .filter(|o| tentadas.get(&(chave_da_tela.clone(), base_da_acao(o))).copied().unwrap_or(0) < LIMITE_REPETICAO)
            .filter(|o| !restricao.is_some_and(|r| r.bloqueia(o, &feitas)))
            .cloned()
            .collect();
        if disponiveis.is_empty() {
            parou_por = format!("todas as opcoes desta tela ja foram tentadas {LIMITE_REPETICAO} vezes");
            break;
        }
        let mut contexto = navegador.contexto();
        // Experiencia aprendida entra na frente do texto da tela (o orcamento do prompt corta o fim, nao o comeco).
        if let Some(p) = politica {
            let conselho = p.conselho(&estado_pol, &disponiveis);
            if !conselho.is_empty() {
                contexto = format!("{conselho}\n{contexto}");
            }
        }
        let mut via_fallback = false;
        // Empacou: PARADOS_PARA_POLITICA passos seguidos sem mudar a tela. O LLM ja mostrou que nao resolve; a escolha
        // passa a ser a da politica (UCB entre as opcoes sem texto), marcada como tal.
        let empacou = historico.len() >= PARADOS_PARA_POLITICA
            && historico.iter().rev().take(PARADOS_PARA_POLITICA).all(|h| h.contains("(a tela nao mudou)"));
        // Modo "so conselho" (braco B do experimento): a politica aconselha no prompt mas nao escolhe nada.
        let politica_escolhe = politica.filter(|p| !p.so_conselho());
        let da_politica = politica_escolhe.filter(|_| empacou).and_then(|p| {
            let sem_texto: Vec<String> = disponiveis.iter().filter(|o| !o.starts_with("Preencher: ")).cloned().collect();
            p.melhor_por_ucb(&estado_pol, &sem_texto).cloned()
        });
        // Confiante: a politica ja viu esta tela vezes suficientes e uma opcao se destaca com folga. Escolhe sozinha,
        // sem gastar uma chamada ao LLM. Toda AUDITORIA_CONFIANTE-esima decisao confiante vai ao LLM mesmo assim,
        // para a politica nao parar de ser contrariada (e de aprender onde errou).
        let confiante = if da_politica.is_some() {
            None
        } else {
            politica_escolhe.and_then(|p| p.escolha_confiante(&estado_pol, &disponiveis).cloned()).filter(|_| {
                decisoes_confiantes += 1;
                decisoes_confiantes % AUDITORIA_CONFIANTE != 0
            })
        };
        let resposta = match da_politica.or(confiante) {
            Some(a) => {
                via_politica = true;
                Ok(Some(a))
            }
            None => agente.proxima_acao_com_contexto(objetivo, &estado, &disponiveis, &historico, &contexto),
        };
        let acao = match resposta {
            Ok(Some(acao)) => {
                fallbacks_seguidos = 0;
                acao
            }
            Ok(None) => {
                parou_por = OBJETIVO_CUMPRIDO.into();
                break;
            }
            // Resposta malformada do modelo. Com temperatura 0 perguntar de novo da a mesma resposta, entao a
            // jornada segue com uma escolha DETERMINISTICA e MARCADA (a primeira opcao que nao exige texto), ate
            // `MAX_FALLBACKS_SEGUIDOS` vezes; depois para, com o motivo no relatorio.
            Err(e) if escolha_invalida(&e) => {
                fallbacks_seguidos += 1;
                match disponiveis.iter().find(|o| !o.starts_with("Preencher: ")) {
                    Some(alternativa) if fallbacks_seguidos <= MAX_FALLBACKS_SEGUIDOS => {
                        via_fallback = true;
                        alternativa.clone()
                    }
                    _ => {
                        parou_por = format!("o agente nao conseguiu escolher: {e}");
                        break;
                    }
                }
            }
            // qualquer outro erro (cadeia esgotada, cota, configuracao) interrompe de vez
            Err(e) => {
                parou_por = format!("erro do agente: {e}");
                erro = Some(e);
                break;
            }
        };
        let mut uso = agente.uso_do_passo();
        if via_fallback {
            uso.modelo = Some(MODELO_FALLBACK.to_string());
        }
        if via_politica {
            uso.modelo = Some(MODELO_POLITICA.to_string());
        }
        // O que este passo causou no backend: diferenca entre a observacao de antes e a de depois do clique.
        let obs_antes = navegador.observacao();
        nav!(navegador.clicar(&acao));
        *tentadas.entry((chave_da_tela.clone(), base_da_acao(&acao))).or_insert(0) += 1;
        let destino = nav!(navegador.estado());
        let mut efeito = crate::efeito::Efeito::de_delta(&obs_antes, &navegador.observacao());
        // Achados GRAVES novos nos logs do servidor (back, BFF, front) neste passo entram no efeito, como as respostas HTTP.
        if let Some(fonte) = fonte_extra.as_mut() {
            for achado in fonte() {
                if !efeito.falhas.contains(&achado) {
                    efeito.falhas.push(achado);
                }
            }
        }
        // Numa app real o mesmo clique leva a lugares diferentes conforme o estado escondido (campo preenchido ou
        // nao): o grafo recusa o destino ambiguo e o passo segue, marcado como `ambigua`.
        let ambigua = grafo.registrar(versao, &estado, &acao, &destino).is_err();
        // O agente fica sabendo o que o servidor fez (ou quebrou) no passo, junto da tela.
        let nota_back = efeito.resumo();
        let mudou = crate::aprendizado::chave_estado(&destino) != chave_da_tela;
        let base = if !mudou { format!("{acao} (a tela nao mudou)") } else { acao.clone() };
        historico.push(if nota_back.is_empty() { base } else { format!("{base} {nota_back}") });
        passos.push(Passo {
            origem: estado,
            acao,
            destino,
            ambigua,
            modelo: uso.modelo,
            tokens: uso.tokens,
            efeito,
            criterios: Vec::new(),
        });
        // Criterios de aceite que ficaram verdadeiros NESTE passo (o avaliador entrega so a borda de subida: cada
        // criterio paga uma vez so). Entram no passo, no historico do LLM e na recompensa.
        let novos = match (criterios.as_mut(), passos.last()) {
            (Some(avaliar), Some(ultimo)) => avaliar(ultimo),
            _ => Vec::new(),
        };
        if !novos.is_empty() {
            historico.push(format!("[criterio cumprido: {}]", novos.join(", ")));
            if let Some(ultimo) = passos.last_mut() {
                ultimo.criterios = novos.clone();
            }
            cumpridos_cod.extend(novos.iter().map(|n| codigo_do_criterio(n)));
        }
        if let Some(ultimo) = passos.last() {
            // aprende na hora: a recompensa mede o OBJETIVO (criterio cumprido) acima de tudo, e depois progresso
            // (tela nova, etapa que avanca ou recua, passo inutil, escrita no servidor). A politica enxerga o estado
            // COM os criterios ja cumpridos: depois do CA01, "Aprovar" na mesma tela e outra situacao.
            if let Some(p) = politica {
                let t = crate::aprendizado::Transicao {
                    origem: estado_pol.clone(),
                    acao: ultimo.acao.clone(),
                    destino: format!("{}{}", ultimo.destino, sufixo_criterios(&cumpridos_cod)),
                    efeito: ultimo.efeito.clone(),
                    criterios: ultimo.criterios.clone(),
                };
                let r = crate::aprendizado::recompensa(&t, &mut vistas);
                p.aprender_de(&t, r, via_politica);
                if !novos.is_empty() {
                    // o caminho ate o criterio tambem merece: credito aos ultimos passos, descontado pela distancia
                    p.creditar_retroativamente(&recentes, crate::aprendizado::RECOMPENSA_CRITERIO);
                }
                recentes.push(t);
                if recentes.len() > crate::aprendizado::CREDITO_RETROATIVO_PASSOS {
                    recentes.remove(0);
                }
            }
            ao_feito(passos.len(), ultimo);
        }
        if let Err(e) = ao_passo(passos.len(), navegador) {
            historico.push(format!("(a captura do passo falhou: {e})"));
        }
    }
    Ok(Jornada { passos, parou_por, erro })
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::collections::HashMap;

    /// Maquina de estados em memoria, igual a da aplicacao de teste, sem navegador.
    struct Falso {
        atual: String,
        mapa: HashMap<(String, String), String>,
    }

    impl Falso {
        fn jornada() -> Self {
            let mut mapa = HashMap::new();
            for (o, a, d) in [
                ("login", "entrar", "painel"),
                ("painel", "abrir_chat", "chat"),
                ("painel", "sair", "login"),
                ("chat", "perguntar", "chat"),
                ("chat", "voltar", "painel"),
            ] {
                mapa.insert((o.to_string(), a.to_string()), d.to_string());
            }
            Falso { atual: "login".into(), mapa }
        }
    }

    impl Navegador for Falso {
        fn abrir(&mut self, _: &str) -> Result<(), String> {
            self.atual = "login".into();
            Ok(())
        }
        fn clicar(&mut self, acao: &str) -> Result<(), String> {
            let destino = self
                .mapa
                .get(&(self.atual.clone(), acao.to_string()))
                .ok_or_else(|| format!("acao {acao} nao existe em {}", self.atual))?;
            self.atual = destino.clone();
            Ok(())
        }
        fn preencher(&mut self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn estado(&mut self) -> Result<String, String> {
            Ok(self.atual.clone())
        }
        fn clicaveis(&mut self) -> Result<Vec<String>, String> {
            let mut acoes: Vec<String> = self
                .mapa
                .keys()
                .filter(|(o, _)| *o == self.atual)
                .map(|(_, a)| a.clone())
                .collect();
            acoes.sort();
            Ok(acoes)
        }
        fn texto(&mut self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn captura(&mut self) -> Result<Vec<u8>, String> {
            Ok(vec![])
        }
    }

    #[test]
    fn descobre_as_cinco_transicoes_da_jornada() {
        let mut grafo = VersionedStateGraph::default();
        let cliques = descobrir_estados(&mut Falso::jornada(), "x", "v1", &mut grafo, 100).unwrap();
        assert_eq!(cliques, 5);
        assert_eq!(grafo.novas_em("v1").len(), 5);
        assert_eq!(grafo.destino("v1", "chat", "perguntar"), Some("chat"));
    }

    /// Navegador que "cai" no N-esimo clique (como o Chrome que perde a conexao) e volta a funcionar se reiniciado.
    struct Instavel {
        dentro: Falso,
        cliques: usize,
        cai_em: usize,
        caido: bool,
        reinicios: usize,
        pode_reiniciar: bool,
    }

    impl Navegador for Instavel {
        fn abrir(&mut self, u: &str) -> Result<(), String> {
            self.dentro.abrir(u)
        }
        fn clicar(&mut self, a: &str) -> Result<(), String> {
            self.cliques += 1;
            if self.cliques == self.cai_em {
                self.caido = true;
            }
            if self.caido {
                return Err("Unable to make method calls because underlying connection is closed".into());
            }
            self.dentro.clicar(a)
        }
        fn preencher(&mut self, s: &str, t: &str) -> Result<(), String> {
            self.dentro.preencher(s, t)
        }
        fn estado(&mut self) -> Result<String, String> {
            if self.caido {
                return Err("connection closed".into());
            }
            self.dentro.estado()
        }
        fn clicaveis(&mut self) -> Result<Vec<String>, String> {
            if self.caido {
                return Err("connection closed".into());
            }
            self.dentro.clicaveis()
        }
        fn texto(&mut self, s: &str) -> Result<String, String> {
            self.dentro.texto(s)
        }
        fn captura(&mut self) -> Result<Vec<u8>, String> {
            self.dentro.captura()
        }
        fn reiniciar(&mut self) -> Result<(), String> {
            if !self.pode_reiniciar {
                return Err("sem Chrome".into());
            }
            self.caido = false;
            self.reinicios += 1;
            Ok(())
        }
    }

    fn instavel(pode_reiniciar: bool) -> Instavel {
        Instavel { dentro: Falso::jornada(), cliques: 0, cai_em: 2, caido: false, reinicios: 0, pode_reiniciar }
    }

    #[test]
    fn navegador_que_cai_e_reiniciado_e_a_jornada_continua() {
        let agente = Roteiro(vec![Some("entrar"), Some("abrir_chat"), Some("entrar"), Some("abrir_chat"), None], Default::default());
        let mut nav = instavel(true);
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_com(&agente, &mut nav, "x", "o", "v1", &mut grafo, 10, &mut |_, _| Ok(())).unwrap();
        assert_eq!(nav.reinicios, 1);
        assert!(j.erro.is_none() && j.parou_por == OBJETIVO_CUMPRIDO, "{j:?}");
        assert_eq!(j.passos.len(), 3, "o clique que falhou nao vira passo");
    }

    #[test]
    fn navegador_que_cai_sem_poder_reiniciar_devolve_os_passos_ja_feitos() {
        let agente = Roteiro(vec![Some("entrar"), Some("abrir_chat"), None], Default::default());
        let mut nav = instavel(false);
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_com(&agente, &mut nav, "x", "o", "v1", &mut grafo, 10, &mut |_, _| Ok(())).unwrap();
        assert_eq!(j.passos.len(), 1);
        assert!(j.erro.is_some() && j.parou_por.starts_with("erro do navegador"), "{j:?}");
    }

    #[test]
    fn respeita_o_limite_de_cliques() {
        let mut grafo = VersionedStateGraph::default();
        let cliques = descobrir_estados(&mut Falso::jornada(), "x", "v1", &mut grafo, 2).unwrap();
        assert_eq!(cliques, 2);
        assert!(grafo.novas_em("v1").len() <= 2);
    }

    #[test]
    fn segunda_versao_igual_nao_tem_transicao_nova() {
        let mut grafo = VersionedStateGraph::default();
        descobrir_estados(&mut Falso::jornada(), "x", "v1", &mut grafo, 100).unwrap();
        descobrir_estados(&mut Falso::jornada(), "x", "v2", &mut grafo, 100).unwrap();
        assert!(grafo.novas_em("v2").is_empty());
    }

    struct Roteiro(Vec<Option<&'static str>>, std::cell::Cell<usize>);

    impl crate::portas::AgenteQA for Roteiro {
        fn planejar(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn proxima_acao(&self, _: &str, _: &str, _: &[String], _: &[String]) -> Result<Option<String>, String> {
            let i = self.1.get();
            self.1.set(i + 1);
            match self.0.get(i) {
                Some(a) => Ok(a.map(String::from)),
                None => Err("agente sem resposta (cota esgotada)".into()),
            }
        }
    }

    struct ComPlano(std::cell::RefCell<Vec<String>>, Result<Vec<String>, String>);

    impl crate::portas::AgenteQA for ComPlano {
        fn planejar(&self, _: &str) -> Result<Vec<String>, String> {
            self.1.clone()
        }
        fn proxima_acao(&self, objetivo: &str, _: &str, _: &[String], _: &[String]) -> Result<Option<String>, String> {
            self.0.borrow_mut().push(objetivo.to_string());
            Ok(None)
        }
    }

    #[test]
    fn politica_confiante_escolhe_sem_chamar_o_llm_e_o_passo_fica_marcado() {
        let pol = crate::aprendizado::Politica::nova();
        let t = crate::aprendizado::Transicao {
            origem: "login".into(),
            acao: "entrar".into(),
            destino: "painel".into(),
            ..Default::default()
        };
        for _ in 0..4 {
            pol.aprender(&t, 2.0);
        }
        let agente = ComPlano(Default::default(), Ok(vec![]));
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_observada(
            &agente,
            &mut Falso::jornada(),
            "x",
            "o",
            "v1",
            &mut grafo,
            5,
            &mut |_, _| Ok(()),
            &mut |_, _| {},
            Some(&pol),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(j.passos.len(), 1);
        assert_eq!(j.passos[0].acao, "entrar");
        assert_eq!(j.passos[0].modelo.as_deref(), Some(MODELO_POLITICA));
        // o LLM so foi chamado na tela seguinte, onde a politica nao tem base (e disse "cumprido")
        assert_eq!(agente.0.borrow().len(), 1);
        assert_eq!(j.parou_por, OBJETIVO_CUMPRIDO);
    }

    /// Agente que prefere SEMPRE a acao "perguntar" quando ela existe e anota as opcoes que viu.
    struct Insistente(std::cell::RefCell<Vec<Vec<String>>>);

    impl crate::portas::AgenteQA for Insistente {
        fn planejar(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn proxima_acao(&self, _: &str, _: &str, opcoes: &[String], _: &[String]) -> Result<Option<String>, String> {
            self.0.borrow_mut().push(opcoes.to_vec());
            Ok(opcoes.iter().find(|o| *o == "perguntar").or(opcoes.first()).cloned())
        }
    }

    #[test]
    fn restricao_de_escrita_vale_para_todos_e_some_com_a_opcao_ao_atingir_o_maximo() {
        let agente = Insistente(Default::default());
        let r = Restricao::nova("Perguntar", 1).unwrap();
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_observada(
            &agente,
            &mut Falso::jornada(),
            "x",
            "o",
            "v1",
            &mut grafo,
            8,
            &mut |_, _| Ok(()),
            &mut |_, _| {},
            None,
            Some(&r),
            None,
            None,
        )
        .unwrap();
        let perguntas = j.passos.iter().filter(|p| p.acao == "perguntar").count();
        assert_eq!(perguntas, 1, "{:?}", j.passos.iter().map(|p| p.acao.clone()).collect::<Vec<_>>());
        let vistas = agente.0.borrow();
        let depois: Vec<&Vec<String>> = vistas.iter().skip_while(|o| !o.contains(&"perguntar".to_string())).skip(1).collect();
        assert!(!depois.is_empty() && depois.iter().all(|o| !o.contains(&"perguntar".to_string())), "{vistas:?}");
        assert_eq!(Restricao::nova("", 3), None);
        assert_eq!(Restricao::nova("salvar", 0), None);
    }

    /// Tela que so muda o contador do titulo ("Pagina1" -> "Pagina2"): a MESMA tela para a politica e a anti-repeticao.
    struct Giratorio(u32);

    impl Navegador for Giratorio {
        fn abrir(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn clicar(&mut self, _: &str) -> Result<(), String> {
            self.0 += 1;
            Ok(())
        }
        fn preencher(&mut self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn estado(&mut self) -> Result<String, String> {
            Ok(format!("Pagina{}", self.0))
        }
        fn clicaveis(&mut self) -> Result<Vec<String>, String> {
            Ok(vec!["x".into()])
        }
        fn texto(&mut self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn captura(&mut self) -> Result<Vec<u8>, String> {
            Ok(vec![])
        }
    }

    #[test]
    fn anti_repeticao_usa_a_chave_abstrata_e_nao_o_estado_cru() {
        // Sem a chave abstrata, "Pagina1", "Pagina2"... seriam telas diferentes e o agente clicaria "x" ate o limite.
        let agente = Roteiro(vec![Some("x"); 20], Default::default());
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada(&agente, &mut Giratorio(1), "u", "o", "v1", &mut grafo, 10).unwrap();
        assert_eq!(j.len(), LIMITE_REPETICAO, "so {LIMITE_REPETICAO} cliques na mesma tela abstrata, fez {}", j.len());
    }

    #[test]
    fn criterio_cumprido_paga_o_passo_e_da_credito_retroativo_aos_anteriores() {
        let pol = crate::aprendizado::Politica::nova();
        let agente = Roteiro(vec![Some("entrar"), Some("abrir_chat"), None], Default::default());
        let mut grafo = VersionedStateGraph::default();
        let mut avaliar = |p: &Passo| -> Vec<String> {
            if p.acao == "abrir_chat" {
                vec!["CA01".to_string()]
            } else {
                Vec::new()
            }
        };
        let j = jornada_guiada_observada(
            &agente,
            &mut Falso::jornada(),
            "x",
            "o",
            "v1",
            &mut grafo,
            10,
            &mut |_, _| Ok(()),
            &mut |_, _| {},
            Some(&pol),
            None,
            None,
            Some(&mut avaliar),
        )
        .unwrap();
        assert_eq!(j.passos[1].criterios, vec!["CA01".to_string()]);
        assert!(j.passos[0].criterios.is_empty());
        let do_criterio = pol.celula("painel", "abrir_chat").unwrap().q;
        let anterior = pol.celula("login", "entrar").unwrap().q;
        assert!(do_criterio > 1.0, "o passo do criterio recebe a recompensa grande: {do_criterio}");
        assert!(anterior > 1.0, "o passo que levou ate ele recebe credito retroativo: {anterior}");
        assert_eq!(j.parou_por, OBJETIVO_CUMPRIDO);
    }

    #[test]
    fn restricao_por_tipo_permite_uma_aprovacao_e_uma_reprovacao_e_no_maximo_isso() {
        let r = Restricao::nova("aprovar|reprovar", 2).unwrap().com_max_por_tipo(1);
        assert!(r.bloqueia("Aprovar", &["Aprovar"]), "a 2a aprovacao fica bloqueada");
        assert!(!r.bloqueia("Reprovar", &["Aprovar"]), "a reprovacao ainda e permitida");
        assert!(!r.bloqueia("Voltar", &["Aprovar"]), "o que nao e escrita nunca e bloqueado");
        assert!(r.bloqueia("Reprovar", &["Aprovar", "Reprovar"]) && r.bloqueia("Aprovar", &["Aprovar", "Reprovar"]));
        // sem max_por_tipo vale so o total: duas aprovacoes seriam permitidas (o defeito visto na rodada 10)
        let so_total = Restricao::nova("aprovar|reprovar", 2).unwrap();
        assert!(!so_total.bloqueia("Aprovar", &["Aprovar"]));
    }

    #[test]
    fn depois_de_cumprir_um_criterio_a_politica_ve_outro_estado_na_mesma_tela() {
        let pol = crate::aprendizado::Politica::nova();
        let agente = Roteiro(vec![Some("entrar"), Some("abrir_chat"), None], Default::default());
        let mut grafo = VersionedStateGraph::default();
        let mut avaliar = |p: &Passo| -> Vec<String> {
            if p.acao == "entrar" {
                vec!["CA01 aprovacao registrada".to_string()]
            } else {
                Vec::new()
            }
        };
        jornada_guiada_observada(
            &agente,
            &mut Falso::jornada(),
            "x",
            "o",
            "v1",
            &mut grafo,
            10,
            &mut |_, _| Ok(()),
            &mut |_, _| {},
            Some(&pol),
            None,
            None,
            Some(&mut avaliar),
        )
        .unwrap();
        assert!(pol.celula("painel / ✓[CA01]", "abrir_chat").is_some(), "o 2o passo foi aprendido no estado COM o criterio");
        assert!(pol.celula("painel", "abrir_chat").is_none(), "e nao no estado sem ele");
    }

    #[test]
    fn modo_so_conselho_nao_deixa_a_politica_escolher_mesmo_confiante() {
        let pol = crate::aprendizado::Politica::nova();
        let t = crate::aprendizado::Transicao {
            origem: "login".into(),
            acao: "entrar".into(),
            destino: "painel".into(),
            ..Default::default()
        };
        for _ in 0..4 {
            pol.aprender(&t, 2.0);
        }
        pol.definir_so_conselho(true);
        let agente = ComPlano(Default::default(), Ok(vec![]));
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_observada(
            &agente,
            &mut Falso::jornada(),
            "x",
            "o",
            "v1",
            &mut grafo,
            5,
            &mut |_, _| Ok(()),
            &mut |_, _| {},
            Some(&pol),
            None,
            None,
            None,
        )
        .unwrap();
        // quem decidiu foi o LLM (que disse "cumprido" na 1a pergunta): a politica, mesmo confiante, ficou calada
        assert!(j.passos.is_empty());
        assert_eq!(agente.0.borrow().len(), 1);
    }

    #[test]
    fn plano_do_agente_entra_no_objetivo_de_cada_escolha() {
        let agente = ComPlano(Default::default(), Ok(vec!["abrir o chat".into(), "perguntar".into()]));
        let mut grafo = VersionedStateGraph::default();
        jornada_guiada(&agente, &mut Falso::jornada(), "x", "falar", "v1", &mut grafo, 5).unwrap();
        let visto = agente.0.borrow();
        assert!(visto[0].starts_with("falar") && visto[0].contains("abrir o chat; perguntar"), "{visto:?}");
    }

    #[test]
    fn plano_com_erro_grave_interrompe_e_plano_malformado_segue_sem_plano() {
        let grave = ComPlano(Default::default(), Err("cadeia esgotada (cota)".into()));
        let mut grafo = VersionedStateGraph::default();
        let r = jornada_guiada(&grave, &mut Falso::jornada(), "x", "falar", "v1", &mut grafo, 5);
        assert!(r.is_err());
        assert!(grave.0.borrow().is_empty());
        let malformado = ComPlano(Default::default(), Err("a resposta nao era JSON".into()));
        jornada_guiada(&malformado, &mut Falso::jornada(), "x", "falar", "v1", &mut grafo, 5).unwrap();
        assert_eq!(malformado.0.borrow()[0], "falar");
    }

    #[test]
    fn jornada_segue_o_agente_ate_ele_dizer_que_cumpriu() {
        let agente = Roteiro(vec![Some("entrar"), Some("abrir_chat"), None], Default::default());
        let mut grafo = VersionedStateGraph::default();
        let passos =
            jornada_guiada(&agente, &mut Falso::jornada(), "x", "falar com o chat", "v1", &mut grafo, 10).unwrap();
        assert_eq!(passos.len(), 2);
        assert_eq!(passos[1].destino, "chat");
        assert_eq!(grafo.destino("v1", "painel", "abrir_chat"), Some("chat"));
    }

    #[test]
    fn jornada_respeita_o_limite_de_passos() {
        let agente = Roteiro(vec![Some("entrar"), Some("abrir_chat"), Some("perguntar")], Default::default());
        let mut grafo = VersionedStateGraph::default();
        let passos = jornada_guiada(&agente, &mut Falso::jornada(), "x", "o", "v1", &mut grafo, 2).unwrap();
        assert_eq!(passos.len(), 2);
    }

    struct Confuso;

    impl crate::portas::AgenteQA for Confuso {
        fn planejar(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn proxima_acao(&self, _: &str, estado: &str, _: &[String], _: &[String]) -> Result<Option<String>, String> {
            if estado == "login" {
                Ok(Some("entrar".into()))
            } else {
                Err("acao fora das opcoes oferecidas: \"Identidade6\"".into())
            }
        }
    }

    #[test]
    fn resposta_malformada_do_modelo_cai_na_escolha_deterministica_marcada_e_depois_para() {
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_com(&Confuso, &mut Falso::jornada(), "x", "o", "v1", &mut grafo, 20, &mut |_, _| Ok(()))
            .unwrap();
        // 1 escolha do agente + MAX_FALLBACKS_SEGUIDOS escolhas deterministicas, e so entao a parada
        assert_eq!(j.passos.len(), 1 + MAX_FALLBACKS_SEGUIDOS);
        assert_eq!(j.passos[0].modelo, None);
        assert!(j.passos[1..].iter().all(|p| p.modelo.as_deref() == Some(MODELO_FALLBACK)));
        assert!(j.parou_por.starts_with("o agente nao conseguiu escolher"), "{}", j.parou_por);
        assert!(j.erro.is_none());
    }

    #[test]
    fn fallback_nunca_escolhe_campo_de_texto_porque_nao_ha_o_que_digitar() {
        struct SoCampos;
        impl Navegador for SoCampos {
            fn abrir(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn clicar(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn preencher(&mut self, _: &str, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn estado(&mut self) -> Result<String, String> {
                Ok("form".into())
            }
            fn clicaveis(&mut self) -> Result<Vec<String>, String> {
                Ok(vec!["Preencher: nome".into()])
            }
            fn texto(&mut self, _: &str) -> Result<String, String> {
                Ok(String::new())
            }
            fn captura(&mut self) -> Result<Vec<u8>, String> {
                Ok(vec![])
            }
        }
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_com(&Confuso, &mut SoCampos, "x", "o", "v1", &mut grafo, 5, &mut |_, _| Ok(())).unwrap();
        assert!(j.passos.is_empty());
        assert!(j.parou_por.starts_with("o agente nao conseguiu escolher"), "{}", j.parou_por);
    }

    /// Tela em que nenhum clique muda nada: o pior caso para um agente que insiste na primeira opcao.
    struct Preso;

    impl Navegador for Preso {
        fn abrir(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn clicar(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn preencher(&mut self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn estado(&mut self) -> Result<String, String> {
            Ok("assistente".into())
        }
        fn clicaveis(&mut self) -> Result<Vec<String>, String> {
            Ok(vec!["nome".into(), "outra".into()])
        }
        fn texto(&mut self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
        fn captura(&mut self) -> Result<Vec<u8>, String> {
            Ok(vec![])
        }
    }

    struct PrimeiraOpcao;

    impl crate::portas::AgenteQA for PrimeiraOpcao {
        fn planejar(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn proxima_acao(&self, _: &str, _: &str, opcoes: &[String], _: &[String]) -> Result<Option<String>, String> {
            Ok(opcoes.first().cloned())
        }
    }

    #[test]
    fn opcao_ja_tentada_duas_vezes_sai_da_lista_e_a_jornada_acaba_quando_nao_resta_nenhuma() {
        let mut grafo = VersionedStateGraph::default();
        let j = jornada_guiada_com(&PrimeiraOpcao, &mut Preso, "x", "o", "v1", &mut grafo, 50, &mut |_, _| Ok(()))
            .unwrap();
        let acoes: Vec<&str> = j.passos.iter().map(|p| p.acao.as_str()).collect();
        assert_eq!(acoes, vec!["nome", "nome", "outra", "outra"]);
        assert!(j.parou_por.starts_with("todas as opcoes desta tela"), "{}", j.parou_por);
    }

    #[test]
    fn preencher_conta_como_a_mesma_acao_mesmo_com_texto_diferente() {
        assert_eq!(base_da_acao("Preencher: nome = Ana"), base_da_acao("Preencher: nome = Beto"));
        assert_ne!(base_da_acao("Preencher: nome = Ana"), base_da_acao("Preencher: classe = Mago"));
    }

    #[test]
    fn jornada_diz_por_que_parou_no_limite_e_no_objetivo_cumprido() {
        let mut grafo = VersionedStateGraph::default();
        let limite = Roteiro(vec![Some("entrar"), Some("abrir_chat")], Default::default());
        let j = jornada_guiada_com(&limite, &mut Falso::jornada(), "x", "o", "v1", &mut grafo, 2, &mut |_, _| Ok(()))
            .unwrap();
        assert_eq!(j.parou_por, "limite de 2 passos");
        let cumprido = Roteiro(vec![Some("entrar"), None], Default::default());
        let mut g2 = VersionedStateGraph::default();
        let j2 = jornada_guiada_com(&cumprido, &mut Falso::jornada(), "x", "o", "v1", &mut g2, 5, &mut |_, _| Ok(()))
            .unwrap();
        assert_eq!(j2.parou_por, "o agente considerou o objetivo cumprido");
    }

    #[test]
    fn erro_do_agente_interrompe_sem_inventar_passo() {
        let agente = Roteiro(vec![Some("entrar")], Default::default());
        let mut grafo = VersionedStateGraph::default();
        let r = jornada_guiada(&agente, &mut Falso::jornada(), "x", "o", "v1", &mut grafo, 10);
        assert!(r.unwrap_err().contains("cota"));
        assert_eq!(grafo.novas_em("v1").len(), 1);
    }
}
