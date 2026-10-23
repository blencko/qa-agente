//! Rastro de uma execucao: cada passo e registrado NA HORA, em tres lugares, para que parar a rodada no meio (cota,
//! queda, kill) nunca perca o que ela fez nem onde estava.
//!   1. stdout: uma linha por passo;
//!   2. `<pasta>/passos.jsonl` (acrescenta) e `<pasta>/checkpoint.json` (sobrescreve: onde parou);
//!   3. banco do RAG (`qa_agente.execucao` e `qa_agente.passo`, via `docker exec postgres-rag psql`), com upsert por passo.
//! O banco e melhor esforco: se o container nao responde, o rastro em arquivo segue e o aviso sai no stdout.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::exploracao::Passo;

const DDL: &str = "CREATE SCHEMA IF NOT EXISTS qa_agente;
CREATE TABLE IF NOT EXISTS qa_agente.execucao (
  id text PRIMARY KEY, alvo text NOT NULL, objetivo text NOT NULL,
  iniciada timestamptz NOT NULL DEFAULT now(), atualizada timestamptz NOT NULL DEFAULT now(),
  passos integer NOT NULL DEFAULT 0, tokens_total bigint NOT NULL DEFAULT 0,
  estado_final text NOT NULL DEFAULT '', encerrada boolean NOT NULL DEFAULT false, parou_por text NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS qa_agente.passo (
  execucao text NOT NULL REFERENCES qa_agente.execucao(id), n integer NOT NULL, ts timestamptz NOT NULL DEFAULT now(),
  origem text NOT NULL, acao text NOT NULL, destino text NOT NULL, ambigua boolean NOT NULL,
  modelo text, tokens integer, PRIMARY KEY (execucao, n));
ALTER TABLE qa_agente.passo ADD COLUMN IF NOT EXISTS efeito jsonb;
ALTER TABLE qa_agente.execucao ADD COLUMN IF NOT EXISTS erros_backend integer NOT NULL DEFAULT 0;";

/// Escapa texto para literal SQL entre aspas simples (sem NUL).
pub fn lit(texto: &str) -> String {
    format!("'{}'", texto.replace('\0', "").replace('\'', "''"))
}

/// SQL do upsert de um passo e do resumo da execucao.
pub fn sql_do_passo(id: &str, n: usize, p: &Passo, tokens_total: u64) -> String {
    let modelo = p.modelo.as_deref().map(lit).unwrap_or_else(|| "NULL".into());
    let tokens = p.tokens.map(|t| t.to_string()).unwrap_or_else(|| "NULL".into());
    format!(
        "INSERT INTO qa_agente.passo (execucao,n,origem,acao,destino,ambigua,modelo,tokens,efeito) VALUES ({id},{n},{o},{a},{d},{amb},{modelo},{tokens},{efeito}::jsonb) \
         ON CONFLICT (execucao,n) DO UPDATE SET origem=EXCLUDED.origem, acao=EXCLUDED.acao, destino=EXCLUDED.destino, efeito=EXCLUDED.efeito;\n\
         UPDATE qa_agente.execucao SET passos={n}, tokens_total={tokens_total}, estado_final={d}, erros_backend=erros_backend+{erros_back}, atualizada=now() WHERE id={id};",
        efeito = lit(&serde_json::to_string(&p.efeito).unwrap_or_else(|_| "{}".into())),
        erros_back = p.efeito.erros_5xx.len() + p.efeito.falhas.len(),
        id = lit(id),
        o = lit(&p.origem),
        a = lit(&p.acao),
        d = lit(&p.destino),
        amb = p.ambigua,
    )
}

pub struct Rastro {
    id: String,
    pasta: PathBuf,
    banco: bool,
    tokens_total: u64,
}

impl Rastro {
    /// Cria a pasta, registra a execucao no banco (se der) e devolve o rastro.
    pub fn iniciar(id: &str, alvo: &str, objetivo: &str, pasta: &Path) -> Rastro {
        let _ = std::fs::create_dir_all(pasta);
        let mut r = Rastro { id: id.to_string(), pasta: pasta.to_path_buf(), banco: true, tokens_total: 0 };
        let sql = format!(
            "{DDL}\nINSERT INTO qa_agente.execucao (id,alvo,objetivo) VALUES ({},{},{}) ON CONFLICT (id) DO NOTHING;",
            lit(id),
            lit(alvo),
            lit(objetivo)
        );
        if let Err(e) = psql(&sql) {
            println!("[rastro] banco indisponivel, seguindo so com arquivo: {e}");
            r.banco = false;
        }
        println!("[rastro] execucao {id} em {}", pasta.display());
        r
    }

    /// Registra um passo agora: stdout, jsonl, checkpoint e banco.
    pub fn passo(&mut self, n: usize, p: &Passo) {
        self.tokens_total += p.tokens.unwrap_or(0);
        println!(
            "[passo {n}] {} --[{}]--> {}{} | {} | {}t (total {}t) {}",
            p.origem,
            p.acao,
            p.destino,
            if p.origem == p.destino { " (sem mudanca)" } else { "" },
            p.modelo.as_deref().unwrap_or("-"),
            p.tokens.unwrap_or(0),
            self.tokens_total,
            p.efeito.resumo()
        );
        let _ = std::io::stdout().flush();
        let linha = serde_json::json!({
            "n": n, "origem": p.origem, "acao": p.acao, "destino": p.destino, "ambigua": p.ambigua,
            "modelo": p.modelo, "tokens": p.tokens, "efeito": p.efeito,
        });
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.pasta.join("passos.jsonl")) {
            let _ = writeln!(f, "{linha}");
        }
        self.checkpoint(n, &p.destino, &p.acao, false, "");
        if self.banco {
            if let Err(e) = psql(&sql_do_passo(&self.id, n, p, self.tokens_total)) {
                println!("[rastro] banco falhou no passo {n}: {e}");
            }
        }
    }

    /// Marca o fim (ou a parada) com o motivo. Chamado tambem em erro: o relatorio parcial e a regra.
    pub fn encerrar(&mut self, passos: usize, estado_final: &str, parou_por: &str) {
        println!("[rastro] encerrada apos {passos} passos: {parou_por}");
        self.checkpoint(passos, estado_final, "", true, parou_por);
        if self.banco {
            let sql = format!(
                "UPDATE qa_agente.execucao SET encerrada=true, parou_por={}, atualizada=now() WHERE id={};",
                lit(parou_por),
                lit(&self.id)
            );
            if let Err(e) = psql(&sql) {
                println!("[rastro] banco falhou ao encerrar: {e}");
            }
        }
    }

    fn checkpoint(&self, n: usize, estado: &str, ultima_acao: &str, encerrada: bool, parou_por: &str) {
        let v = serde_json::json!({
            "execucao": self.id, "passos": n, "estado_atual": estado, "ultima_acao": ultima_acao,
            "tokens_total": self.tokens_total, "encerrada": encerrada, "parou_por": parou_por,
        });
        let _ = std::fs::write(self.pasta.join("checkpoint.json"), v.to_string());
    }
}

