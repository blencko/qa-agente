//! Fluxo completo dos passos 2 a 12 contra a aplicacao de teste, com Chrome de verdade.
//! Exige Chrome instalado: `cargo test -- --ignored`.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use qa_agente::avaliacao::EvaluationEngine;
use qa_agente::controlador::ExecutionController;
use qa_agente::evidencias::{consolidar, Evidencias};
use qa_agente::exploracao::descobrir_estados;
use qa_agente::gate_humano::{encaminhar, Encaminhamento};
use qa_agente::grafo_estados::VersionedStateGraph;
use qa_agente::modelos::{Acao, Gatilho, Orcamento, OrigemGatilho};
use qa_agente::navegador_chrome::ChromeNavegador;
use qa_agente::portas::Navegador;
use qa_agente::regressao::RegressionRunner;

fn url_da_aplicacao() -> String {
    let raiz = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
    format!("file:///{raiz}/fixtures/app.html")
}

#[test]
#[ignore = "exige Chrome instalado"]
fn jornada_completa_da_aplicacao_de_teste() {
    let url = url_da_aplicacao();
    let navegador = Rc::new(RefCell::new(ChromeNavegador::novo(None).expect("subir o Chrome")));

    // Passos 2 e 3: sem resultado anterior, o controlador manda executar.
    let gatilho = Gatilho {
        origem: OrigemGatilho::Deploy,
        versao: "v1".into(),
        impacto: BTreeSet::from(["login".to_string()]),
    };
    let decisao = ExecutionController::new(vec![], 1.0, None)
        .decidir(&gatilho, &Orcamento { limite: 10.0, gasto: 0.0 });
    assert_eq!(decisao.acao, Acao::Executar);

    // Passos 7 e 8: descobre a jornada inteira.
    let mut grafo = VersionedStateGraph::default();
    let cliques = descobrir_estados(&mut *navegador.borrow_mut(), &url, "v1", &mut grafo, 50).unwrap();
    assert_eq!(cliques, 5);
    assert_eq!(grafo.novas_em("v1").len(), 5);
    assert_eq!(grafo.destino("v1", "painel", "abrir_chat"), Some("chat"));

    // Passo 9: happy path de login ate o chat.
    let mut regressao = RegressionRunner::default();
    let nav = Rc::clone(&navegador);
    let alvo = url.clone();
    regressao.adicionar("login_ate_chat", move || {
        let mut n = nav.borrow_mut();
        n.abrir(&alvo)?;
        n.clicar("entrar")?;
        n.clicar("abrir_chat")?;
        match n.estado()?.as_str() {
            "chat" => Ok(()),
            outro => Err(format!("esperava chat, veio {outro}")),
        }
    });
    let caminhos = regressao.executar();
    assert!(caminhos.iter().all(|c| c.passou), "{caminhos:?}");

    // Passo 10: resposta do chat contra o gabarito de negocio.
    navegador.borrow_mut().clicar("perguntar").unwrap();
    let resposta = navegador.borrow_mut().texto("#resposta").unwrap();
    let mut avaliacao = EvaluationEngine::default();
    avaliacao.gabarito("saldo", |r| r.contains("R$ 10,00"));
    let vereditos = vec![avaliacao.avaliar("saldo", &resposta)];
    assert!(vereditos[0].passou, "resposta: {resposta}");

    // Passo 11: evidencia real em disco.
    let png = navegador.borrow_mut().captura().unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "a captura nao e um PNG");
    let pasta = format!("{}/target/qa-evidencias", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&pasta).unwrap();
    let arquivo = format!("{pasta}/chat-v1.png");
    std::fs::write(&arquivo, &png).unwrap();
    let relatorio = consolidar(
        "v1",
        &caminhos,
        &vereditos,
        Evidencias { screenshots: vec![arquivo], ..Evidencias::default() },
    );
    assert!(relatorio.passou);

    // Passo 12: sem opiniao do motor, o relatorio vai ao humano marcado como sem base.
    assert_eq!(encaminhar(&relatorio, None), Encaminhamento::HumanoSemBase);
}
