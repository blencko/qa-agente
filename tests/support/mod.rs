//! Apoio da medicao com defeitos plantados: um servidor HTTP minimo com defeitos CONHECIDOS (`fixtures/bugs-plantados/`),
//! o catalogo desses defeitos e o corretor que cruza o `relatorio.json` do agente com o catalogo.
//! Sem dependencia nova: so std, serde_json e ureq, que o crate ja tem.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

pub const NOME_DO_ALVO: &str = "bugs-plantados";

// ---------------------------------------------------------------------------------------------------------------
// Catalogo: cada defeito diz ONDE o agente poderia te-lo visto no relatorio.
// ---------------------------------------------------------------------------------------------------------------

pub struct Bug {
    pub id: &'static str,
    pub descricao: &'static str,
    /// O agente "achou" quando esta funcao enxerga o sintoma no `relatorio.json`.
    pub achado: fn(&Value) -> bool,
    /// Trecho que identifica uma entrada de `falhas` como sendo deste defeito (para separar falso positivo).
    pub marcador_de_falha: Option<&'static str>,
}

fn lista<'a>(r: &'a Value, caminho: &[&str]) -> Vec<&'a str> {
    let mut atual = r;
    for parte in caminho {
        match atual.get(*parte) {
            Some(v) => atual = v,
            None => return vec![],
        }
    }
    atual.as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default()
}

fn estado_reprovado(r: &Value, nome: &str) -> bool {
    r.get("estado_no_backend")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().any(|e| e["nome"] == nome && e["ok"] == false))
        .unwrap_or(false)
}

pub const BUGS: [Bug; 6] = [
    Bug {
        id: "B1-exportar-5xx-escondido",
        descricao: "Exportar dados devolve 500, mas a tela diz 'Exportado com sucesso'",
        achado: |r| lista(r, &["backend", "erros_5xx"]).iter().any(|e| e.contains("/api/exportar")),
        marcador_de_falha: Some("/api/exportar"),
    },
    Bug {
        id: "B2-tema-nao-persiste",
        descricao: "Salvar tema escuro responde 200, mas o servidor nao grava",
        achado: |r| estado_reprovado(r, "preferencias"),
        marcador_de_falha: Some("preferencias"),
    },
    Bug {
        id: "B3-titulo-vazio-aceito",
        descricao: "Criar tarefa com titulo vazio e aceito (201) em vez de rejeitado (422)",
        achado: |r| estado_reprovado(r, "tarefas validas"),
        marcador_de_falha: Some("tarefas validas"),
    },
    Bug {
        id: "B4-sync-sem-resposta",
        descricao: "Sincronizar: o servidor fecha a conexao sem responder e a tela diz 'Sincronizado'",
        // o navegador guarda so o texto do erro ("net::ERR_EMPTY_RESPONSE"), sem a URL: aceita os dois formatos
        achado: |r| {
            lista(r, &["backend", "falhas_de_rede"]).iter().any(|e| e.contains("/api/sync") || e.contains("ERR_EMPTY_RESPONSE"))
        },
        marcador_de_falha: Some("requisicao sem resposta"),
    },
    Bug {
        id: "B5-panico-no-log",
        descricao: "Gerar resumo: o servidor entra em panico (so no log) e responde 200 com texto de reserva",
        achado: |r| lista(r, &["backend", "log_servidor", "graves"]).iter().any(|e| e.to_lowercase().contains("panicked")),
        marcador_de_falha: Some("log do servidor"),
    },
    Bug {
        id: "B6-erro-de-console",
        descricao: "Ajuda: o JavaScript da tela lanca TypeError (elemento inexistente)",
        achado: |r| lista(r, &["front", "erros_console"]).iter().any(|e| e.contains("TypeError") || e.contains("Uncaught") || e.contains("null")),
        marcador_de_falha: None,
    },
];

pub struct Pontuacao {
    pub achados: Vec<&'static str>,
    pub perdidos: Vec<&'static str>,
    /// Entradas de `falhas` que nenhum defeito plantado explica (ex.: sonda, "sem progresso").
    pub extras: Vec<String>,
}

