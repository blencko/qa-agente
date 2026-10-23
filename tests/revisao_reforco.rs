//! Sondas da revisao do reforco e do aprendizado. Cada teste afirma o comportamento CORRETO e esta marcado
//! `#[ignore]` enquanto a lacuna existir: `cargo test --test revisao_reforco -- --ignored` mostra quais ainda falham.
//! Quando a lacuna for corrigida, remova o `#[ignore]` do teste (ele passa a proteger a correcao).

use qa_agente::aprendizado::{recompensa, Memoria, Politica, Transicao};
use qa_agente::efeito::Efeito;

fn passo(tela: &str, acao: &str, efeito: Efeito) -> Transicao {
    Transicao { origem: tela.into(), acao: acao.into(), destino: tela.into(), efeito, ..Default::default() }
}

#[test]
fn ucb_sem_experiencia_nao_escolhe_a_ultima_opcao_da_lista() {
    let pol = Politica::nova();
    let opcoes: Vec<String> = ["Aprovar", "Reprovar", "Gerenciar cookies"].iter().map(|s| s.to_string()).collect();
    let escolhida = pol.melhor_por_ucb("tela nunca vista", &opcoes).unwrap();
    assert_ne!(escolhida, "Gerenciar cookies", "sem dado nenhum, a politica foi para a ultima opcao");
}

#[test]
fn aprovar_um_segundo_caso_diferente_nao_e_punido() {
    let aprovou = Efeito {
        endpoints: vec!["2xx POST /api/casos/aprovar/{id}".into()],
        escritas: vec!["POST /api/casos/aprovar/{id}".into()],
        ..Default::default()
    };
    let mut mem = Memoria::default();
    let primeira = recompensa(&passo("/painel/casos/|Casos", "Aprovar", aprovou.clone()), &mut mem);
    let segunda = recompensa(&passo("/painel/casos/|Casos", "Aprovar", aprovou), &mut mem);
    assert!(primeira > 0.0, "a primeira aprovacao rende: {primeira}");
    assert!(segunda >= 0.0, "a segunda e outro caso (outro id): nao pode ser punida, valeu {segunda}");
}

#[test]
fn primeiro_clique_que_abre_um_widget_nao_e_punido_como_parado() {
    let mut mem = Memoria::default();
    let r = recompensa(&passo("/painel/casos/|Casos", "Selecionar período de Data", Efeito::default()), &mut mem);
    assert!(r >= 0.0, "abrir o seletor de datas e necessario para o historico, e valeu {r}");
}

#[test]
fn duas_rodadas_que_salvam_na_mesma_politica_nao_perdem_o_aprendizado_uma_da_outra() {
    let arquivo = std::env::temp_dir().join(format!("qa-revisao-politica-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&arquivo);
    Politica::nova().salvar(&arquivo).unwrap();
    let (a, b) = (Politica::carregar(&arquivo).unwrap(), Politica::carregar(&arquivo).unwrap());
    let t = |acao: &str| Transicao {
        origem: "a / 1 X".into(),
        acao: acao.into(),
        destino: "a / 2 Y".into(),
        ..Default::default()
    };
    a.aprender(&t("ir pelo caminho A"), 2.0);
    b.aprender(&t("ir pelo caminho B"), 2.0);
    a.salvar(&arquivo).unwrap();
    b.salvar(&arquivo).unwrap();
    let final_ = Politica::carregar(&arquivo).unwrap();
    let _ = std::fs::remove_file(&arquivo);
    assert!(final_.celula("a / 1 X", "ir pelo caminho A").is_some(), "o aprendizado da rodada A foi sobrescrito");
    assert!(final_.celula("a / 1 X", "ir pelo caminho B").is_some());
}

#[test]
fn contagem_de_telas_inclui_as_telas_de_destino_vistas() {
    let pol = Politica::nova();
    pol.aprender(
        &Transicao { origem: "a / 1 X".into(), acao: "ir".into(), destino: "a / 2 Y".into(), ..Default::default() },
        1.0,
    );
    assert_eq!(pol.estados(), 2, "a politica viu duas telas (origem e destino) mas conta {}", pol.estados());
}

#[test]
fn rename_sobre_arquivo_aberto_por_outro_handle_do_proprio_processo_funciona_no_windows() {
    // Mede a hipotese do carrasco (rename atomico falharia com o destino aberto): vale para handles abertos pelo
    // proprio Rust (`File::open` compartilha leitura, escrita e delecao); outro programa sem esse compartilhamento pode falhar.
    let base = std::env::temp_dir().join(format!("qa-revisao-rename-{}", std::process::id()));
    let (alvo, tmp) = (base.with_extension("json"), base.with_extension("tmp"));
    std::fs::write(&alvo, "antigo").unwrap();
    let aberto = std::fs::File::open(&alvo).unwrap();
    std::fs::write(&tmp, "novo").unwrap();
    let resultado = std::fs::rename(&tmp, &alvo);
    drop(aberto);
    let conteudo = std::fs::read_to_string(&alvo).unwrap_or_default();
    let _ = (std::fs::remove_file(&alvo), std::fs::remove_file(&tmp));
    println!("rename com destino aberto: {resultado:?}; conteudo depois: {conteudo:?}");
    assert!(resultado.is_ok(), "o rename falhou com destino aberto por outro handle: {resultado:?}");
}

#[test]
fn texto_real_do_erro_de_conexao_recusada_do_gateway() {
    use qa_agente::omniroute::{eh_falha_do_gateway, saude_do_gateway};
    let e = saude_do_gateway("http://127.0.0.1:1", "k", std::time::Duration::from_secs(2)).unwrap_err();
    println!("erro real: {e}");
    assert!(eh_falha_do_gateway(&e), "nao reconhecido como falha do gateway: {e}");
}
