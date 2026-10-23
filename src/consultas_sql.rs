//! Consultas SQL SOMENTE-LEITURA ao banco do projeto, rodadas antes e depois da jornada: o relatorio diz o que mudou no dado
//! gravado e se cada criterio de aceite deixou o rastro esperado. Rodam dentro do container do banco (a senha vem do
//! proprio container e nunca passa por aqui); o SQL vai pela entrada padrao, sem aspas de shell.

use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq)]
pub struct ConsultaSql {
    pub nome: String,
    pub sql: String,
    pub container: String,
    pub banco: String,
    /// Trecho que o resultado DEPOIS da jornada precisa conter (vazio = so registra o antes e o depois).
    pub depois_contem: String,
    /// Trecho que precisa estar numa linha NOVA (presente depois e ausente antes). Ao contrario de `depois_contem`, nao
    /// e satisfeito por dado que ja existia antes da jornada.
    pub novo_contem: String,
    /// So exige `novo_contem` se alguma escrita da rodada (ex.: `POST /v1/x/reject/{id}`) contiver este trecho.
    /// Vazio = sempre exige.
    pub quando_escrita: String,
    /// A consulta lista violacoes de uma invariante: qualquer linha NOVA depois da jornada reprova.
    pub sem_novo: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultadoSql {
    pub nome: String,
    pub antes: String,
    pub depois: String,
    pub mudou: bool,
    pub ok: bool,
    pub detalhe: String,
}

const PROIBIDAS: [&str; 14] = [
    "insert", "update", "delete", "drop", "truncate", "alter", "create", "replace", "grant", "revoke", "call", "into outfile",
    "load_file", "sleep(",
];

/// So `SELECT` (ou `SHOW`/`DESCRIBE`), uma unica instrucao e nenhuma palavra de escrita. Recusa na duvida.
pub fn somente_leitura(sql: &str) -> Result<(), String> {
    let s = sql.trim().trim_end_matches(';').to_lowercase();
    if s.is_empty() {
        return Err("consulta vazia".into());
    }
    if s.contains(';') {
        return Err("so uma instrucao por consulta".into());
    }
    if !(s.starts_with("select ") || s.starts_with("show ") || s.starts_with("describe ")) {
        return Err("a consulta precisa comecar por SELECT, SHOW ou DESCRIBE".into());
    }
    let palavras: Vec<&str> = s.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '(')).collect();
    for p in PROIBIDAS {
        let achou = if p.contains(' ') || p.ends_with('(') { s.contains(p) } else { palavras.contains(&p) };
        if achou {
            return Err(format!("palavra de escrita na consulta: {p}"));
        }
    }
    Ok(())
}

/// Roda uma consulta e devolve o resultado em texto (colunas separadas por tab, sem cabecalho), no maximo 2000 caracteres.
pub fn executar(c: &ConsultaSql) -> Result<String, String> {
    somente_leitura(&c.sql)?;
    let comando = format!("mysql -uroot -p\"$MYSQL_ROOT_PASSWORD\" -N -B {} 2>&1 | grep -v 'Using a password'", c.banco);
    let mut filho = Command::new("docker")
        .args(["exec", "-i", &c.container, "sh", "-c", &comando])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    filho.stdin.take().ok_or("sem stdin")?.write_all(c.sql.as_bytes()).map_err(|e| e.to_string())?;
    let saida = filho.wait_with_output().map_err(|e| e.to_string())?;
    let texto = String::from_utf8_lossy(&saida.stdout).trim().to_string();
    if texto.starts_with("ERROR") {
        return Err(texto.chars().take(200).collect());
    }
    Ok(texto.chars().take(2000).collect())
}

