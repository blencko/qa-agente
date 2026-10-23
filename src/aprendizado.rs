//! Aprendizado por reforco (Q-learning tabular) sobre o que as rodadas ja fizeram.
//!
//! Estado = tela abstraida (`chave_estado`); acao = opcao sem o texto digitado (`chave_acao`). A recompensa de cada
//! transicao mede PROGRESSO, nao "mudou qualquer coisa": tela nova na execucao, etapa do assistente que avanca (ou
//! recua) e passo que nao muda nada. A tabela Q vive em JSON (`Politica::salvar`) e e espelhada no banco do RAG.
//! O agente LLM continua escolhendo; a politica entra como EXPERIENCIA no prompt (`conselho`) e, quando a jornada
//! empaca, como escolha gulosa por UCB. Tudo em memoria + arquivo: sem dependencia nova.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const ALFA: f64 = 0.3;
pub const GAMA: f64 = 0.9;
/// Peso da exploracao no UCB.
pub const C_UCB: f64 = 0.6;
/// Quando a politica escolhe sem perguntar ao LLM: visitas minimas, valor minimo e folga sobre a segunda opcao.
pub const N_MIN_CONFIANTE: u32 = 3;
pub const Q_MIN_CONFIANTE: f64 = 1.0;
pub const MARGEM_CONFIANTE: f64 = 0.5;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Celula {
    pub q: f64,
    pub n: u32,
}

type Tabela = BTreeMap<String, BTreeMap<String, Celula>>;

/// Uma transicao observada: origem, acao e destino (textos crus do estado do navegador).
#[derive(Debug, Clone, PartialEq)]
pub struct Transicao {
    pub origem: String,
    pub acao: String,
    pub destino: String,
    /// O que o passo causou no backend (vazio quando o log nao tem essa informacao).
    pub efeito: crate::efeito::Efeito,
}

/// Tira o contador colado ao nome ("Identidade6" -> "Identidade"): o numero muda a cada interacao e nao e a tela.
fn sem_contador(segmento: &str) -> String {
    let t = segmento.trim_end_matches(|c: char| c.is_ascii_digit());
    if t.len() < segmento.len() && t.chars().last().is_some_and(|c| c.is_alphabetic()) {
        t.to_string()
    } else {
        segmento.to_string()
    }
}

/// Tela abstraida: segmentos do estado sem contadores. Mantem a etapa ("3 Pericias e equipamento").
pub fn chave_estado(estado: &str) -> String {
    estado.split(" / ").map(|s| sem_contador(s.trim())).collect::<Vec<_>>().join(" / ")
}

/// Numero da etapa do assistente quando algum segmento comeca por "N " (ex.: "3 Pericias e equipamento3" -> 3).
pub fn etapa(estado: &str) -> Option<u32> {
    estado.split(" / ").find_map(|s| {
        let (num, resto) = s.trim().split_once(' ')?;
        (resto.chars().next().is_some_and(|c| c.is_alphabetic())).then(|| num.parse().ok()).flatten()
    })
}

/// "Preencher: Nome = Aria" e "Preencher: Nome" sao a mesma acao para o aprendizado.
pub fn chave_acao(acao: &str) -> String {
    acao.split(" = ").next().unwrap_or(acao).to_string()
}

/// O que uma execucao ja viu: telas visitadas e a etapa mais alta do assistente que alcancou.
#[derive(Debug, Default)]
pub struct Memoria {
    vistas: BTreeSet<String>,
    max_etapa: u32,
    /// Quantas vezes cada par (tela, acao) ja foi feito nesta execucao.
    pares: std::collections::BTreeMap<(String, String), u32>,
    /// Passos seguidos sem a tela mudar e sem efeito novo no backend.
    parados: u32,
    /// Rotas de API, eventos de WebSocket e erros do backend ja vistos nesta execucao.
    endpoints: BTreeSet<String>,
    eventos: BTreeSet<String>,
    achados: BTreeSet<String>,
}

