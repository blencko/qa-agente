//! Mede QUANTOS DEFEITOS o agente acha (nao quantos passos dá): sobe o app de `fixtures/bugs-plantados/` com seis
//! defeitos conhecidos, roda o binario `qa-agente` de verdade e cruza o `relatorio.json` com o catalogo.
//!
//! Dois testes, ambos `--ignored` (Chrome + `QA_OMNIROUTE_CHAVE` + COTA: so rode com ordem do Daniel):
//!
//! 1. `mede_quantos_defeitos_plantados_o_agente_acha`: uma medicao simples.
//!    Ambiente: QA_RODADAS (1), QA_LIMITE_PASSOS (40), QA_COBERTURA_MINIMA (0 = so mede).
//!
//! 2. `ab_a_politica_ajuda`: experimento A/B da politica aprendida.
//!    Bracos intercalados por rodada: A = desligada, B = so conselho, C = completa, A' = desligada de novo (mede o
//!    RUIDO do LLM: se C - A nao passa de A - A', nao ha efeito). Treino previo com a politica completa gera uma
//!    politica-base; cada rodada de medicao parte de uma COPIA dela, congelada (nao aprende), em servidor e pasta
//!    novos, com UM modelo so fixado em QA_OMNIROUTE_MODELOS (o rodizio de modelos confunde os bracos).
//!    Metrica primaria: passos ate os defeitos visiveis no rastro aparecerem (B1, B4, B5); secundarias: tokens,
//!    chamadas ao LLM, cobertura final de defeitos.
//!    Ambiente obrigatorio: QA_AUTORIZADO_PELO_DANIEL=1 e QA_MODELO_FIXO=<modelo>.
//!    Opcional: QA_AB_RODADAS (5, minimo 5), QA_AB_TREINO (3), QA_LIMITE_PASSOS (40).
//!    Usa as flags QA_POLITICA_MODO=completa|conselho|desligada e QA_POLITICA_CONGELADA=1 do binario.
//!
//! Roda: `cargo test --test medicao_bugs -- --ignored --nocapture <nome-do-teste>`.
//! Cada rodada grava uma linha em `qa-saida/medicao-bugs.jsonl` (simples) ou `qa-saida/medicao-ab.jsonl` (A/B).

mod support;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};
use support::{mediana, metricas, pontuar, resumo, Metricas, Servidor, BUGS, NOME_DO_ALVO};

fn numero(nome: &str, padrao: f64) -> f64 {
    std::env::var(nome).ok().and_then(|v| v.parse().ok()).unwrap_or(padrao)
}

fn acrescentar(arquivo: &str, linha: &Value) {
    let pasta = Path::new(env!("CARGO_MANIFEST_DIR")).join("qa-saida");
    std::fs::create_dir_all(&pasta).unwrap();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(pasta.join(arquivo)).unwrap();
    writeln!(f, "{linha}").unwrap();
}

struct Rodada {
    relatorio: Value,
    passos_jsonl: String,
    codigo: i32,
}

/// Uma rodada completa do agente contra um servidor novo, em pasta propria (qa-saida e politica nao se misturam com
/// as do usuario). `ambiente` = variaveis extras do binario (politica, modelo...). A pasta e apagada no fim.
fn rodar_uma(limite: usize, ambiente: &[(&str, String)]) -> Option<Rodada> {
    const TENTATIVAS: usize = 3;
    for t in 1..=TENTATIVAS {
        match tentar_uma(limite, ambiente) {
            Ok(r) => return Some(r),
            Err(e) => {
                let curto: String = e.chars().take(300).collect();
                println!("  rodada invalida (falha do agente/gateway, tentativa {t}/{TENTATIVAS}): {curto}");
                if t < TENTATIVAS {
                    std::thread::sleep(std::time::Duration::from_secs(20));
                }
            }
        }
    }
    None
}

/// Uma tentativa. `Err` = o AGENTE falhou (codigo 2: gateway fora, cadeia esgotada...), nao a app: nao e medida.
fn tentar_uma(limite: usize, ambiente: &[(&str, String)]) -> Result<Rodada, String> {
    static CONTADOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = CONTADOR.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let trabalho = std::env::temp_dir().join(format!("qa-medicao-{}-{n}", std::process::id()));
    let srv = Servidor::iniciar(&trabalho);
    let alvo = trabalho.join("alvo.json");
    std::fs::write(&alvo, srv.alvo_json(limite)).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_qa-agente"));
    cmd.arg(&alvo).current_dir(&trabalho);
    for (k, v) in ambiente {
        cmd.env(k, v);
    }
    let saida = cmd.output().expect("executar qa-agente");
    let codigo = saida.status.code().unwrap_or(-1);
    let pasta = trabalho.join("qa-saida").join(NOME_DO_ALVO);
    let resultado = if codigo == 2 {
        Err(String::from_utf8_lossy(&saida.stderr).to_string())
    } else {
        std::fs::read_to_string(pasta.join("relatorio.json"))
            .map_err(|e| format!("relatorio.json: {e}"))
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
            .map(|relatorio| Rodada {
                relatorio,
                passos_jsonl: std::fs::read_to_string(pasta.join("passos.jsonl")).unwrap_or_default(),
                codigo,
            })
    };
    drop(srv);
    let _ = std::fs::remove_dir_all(&trabalho);
    resultado
}