/// Linhas presentes depois e ausentes antes (diferenca de multiconjuntos: uma linha repetida conta pelas copias novas).
pub fn linhas_novas(antes: &str, depois: &str) -> Vec<String> {
    let mut restantes: Vec<&str> = antes.lines().collect();
    let mut novas = Vec::new();
    for linha in depois.lines() {
        match restantes.iter().position(|a| *a == linha) {
            Some(i) => {
                restantes.swap_remove(i);
            }
            None => novas.push(linha.to_string()),
        }
    }
    novas
}

/// Junta o antes e o depois de uma consulta no veredito do criterio. `escritas` sao as requisicoes de escrita com 2xx
/// vistas na rodada inteira (decidem se `novo_contem` se aplica).
pub fn comparar(c: &ConsultaSql, antes: Result<String, String>, depois: Result<String, String>, escritas: &[String]) -> ResultadoSql {
    let (antes_txt, depois_txt) = (antes.clone().unwrap_or_else(|e| format!("(falhou: {e})")), depois.clone().unwrap_or_else(|e| format!("(falhou: {e})")));
    let mudou = antes_txt != depois_txt;
    let novas = linhas_novas(&antes_txt, &depois_txt);
    let quando = c.quando_escrita.to_lowercase();
    let aplicavel = quando.is_empty() || escritas.iter().any(|e| e.to_lowercase().contains(&quando));
    let (ok, detalhe) = match (&antes, &depois) {
        (Err(e), _) | (_, Err(e)) => (false, format!("consulta falhou: {e}")),
        (_, Ok(d)) if !c.depois_contem.is_empty() && !d.contains(&c.depois_contem) => {
            (false, format!("esperava {:?} no resultado depois da jornada; veio {:?}", c.depois_contem, resumo(d)))
        }
        (_, Ok(_)) if c.sem_novo && !novas.is_empty() => {
            (false, format!("apareceram {} linha(s) que violam a invariante: {:?}", novas.len(), resumo(&novas.join("\n"))))
        }
        (_, Ok(_)) if !c.novo_contem.is_empty() && aplicavel && !novas.iter().any(|l| l.contains(&c.novo_contem)) => (
            false,
            format!(
                "a escrita {:?} aconteceu, mas nenhuma linha NOVA contem {:?}; linhas novas: {:?}",
                c.quando_escrita,
                c.novo_contem,
                resumo(&novas.join("\n"))
            ),
        ),
        (_, Ok(d)) => {
            let nota = if !c.novo_contem.is_empty() && !aplicavel { format!(" [nao exercitado: sem escrita {:?} na rodada]", c.quando_escrita) } else { String::new() };
            (true, format!("antes {:?} -> depois {:?}{}{nota}", resumo(&antes_txt), resumo(d), if mudou { " (mudou)" } else { " (igual)" }))
        }
    };
    ResultadoSql { nome: c.nome.clone(), antes: antes_txt, depois: depois_txt, mudou, ok, detalhe }
}

fn resumo(t: &str) -> String {
    t.replace('\n', " | ").replace('\t', " ").chars().take(160).collect()
}

#[cfg(test)]
mod testes {
    use super::*;

    fn consulta(sql: &str, contem: &str) -> ConsultaSql {
        ConsultaSql {
            nome: "n".into(),
            sql: sql.into(),
            container: "c".into(),
            banco: "b".into(),
            depois_contem: contem.into(),
            novo_contem: String::new(),
            quando_escrita: String::new(),
            sem_novo: false,
        }
    }

    fn delta(novo_contem: &str, quando: &str) -> ConsultaSql {
        ConsultaSql { novo_contem: novo_contem.into(), quando_escrita: quando.into(), ..consulta("select 1", "") }
    }