/// Recompensa de uma transicao. Premia PROGRESSO, e so uma vez: tela nova e etapa que bate o recorde da execucao.
/// Avancar de novo uma etapa que ja tinha sido alcancada nao paga (senao voltar e avancar viraria um moedeiro).
pub fn recompensa(t: &Transicao, mem: &mut Memoria) -> f64 {
    let (o, d) = (chave_estado(&t.origem), chave_estado(&t.destino));
    let mut r = 0.0;
    // Backend: o servidor respondendo algo NOVO e progresso mesmo quando a tela nao muda (ex.: salvou). E cada erro
    // novo do servidor e um achado de QA, pago uma vez so por assinatura, para nao premiar quebrar o mesmo ponto.
    let ef = &t.efeito;
    let novas_rotas = ef.endpoints.iter().filter(|e| !e.starts_with('5') && mem.endpoints.insert((*e).clone())).count();
    let novos_eventos = ef.eventos_ws.iter().filter(|e| mem.eventos.insert((*e).clone())).count();
    let novos_achados =
        ef.erros_5xx.iter().chain(&ef.falhas).filter(|e| mem.achados.insert((*e).clone())).count();
    r += 0.6 * novas_rotas.min(3) as f64 + 0.3 * novos_eventos.min(2) as f64 + 1.5 * novos_achados.min(2) as f64;
    let efeito_novo = novas_rotas + novos_eventos > 0;
    if o == d && efeito_novo {
        mem.parados = 0; // a tela nao mudou, mas o servidor reagiu a algo novo: nao e ficar parado
    } else if o == d {
        // Ficar parado custa mais a cada passo seguido: o agente nao ganha nada rodando em circulos na mesma tela.
        mem.parados += 1;
        r -= 0.5 * mem.parados as f64;
    } else {
        mem.parados = 0;
        if mem.vistas.insert(d.clone()) {
            r += 1.2; // tela nova nesta execucao
        } else {
            r -= 0.3; // voltou para uma tela que ja tinha visto: nao e progresso
        }
    }
    // Repetir o MESMO par (tela, acao) ja feito nesta execucao e farmar recompensa que ja foi paga: perde, e perde mais
    // a cada repeticao (limitado para a penalidade nao dominar o aprendizado).
    let vezes = mem.pares.entry((o.clone(), chave_acao(&t.acao))).or_insert(0);
    *vezes += 1;
    if *vezes > 1 {
        r -= (0.7 * (*vezes - 1) as f64).min(3.0);
    }
    mem.vistas.insert(o);
    if let (Some(a), Some(b)) = (etapa(&t.origem), etapa(&t.destino)) {
        mem.max_etapa = mem.max_etapa.max(a);
        if b > mem.max_etapa {
            r += 2.0 * (b - mem.max_etapa) as f64; // etapa nova: recorde da execucao
            mem.max_etapa = b;
        } else if b < a {
            r -= (a - b) as f64; // voltou: desfez progresso
        }
    }
    r
}

/// Recuar de etapa encerra o valor futuro da acao: de la se "pode avancar de novo", mas isso nao e mérito do recuo.
fn recuou(t: &Transicao) -> bool {
    matches!((etapa(&t.origem), etapa(&t.destino)), (Some(a), Some(b)) if b < a)
}

/// Tabela Q com interior mutavel: a jornada le o conselho e grava a experiencia pelo mesmo objeto.
#[derive(Debug, Default)]
pub struct Politica {
    tabela: RefCell<Tabela>,
}

#[derive(Serialize, Deserialize)]
struct Arquivo {
    versao: u32,
    q: Tabela,
}

impl Politica {
    pub fn nova() -> Self {
        Self::default()
    }

