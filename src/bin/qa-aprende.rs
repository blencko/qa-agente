//! Treina a politica (Q-learning) com os logs de execucoes anteriores e mostra o que ela aprendeu.
//! Uso: qa-aprende [--politica qa-politica.json] [--epocas 30] [--banco] <log> [<log> ...]
//! Cada log e uma execucao: `passos.jsonl` ou a saida de texto do teste/rastro. `--banco` espelha a tabela Q em
//! `qa_agente.politica` (banco do RAG). Rodar de novo acumula: a politica salva e o ponto de partida.

use std::path::PathBuf;
use std::process::ExitCode;

use qa_agente::aprendizado::{ler_transicoes, Politica};
use qa_agente::rastro::{lit, psql};

fn main() -> ExitCode {
    let mut politica = PathBuf::from("qa-politica.json");
    let mut epocas = 30usize;
    let mut banco = false;
    let mut logs: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--politica" => politica = PathBuf::from(args.next().unwrap_or_default()),
            "--epocas" => epocas = args.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--banco" => banco = true,
            _ => logs.push(PathBuf::from(a)),
        }
    }
    if logs.is_empty() {
        eprintln!("uso: qa-aprende [--politica arq.json] [--epocas N] [--banco] <log> [<log> ...]");
        return ExitCode::from(2);
    }
    let pol = match Politica::carregar(&politica) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let mut execucoes = Vec::new();
    for caminho in &logs {
        match std::fs::read(caminho) {
            Ok(bytes) => {
                let ts = ler_transicoes(&String::from_utf8_lossy(&bytes));
                println!("{}: {} transicoes", caminho.display(), ts.len());
                if !ts.is_empty() {
                    execucoes.push(ts);
                }
            }
            Err(e) => eprintln!("{}: {e}", caminho.display()),
        }
    }
    let usadas = pol.treinar(&execucoes, epocas);
    println!("treino: {usadas} transicoes x {epocas} epocas -> {} telas, {} pares tela-acao", pol.estados(), pol.entradas());
    if let Err(e) = pol.salvar(&politica) {
        eprintln!("nao salvou a politica: {e}");
        return ExitCode::from(2);
    }
    let mut linhas = pol.linhas();
    linhas.sort_by(|a, b| b.2.q.partial_cmp(&a.2.q).unwrap_or(std::cmp::Ordering::Equal));
    let mostra = |(s, a, c): &(String, String, qa_agente::aprendizado::Celula)| {
        let tela: String = s.chars().rev().take(55).collect::<Vec<_>>().into_iter().rev().collect();
        println!("  {:+.2} (n={:<3}) ...{tela}  --[{a}]", c.q, c.n);
    };
    println!("melhores:");
    linhas.iter().filter(|l| l.2.n >= 2).take(8).for_each(mostra);
    println!("piores:");
    linhas.iter().rev().filter(|l| l.2.n >= 2).take(8).for_each(mostra);

    if banco {
        let mut sql = String::from(
            "CREATE SCHEMA IF NOT EXISTS qa_agente;\n\
             CREATE TABLE IF NOT EXISTS qa_agente.politica (tela text NOT NULL, acao text NOT NULL, q double precision NOT NULL, \
             n integer NOT NULL, atualizada timestamptz NOT NULL DEFAULT now(), PRIMARY KEY (tela, acao));\n",
        );
        for (s, a, c) in &linhas {
            sql.push_str(&format!(
                "INSERT INTO qa_agente.politica (tela,acao,q,n) VALUES ({},{},{},{}) ON CONFLICT (tela,acao) DO UPDATE \
                 SET q=EXCLUDED.q, n=EXCLUDED.n, atualizada=now();\n",
                lit(s),
                lit(a),
                c.q,
                c.n
            ));
        }
        match psql(&sql) {
            Ok(()) => println!("espelhado em qa_agente.politica ({} linhas)", linhas.len()),
            Err(e) => {
                eprintln!("banco: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}
