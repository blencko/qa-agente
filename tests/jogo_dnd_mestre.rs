//! QA guiado por LLM (via OmniRoute) contra o jogo dnd-mestre, no ambiente E2E descartavel do proprio jogo.
//!
//! Pre-requisitos (nada disso toca a campanha real):
//!   1. `tools/jogo-ambiente.mjs` de pe: sobe banco dnd_teste/schema descartavel, narrador falso e login de teste;
//!   2. `QA_OMNIROUTE_CHAVE` definida; `QA_OBJETIVO` e opcional.
//! Rode com `cargo test --test jogo_dnd_mestre -- --ignored --nocapture`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use qa_agente::agente_llm::AgenteLlm;
use qa_agente::avaliacao::Veredito;
use qa_agente::controlador::ExecutionController;
use qa_agente::evidencias::{consolidar, Evidencias};
use qa_agente::exploracao::jornada_guiada_observada;
use qa_agente::rastro::Rastro;
use qa_agente::gate_humano::encaminhar;
use qa_agente::grafo_estados::VersionedStateGraph;
use qa_agente::modelos::{Gatilho, Orcamento, OrigemGatilho};
use qa_agente::motor::{ConsultaMotor, Motor};
use qa_agente::navegador_chrome::{ChromeNavegador, EVITAR_PADRAO};
use qa_agente::omniroute::OmniRoute;
use qa_agente::portas::Navegador;
use qa_agente::regressao::ResultadoCaminho;
use serde_json::Value;

fn estado_do_ambiente() -> PathBuf {
    // `E2E_BASE` (a mesma do ambiente do jogo) permite varias instancias em paralelo, cada uma com a sua pasta.
    match std::env::var("E2E_BASE") {
        Ok(base) => PathBuf::from(base).join("estado"),
        Err(_) => std::env::temp_dir().join("dnd-e2e").join("estado"),
    }
}

fn ler_json(nome: &str) -> Value {
    let caminho = estado_do_ambiente().join(nome);
    let texto = std::fs::read_to_string(&caminho)
        .unwrap_or_else(|e| panic!("sem {caminho:?} ({e}): suba tools/jogo-ambiente.mjs antes"));
    serde_json::from_str(&texto).expect("json do ambiente")
}