    pub fn carregar(caminho: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(caminho) {
            Ok(txt) => {
                let a: Arquivo = serde_json::from_str(&txt).map_err(|e| format!("politica ilegivel: {e}"))?;
                Ok(Politica { tabela: RefCell::new(a.q) })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::nova()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn salvar(&self, caminho: &Path) -> Result<(), String> {
        let a = Arquivo { versao: 1, q: self.tabela.borrow().clone() };
        let txt = serde_json::to_string(&a).map_err(|e| e.to_string())?;
        // escreve num temporario e renomeia: parar no meio nunca deixa a politica pela metade
        let tmp = caminho.with_extension("tmp");
        std::fs::write(&tmp, txt).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, caminho).map_err(|e| e.to_string())
    }

    fn max_q(t: &Tabela, estado: &str) -> f64 {
        t.get(estado).map(|m| m.values().map(|c| c.q).fold(f64::NEG_INFINITY, f64::max)).filter(|v| v.is_finite()).unwrap_or(0.0)
    }

    /// Atualizacao TD: Q(s,a) += alfa * (r + gama * max Q(s',.) - Q(s,a)). Conta uma visita.
    pub fn aprender(&self, t: &Transicao, r: f64) {
        self.aprender_contando(t, r, true);
    }

    /// `contar = false` atualiza o valor sem somar visita: as epocas repetidas do treino offline reprocessam o MESMO
    /// dado, e contar cada repeticao inflaria `n` (e a confianca da politica) sem experiencia nova.
    fn aprender_contando(&self, t: &Transicao, r: f64, contar: bool) {
        let mut tab = self.tabela.borrow_mut();
        let (s, a, d) = (chave_estado(&t.origem), chave_acao(&t.acao), chave_estado(&t.destino));
        let futuro = if s == d || recuou(t) { 0.0 } else { Self::max_q(&tab, &d) };
        let c = tab.entry(s).or_default().entry(a).or_default();
        c.q += ALFA * (r + GAMA * futuro - c.q);
        if contar {
            c.n += 1;
        }
    }

    /// Repassa varias execucoes (cada uma em ordem) `epocas` vezes. Devolve quantas transicoes usou por epoca.
    /// So a primeira epoca conta visitas: as demais refinam o valor sobre o mesmo dado.
    pub fn treinar(&self, execucoes: &[Vec<Transicao>], epocas: usize) -> usize {
        for epoca in 0..epocas {
            for exec in execucoes {
                let mut mem = Memoria::default();
                for t in exec {
                    let r = recompensa(t, &mut mem);
                    self.aprender_contando(t, r, epoca == 0);
                }
            }
        }
        execucoes.iter().map(Vec::len).sum()
    }

    /// Escolha sem LLM, so quando a politica tem base: a melhor opcao (sem campo de texto) foi vista pelo menos
    /// `N_MIN_CONFIANTE` vezes, vale pelo menos `Q_MIN_CONFIANTE` e abre `MARGEM_CONFIANTE` sobre a segunda.
    pub fn escolha_confiante<'a>(&self, estado: &str, opcoes: &'a [String]) -> Option<&'a String> {
        let tab = self.tabela.borrow();
        let linha = tab.get(&chave_estado(estado))?;
        let mut pontuadas: Vec<(&String, Celula)> = opcoes
            .iter()
            .filter(|o| !o.starts_with("Preencher: "))
            .filter_map(|o| linha.get(&chave_acao(o)).map(|c| (o, *c)))
            .collect();
        pontuadas.sort_by(|a, b| b.1.q.partial_cmp(&a.1.q).unwrap_or(std::cmp::Ordering::Equal));
        let (melhor, c) = pontuadas.first()?;
        if c.n < N_MIN_CONFIANTE || c.q < Q_MIN_CONFIANTE {
            return None;
        }
        let segundo = pontuadas.get(1).map(|p| p.1.q).unwrap_or(0.0);
        (c.q - segundo >= MARGEM_CONFIANTE).then_some(*melhor)
    }

    pub fn celula(&self, estado: &str, acao: &str) -> Option<Celula> {
        self.tabela.borrow().get(&chave_estado(estado))?.get(&chave_acao(acao)).copied()
    }

    /// Todas as linhas (tela, acao, q, n), para relatorio e espelho no banco.
    pub fn linhas(&self) -> Vec<(String, String, Celula)> {
        self.tabela
            .borrow()
            .iter()
            .flat_map(|(s, m)| m.iter().map(move |(a, c)| (s.clone(), a.clone(), *c)))
            .collect()
    }