/// Onde o espelho em banco grava: container, usuario e banco do Postgres. Variaveis `QA_RASTRO_CONTAINER`,
/// `QA_RASTRO_USUARIO` e `QA_RASTRO_BD`; `QA_RASTRO_BANCO=desligado` mantem so o rastro em arquivo.
pub fn destino_do_banco(ambiente: &dyn Fn(&str) -> Option<String>) -> Option<(String, String, String)> {
    if ambiente("QA_RASTRO_BANCO").is_some_and(|v| v.eq_ignore_ascii_case("desligado")) {
        return None;
    }
    let pega = |k: &str, padrao: &str| ambiente(k).filter(|v| !v.trim().is_empty()).unwrap_or_else(|| padrao.to_string());
    Some((pega("QA_RASTRO_CONTAINER", "postgres-rag"), pega("QA_RASTRO_USUARIO", "raguser"), pega("QA_RASTRO_BD", "rag_ecosystem")))
}

/// Executa SQL no Postgres do espelho (ver `destino_do_banco`), com a senha do proprio container.
pub fn psql(sql: &str) -> Result<(), String> {
    let (container, usuario, banco) =
        destino_do_banco(&|k| std::env::var(k).ok()).ok_or_else(|| "espelho em banco desligado (QA_RASTRO_BANCO)".to_string())?;
    let comando = format!("PGPASSWORD=\"$POSTGRES_PASSWORD\" psql -U {usuario} -d {banco} -v ON_ERROR_STOP=1 -q");
    let mut filho = Command::new("docker")
        .args(["exec", "-i", &container, "sh", "-c", &comando])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    filho.stdin.take().ok_or("sem stdin")?.write_all(sql.as_bytes()).map_err(|e| e.to_string())?;
    let saida = filho.wait_with_output().map_err(|e| e.to_string())?;
    if saida.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&saida.stderr).trim().chars().take(200).collect())
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn passo() -> Passo {
        Passo {
            origem: "a".into(),
            acao: "Preencher: nome = O'Brien".into(),
            destino: "b".into(),
            ambigua: false,
            modelo: Some("m".into()),
            tokens: Some(10),
            efeito: Default::default(),
        }
    }

    #[test]
    fn destino_do_banco_tem_padrao_aceita_troca_e_pode_ser_desligado() {
        let nada = |_: &str| None;
        assert_eq!(destino_do_banco(&nada), Some(("postgres-rag".into(), "raguser".into(), "rag_ecosystem".into())));
        let trocado = |k: &str| match k {
            "QA_RASTRO_CONTAINER" => Some("meu-pg".to_string()),
            "QA_RASTRO_BD" => Some("qa".to_string()),
            _ => None,
        };
        assert_eq!(destino_do_banco(&trocado), Some(("meu-pg".into(), "raguser".into(), "qa".into())));
        let desligado = |k: &str| (k == "QA_RASTRO_BANCO").then(|| "Desligado".to_string());
        assert_eq!(destino_do_banco(&desligado), None);
    }

    #[test]
    fn literal_sql_dobra_aspas_e_tira_nul() {
        assert_eq!(lit("O'Brien\0"), "'O''Brien'");
    }

    #[test]
    fn sql_do_passo_escapa_o_texto_e_atualiza_a_execucao() {
        let s = sql_do_passo("e1", 3, &passo(), 30);
        assert!(s.contains("'Preencher: nome = O''Brien'"), "{s}");
        assert!(s.contains("passos=3") && s.contains("tokens_total=30") && s.contains("ON CONFLICT"), "{s}");
    }

    #[test]
    fn passo_grava_jsonl_e_checkpoint_mesmo_sem_banco() {
        let pasta = std::env::temp_dir().join(format!("qa-rastro-teste-{}", std::process::id()));
        let mut r = Rastro { id: "t".into(), pasta: pasta.clone(), banco: false, tokens_total: 0 };
        std::fs::create_dir_all(&pasta).unwrap();
        r.passo(1, &passo());
        r.passo(2, &passo());
        r.encerrar(2, "b", "limite");
        let linhas = std::fs::read_to_string(pasta.join("passos.jsonl")).unwrap();
        assert_eq!(linhas.lines().count(), 2);
        let cp = std::fs::read_to_string(pasta.join("checkpoint.json")).unwrap();
        assert!(cp.contains("\"encerrada\":true") && cp.contains("\"passos\":2") && cp.contains("limite"), "{cp}");
        let _ = std::fs::remove_dir_all(&pasta);
    }
}
