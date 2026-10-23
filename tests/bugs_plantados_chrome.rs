//! Prova, no Chrome de verdade e SEM LLM, que o navegador do agente enxerga os sintomas plantados: um roteiro fixo
//! clica em cada gatilho e a `Observacao` precisa mostrar o que o relatorio vai mostrar.
//! Exige Chrome instalado: `cargo test --test bugs_plantados_chrome -- --ignored`.

mod support;

use qa_agente::navegador_chrome::{ChromeNavegador, EVITAR_PADRAO};
use qa_agente::portas::Navegador;
use support::Servidor;

fn clicar_rotulo(nav: &mut ChromeNavegador, contem: &str) {
    let opcoes = nav.clicaveis().unwrap();
    let alvo = opcoes.iter().find(|o| o.contains(contem)).unwrap_or_else(|| panic!("sem {contem:?} em {opcoes:?}")).clone();
    nav.clicar(&alvo).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
}

fn subir(nome: &str) -> (Servidor, ChromeNavegador) {
    let srv = Servidor::iniciar(&std::env::temp_dir().join(format!("qa-bugs-{nome}-{}", std::process::id())));
    let nav = ChromeNavegador::novo_generico(None, EVITAR_PADRAO).expect("subir o Chrome");
    (srv, nav)
}

#[test]
#[ignore = "exige Chrome instalado"]
fn o_navegador_ve_5xx_e_requisicao_sem_resposta_e_o_panico_fica_no_log() {
    let (srv, mut nav) = subir("rede");

    nav.abrir(&format!("{}/", srv.url)).unwrap();
    clicar_rotulo(&mut nav, "Sincronizar");
    assert_eq!(nav.texto("#msg").unwrap(), "Sincronizado", "a tela mente de proposito (B4)");

    nav.abrir(&format!("{}/config", srv.url)).unwrap();
    clicar_rotulo(&mut nav, "Exportar");
    assert_eq!(nav.texto("#msg").unwrap(), "Exportado com sucesso", "a tela mente de proposito (B1)");

    nav.abrir(&format!("{}/relatorios", srv.url)).unwrap();
    clicar_rotulo(&mut nav, "Gerar resumo");

    let r = nav.observacao().resumo();
    assert!(r.erros_5xx.iter().any(|e| e.contains("/api/exportar")), "{r:?}");
    assert!(r.falhas_de_rede.iter().any(|e| e.contains("/api/sync") && e.contains("ERR_EMPTY_RESPONSE")), "{r:?}");
    assert!(r.erros_4xx.iter().any(|e| e.contains("favicon")), "o 404 de controle e informativo: {r:?}");
    let log = std::fs::read_to_string(&srv.log).unwrap();
    assert!(log.contains("panicked"), "{log}");
}

/// Excecao JavaScript nao tratada (B6) chega a `erros_console` com a MENSAGEM real (antes da correcao do navegador, em
/// 23/10/2026, nao chegava: so o dominio Log era ligado). Regressao dessa lacuna.
#[test]
#[ignore = "exige Chrome instalado"]
fn excecao_js_nao_tratada_chega_ao_relatorio_com_a_mensagem() {
    let (srv, mut nav) = subir("console");
    nav.abrir(&format!("{}/", srv.url)).unwrap();
    clicar_rotulo(&mut nav, "Ajuda");
    let r = nav.observacao().resumo();
    assert!(r.erros_console.iter().any(|e| e.contains("TypeError")), "{r:?}");
}