    pub fn estados(&self) -> usize {
        self.tabela.borrow().len()
    }

    pub fn entradas(&self) -> usize {
        self.tabela.borrow().values().map(BTreeMap::len).sum()
    }

    /// Escolha gulosa por UCB entre as opcoes: valor aprendido + bonus por pouco testada. Opcao nunca testada
    /// naquela tela tem bonus maximo, entao a politica tambem explora.
    pub fn melhor_por_ucb<'a>(&self, estado: &str, opcoes: &'a [String]) -> Option<&'a String> {
        let tab = self.tabela.borrow();
        let linha = tab.get(&chave_estado(estado));
        let total: u32 = linha.map(|m| m.values().map(|c| c.n).sum()).unwrap_or(0);
        let ln_total = ((total + 1) as f64).ln();
        opcoes
            .iter()
            .map(|o| {
                let c = linha.and_then(|m| m.get(&chave_acao(o))).copied().unwrap_or_default();
                let bonus = C_UCB * (ln_total / (c.n as f64 + 1.0)).sqrt();
                (o, c.q + bonus)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(o, _)| o)
    }

    /// Texto curto de experiencia para o prompt: o que funcionou e o que nao adiantou nesta tela, e o que ainda
    /// ninguem tentou. Vazio quando a politica nao conhece a tela.
    pub fn conselho(&self, estado: &str, opcoes: &[String]) -> String {
        let tab = self.tabela.borrow();
        let Some(linha) = tab.get(&chave_estado(estado)) else { return String::new() };
        let mut vistas: Vec<(&String, Celula)> =
            opcoes.iter().filter_map(|o| linha.get(&chave_acao(o)).map(|c| (o, *c))).filter(|(_, c)| c.n > 0).collect();
        vistas.sort_by(|a, b| b.1.q.partial_cmp(&a.1.q).unwrap_or(std::cmp::Ordering::Equal));
        let curto = |s: &str| s.chars().take(40).collect::<String>();
        let bons: Vec<String> =
            vistas.iter().filter(|(_, c)| c.q > 0.3).take(3).map(|(o, c)| format!("{} ({:+.1})", curto(o), c.q)).collect();
        let ruins: Vec<String> = vistas
            .iter()
            .rev()
            .filter(|(_, c)| c.q < -0.1)
            .take(3)
            .map(|(o, c)| format!("{} ({:+.1})", curto(o), c.q))
            .collect();
        let novas: Vec<String> = opcoes
            .iter()
            .filter(|o| !linha.contains_key(&chave_acao(o)))
            .take(3)
            .map(|o| curto(o))
            .collect();
        let mut partes = Vec::new();
        if !bons.is_empty() {
            partes.push(format!("deu certo aqui: {}", bons.join("; ")));
        }
        if !ruins.is_empty() {
            partes.push(format!("nao adiantou ou desfez progresso: {}", ruins.join("; ")));
        }
        if !novas.is_empty() {
            partes.push(format!("nunca tentado aqui: {}", novas.join("; ")));
        }
        if partes.is_empty() {
            String::new()
        } else {
            format!("[Experiencia aprendida] {}", partes.join(" | "))
        }
    }
}

/// Le transicoes de um log de execucao: `passos.jsonl` ou a saida de texto do teste/rastro
/// (`  origem --[acao]--> destino` e `[passo N] origem --[acao]--> destino (sem mudanca) | modelo | tokens`).
pub fn ler_transicoes(texto: &str) -> Vec<Transicao> {
    let mut saida = Vec::new();
    for linha in texto.lines() {
        let l = linha.trim();
        if l.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                if let (Some(o), Some(a), Some(d)) = (v["origem"].as_str(), v["acao"].as_str(), v["destino"].as_str()) {
                    let efeito = serde_json::from_value(v["efeito"].clone()).unwrap_or_default();
                    saida.push(Transicao { origem: o.into(), acao: a.into(), destino: d.into(), efeito });
                }
            }
            continue;
        }
        let l = match l.strip_prefix("[passo ") {
            Some(resto) => resto.split_once("] ").map(|(_, r)| r).unwrap_or(resto),
            None => l,
        };
        let Some((origem, resto)) = l.split_once(" --[") else { continue };
        let Some((acao, destino)) = resto.split_once("]--> ") else { continue };
        let destino = destino.split(" | ").next().unwrap_or(destino);
        let destino = destino.strip_suffix(" (sem mudanca)").unwrap_or(destino);
        saida.push(Transicao { origem: origem.trim().into(), acao: acao.into(), destino: destino.trim().into(), efeito: Default::default() });
    }
    saida
}