#[test]
#[ignore = "exige o ambiente E2E do jogo de pe e o OmniRoute"]
fn qa_guiado_pelo_llm_navega_o_jogo() {
    let estado = ler_json("estado.json");
    let url = estado["urlTela"].as_str().expect("urlTela").to_string();
    let sessao = ler_json("escuro.json");
    let cookie = &sessao["cookies"][0];

    let objetivo = std::env::var("QA_OBJETIVO").unwrap_or_else(|_| {
        "Conhecer as telas principais do jogo, abrindo cada area disponivel, sem criar, editar nem apagar nada".into()
    });

    // Passos 2 e 3, com o motor real como contraponto.
    let motor = Motor::default();
    let gatilho = Gatilho {
        origem: OrigemGatilho::Manual,
        versao: "jogo-teste".into(),
        impacto: BTreeSet::new(),
    };
    let decisao = ExecutionController::new(vec![], 1.0, Some(&motor as &dyn ConsultaMotor))
        .decidir(&gatilho, &Orcamento { limite: 10.0, gasto: 0.0 });
    println!("controlador: {:?} | motor: {:?} divergiu={} indisponivel={}",
        decisao.acao, decisao.opiniao_motor, decisao.divergiu_do_motor, decisao.motor_indisponivel);

    // Passos 4 a 8: agente guiado pelo OmniRoute, navegador generico, State Graph.
    let mut navegador = ChromeNavegador::novo_generico(None, EVITAR_PADRAO).expect("subir o Chrome");
    navegador
        .definir_cookie(
            cookie["name"].as_str().unwrap(),
            cookie["value"].as_str().unwrap(),
            cookie["domain"].as_str().unwrap(),
        )
        .expect("injetar a sessao de teste");
    let agente = AgenteLlm::new(OmniRoute::do_ambiente().expect("configuracao do OmniRoute"));
    let mut grafo = VersionedStateGraph::default();
    let limite: usize = std::env::var("QA_LIMITE_PASSOS").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
    let pasta_passos = format!("{}/target/qa-evidencias/jogo", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&pasta_passos).unwrap();
    // Rastro ao vivo: cada passo vai para stdout, passos.jsonl, checkpoint.json e o banco na hora em que acontece.
    let id = std::env::var("QA_EXECUCAO_ID").unwrap_or_else(|_| {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        format!("jogo-{s}")
    });
    let mut rastro = Rastro::iniciar(&id, "dnd-mestre", &objetivo, &PathBuf::from(&pasta_passos).join(&id));
    // Politica aprendida (Q-learning): experiencia no prompt, atualizada a cada passo, salva a cada 5 e no fim.
    let caminho_politica =
        PathBuf::from(std::env::var("QA_POLITICA").unwrap_or_else(|_| format!("{}/politicas/dnd-mestre.json", env!("CARGO_MANIFEST_DIR"))));
    let politica = qa_agente::aprendizado::Politica::carregar(&caminho_politica).unwrap_or_default();
    println!("[politica] {} telas, {} pares aprendidos em {}", politica.estados(), politica.entradas(), caminho_politica.display());
    let jornada = jornada_guiada_observada(
        &agente,
        &mut navegador,
        &url,
        &objetivo,
        "jogo-teste",
        &mut grafo,
        limite,
        &mut |n, nav| {
            let png = nav.captura()?;
            std::fs::write(format!("{pasta_passos}/{id}/passo-{n:02}.png"), png).map_err(|e| e.to_string())
        },
        &mut |n, p| {
            rastro.passo(n, p);
            if n % 5 == 0 {
                let _ = politica.salvar(&caminho_politica);
            }
        },
        Some(&politica),
        None,
        None,
        None,
    );
    let jornada = match jornada {
        Ok(j) => j,
        Err(e) => {
            // erro do navegador no meio: o rastro ja tem tudo ate aqui; so falta dizer que parou e por que
            rastro.encerrar(0, "", &format!("erro: {e}"));
            panic!("a jornada guiada falhou: {e}");
        }
    };
    let ultimo = jornada.passos.last().map(|p| p.destino.clone()).unwrap_or_default();
    rastro.encerrar(jornada.passos.len(), &ultimo, &jornada.parou_por);
    let _ = politica.salvar(&caminho_politica);
    let passos = jornada.passos;
    for p in &passos {
        println!("  {} --[{}]--> {}", p.origem, p.acao, p.destino);
    }

    // Passo 11: evidencia real.
    let png = navegador.captura().expect("captura");
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    let pasta = format!("{}/target/qa-evidencias", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&pasta).unwrap();
    let arquivo = format!("{pasta}/jogo-final.png");
    std::fs::write(&arquivo, &png).unwrap();

    let caminhos = vec![ResultadoCaminho {
        nome: "jornada_guiada".into(),
        passou: !passos.is_empty(),
        detalhe: format!("{} passos, {} transicoes novas", passos.len(), grafo.novas_em("jogo-teste").len()),
    }];
    let relatorio = consolidar(
        "jogo-teste",
        &caminhos,
        &Vec::<Veredito>::new(),
        Evidencias { screenshots: vec![arquivo], ..Evidencias::default() },
    );
    println!("relatorio: passou={} falhas={:?}", relatorio.passou, relatorio.falhas);

    // Passo 12: o motor decide o que chega ao humano e com que marca.
    let opiniao = motor.opiniao("decisao-de-execucao-qa", "geral", &["executar", "pular", "bloquear"], "executar").ok();
    println!("gate humano: {:?}", encaminhar(&relatorio, opiniao.as_ref()));

    assert!(!passos.is_empty(), "o agente nao deu nenhum passo");
    assert!(relatorio.passou);
}