#[test]
#[ignore = "exige Chrome e QA_OMNIROUTE_CHAVE; gasta tokens"]
fn mede_quantos_defeitos_plantados_o_agente_acha() {
    assert!(std::env::var("QA_OMNIROUTE_CHAVE").is_ok(), "defina QA_OMNIROUTE_CHAVE");
    let rodadas = numero("QA_RODADAS", 1.0) as usize;
    let limite = numero("QA_LIMITE_PASSOS", 40.0) as usize;
    let minimo = numero("QA_COBERTURA_MINIMA", 0.0);

    let mut soma = 0.0;
    for n in 1..=rodadas {
        let politica = std::env::temp_dir().join(format!("qa-politica-medicao-{}-{n}.json", std::process::id()));
        let r = rodar_uma(limite, &[("QA_POLITICA", politica.display().to_string())]).expect("o agente falhou nas 3 tentativas");
        let _ = std::fs::remove_file(&politica);
        let p = pontuar(&r.relatorio);
        soma += p.cobertura();
        let m = metricas(&r.passos_jsonl);

        println!(
            "\n== rodada {n}/{rodadas}: {}/{} defeitos (codigo de saida {}, {} passos, parou: {})",
            p.achados.len(),
            BUGS.len(),
            r.codigo,
            m.passos,
            r.relatorio["parou_por"]
        );
        for b in &BUGS {
            let marca = if p.achados.contains(&b.id) { "ACHOU  " } else { "perdeu " };
            println!("  {marca} {} - {}", b.id, b.descricao);
        }
        for e in &p.extras {
            println!("  extra (nao e defeito plantado): {e}");
        }
        acrescentar(
            "medicao-bugs.jsonl",
            &json!({
                "rodada": n, "cobertura": p.cobertura(), "achados": p.achados, "perdidos": p.perdidos,
                "extras": p.extras, "codigo": r.codigo, "passos": m.passos, "ate_visiveis": m.ate_visiveis,
                "tokens": m.tokens, "chamadas_llm": m.chamadas_llm, "limite_passos": limite,
            }),
        );
    }
    let media = soma / rodadas as f64;
    println!("\ncobertura media: {:.0}% em {rodadas} rodada(s)", media * 100.0);
    assert!(media >= minimo, "cobertura {media:.2} abaixo do minimo {minimo:.2}");
}

const BRACOS: [(&str, &str); 4] = [("A", "desligada"), ("B", "conselho"), ("C", "completa"), ("A'", "desligada")];

#[derive(Default)]
struct Coleta {
    custo: Vec<f64>,
    tokens: Vec<f64>,
    chamadas: Vec<f64>,
    cobertura: Vec<f64>,
    /// Rodadas em que o AGENTE falhou (gateway) mesmo com 3 tentativas: fora das medianas, mas contadas.
    invalidas: usize,
}