#[cfg(test)]
mod testes {
    use super::*;

    fn t(o: &str, a: &str, d: &str) -> Transicao {
        Transicao { origem: o.into(), acao: a.into(), destino: d.into(), efeito: Default::default() }
    }

    #[test]
    fn estado_perde_o_contador_mas_mantem_a_etapa() {
        assert_eq!(
            chave_estado("/|Novo personagem / 3 Perícias e equipamento2 / Retrato"),
            "/|Novo personagem / 3 Perícias e equipamento / Retrato"
        );
        assert_eq!(chave_estado("/|Novo personagem / 1 Identidade6"), chave_estado("/|Novo personagem / 1 Identidade1"));
        assert_eq!(etapa("/|Novo personagem / 3 Perícias e equipamento2 / Retrato"), Some(3));
        assert_eq!(etapa("/|Mestre de D&D"), None);
        assert_eq!(chave_acao("Preencher: Nome = Aria"), "Preencher: Nome");
    }

    #[test]
    fn recompensa_premia_avanco_e_pune_recuo_e_passo_inutil() {
        let mut v = Memoria::default();
        let avanca = recompensa(&t("p / 1 Identidade", "Avançar", "p / 2 Atributos"), &mut v);
        let inutil = recompensa(&t("p / 2 Atributos", "Usar ficha", "p / 2 Atributos"), &mut v);
        let recua = recompensa(&t("p / 3 Perícias", "antecedente", "p / 1 Identidade"), &mut v);
        assert!(avanca > 2.0, "{avanca}");
        assert!(inutil < 0.0, "{inutil}");
        assert!(recua < avanca && recua < 0.5, "{recua}");
        // tela ja vista nesta execucao nao ganha o bonus de novidade de novo
        let de_novo = recompensa(&t("p / 2 Atributos", "x", "p / 1 Identidade"), &mut v);
        assert!(de_novo < 0.0, "{de_novo}");
    }

    #[test]
    fn ficar_parado_ou_girar_em_circulos_so_perde_e_perde_cada_vez_mais() {
        // parado: cada passo seguido sem mudar a tela custa mais que o anterior
        let mut m = Memoria::default();
        let parado: Vec<f64> = (0..5).map(|_| recompensa(&t("a / 1 X", "clicar", "a / 1 X"), &mut m)).collect();
        assert!(parado.windows(2).all(|w| w[1] < w[0]) && parado.iter().sum::<f64>() < -5.0, "{parado:?}");

        // ciclo A -> B -> A -> B ...: so a primeira ida a B e a volta a A (tela nova) rendem; o resto e prejuizo
        let mut m = Memoria::default();
        let mut soma_depois_da_volta = 0.0;
        for i in 0..8 {
            let r = if i % 2 == 0 {
                recompensa(&t("a / 1 X", "ir", "a / 2 Y"), &mut m)
            } else {
                recompensa(&t("a / 2 Y", "voltar", "a / 1 X"), &mut m)
            };
            if i >= 2 {
                assert!(r < 0.0, "passo {i} do ciclo recompensou {r}");
                soma_depois_da_volta += r;
            }
        }
        assert!(soma_depois_da_volta < -5.0, "{soma_depois_da_volta}");

        // farmar o recorde de etapa de novo (voltar e avancar) deixa saldo negativo
        let mut m = Memoria::default();
        let primeira = recompensa(&t("p / 1 A", "Avançar", "p / 2 B"), &mut m);
        let mut saldo = 0.0;
        for _ in 0..3 {
            saldo += recompensa(&t("p / 2 B", "voltar", "p / 1 A"), &mut m);
            saldo += recompensa(&t("p / 1 A", "Avançar", "p / 2 B"), &mut m);
        }
        assert!(primeira > 2.0 && saldo < 0.0, "{primeira} {saldo}");
    }