impl Pontuacao {
    pub fn cobertura(&self) -> f64 {
        self.achados.len() as f64 / BUGS.len() as f64
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Metricas de uma rodada a partir do rastro por passo (`passos.jsonl`): base do experimento A/B da politica.
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Metricas {
    pub passos: usize,
    /// Passo (1-based) em que os tres defeitos visiveis no rastro (B1 5xx, B4 sem resposta, B5 panico no log) ja
    /// tinham aparecido. B2, B3 e B6 so aparecem no relatorio final, nao em passo.
    pub ate_visiveis: Option<usize>,
    pub tokens: u64,
    /// Passos decididos pelo LLM (nao pela politica nem pelo fallback).
    pub chamadas_llm: usize,
}

impl Metricas {
    /// Custo em passos para a metrica primaria: quem nao chegou la paga o limite mais 50%.
    pub fn custo_em_passos(&self, limite: usize) -> f64 {
        self.ate_visiveis.unwrap_or(limite + limite / 2) as f64
    }
}

pub fn metricas(passos_jsonl: &str) -> Metricas {
    let (mut b1, mut b4, mut b5) = (false, false, false);
    let mut m = Metricas { passos: 0, ate_visiveis: None, tokens: 0, chamadas_llm: 0 };
    for linha in passos_jsonl.lines().filter(|l| l.trim_start().starts_with('{')) {
        let Ok(p) = serde_json::from_str::<Value>(linha) else { continue };
        m.passos += 1;
        m.tokens += p["tokens"].as_u64().unwrap_or(0);
        let modelo = p["modelo"].as_str().unwrap_or("");
        if modelo != qa_agente::exploracao::MODELO_POLITICA && modelo != qa_agente::exploracao::MODELO_FALLBACK {
            m.chamadas_llm += 1;
        }
        let e = &p["efeito"];
        let texto = |campo: &str| -> Vec<String> {
            e[campo].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
        };
        b1 |= texto("erros_5xx").iter().any(|x| x.contains("/api/exportar"));
        let falhas = texto("falhas");
        b4 |= falhas.iter().any(|x| x.contains("ERR_EMPTY_RESPONSE") || x.contains("/api/sync"));
        b5 |= falhas.iter().any(|x| x.to_lowercase().contains("panicked"));
        if m.ate_visiveis.is_none() && b1 && b4 && b5 {
            m.ate_visiveis = Some(m.passos);
        }
    }
    m
}

pub fn mediana(valores: &[f64]) -> f64 {
    if valores.is_empty() {
        return f64::NAN;
    }
    let mut v = valores.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let meio = v.len() / 2;
    if v.len() % 2 == 1 { v[meio] } else { (v[meio - 1] + v[meio]) / 2.0 }
}

/// (mediana, minimo, maximo)
pub fn resumo(valores: &[f64]) -> (f64, f64, f64) {
    let min = valores.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = valores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    (mediana(valores), min, max)
}

pub fn pontuar(relatorio: &Value) -> Pontuacao {
    let (achados, perdidos): (Vec<_>, Vec<_>) = BUGS.iter().partition(|b| (b.achado)(relatorio));
    let falhas: Vec<String> = relatorio["falhas"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let extras = falhas
        .into_iter()
        .filter(|f| !BUGS.iter().any(|b| b.marcador_de_falha.is_some_and(|m| f.contains(m))))
        .collect();
    Pontuacao {
        achados: achados.iter().map(|b| b.id).collect(),
        perdidos: perdidos.iter().map(|b| b.id).collect(),
        extras,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Servidor
// ---------------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Estado {
    tarefas: Vec<String>,
    tarefas_invalidas: usize,
    tema_pedido: Option<String>,
    visitas: usize,
}

pub struct Servidor {
    pub url: String,
    pub log: PathBuf,
    porta: u16,
    parar: Arc<AtomicBool>,
}

impl Servidor {
    /// Sobe em porta efemera de 127.0.0.1; o log do "servidor" fica em `pasta/servidor.log`.
    pub fn iniciar(pasta: &Path) -> Servidor {
        std::fs::create_dir_all(pasta).expect("criar pasta do servidor");
        let log = pasta.join("servidor.log");
        std::fs::write(&log, "").expect("criar log");
        let escuta = TcpListener::bind("127.0.0.1:0").expect("porta livre");
        let porta = escuta.local_addr().unwrap().port();
        let parar = Arc::new(AtomicBool::new(false));
        let estado = Arc::new(Mutex::new(Estado::default()));
        let (parar_t, log_t) = (parar.clone(), log.clone());
        std::thread::spawn(move || {
            for conexao in escuta.incoming() {
                if parar_t.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conexao) = conexao else { continue };
                let (estado, log) = (estado.clone(), log_t.clone());
                std::thread::spawn(move || atender(conexao, &estado, &log));
            }
        });
        Servidor { url: format!("http://127.0.0.1:{porta}"), log, porta, parar }
    }

    /// Descricao do alvo no formato que o `qa-agente` le (`alvos/exemplo.json`).
    pub fn alvo_json(&self, limite_passos: usize) -> String {
        let log = self.log.display().to_string();
        serde_json::to_string_pretty(&json!({
            "nome": NOME_DO_ALVO,
            "url": format!("{}/", self.url),
            "objetivo": "Explorar todas as telas e usar cada funcionalidade como um usuario comum, anotando qualquer comportamento incorreto.",
            "modo": "generico",
            "limite_passos": limite_passos,
            "contexto_ao_llm": false,
            "acoes_de_escrita": "",
            "max_escritas": 0,
            "sondas": [{ "nome": "front", "url": format!("{}/", self.url) }],
            "log_servidor": log,
            "logs": [{ "nome": "servidor", "caminho": log }],
            "verificacoes": verificacoes_json(&self.url),
        }))
        .unwrap()
    }
}

pub fn verificacoes_json(url: &str) -> Value {
    json!([
        { "nome": "preferencias", "url": format!("{url}/api/auditoria"), "contem": "preferencias=ok" },
        { "nome": "tarefas validas", "url": format!("{url}/api/auditoria"), "contem": "tarefas_invalidas=0" },
    ])
}

impl Drop for Servidor {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.porta));
    }
}