#[test]
#[ignore = "exige Chrome, QA_OMNIROUTE_CHAVE, QA_AUTORIZADO_PELO_DANIEL=1 e QA_MODELO_FIXO; gasta cota"]
fn ab_a_politica_ajuda() {
    assert!(std::env::var("QA_OMNIROUTE_CHAVE").is_ok(), "defina QA_OMNIROUTE_CHAVE");
    assert_eq!(
        std::env::var("QA_AUTORIZADO_PELO_DANIEL").as_deref(),
        Ok("1"),
        "este experimento gasta cota: so com ordem explicita do Daniel (QA_AUTORIZADO_PELO_DANIEL=1)"
    );
    let modelo = std::env::var("QA_MODELO_FIXO").expect("defina QA_MODELO_FIXO: um modelo so, o rodizio confunde os bracos");
    assert!(!modelo.contains(','), "QA_MODELO_FIXO deve ser UM modelo, nao uma lista: {modelo}");
    let rodadas = numero("QA_AB_RODADAS", 5.0) as usize;
    assert!(rodadas >= 5, "menos de 5 rodadas por braco nao distingue efeito de ruido");
    let treino = numero("QA_AB_TREINO", 3.0) as usize;
    let limite = numero("QA_LIMITE_PASSOS", 40.0) as usize;

    // Treino: politica completa, aprendendo, acumulando numa politica-base.
    let base: PathBuf = std::env::temp_dir().join(format!("qa-politica-base-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&base);
    for i in 1..=treino {
        let Some(r) = rodar_uma(
            limite,
            &[
                ("QA_POLITICA", base.display().to_string()),
                ("QA_POLITICA_MODO", "completa".into()),
                ("QA_OMNIROUTE_MODELOS", modelo.clone()),
            ],
        ) else {
            println!("treino {i}/{treino}: PULADO (agente falhou nas 3 tentativas)");
            continue;
        };
        println!("treino {i}/{treino}: {} passos, parou: {}", metricas(&r.passos_jsonl).passos, r.relatorio["parou_por"]);
    }
    assert!(base.exists(), "o treino nao gravou a politica-base em {}", base.display());

    // Medicao: bracos intercalados, cada rodada com uma copia nova e congelada da base.
    let mut coleta: Vec<Coleta> = BRACOS.iter().map(|_| Coleta::default()).collect();
    for rodada in 1..=rodadas {
        for (i, (nome, modo)) in BRACOS.iter().enumerate() {
            let copia = std::env::temp_dir().join(format!("qa-politica-ab-{}-{rodada}-{i}.json", std::process::id()));
            std::fs::copy(&base, &copia).expect("copiar a politica-base");
            let r = rodar_uma(
                limite,
                &[
                    ("QA_POLITICA", copia.display().to_string()),
                    ("QA_POLITICA_MODO", (*modo).into()),
                    ("QA_POLITICA_CONGELADA", "1".into()),
                    ("QA_OMNIROUTE_MODELOS", modelo.clone()),
                ],
            );
            let _ = std::fs::remove_file(&copia);
            let Some(r) = r else {
                println!("rodada {rodada}/{rodadas} braco {nome:<2} ({modo:<9}): INVALIDA (agente falhou nas 3 tentativas), fora das medianas");
                coleta[i].invalidas += 1;
                acrescentar("medicao-ab.jsonl", &json!({ "rodada": rodada, "braco": nome, "modo": modo, "invalida": true }));
                continue;
            };
            let m: Metricas = metricas(&r.passos_jsonl);
            let p = pontuar(&r.relatorio);
            println!(
                "rodada {rodada}/{rodadas} braco {nome:<2} ({modo:<9}): visiveis em {:>3} passos, {} tokens, {} chamadas LLM, {}/{} defeitos",
                m.ate_visiveis.map_or("-".to_string(), |x| x.to_string()),
                m.tokens,
                m.chamadas_llm,
                p.achados.len(),
                BUGS.len()
            );
            let c = &mut coleta[i];
            c.custo.push(m.custo_em_passos(limite));
            c.tokens.push(m.tokens as f64);
            c.chamadas.push(m.chamadas_llm as f64);
            c.cobertura.push(p.cobertura());
            acrescentar(
                "medicao-ab.jsonl",
                &json!({
                    "rodada": rodada, "braco": nome, "modo": modo, "modelo": modelo, "limite_passos": limite,
                    "passos": m.passos, "ate_visiveis": m.ate_visiveis, "tokens": m.tokens,
                    "chamadas_llm": m.chamadas_llm, "cobertura": p.cobertura(), "extras": p.extras,
                }),
            );
        }
    }
    let _ = std::fs::remove_file(&base);

    println!("\n== resumo por braco (mediana [min-max]), {rodadas} rodadas, modelo {modelo}, politica de {treino} rodadas de treino");
    for (i, (nome, modo)) in BRACOS.iter().enumerate() {
        let c = &coleta[i];
        let f = |v: &[f64]| {
            let (m, lo, hi) = resumo(v);
            format!("{m:.1} [{lo:.0}-{hi:.0}]")
        };
        println!(
            "  {nome:<2} {modo:<9} validas {} invalidas {} | passos ate os visiveis {:<14} tokens {:<22} chamadas LLM {:<14} cobertura {}",
            c.custo.len(),
            c.invalidas,
            f(&c.custo),
            f(&c.tokens),
            f(&c.chamadas),
            f(&c.cobertura.iter().map(|x| x * 100.0).collect::<Vec<_>>())
        );
    }
    let (a, b, c, a2) = (mediana(&coleta[0].custo), mediana(&coleta[1].custo), mediana(&coleta[2].custo), mediana(&coleta[3].custo));
    if [a, b, c, a2].iter().any(|x| x.is_nan()) {
        println!("\nSEM VEREDITO: algum braco ficou sem nenhuma rodada valida (gateway fora?). Veja as invalidas acima.");
        return;
    }
    let ruido = (a - a2).abs();
    println!("\nruido do LLM (|A - A'|): {ruido:.1} passos");
    for (nome, v) in [("B (conselho)", b), ("C (completa)", c)] {
        let ganho = (a + a2) / 2.0 - v;
        let veredito = if ganho.abs() <= ruido { "DENTRO DO RUIDO: nao prova efeito" } else if ganho > 0.0 { "AJUDOU" } else { "ATRAPALHOU" };
        println!("  {nome}: ganho de {ganho:+.1} passos sobre o controle -> {veredito}");
    }
}
