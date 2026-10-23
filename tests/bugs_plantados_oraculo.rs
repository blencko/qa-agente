//! Valida a REGUA da medicao, sem Chrome nem LLM: cada defeito plantado e observavel pelos mesmos canais que o agente
//! usa (status HTTP, conexao sem resposta, log, verificacao de estado) e o corretor pontua o relatorio certo.
//! Roda no `cargo test` normal. A medicao do agente em si esta em `medicao_bugs.rs` (`--ignored`).

mod support;

use qa_agente::alvo::Alvo;
use qa_agente::verificacoes::{self, Verificacao};
use serde_json::json;
use support::{metricas, pontuar, Servidor, BUGS, NOME_DO_ALVO};

fn pasta(nome: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("qa-bugs-{nome}-{}", std::process::id()))
}

fn pedir(metodo: &str, url: &str, corpo: Option<&str>) -> Result<(u16, String), String> {
    let req = ureq::request(metodo, url).timeout(std::time::Duration::from_secs(5));
    let r = match corpo {
        Some(c) => req.set("Content-Type", "application/json").send_string(c),
        None => req.call(),
    };
    match r {
        Ok(r) => Ok((r.status(), r.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(s, r)) => Ok((s, r.into_string().unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

fn verificacoes_do(srv: &Servidor) -> Vec<Verificacao> {
    let alvo = Alvo::de_json(&srv.alvo_json(10)).unwrap();
    alvo.verificacoes()
}

#[test]
fn estado_limpo_passa_nas_verificacoes_e_os_controles_sao_saudaveis() {
    let srv = Servidor::iniciar(&pasta("limpo"));
    let r = verificacoes::executar(&verificacoes_do(&srv), None);
    assert!(r.iter().all(|v| v.ok), "{r:?}");
    assert_eq!(pedir("GET", &format!("{}/api/ping", srv.url), None).unwrap().0, 200);
    assert_eq!(pedir("GET", &format!("{}/favicon.ico", srv.url), None).unwrap().0, 404);
    // tarefa valida nao suja o estado
    let (s, _) = pedir("POST", &format!("{}/api/tarefas", srv.url), Some("{\"titulo\":\"comprar pao\"}")).unwrap();
    assert_eq!(s, 201);
    assert!(verificacoes::executar(&verificacoes_do(&srv), None).iter().all(|v| v.ok));
    assert_eq!(std::fs::read_to_string(&srv.log).unwrap(), "", "nenhum controle pode escrever no log");
}

#[test]
fn b1_exportar_devolve_500() {
    let srv = Servidor::iniciar(&pasta("b1"));
    assert_eq!(pedir("POST", &format!("{}/api/exportar", srv.url), Some("")).unwrap().0, 500);
}

#[test]
fn b2_tema_responde_200_mas_a_verificacao_de_estado_reprova() {
    let srv = Servidor::iniciar(&pasta("b2"));
    let (s, _) = pedir("POST", &format!("{}/api/preferencias", srv.url), Some("{\"tema\":\"escuro\"}")).unwrap();
    assert_eq!(s, 200);
    let r = verificacoes::executar(&verificacoes_do(&srv), None);
    let prefs = r.iter().find(|v| v.nome == "preferencias").unwrap();
    assert!(!prefs.ok, "{prefs:?}");
    assert!(r.iter().find(|v| v.nome == "tarefas validas").unwrap().ok);
}

#[test]
fn b3_titulo_vazio_e_aceito_e_a_verificacao_de_estado_reprova() {
    let srv = Servidor::iniciar(&pasta("b3"));
    let (s, _) = pedir("POST", &format!("{}/api/tarefas", srv.url), Some("{\"titulo\":\"   \"}")).unwrap();
    assert_eq!(s, 201);
    let r = verificacoes::executar(&verificacoes_do(&srv), None);
    assert!(!r.iter().find(|v| v.nome == "tarefas validas").unwrap().ok);
    assert!(r.iter().find(|v| v.nome == "preferencias").unwrap().ok);
}

#[test]
fn b4_sync_fecha_a_conexao_sem_responder() {
    let srv = Servidor::iniciar(&pasta("b4"));
    assert!(pedir("GET", &format!("{}/api/sync", srv.url), None).is_err());
}

#[test]
fn b5_resumo_da_200_mas_o_panico_fica_no_log() {
    let srv = Servidor::iniciar(&pasta("b5"));
    assert_eq!(pedir("GET", &format!("{}/api/relatorio/resumo", srv.url), None).unwrap().0, 200);
    let log = std::fs::read_to_string(&srv.log).unwrap();
    let (graves, _) = qa_agente::log_servidor::classificar(&log.lines().map(String::from).collect::<Vec<_>>());
    assert_eq!(graves.len(), 1, "{log}");
}

#[test]
fn o_alvo_gerado_e_um_alvo_valido_com_log_e_verificacoes() {
    let srv = Servidor::iniciar(&pasta("alvo"));
    let alvo = Alvo::de_json(&srv.alvo_json(25)).unwrap();
    assert_eq!(alvo.nome, NOME_DO_ALVO);
    assert_eq!(alvo.limite_passos, 25);
    assert_eq!(alvo.verificacoes().len(), 2);
    assert_eq!(alvo.logs.len(), 1);
    assert!(alvo.log_servidor.is_some());
}

fn relatorio_com_tudo() -> serde_json::Value {
    json!({
        "falhas": ["backend 5xx: 500 http://x/api/exportar", "preferencias", "tarefas validas",
                   "requisicao sem resposta: http://x/api/sync", "log do servidor: thread 'r' panicked at"],
        "front": { "erros_console": ["Uncaught TypeError: Cannot set properties of null (setting 'textContent')"] },
        "backend": {
            "erros_5xx": ["500 http://x/api/exportar"],
            "falhas_de_rede": ["http://x/api/sync"],
            "log_servidor": { "graves": ["thread 'r' panicked at"] },
        },
        "estado_no_backend": [{ "nome": "preferencias", "ok": false }, { "nome": "tarefas validas", "ok": false }],
    })
}

#[test]
fn corretor_da_nota_cheia_quando_o_relatorio_mostra_os_seis_sintomas() {
    let p = pontuar(&relatorio_com_tudo());
    assert_eq!(p.achados.len(), BUGS.len(), "perdidos: {:?}", p.perdidos);
    assert!(p.extras.is_empty(), "{:?}", p.extras);
    assert_eq!(p.cobertura(), 1.0);
}

#[test]
fn corretor_da_zero_a_relatorio_verde_e_nao_inventa_achado() {
    let verde = json!({ "falhas": [], "front": { "erros_console": [] },
        "backend": { "erros_5xx": [], "falhas_de_rede": [], "log_servidor": { "graves": [] } },
        "estado_no_backend": [{ "nome": "preferencias", "ok": true }, { "nome": "tarefas validas", "ok": true }] });
    let p = pontuar(&verde);
    assert!(p.achados.is_empty());
    assert_eq!(p.perdidos.len(), BUGS.len());
    assert_eq!(p.cobertura(), 0.0);
    // relatorio sem os campos (rodada que caiu antes de gravar) tambem nao explode nem pontua
    assert!(pontuar(&json!({})).achados.is_empty());
}

#[test]
fn corretor_separa_falso_positivo_do_defeito_plantado() {
    let mut r = relatorio_com_tudo();
    r["falhas"].as_array_mut().unwrap().push(json!("sonda front (depois)"));
    let p = pontuar(&r);
    assert_eq!(p.extras, vec!["sonda front (depois)".to_string()]);
}

#[test]
fn defeito_so_conta_quando_o_sintoma_certo_aparece() {
    // 5xx em outra rota nao e o B1; 4xx de favicon nao e achado
    let r = json!({ "falhas": ["backend 5xx: 500 http://x/api/outra"],
        "backend": { "erros_5xx": ["500 http://x/api/outra"], "erros_4xx": ["404 http://x/favicon.ico"] } });
    let p = pontuar(&r);
    assert!(p.achados.is_empty(), "{:?}", p.achados);
    assert_eq!(p.extras.len(), 1);
}

fn passo(n: usize, modelo: &str, tokens: u64, efeito: serde_json::Value) -> String {
    json!({ "n": n, "modelo": modelo, "tokens": tokens, "efeito": efeito }).to_string() + "\n"
}

#[test]
fn metricas_contam_passos_tokens_chamadas_e_o_passo_em_que_os_tres_visiveis_apareceram() {
    let jsonl = [
        passo(1, "gpt", 100, json!({})),
        passo(2, "gpt", 100, json!({ "erros_5xx": ["5xx /api/exportar"] })),
        passo(3, "politica-ucb", 0, json!({ "falhas": ["rede: net::ERR_EMPTY_RESPONSE"] })),
        passo(4, "fallback-deterministico", 0, json!({})),
        passo(5, "gpt", 50, json!({ "falhas": ["thread 'r' panicked at x"] })),
        passo(6, "gpt", 50, json!({})),
    ]
    .concat();
    let m = metricas(&jsonl);
    assert_eq!((m.passos, m.tokens, m.chamadas_llm), (6, 300, 4));
    assert_eq!(m.ate_visiveis, Some(5));
    assert_eq!(m.custo_em_passos(40), 5.0);
}

#[test]
fn metricas_de_rodada_que_nao_achou_tudo_pagam_o_limite_mais_cinquenta_por_cento() {
    let jsonl = passo(1, "gpt", 10, json!({ "erros_5xx": ["5xx /api/exportar"] }));
    let m = metricas(&jsonl);
    assert_eq!(m.ate_visiveis, None);
    assert_eq!(m.custo_em_passos(40), 60.0);
    // arquivo vazio ou com ruido nao explode
    assert_eq!(metricas("").passos, 0);
    assert_eq!(metricas("ruido\n{quebrado\n").passos, 0);
}

#[test]
fn mediana_e_resumo_dos_bracos() {
    assert_eq!(support::mediana(&[3.0, 1.0, 2.0]), 2.0);
    assert_eq!(support::mediana(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    assert_eq!(support::resumo(&[5.0, 1.0, 9.0]), (5.0, 1.0, 9.0));
    assert!(support::mediana(&[]).is_nan());
}
