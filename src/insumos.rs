use serde_json::{json, Value};

use crate::evidencias::Relatorio;
use crate::exploracao::Passo;

/// Tudo o que uma rodada de QA ensina ao motor de decisao, em linhas JSON (uma por passo e uma por execucao).
pub struct Execucao<'a> {
    pub alvo: &'a str,
    pub objetivo: &'a str,
    pub passos: &'a [Passo],
    pub parou_por: &'a str,
    pub segundos: u64,
    pub transicoes_novas: usize,
    pub relatorio: &'a Relatorio,
}

fn curto(texto: &str, n: usize) -> String {
    texto.chars().take(n).collect()
}

/// Um passo e util quando mudou a tela: clique que nao leva a lugar nenhum e passo gasto.
pub fn passo_util(p: &Passo) -> bool {
    p.origem != p.destino
}

/// Fracao minima de passos que precisam mudar a tela para a jornada contar como progresso.
pub const TAXA_UTIL_MINIMA: f64 = 0.2;

/// A jornada so e "verde" se o agente cumpriu o objetivo ou se ao menos 20% dos passos mudaram a tela. Um QA que
/// clica 80 vezes sem sair do lugar nao testou nada, e chamar isso de verde e falso conforto.
pub fn progresso(passos: &[Passo], parou_por: &str) -> bool {
    if parou_por == crate::exploracao::OBJETIVO_CUMPRIDO {
        return true;
    }
    if passos.is_empty() {
        return false;
    }
    passos.iter().filter(|p| passo_util(p)).count() as f64 / passos.len() as f64 >= TAXA_UTIL_MINIMA
}

pub fn linhas(e: &Execucao) -> Vec<Value> {
    let mut saida: Vec<Value> = e
        .passos
        .iter()
        .enumerate()
        .map(|(i, p)| {
            json!({
                "tipo": "passo",
                "alvo": e.alvo,
                "n": i + 1,
                "origem": curto(&p.origem, 80),
                "acao": curto(&p.acao, 80),
                "destino": curto(&p.destino, 80),
                "mudou_de_tela": passo_util(p),
                "ambigua": p.ambigua,
                "preencheu_campo": p.acao.starts_with("Preencher: "),
                "modelo": p.modelo,
                "tokens": p.tokens,
            })
        })
        .collect();

    let uteis = e.passos.iter().filter(|p| passo_util(p)).count();
    let taxa_util = if e.passos.is_empty() { 0.0 } else { uteis as f64 / e.passos.len() as f64 };
    let b = &e.relatorio.backend;
    let tokens_total: u64 = e.passos.iter().filter_map(|p| p.tokens).sum();
    let mut modelos: Vec<&str> = e.passos.iter().filter_map(|p| p.modelo.as_deref()).collect();
    modelos.sort_unstable();
    modelos.dedup();
    saida.push(json!({
        "tipo": "execucao",
        "alvo": e.alvo,
        "objetivo": curto(e.objetivo, 200),
        "passos": e.passos.len(),
        "passos_uteis": uteis,
        "taxa_util": taxa_util,
        "segundos": e.segundos,
        // Custo da rodada em tokens e quais modelos o gateway realmente serviu (ha troca em fallback).
        "tokens_total": tokens_total,
        "modelos": modelos,
        "parou_por": e.parou_por,
        "objetivo_cumprido": e.parou_por == crate::exploracao::OBJETIVO_CUMPRIDO,
        "passou": e.relatorio.passou,
        "falhas": e.relatorio.falhas.len(),
        "transicoes_novas": e.transicoes_novas,
        "backend": {
            "requisicoes": b.requisicoes,
            "erros_5xx": b.erros_5xx.len(),
            "erros_4xx": b.erros_4xx.len(),
            "falhas_de_rede": b.falhas_de_rede.len(),
            "ws_enviados": b.ws_enviados,
            "ws_recebidos": b.ws_recebidos,
            "ws_eventos": b.ws_eventos.len(),
            "erros_ws": b.erros_ws.len(),
            "log_graves": b.log_graves.len(),
            "erros_console": b.erros_console.len(),
        },
        // Candidato a rotulo do motor, no formato do alimentar.mjs. E uma heuristica (taxa de passos uteis), nao um
        // rotulo de producao: o tipo ainda nao esta entre os estaveis do livro, entao fica em ensaio.
        "candidato_motor": {
            "rotulo": "ensaio",
            "tipo": "decisao-de-execucao-qa",
            "balde": "geral",
            "opcoes": ["executar", "pular", "bloquear"],
            "escolhida": "executar",
            "resultado": taxa_util,
        },
    }));
    saida
}