enum Resposta {
    Http(u16, &'static str, String),
    /// Fecha a conexao sem escrever nada: o navegador ve "resposta vazia".
    Fechar,
}

fn atender(mut c: TcpStream, estado: &Mutex<Estado>, log: &Path) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    let mut dados: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    let fim_dos_cabecalhos = loop {
        if let Some(i) = dados.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match c.read(&mut buf) {
            Ok(0) | Err(_) => return, // conexao especulativa do Chrome, sem pedido
            Ok(n) => dados.extend_from_slice(&buf[..n]),
        }
    };
    let cabecalhos = String::from_utf8_lossy(&dados[..fim_dos_cabecalhos]).to_string();
    let tamanho: usize = cabecalhos
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
        .unwrap_or(0);
    while dados.len() < fim_dos_cabecalhos + tamanho {
        match c.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => dados.extend_from_slice(&buf[..n]),
        }
    }
    let corpo = String::from_utf8_lossy(&dados[fim_dos_cabecalhos..]).to_string();
    let mut partes = cabecalhos.lines().next().unwrap_or("").split_whitespace();
    let (metodo, caminho) = (partes.next().unwrap_or(""), partes.next().unwrap_or(""));
    let caminho = caminho.split('?').next().unwrap_or("");

    match rota(metodo, caminho, &corpo, estado, log) {
        Resposta::Fechar => {
            let _ = c.shutdown(Shutdown::Both);
        }
        Resposta::Http(status, tipo, texto) => {
            let _ = write!(
                c,
                "HTTP/1.1 {status} X\r\nContent-Type: {tipo}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{texto}",
                texto.len()
            );
        }
    }
}

fn pagina(nome: &str) -> Resposta {
    let arquivo = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join("bugs-plantados").join(nome);
    match std::fs::read_to_string(arquivo) {
        Ok(t) => Resposta::Http(200, "text/html; charset=utf-8", t),
        Err(_) => Resposta::Http(404, "text/plain", "pagina ausente".into()),
    }
}

fn json_ok(texto: &str) -> Resposta {
    Resposta::Http(200, "application/json", texto.into())
}

fn rota(metodo: &str, caminho: &str, corpo: &str, estado: &Mutex<Estado>, log: &Path) -> Resposta {
    let mut e = estado.lock().unwrap();
    match (metodo, caminho) {
        ("GET", "/") => pagina("index.html"),
        ("GET", "/tarefas") => pagina("tarefas.html"),
        ("GET", "/config") => pagina("config.html"),
        ("GET", "/relatorios") => pagina("relatorios.html"),
        // controles: um 404 de favicon e um endpoint saudavel nao podem virar achado
        ("GET", "/favicon.ico") => Resposta::Http(404, "text/plain", "sem favicon".into()),
        ("GET", "/api/ping") => {
            e.visitas += 1;
            json_ok(&e.visitas.to_string())
        }
        ("GET", "/api/tarefas") => json_ok(&serde_json::to_string(&e.tarefas).unwrap()),
        ("POST", "/api/tarefas") => {
            let titulo = serde_json::from_str::<Value>(corpo)
                .ok()
                .and_then(|v| v["titulo"].as_str().map(String::from))
                .unwrap_or_default();
            // B3: titulo vazio deveria dar 422; o servidor aceita e grava
            if titulo.trim().is_empty() {
                e.tarefas_invalidas += 1;
            }
            e.tarefas.push(titulo);
            Resposta::Http(201, "application/json", "{\"ok\":true}".into())
        }
        ("GET", "/api/preferencias") => json_ok("{\"tema\":\"claro\"}"),
        ("POST", "/api/preferencias") => {
            // B2: responde 200 e NAO grava: so lembra o que foi pedido, para o auditor enxergar a divergencia
            e.tema_pedido = serde_json::from_str::<Value>(corpo).ok().and_then(|v| v["tema"].as_str().map(String::from));
            json_ok("{\"ok\":true}")
        }
        // B1: falha de verdade, mas a tela anuncia sucesso
        ("POST", "/api/exportar") => Resposta::Http(500, "application/json", "{\"erro\":\"falha ao exportar\"}".into()),
        // B4: nunca responde
        ("GET", "/api/sync") => Resposta::Fechar,
        ("GET", "/api/relatorio/resumo") => {
            // B5: o erro so aparece no log; o cliente recebe 200 com texto de reserva
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(log) {
                let _ = writeln!(f, "thread 'relatorio' panicked at 'index out of bounds: the len is 0 but the index is 0'");
            }
            json_ok("\"resumo indisponivel\"")
        }
        // oraculo de estado: o que o QA de verdade leria no banco
        ("GET", "/api/auditoria") => {
            let prefs = match &e.tema_pedido {
                Some(t) if t != "claro" => "divergente",
                _ => "ok",
            };
            Resposta::Http(200, "text/plain", format!("preferencias={prefs};tarefas_invalidas={}", e.tarefas_invalidas))
        }
        _ => Resposta::Http(404, "text/plain", "nao encontrado".into()),
    }
}
