//! Chamada real ao gateway OmniRoute. Exige `QA_OMNIROUTE_CHAVE` e o gateway no ar: `cargo test -- --ignored`.

use qa_agente::agente_llm::AgenteLlm;
use qa_agente::omniroute::{ErroGeracao, Gerador, OmniRoute};
use qa_agente::portas::AgenteQA;

#[test]
#[ignore = "exige o OmniRoute no ar e QA_OMNIROUTE_CHAVE"]
fn agente_escolhe_uma_opcao_real_pela_cadeia_de_modelos() {
    let gateway = OmniRoute::do_ambiente().expect("configuracao do OmniRoute");
    match gateway.gerar("Responda so com a palavra OK.", "Diga OK.") {
        Ok(r) => println!("modelo que respondeu: {}", r.modelo),
        Err(ErroGeracao::CadeiaEsgotada { tentativas, cota }) => {
            panic!("cadeia esgotada (cota: {cota}): {tentativas:?}")
        }
        Err(outro) => panic!("{outro:?}"),
    }

    let agente = AgenteLlm::new(OmniRoute::do_ambiente().unwrap());
    let opcoes = vec!["Entrar".to_string(), "Ajuda".to_string(), "Sobre".to_string()];
    let escolha = agente
        .proxima_acao("entrar no jogo", "tela inicial", &opcoes, &[])
        .expect("o agente tem de devolver uma das opcoes");
    println!("acao escolhida: {escolha:?}");
    assert!(escolha.is_some_and(|a| opcoes.contains(&a)));
}