/// Uma linha JSON por registro, pronta para anexar a um arquivo `.jsonl`.
pub fn como_jsonl(registros: &[Value]) -> String {
    registros.iter().map(|r| format!("{r}\n")).collect()
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::evidencias::{consolidar, Evidencias};

    fn passo(origem: &str, acao: &str, destino: &str) -> Passo {
        Passo { origem: origem.into(), acao: acao.into(), destino: destino.into(), ambigua: false, modelo: None, tokens: None, efeito: Default::default(), criterios: Vec::new() }
    }

    fn relatorio() -> Relatorio {
        consolidar("v1", &[], &[], Evidencias::default())
    }

    fn execucao<'a>(passos: &'a [Passo], r: &'a Relatorio) -> Execucao<'a> {
        Execucao {
            alvo: "jogo",
            objetivo: "jogar",
            passos,
            parou_por: "limite",
            segundos: 12,
            transicoes_novas: 2,
            relatorio: r,
        }
    }

    #[test]
    fn uma_linha_por_passo_mais_uma_de_execucao() {
        let passos = vec![passo("a", "ir", "b"), passo("b", "Avancar", "b"), passo("b", "Preencher: nome = X", "b")];
        let r = relatorio();
        let l = linhas(&execucao(&passos, &r));
        assert_eq!(l.len(), 4);
        assert_eq!(l[0]["tipo"], "passo");
        assert_eq!(l[3]["tipo"], "execucao");
    }

    #[test]
    fn mede_passos_uteis_e_marca_campos_preenchidos() {
        let passos = vec![passo("a", "ir", "b"), passo("b", "Avancar", "b"), passo("b", "Preencher: nome = X", "b")];
        let r = relatorio();
        let l = linhas(&execucao(&passos, &r));
        assert_eq!(l[0]["mudou_de_tela"], true);
        assert_eq!(l[1]["mudou_de_tela"], false);
        assert_eq!(l[2]["preencheu_campo"], true);
        assert_eq!(l[3]["passos_uteis"], 1);
        assert!((l[3]["taxa_util"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn candidato_do_motor_vem_em_ensaio_com_resultado_na_escala() {
        let passos = vec![passo("a", "ir", "b")];
        let r = relatorio();
        let l = linhas(&execucao(&passos, &r));
        let c = &l[1]["candidato_motor"];
        assert_eq!(c["rotulo"], "ensaio");
        assert_eq!(c["tipo"], "decisao-de-execucao-qa");
        let resultado = c["resultado"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&resultado));
    }

    #[test]
    fn jornada_presa_na_mesma_tela_nao_conta_como_progresso() {
        let presos: Vec<Passo> = (0..10).map(|_| passo("a", "clicar", "a")).collect();
        assert!(!progresso(&presos, "limite de 10 passos"));
        assert!(!progresso(&[], "nada clicavel na tela"));
    }

    #[test]
    fn progresso_exige_um_quinto_dos_passos_ou_o_objetivo_cumprido() {
        let mut passos: Vec<Passo> = (0..8).map(|_| passo("a", "clicar", "a")).collect();
        passos.push(passo("a", "ir", "b"));
        assert!(!progresso(&passos, "limite"));
        passos.push(passo("b", "ir", "c"));
        assert!(progresso(&passos, "limite"));
        assert!(progresso(&[passo("a", "x", "a")], crate::exploracao::OBJETIVO_CUMPRIDO));
    }

    #[test]
    fn execucao_sem_passos_nao_divide_por_zero() {
        let r = relatorio();
        let l = linhas(&execucao(&[], &r));
        assert_eq!(l.len(), 1);
        assert_eq!(l[0]["taxa_util"], 0.0);
    }

    #[test]
    fn jsonl_tem_uma_linha_valida_por_registro() {
        let passos = vec![passo("a", "ir", "b")];
        let r = relatorio();
        let texto = como_jsonl(&linhas(&execucao(&passos, &r)));
        assert_eq!(texto.lines().count(), 2);
        assert!(texto.lines().all(|l| serde_json::from_str::<Value>(l).is_ok()));
    }
}
