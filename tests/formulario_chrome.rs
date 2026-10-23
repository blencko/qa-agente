//! O modo generico enxerga e opera campo com label, caixa de marcar e lista de selecao, de verdade, no Chrome.
//! Exige Chrome instalado: `cargo test --test formulario_chrome -- --ignored`.

use qa_agente::navegador_chrome::{ChromeNavegador, EVITAR_PADRAO};
use qa_agente::portas::Navegador;

fn url() -> String {
    let raiz = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
    format!("file:///{raiz}/fixtures/formulario.html")
}

#[test]
#[ignore = "exige Chrome instalado"]
fn modo_generico_preenche_marca_e_seleciona() {
    let mut nav = ChromeNavegador::novo_generico(None, EVITAR_PADRAO).expect("subir o Chrome");
    nav.abrir(&url()).unwrap();

    let opcoes = nav.clicaveis().unwrap();
    assert!(opcoes.contains(&"Preencher: Nome do personagem".to_string()), "{opcoes:?}");
    assert!(opcoes.contains(&"Marcar: Aceito os termos".to_string()), "{opcoes:?}");
    assert!(opcoes.contains(&"Selecionar: Classe = Mago".to_string()), "{opcoes:?}");

    assert_eq!(nav.texto("#resumo").unwrap(), "nome=;termos=false;classe=;especie=");
    nav.clicar("Preencher: Nome do personagem = Aria").unwrap();
    nav.clicar("Marcar: Aceito os termos").unwrap();
    nav.clicar("Selecionar: Classe = Mago").unwrap();
    // o select dentro de um label: o rotulo e so o texto do label, sem as opcoes (bug visto no jogo)
    assert!(opcoes.contains(&"Selecionar: Especie — = Anão".to_string()), "{opcoes:?}");
    nav.clicar("Selecionar: Especie — = Anão").unwrap();
    assert_eq!(nav.texto("#resumo").unwrap(), "nome=Aria;termos=true;classe=mago;especie=anao");

    // clicar de novo na caixa desmarca: o efeito e real, nao so um clique sem consequencia
    nav.clicar("Marcar: Aceito os termos").unwrap();
    assert_eq!(nav.texto("#resumo").unwrap(), "nome=Aria;termos=false;classe=mago;especie=anao");
}

#[test]
#[ignore = "exige Chrome instalado"]
fn banner_de_cookies_e_aceito_uma_vez_ao_abrir_e_nao_vira_opcao_do_agente() {
    let raiz = env!("CARGO_MANIFEST_DIR").replace('\\', "/");
    let mut nav = ChromeNavegador::novo_generico(None, EVITAR_PADRAO).expect("subir o Chrome");
    nav.abrir(&format!("file:///{raiz}/fixtures/banner.html")).unwrap();
    let opcoes = nav.clicaveis().unwrap();
    assert!(opcoes.contains(&"Entrar na campanha".to_string()), "{opcoes:?}");
    assert!(!opcoes.iter().any(|o| o.to_lowercase().contains("cookie")), "banner ainda visivel: {opcoes:?}");
}