    #[test]
    fn aceita_select_e_recusa_escrita_e_encadeamento() {
        assert!(somente_leitura("SELECT j.status, count(*) FROM journey_203 j GROUP BY j.status").is_ok());
        assert!(somente_leitura("select * from user where login='x';").is_ok());
        assert!(somente_leitura("SHOW COLUMNS FROM user").is_ok());
        for ruim in [
            "UPDATE user SET x=1",
            "delete from user",
            "select 1; drop table user",
            "select * from a into outfile '/tmp/x'",
            "select sleep(5)",
            "insert into a values (1)",
            "",
        ] {
            assert!(somente_leitura(ruim).is_err(), "deveria recusar: {ruim}");
        }
        // coluna que so CONTEM a palavra nao e a palavra
        assert!(somente_leitura("select updated_at, created_at from t").is_ok());
    }

    #[test]
    fn comparar_diz_o_que_mudou_e_confere_o_esperado() {
        let c = consulta("select 1", "LEADER_APPROVED");
        let ok = comparar(&c, Ok("".into()), Ok("LEADER_APPROVED\t1\t2\t1".into()), &[]);
        assert!(ok.ok && ok.mudou, "{ok:?}");
        let falta = comparar(&c, Ok("".into()), Ok("LEADER_REJECTED\t1\t5\t1".into()), &[]);
        assert!(!falta.ok && falta.detalhe.contains("esperava"), "{falta:?}");
        let quebrou = comparar(&c, Ok("".into()), Err("ERROR 1054".into()), &[]);
        assert!(!quebrou.ok && quebrou.detalhe.contains("consulta falhou"));
        let livre = comparar(&consulta("select 1", ""), Ok("5".into()), Ok("5".into()), &[]);
        assert!(livre.ok && !livre.mudou);
    }

    #[test]
    fn linhas_novas_e_diferenca_de_multiconjuntos() {
        assert_eq!(linhas_novas("a\nb", "a\nb\nc"), vec!["c"]);
        assert_eq!(linhas_novas("a\nb", "b\na"), Vec::<String>::new(), "ordem nao importa");
        assert_eq!(linhas_novas("a", "a\na"), vec!["a"], "copia repetida nova conta");
        assert_eq!(linhas_novas("", "x"), vec!["x"]);
    }

    #[test]
    fn dado_antigo_nao_satisfaz_o_criterio_de_linha_nova() {
        let escrita = vec!["POST /api/casos/reprovar/{id}".to_string()];
        let c = delta("LEADER_REJECTED", "reprovar");
        // reprovar foi chamado, mas o banco so tem a linha que ja existia antes: o bug de "reprovar aprova"
        let preso = comparar(&c, Ok("LEADER_REJECTED\t1\t5\t1".into()), Ok("LEADER_REJECTED\t1\t5\t1\nLEADER_APPROVED\t1\t2\t1".into()), &escrita);
        assert!(!preso.ok && preso.detalhe.contains("nenhuma linha NOVA"), "{preso:?}");
        // o rastro certo apareceu como linha nova
        let certo = comparar(&c, Ok("LEADER_REJECTED\t1\t5\t1".into()), Ok("LEADER_REJECTED\t1\t5\t1\nLEADER_REJECTED\t1\t5\t2".into()), &escrita);
        assert!(certo.ok, "{certo:?}");
        // sem a escrita na rodada o criterio nao se aplica (o agente nao tentou): nao e falha
        let nao_tentou = comparar(&c, Ok("x".into()), Ok("x".into()), &["POST /api/casos/aprovar/{id}".to_string()]);
        assert!(nao_tentou.ok && nao_tentou.detalhe.contains("nao exercitado"), "{nao_tentou:?}");
    }

    #[test]
    fn invariante_reprova_com_qualquer_linha_nova() {
        let c = ConsultaSql { sem_novo: true, ..consulta("select 1", "") };
        let ja_existia = comparar(&c, Ok("7".into()), Ok("7".into()), &[]);
        assert!(ja_existia.ok, "violacao antiga nao conta: {ja_existia:?}");
        let nova = comparar(&c, Ok("7".into()), Ok("7\n9".into()), &[]);
        assert!(!nova.ok && nova.detalhe.contains("violam a invariante"), "{nova:?}");
    }
}