    fn com_efeito(o: &str, a: &str, d: &str, ef: crate::efeito::Efeito) -> Transicao {
        Transicao { efeito: ef, ..t(o, a, d) }
    }

    #[test]
    fn backend_novo_vale_mesmo_sem_a_tela_mudar_e_so_na_primeira_vez() {
        use crate::efeito::Efeito;
        let salvou = Efeito { endpoints: vec!["2xx /api/personagens".into()], ..Default::default() };
        let mut m = Memoria::default();
        let primeira = recompensa(&com_efeito("a / 1 X", "Salvar", "a / 1 X", salvou.clone()), &mut m);
        let de_novo = recompensa(&com_efeito("a / 1 X", "Salvar2", "a / 1 X", salvou), &mut m);
        let sem = recompensa(&t("a / 1 X", "Nada", "a / 1 X"), &mut Memoria::default());
        assert!(primeira > 0.0, "salvar no servidor sem mudar a tela e progresso: {primeira}");
        assert!(de_novo < 0.0, "a mesma rota de novo nao paga e conta como parado: {de_novo}");
        assert!(sem < 0.0);
    }

    #[test]
    fn erro_do_servidor_e_achado_pago_uma_vez_por_assinatura() {
        use crate::efeito::Efeito;
        let quebrou = Efeito {
            endpoints: vec!["5xx /api/mestre".into()],
            erros_5xx: vec!["5xx /api/mestre".into()],
            ..Default::default()
        };
        let mut m = Memoria::default();
        let achado = recompensa(&com_efeito("a / 1 X", "Jogar", "a / 1 X", quebrou.clone()), &mut m);
        let repetido = recompensa(&com_efeito("a / 1 X", "Jogar", "a / 1 X", quebrou), &mut m);
        assert!(achado > 0.5, "{achado}");
        assert!(repetido < 0.0, "quebrar o mesmo ponto de novo nao rende: {repetido}");
    }

    #[test]
    fn efeito_do_backend_vem_do_log_jsonl() {
        let l = "{\"n\":1,\"origem\":\"o\",\"acao\":\"z\",\"destino\":\"d\",\"efeito\":{\"endpoints\":[\"2xx /api/x\"]}}";
        let ts = ler_transicoes(l);
        assert_eq!(ts[0].efeito.endpoints, vec!["2xx /api/x"]);
        assert!(ler_transicoes("{\"origem\":\"o\",\"acao\":\"z\",\"destino\":\"d\"}")[0].efeito.vazio());
    }

    #[test]
    fn politica_aprende_a_avancar_e_a_evitar_o_que_desfaz() {
        let exec = vec![
            t("p / 1 Identidade1", "Avançar", "p / 2 Atributos"),
            t("p / 2 Atributos", "3 Perícias", "p / 3 Perícias2"),
            t("p / 3 Perícias2", "antecedente", "p / 1 Identidade1"),
            t("p / 1 Identidade1", "antecedente", "p / 1 Identidade1"),
        ];
        let pol = Politica::nova();
        assert_eq!(pol.treinar(&[exec], 30), 4);
        let avanca = pol.celula("p / 1 Identidade3", "Avançar").unwrap();
        let recua = pol.celula("p / 3 Perícias9", "antecedente").unwrap();
        assert!(avanca.q > 1.0 && recua.q < 0.0, "{avanca:?} {recua:?}");
        let opcoes = vec!["Avançar".to_string(), "antecedente".to_string()];
        assert_eq!(pol.melhor_por_ucb("p / 1 Identidade2", &opcoes).unwrap(), "Avançar");
        let c = pol.conselho("p / 3 Perícias1", &["antecedente".into(), "Avançar".into()]);
        assert!(c.contains("nao adiantou") && c.contains("antecedente") && c.contains("nunca tentado aqui: Avançar"), "{c}");
        assert_eq!(pol.conselho("tela desconhecida", &["x".into()]), "");
    }

