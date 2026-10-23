use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const LIMITE_BYTES: u64 = 512 * 1024;
const LIMITE_LINHAS: usize = 20;
const GRAVES: [&str; 5] = ["uncaught", "unhandled", "fatal", "exception", "panicked"];

/// Onde o log do servidor estava quando o QA comecou: so as linhas novas depois disso sao do QA.
#[derive(Debug, Clone)]
pub struct MarcaLog {
    caminho: PathBuf,
    inicio: u64,
}

pub fn marcar(caminho: &Path) -> MarcaLog {
    let inicio = std::fs::metadata(caminho).map(|m| m.len()).unwrap_or(0);
    MarcaLog { caminho: caminho.to_path_buf(), inicio }
}

/// O arquivo de log existe? Se nao existe, "nenhuma linha grave" nao quer dizer "servidor saudavel": o QA ficou
/// cego nesse ponto e o relatorio precisa dizer isso.
pub fn existe(marca: &MarcaLog) -> bool {
    marca.caminho.is_file()
}

/// Linhas gravadas depois da marca (no maximo 512 KiB, as mais recentes). Arquivo ausente = nenhuma linha.
pub fn novas_linhas(marca: &MarcaLog) -> Result<Vec<String>, String> {
    let Ok(mut arquivo) = std::fs::File::open(&marca.caminho) else { return Ok(vec![]) };
    let tamanho = arquivo.metadata().map_err(|e| e.to_string())?.len();
    // log rotacionado ou truncado: recomeca do zero em vez de ler de uma posicao que nao existe mais
    let de = if tamanho < marca.inicio { 0 } else { marca.inicio };
    let de = de.max(tamanho.saturating_sub(LIMITE_BYTES));
    arquivo.seek(SeekFrom::Start(de)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    arquivo.take(LIMITE_BYTES).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).lines().map(String::from).collect())
}

/// Varios logs (back, BFF, front) lidos a cada passo: devolve so os achados GRAVES novos desde a ultima chamada, ja
/// com o nome do log e sem numeros (hora, thread, id), para o mesmo erro nao valer como achado novo a cada ocorrencia.
pub struct LogsVivos {
    itens: Vec<(String, MarcaLog)>,
}

impl LogsVivos {
    pub fn novo(logs: Vec<(String, PathBuf)>) -> Self {
        LogsVivos { itens: logs.into_iter().map(|(n, c)| (n, marcar(&c))).collect() }
    }

    pub fn novos_achados(&mut self) -> Vec<String> {
        let mut saida: Vec<String> = Vec::new();
        for (nome, marca) in &mut self.itens {
            let linhas = novas_linhas(marca).unwrap_or_default();
            marca.inicio = std::fs::metadata(&marca.caminho).map(|m| m.len()).unwrap_or(marca.inicio);
            for g in classificar(&linhas).0 {
                let sem_numeros: String = g.chars().filter(|c| !c.is_ascii_digit()).take(120).collect();
                let item = format!("log {nome}: {}", sem_numeros.trim());
                if !saida.contains(&item) {
                    saida.push(item);
                }
            }
        }
        saida
    }
}

/// (graves, avisos): grave e excecao nao tratada, fatal, panic; aviso e qualquer outra linha com "error".
pub fn classificar(linhas: &[String]) -> (Vec<String>, Vec<String>) {
    let mut graves: Vec<String> = Vec::new();
    let mut avisos: Vec<String> = Vec::new();
    for linha in linhas {
        let minuscula = linha.to_lowercase();
        let lista = if GRAVES.iter().any(|g| minuscula.contains(g)) {
            &mut graves
        } else if minuscula.contains("error") {
            &mut avisos
        } else {
            continue;
        };
        let curta: String = linha.chars().take(200).collect();
        if lista.len() < LIMITE_LINHAS && !lista.contains(&curta) {
            lista.push(curta);
        }
    }
    (graves, avisos)
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::io::Write;

    fn arquivo_temporario(nome: &str) -> PathBuf {
        std::env::temp_dir().join(format!("qa-agente-teste-{nome}-{}.log", std::process::id()))
    }

    fn linhas(itens: &[&str]) -> Vec<String> {
        itens.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn le_so_o_que_foi_escrito_depois_da_marca() {
        let caminho = arquivo_temporario("marca");
        std::fs::write(&caminho, "antigo: Error velho\n").unwrap();
        let marca = marcar(&caminho);
        std::fs::OpenOptions::new().append(true).open(&caminho).unwrap().write_all(b"novo 1\nnovo 2\n").unwrap();
        assert_eq!(novas_linhas(&marca).unwrap(), linhas(&["novo 1", "novo 2"]));
        let _ = std::fs::remove_file(&caminho);
    }

    #[test]
    fn arquivo_ausente_nao_tem_linhas_e_nao_existe() {
        let marca = marcar(&arquivo_temporario("ausente"));
        assert!(novas_linhas(&marca).unwrap().is_empty());
        assert!(!existe(&marca));
    }

    #[test]
    fn log_truncado_recomeca_do_inicio() {
        let caminho = arquivo_temporario("truncado");
        std::fs::write(&caminho, "linha comprida que ocupa espaco\n").unwrap();
        let marca = marcar(&caminho);
        std::fs::write(&caminho, "curto\n").unwrap();
        assert_eq!(novas_linhas(&marca).unwrap(), linhas(&["curto"]));
        let _ = std::fs::remove_file(&caminho);
    }

    #[test]
    fn logs_vivos_devolvem_so_o_grave_novo_de_cada_log_sem_numeros() {
        use std::io::Write;
        let a = arquivo_temporario("vivo-a");
        let b = arquivo_temporario("vivo-b");
        std::fs::write(&a, "antigo FATAL 1\n").unwrap();
        std::fs::write(&b, "").unwrap();
        let mut vivos = LogsVivos::novo(vec![("back".into(), a.clone()), ("bff".into(), b.clone())]);
        assert!(vivos.novos_achados().is_empty(), "o que ja estava no log nao e do QA");
        let mut fa = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        writeln!(fa, "10:01:22 [t-7] ERROR Unhandled exception id 55").unwrap();
        writeln!(fa, "10:01:23 [t-9] INFO tudo bem").unwrap();
        let mut fb = std::fs::OpenOptions::new().append(true).open(&b).unwrap();
        writeln!(fb, "fatal no bff").unwrap();
        let achados = vivos.novos_achados();
        assert_eq!(achados.len(), 2, "{achados:?}");
        assert!(achados[0].starts_with("log back: ") && !achados[0].chars().any(|c| c.is_ascii_digit()), "{achados:?}");
        assert!(achados[1].starts_with("log bff: "));
        assert!(vivos.novos_achados().is_empty(), "so o que chegou desde a ultima leitura");
        let mut fa = std::fs::OpenOptions::new().append(true).open(&a).unwrap();
        writeln!(fa, "10:09:59 [t-1] ERROR Unhandled exception id 99").unwrap();
        assert_eq!(vivos.novos_achados().len(), 1, "mesmo erro de novo ainda aparece (a recompensa e que paga so uma vez)");
        let _ = (std::fs::remove_file(a), std::fs::remove_file(b));
    }

    #[test]
    fn separa_grave_de_aviso_e_ignora_o_resto() {
        let (graves, avisos) = classificar(&linhas(&[
            "GET /api/x 200",
            "Uncaught TypeError: x is undefined",
            "validation error: nome vazio",
            "UnhandledPromiseRejection",
            "validation error: nome vazio",
        ]));
        assert_eq!(graves.len(), 2);
        assert_eq!(avisos, vec!["validation error: nome vazio".to_string()]);
    }
}