    #[test]
    fn treino_offline_nao_infla_a_contagem_de_visitas() {
        let pol = Politica::nova();
        pol.treinar(&[vec![t("a / 1 X", "ir", "a / 2 Y")]], 30);
        assert_eq!(pol.celula("a / 1 X", "ir").unwrap().n, 1);
    }

    #[test]
    fn politica_so_escolhe_sozinha_com_base_valor_e_folga() {
        let opcoes = vec!["Avançar".to_string(), "Voltar".to_string(), "Preencher: Nome = x".to_string()];
        let pol = Politica::nova();
        let ir = t("a / 1 X", "Avançar", "a / 2 Y");
        pol.aprender(&ir, 2.0);
        pol.aprender(&ir, 2.0);
        assert!(pol.escolha_confiante("a / 1 X", &opcoes).is_none(), "2 visitas ainda nao bastam");
        pol.aprender(&ir, 2.0);
        pol.aprender(&ir, 2.0);
        assert_eq!(pol.escolha_confiante("a / 1 X", &opcoes).unwrap(), "Avançar");
        // sem folga sobre a segunda opcao, nao decide
        let volta = t("a / 1 X", "Voltar", "a / 2 Y");
        for _ in 0..4 {
            pol.aprender(&volta, 2.0);
        }
        assert!(pol.escolha_confiante("a / 1 X", &opcoes).is_none());
        // tela que a politica nao conhece: nunca decide
        assert!(pol.escolha_confiante("outra / tela", &opcoes).is_none());
        // campo de texto nunca e escolhido sem o LLM (nao ha o que digitar)
        let so_texto = vec!["Preencher: Nome = x".to_string()];
        let pol2 = Politica::nova();
        for _ in 0..5 {
            pol2.aprender(&t("a / 1 X", "Preencher: Nome = x", "a / 2 Y"), 2.0);
        }
        assert!(pol2.escolha_confiante("a / 1 X", &so_texto).is_none());
    }

    #[test]
    fn ucb_explora_a_opcao_nunca_tentada() {
        let pol = Politica::nova();
        for _ in 0..5 {
            pol.aprender(&t("a", "x", "a"), -0.5);
        }
        let opcoes = vec!["x".to_string(), "y".to_string()];
        assert_eq!(pol.melhor_por_ucb("a", &opcoes).unwrap(), "y");
    }

    #[test]
    fn salva_e_recarrega_sem_perder_a_tabela() {
        let pol = Politica::nova();
        pol.treinar(&[vec![t("a / 1 X", "ir", "a / 2 Y")]], 5);
        let arq = std::env::temp_dir().join(format!("qa-politica-teste-{}.json", std::process::id()));
        pol.salvar(&arq).unwrap();
        let outra = Politica::carregar(&arq).unwrap();
        assert_eq!(outra.celula("a / 1 X", "ir"), pol.celula("a / 1 X", "ir"));
        assert_eq!(outra.estados(), pol.estados());
        let _ = std::fs::remove_file(&arq);
        assert_eq!(Politica::carregar(&arq).unwrap().estados(), 0);
    }

    #[test]
    fn le_os_tres_formatos_de_log() {
        let texto = "ruido\n  a / 1 X --[Avançar]--> a / 2 Y\n[passo 3] a / 2 Y --[Voltar]--> a / 2 Y (sem mudanca) | m | 10t (total 10t)\n{\"n\":1,\"origem\":\"o\",\"acao\":\"z\",\"destino\":\"d\"}\n";
        let ts = ler_transicoes(texto);
        assert_eq!(ts.len(), 3);
        assert_eq!(ts[0], t("a / 1 X", "Avançar", "a / 2 Y"));
        assert_eq!(ts[1], t("a / 2 Y", "Voltar", "a / 2 Y"));
        assert_eq!(ts[2].acao, "z");
    }
}
