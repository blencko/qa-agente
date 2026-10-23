use std::time::Duration;

use serde_json::{json, Value};

pub const URL_PADRAO: &str = "http://localhost:20128";
/// Modelos do rodizio: respondem rapido e em JSON sem "pensar em voz alta" (modelos de raciocinio como qwq e r1-distill
/// ficam de fora). Cada chamada comeca por um modelo diferente; os demais sao o reserva dessa chamada.
pub const MODELOS_PADRAO: [&str; 8] = [
    "cfp/nvidia/nemotron-3-120b-a12b",
    "cfp/deepseek-ai/deepseek-v4-flash-0731",
    "cfp/zai-org/glm-4.7-flash",
    "cfp/openai/gpt-oss-120b",
    "cfp/moonshotai/kimi-k2.6",
    "cfp/meta-llama/llama-3.3-70b-instruct-fp8-fast",
    "cfp/qwen/qwen3-30b-a3b-fp8",
    "cfp/google/gemma-4-26b-a4b-it",
];

/// Quanto um modelo que falhou fica fora do rodizio (cota pesa mais que erro comum).
pub const QUARENTENA_ERRO: Duration = Duration::from_secs(60);
pub const QUARENTENA_COTA: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq)]
pub struct Resposta {
    pub texto: String,
    pub modelo: String,
    /// `usage.total_tokens` devolvido pelo gateway, quando houver: base do controle de custo.
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tentativa {
    pub modelo: String,
    pub erro: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ErroGeracao {
    ConfiguracaoInvalida(String),
    /// Toda a cadeia falhou. `cota` vale quando alguma tentativa foi recusada por cota (429): nesse caso
    /// o certo e parar e fazer handoff, nunca seguir sem o LLM nem dar a frente por feita.
    CadeiaEsgotada { tentativas: Vec<Tentativa>, cota: bool },
}

/// Todo texto gerado do projeto passa por aqui; a unica implementacao real e o gateway OmniRoute.
pub trait Gerador {
    fn gerar(&self, sistema: &str, usuario: &str) -> Result<Resposta, ErroGeracao>;
}

pub struct OmniRoute {
    url: String,
    chave: String,
    modelos: Vec<String>,
    tempo: Duration,
    /// Contador do rodizio: cada chamada comeca um modelo adiante da anterior.
    proximo: std::sync::atomic::AtomicUsize,
    /// Ate quando cada modelo fica de fora do rodizio depois de falhar.
    quarentena: std::sync::Mutex<Vec<Option<std::time::Instant>>>,
}

/// Ordem em que os modelos sao tentados numa chamada: a partir de `inicio`, dando a volta, com os que estao em
/// quarentena por ultimo (so entram se nada mais restar). Funcao pura para poder ser testada sem rede.
pub fn ordem_do_rodizio(n: usize, inicio: usize, em_quarentena: &[bool]) -> Vec<usize> {
    let rodada: Vec<usize> = (0..n).map(|i| (inicio + i) % n.max(1)).collect();
    let (livres, presos): (Vec<usize>, Vec<usize>) =
        rodada.into_iter().partition(|&i| !em_quarentena.get(i).copied().unwrap_or(false));
    livres.into_iter().chain(presos).collect()
}

impl OmniRoute {
    pub fn novo(url: &str, chave: &str, modelos: Vec<String>) -> Result<Self, ErroGeracao> {
        if chave.trim().is_empty() {
            return Err(ErroGeracao::ConfiguracaoInvalida("chave do OmniRoute vazia".into()));
        }
        if modelos.is_empty() {
            return Err(ErroGeracao::ConfiguracaoInvalida("cadeia de modelos vazia".into()));
        }
        let n = modelos.len();
        Ok(OmniRoute {
            url: url.trim_end_matches('/').to_string(),
            chave: chave.to_string(),
            modelos,
            tempo: tempo_por_chamada(),
            proximo: std::sync::atomic::AtomicUsize::new(std::process::id() as usize), // processos paralelos nao comecam todos no mesmo modelo
            quarentena: std::sync::Mutex::new(vec![None; n]),
        })
    }

    /// `QA_OMNIROUTE_CHAVE` e obrigatoria; `QA_OMNIROUTE_URL` e `QA_OMNIROUTE_MODELOS` (separados por virgula) tem padrao.
    pub fn do_ambiente() -> Result<Self, ErroGeracao> {
        let chave = std::env::var("QA_OMNIROUTE_CHAVE")
            .map_err(|_| ErroGeracao::ConfiguracaoInvalida("QA_OMNIROUTE_CHAVE nao definida".into()))?;
        let url = std::env::var("QA_OMNIROUTE_URL").unwrap_or_else(|_| URL_PADRAO.to_string());
        let modelos = match std::env::var("QA_OMNIROUTE_MODELOS") {
            Ok(lista) if !lista.trim().is_empty() => lista.split(',').map(|m| m.trim().to_string()).collect(),
            _ => MODELOS_PADRAO.iter().map(|m| m.to_string()).collect(),
        };
        Self::novo(&url, &chave, modelos)
    }

    /// Uma chamada que TENTA DE NOVO o mesmo modelo (ate `RETENTATIVAS_TRANSITORIAS`, com espera crescente) quando o erro
    /// e transitorio, dentro do orcamento da chamada. Importa sobretudo com pool de 1 modelo, onde nao ha outro para cobrir.
    fn chamar_com_retentativa(
        &self,
        modelo: &str,
        sistema: &str,
        usuario: &str,
        inicio: std::time::Instant,
        orcamento: Duration,
    ) -> Result<(String, Option<u64>), String> {
        for tentativa in 0..=RETENTATIVAS_TRANSITORIAS {
            match self.chamar(modelo, sistema, usuario) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    let restante = orcamento.saturating_sub(inicio.elapsed());
                    let espera = (espera_base() * (tentativa as u32 + 1)).min(restante);
                    if !eh_transitorio(&e) || tentativa == RETENTATIVAS_TRANSITORIAS || espera.is_zero() {
                        return Err(if tentativa > 0 { format!("{e} (apos {} tentativas)", tentativa + 1) } else { e });
                    }
                    std::thread::sleep(espera);
                }
            }
        }
        Err("sem tentativas".into())
    }

    /// Pre-voo: o gateway responde dentro de `limite`? Devolve o tempo gasto.
    pub fn saude(&self, limite: Duration) -> Result<Duration, String> {
        saude_do_gateway(&self.url, &self.chave, limite)
    }

    fn chamar(&self, modelo: &str, sistema: &str, usuario: &str) -> Result<(String, Option<u64>), String> {
        let corpo = json!({
            "model": modelo,
            "temperature": 0,
            "messages": [
                {"role": "system", "content": sistema},
                {"role": "user", "content": usuario},
            ],
        });
        let resposta = ureq::post(&format!("{}/v1/chat/completions", self.url))
            .set("Authorization", &format!("Bearer {}", self.chave))
            .timeout(self.tempo)
            .send_json(corpo);
        match resposta {
            Ok(r) => {
                let corpo = r.into_json::<Value>().map_err(|e| e.to_string())?;
                Ok((extrair_conteudo(&corpo)?, extrair_tokens(&corpo)))
            }
            Err(ureq::Error::Status(codigo, r)) => {
                let corpo = r.into_string().unwrap_or_default();
                Err(format!("HTTP {codigo}: {}", corpo.chars().take(200).collect::<String>()))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Gerador for OmniRoute {
    fn gerar(&self, sistema: &str, usuario: &str) -> Result<Resposta, ErroGeracao> {
        let mut tentativas = Vec::new();
        // Rodizio: cada chamada comeca por um modelo diferente (reparte a carga e a cota entre os provedores do
        // gateway); o modelo que falha sai do rodizio por um tempo e os demais cobrem a chamada.
        let inicio = self.proximo.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let agora = std::time::Instant::now();
        let presos: Vec<bool> = self
            .quarentena
            .lock()
            .map(|q| q.iter().map(|ate| ate.is_some_and(|t| t > agora)).collect())
            .unwrap_or_default();
        let orcamento = orcamento_total();
        // Ate RODADAS_NO_GATEWAY voltas pelo pool: se TODAS as falhas de uma volta foram do GATEWAY (conexao recusada,
        // timeout de conexao, resposta cortada), nao ha modelo melhor para tentar; espera um pouco e repete a volta.
        'voltas: for volta in 0..RODADAS_NO_GATEWAY {
            let antes = tentativas.len();
            for i in ordem_do_rodizio(self.modelos.len(), inicio, &presos) {
                // Orcamento total da chamada: depois dele, para de tentar modelos (a jornada devolve o relatorio
                // parcial) em vez de percorrer o pool inteiro a 45 s por modelo travado.
                if !tentativas.is_empty() && agora.elapsed() >= orcamento {
                    break 'voltas;
                }
                let modelo = &self.modelos[i];
                match self.chamar_com_retentativa(modelo, sistema, usuario, agora, orcamento) {
                    Ok((texto, tokens)) => return Ok(Resposta { texto, modelo: modelo.clone(), tokens }),
                    Err(erro) => {
                        // falha do gateway nao e culpa do modelo: nao o poe em quarentena
                        if !eh_falha_do_gateway(&erro) {
                            let pausa = if eh_cota(&erro) { QUARENTENA_COTA } else { QUARENTENA_ERRO };
                            if let Ok(mut q) = self.quarentena.lock() {
                                q[i] = Some(std::time::Instant::now() + pausa);
                            }
                        }
                        tentativas.push(Tentativa { modelo: modelo.clone(), erro });
                    }
                }
            }
            let so_gateway = tentativas.len() > antes && tentativas[antes..].iter().all(|t| eh_falha_do_gateway(&t.erro));
            if !so_gateway || volta + 1 == RODADAS_NO_GATEWAY {
                break;
            }
            let restante = orcamento.saturating_sub(agora.elapsed());
            let espera = (espera_base() * (volta as u32 + 1)).min(restante);
            if espera.is_zero() {
                break;
            }
            std::thread::sleep(espera);
        }
        let cota = tentativas.iter().any(|t| eh_cota(&t.erro));
        Err(ErroGeracao::CadeiaEsgotada { tentativas, cota })
    }
}

/// Voltas pelo pool quando so o gateway esta falhando, e a espera (crescente) entre elas.
pub const RODADAS_NO_GATEWAY: usize = 3;
pub const ESPERA_ENTRE_VOLTAS: Duration = Duration::from_secs(3);
/// Quantas vezes o MESMO modelo e tentado de novo depois de um erro transitorio do gateway (alem da 1a tentativa).
pub const RETENTATIVAS_TRANSITORIAS: usize = 2;

/// Espera-base das retentativas (cresce: 1x, 2x...). `QA_OMNIROUTE_ESPERA_MS` troca (padrao 3000).
pub fn espera_base() -> Duration {
    let ms = std::env::var("QA_OMNIROUTE_ESPERA_MS").ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|m| *m <= 60_000);
    ms.map(Duration::from_millis).unwrap_or(ESPERA_ENTRE_VOLTAS)
}

/// Erro TRANSITORIO do gateway, que vale tentar de novo no mesmo modelo: 502/503/504 ("Safety check unavailable",
/// "Provider returned empty content"...) ou resposta sem conteudo. NAO e transitorio: cota (429, "quota": parar e
/// fazer handoff, nunca insistir), bloqueio deterministico ("Blocked for: ...") e erro de cliente (4xx).
pub fn eh_transitorio(erro: &str) -> bool {
    let e = erro.to_lowercase();
    if eh_cota(&e) || e.contains("blocked for") {
        return false;
    }
    let status_de_gateway = ["http 502", "http 503", "http 504"].iter().any(|s| e.starts_with(s));
    status_de_gateway
        || e.contains("safety check unavailable")
        || e.contains("empty content")
        || e.contains("sem conteudo")
}

/// O erro e do GATEWAY (conexao recusada ou com timeout, resposta cortada, rede), nao de um modelo: erro de rede que
/// nao e "demorou para responder" (essa e de modelo travado) e nao e status HTTP (esse veio do gateway funcionando).
pub fn eh_falha_do_gateway(erro: &str) -> bool {
    let e = erro.to_lowercase();
    let de_rede = ["network error", "connection failed", "connect error", "refused", "os error 1006"].iter().any(|p| e.contains(p));
    !e.starts_with("http ") && de_rede && !e.contains("timed out reading")
}

/// Pre-voo barato: o gateway responde? `GET /v1/models` com a chave. Qualquer resposta HTTP (200, 401...) prova que ele
/// esta de pe; devolve o tempo gasto. Falha de rede ou mais de `limite` e erro.
pub fn saude_do_gateway(url: &str, chave: &str, limite: Duration) -> Result<Duration, String> {
    let inicio = std::time::Instant::now();
    let resposta = ureq::get(&format!("{}/v1/models", url.trim_end_matches('/')))
        .set("Authorization", &format!("Bearer {chave}"))
        .timeout(limite)
        .call();
    match resposta {
        Ok(_) | Err(ureq::Error::Status(_, _)) => Ok(inicio.elapsed()),
        Err(e) => Err(format!("o gateway nao respondeu em {} s: {e}", limite.as_secs())),
    }
}

/// Quanto esperar UM modelo antes de passar ao proximo do rodizio. Um modelo travado nao pode parar a rodada por
/// minutos: 45 s cobre as respostas normais (4 a 18 s medidas) com folga. `QA_OMNIROUTE_TIMEOUT_S` troca (1 a 600).
pub fn tempo_por_chamada() -> Duration {
    tempo_de(std::env::var("QA_OMNIROUTE_TIMEOUT_S").ok().as_deref())
}

/// Quanto a chamada INTEIRA (todos os modelos tentados) pode levar antes de desistir. `QA_OMNIROUTE_ORCAMENTO_S` troca
/// (1 a 3600; padrao 90).
pub fn orcamento_total() -> Duration {
    let v = std::env::var("QA_OMNIROUTE_ORCAMENTO_S").ok();
    let s = v.as_deref().and_then(|v| v.trim().parse::<u64>().ok()).filter(|s| (1..=3600).contains(s)).unwrap_or(90);
    Duration::from_secs(s)
}

fn tempo_de(valor: Option<&str>) -> Duration {
    let segundos = valor.and_then(|v| v.trim().parse::<u64>().ok()).filter(|s| (1..=600).contains(s)).unwrap_or(45);
    Duration::from_secs(segundos)
}

pub fn extrair_conteudo(resposta: &Value) -> Result<String, String> {
    resposta["choices"][0]["message"]["content"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(String::from)
        .ok_or_else(|| "resposta sem conteudo em choices[0].message.content".to_string())
}

pub fn extrair_tokens(resposta: &Value) -> Option<u64> {
    resposta["usage"]["total_tokens"].as_u64()
}

pub fn eh_cota(erro: &str) -> bool {
    let minusculo = erro.to_lowercase();
    minusculo.contains("429") || minusculo.contains("quota")
}

#[cfg(test)]
mod testes {
    use super::*;

    /// Testes que mexem em variavel de ambiente (global do processo) nao podem rodar em paralelo.
    static AMBIENTE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Servidor local que aceita a conexao e a fecha na hora: a falha de rede e imediata (porta fechada demora cerca de 2 s no Windows).
    fn servidor_que_fecha() -> String {
        let escuta = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let porta = escuta.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in escuta.incoming().flatten() {
                drop(c);
            }
        });
        format!("http://127.0.0.1:{porta}")
    }

    /// Servidor HTTP local que responde `(status, corpo)` na ordem (a ultima se repete) e conta os pedidos recebidos.
    fn servidor_com_respostas(
        respostas: Vec<(u16, &'static str)>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let escuta = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let porta = escuta.local_addr().unwrap().port();
        let contador = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = std::sync::Arc::clone(&contador);
        std::thread::spawn(move || {
            for mut fluxo in escuta.incoming().flatten() {
                let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // le o pedido inteiro (cabecalhos e corpo) antes de responder
                let mut lido = Vec::new();
                let mut bloco = [0u8; 4096];
                let (mut fim_cabecalho, mut tamanho) = (None, 0usize);
                loop {
                    let Ok(k) = fluxo.read(&mut bloco) else { break };
                    if k == 0 {
                        break;
                    }
                    lido.extend_from_slice(&bloco[..k]);
                    if fim_cabecalho.is_none() {
                        if let Some(p) = lido.windows(4).position(|w| w == b"\r\n\r\n") {
                            fim_cabecalho = Some(p + 4);
                            let cab = String::from_utf8_lossy(&lido[..p]).to_lowercase();
                            tamanho = cab
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse().ok()))
                                .unwrap_or(0);
                        }
                    }
                    if fim_cabecalho.is_some_and(|f| lido.len() >= f + tamanho) {
                        break;
                    }
                }
                let (status, corpo) = respostas[n.min(respostas.len() - 1)];
                let resposta = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{corpo}",
                    corpo.len()
                );
                let _ = fluxo.write_all(resposta.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{porta}"), contador)
    }

    const OK_JSON: &str = r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"total_tokens":7}}"#;

    #[test]
    fn erro_transitorio_502_e_retentado_no_mesmo_modelo_ate_dar_certo() {
        let _trava = AMBIENTE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("QA_OMNIROUTE_ESPERA_MS", "50");
        std::env::set_var("QA_OMNIROUTE_ORCAMENTO_S", "30");
        let (url, contador) = servidor_com_respostas(vec![
            (502, r#"{"error":"Safety check unavailable"}"#),
            (502, r#"{"error":"Provider returned empty content"}"#),
            (200, OK_JSON),
        ]);
        let o = OmniRoute::novo(&url, "k", vec!["unico".into()]).unwrap();
        let r = o.gerar("s", "u");
        std::env::remove_var("QA_OMNIROUTE_ESPERA_MS");
        std::env::remove_var("QA_OMNIROUTE_ORCAMENTO_S");
        let r = r.expect("deveria ter dado certo na 3a tentativa do mesmo modelo");
        assert_eq!((r.texto.as_str(), r.modelo.as_str(), r.tokens), ("ok", "unico", Some(7)));
        assert_eq!(contador.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn bloqueio_deterministico_e_cota_nao_sao_retentados() {
        let _trava = AMBIENTE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("QA_OMNIROUTE_ESPERA_MS", "50");
        std::env::set_var("QA_OMNIROUTE_ORCAMENTO_S", "30");
        for (status, corpo) in [(502, "Blocked for: code interpreter abuse"), (429, "Individual quota reached")] {
            let (url, contador) = servidor_com_respostas(vec![(status, corpo)]);
            let o = OmniRoute::novo(&url, "k", vec!["unico".into()]).unwrap();
            let erro = o.gerar("s", "u").unwrap_err();
            let ErroGeracao::CadeiaEsgotada { tentativas, cota } = erro else { panic!("esperava cadeia esgotada") };
            assert_eq!(tentativas.len(), 1);
            assert_eq!(contador.load(std::sync::atomic::Ordering::SeqCst), 1, "{status} nao pode ser retentado");
            assert_eq!(cota, status == 429);
        }
        std::env::remove_var("QA_OMNIROUTE_ESPERA_MS");
        std::env::remove_var("QA_OMNIROUTE_ORCAMENTO_S");
    }

    #[test]
    fn so_502_503_504_de_gateway_e_resposta_vazia_sao_transitorios() {
        assert!(eh_transitorio("HTTP 502: Safety check unavailable"));
        assert!(eh_transitorio("HTTP 502: Provider returned empty content"));
        assert!(eh_transitorio("HTTP 503: unavailable"));
        assert!(eh_transitorio("resposta sem conteudo em choices[0].message.content"));
        assert!(!eh_transitorio("HTTP 502: Blocked for: code interpreter abuse"));
        assert!(!eh_transitorio("HTTP 429: Individual quota reached"));
        assert!(!eh_transitorio("HTTP 401: unauthorized"));
        assert!(!eh_transitorio("HTTP 400: bad request"));
    }

    #[test]
    fn falha_do_gateway_se_distingue_de_modelo_travado_e_de_status_http() {
        assert!(eh_falha_do_gateway("http://x/v1/chat/completions: Network Error: Network Error: Connection refused (os error 10061)"));
        assert!(eh_falha_do_gateway("Network Error: Network Error: Error encountered in the status line: U"));
        assert!(eh_falha_do_gateway("Network Error: connection attempt failed (os error 10060)"));
        assert!(eh_falha_do_gateway("http://127.0.0.1:1/v1/x: Connection Failed: Connect error: nao foi possivel conectar (os error 10061)"));
        assert!(!eh_falha_do_gateway("Network Error: Network Error: timed out reading response"), "modelo travado nao e falha do gateway");
        assert!(!eh_falha_do_gateway("HTTP 502: bad gateway"), "status HTTP veio do gateway funcionando");
    }

    #[test]
    fn gateway_fora_do_ar_repete_a_volta_pelo_pool_com_espera_ate_o_orcamento() {
        let _trava = AMBIENTE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("QA_OMNIROUTE_ORCAMENTO_S", "5");
        let modelos: Vec<String> = (0..3).map(|i| format!("m{i}")).collect();
        // porta 1: conexao recusada na hora, falha do gateway
        let o = OmniRoute::novo(&servidor_que_fecha(), "k", modelos).unwrap();
        let inicio = std::time::Instant::now();
        let erro = o.gerar("s", "u").unwrap_err();
        std::env::remove_var("QA_OMNIROUTE_ORCAMENTO_S");
        let ErroGeracao::CadeiaEsgotada { tentativas, .. } = erro else { panic!("esperava cadeia esgotada") };
        assert!(tentativas.len() >= 6, "deu pelo menos 2 voltas pelo pool: {}", tentativas.len());
        assert!(inicio.elapsed() >= Duration::from_secs(3), "esperou entre as voltas: {:?}", inicio.elapsed());
        assert!(inicio.elapsed() < Duration::from_secs(9), "e respeitou o orcamento: {:?}", inicio.elapsed());
    }

    #[test]
    fn saude_do_gateway_mede_o_tempo_e_falha_com_rede_fora() {
        assert!(saude_do_gateway("http://127.0.0.1:1", "k", Duration::from_secs(2)).is_err());
    }

    #[test]
    fn orcamento_total_para_de_tentar_modelos_com_servidor_que_nunca_responde() {
        let _trava = AMBIENTE.lock().unwrap_or_else(|e| e.into_inner());
        use std::net::TcpListener;
        let escuta = TcpListener::bind("127.0.0.1:0").unwrap();
        let porta = escuta.local_addr().unwrap().port();
        // aceita conexoes e nunca responde (modelo travado)
        std::thread::spawn(move || {
            let mut presos = Vec::new();
            for c in escuta.incoming().flatten() {
                presos.push(c);
            }
        });
        std::env::set_var("QA_OMNIROUTE_TIMEOUT_S", "1");
        std::env::set_var("QA_OMNIROUTE_ORCAMENTO_S", "2");
        let modelos: Vec<String> = (0..6).map(|i| format!("m{i}")).collect();
        let o = OmniRoute::novo(&format!("http://127.0.0.1:{porta}"), "k", modelos).unwrap();
        let inicio = std::time::Instant::now();
        let erro = o.gerar("s", "u").unwrap_err();
        std::env::remove_var("QA_OMNIROUTE_TIMEOUT_S");
        std::env::remove_var("QA_OMNIROUTE_ORCAMENTO_S");
        let ErroGeracao::CadeiaEsgotada { tentativas, .. } = erro else { panic!("esperava cadeia esgotada") };
        assert!(tentativas.len() < 6, "parou antes de percorrer o pool inteiro: {}", tentativas.len());
        assert!(inicio.elapsed() < Duration::from_secs(5), "{:?}", inicio.elapsed());
    }

    #[test]
    fn tempo_por_chamada_tem_padrao_curto_e_ignora_valor_invalido() {
        assert_eq!(tempo_de(None), Duration::from_secs(45));
        assert_eq!(tempo_de(Some("20")), Duration::from_secs(20));
        assert_eq!(tempo_de(Some("0")), Duration::from_secs(45));
        assert_eq!(tempo_de(Some("9999")), Duration::from_secs(45));
        assert_eq!(tempo_de(Some("abc")), Duration::from_secs(45));
    }

    #[test]
    fn rodizio_gira_o_ponto_de_partida_e_manda_os_presos_para_o_fim() {
        assert_eq!(ordem_do_rodizio(4, 0, &[false; 4]), vec![0, 1, 2, 3]);
        assert_eq!(ordem_do_rodizio(4, 1, &[false; 4]), vec![1, 2, 3, 0]);
        assert_eq!(ordem_do_rodizio(4, 6, &[false; 4]), vec![2, 3, 0, 1]);
        // o modelo 1 esta em quarentena: so entra depois dos livres, mesmo sendo o da vez
        assert_eq!(ordem_do_rodizio(4, 1, &[false, true, false, false]), vec![2, 3, 0, 1]);
        // todos presos: a chamada ainda tenta todos, na ordem do rodizio (nunca fica sem tentar)
        assert_eq!(ordem_do_rodizio(3, 1, &[true; 3]), vec![1, 2, 0]);
    }

    #[test]
    fn cada_chamada_comeca_por_um_modelo_diferente_ate_dar_a_volta() {
        let o = OmniRoute::novo("http://127.0.0.1:1", "k", vec!["a".into(), "b".into(), "c".into()]).unwrap();
        let primeiros: Vec<usize> = (0..6)
            .map(|_| {
                let inicio = o.proximo.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                ordem_do_rodizio(3, inicio, &[false; 3])[0]
            })
            .collect();
        let distintos: std::collections::BTreeSet<_> = primeiros.iter().take(3).collect();
        assert_eq!(distintos.len(), 3, "{primeiros:?}");
        assert_eq!(primeiros[0..3], primeiros[3..6]);
    }

    #[test]
    fn extrai_o_conteudo_da_primeira_escolha() {
        let r = json!({"choices": [{"message": {"content": "ola"}}]});
        assert_eq!(extrair_conteudo(&r).unwrap(), "ola");
    }

    #[test]
    fn conteudo_vazio_ou_ausente_e_erro() {
        assert!(extrair_conteudo(&json!({"choices": [{"message": {"content": "  "}}]})).is_err());
        assert!(extrair_conteudo(&json!({"choices": []})).is_err());
        assert!(extrair_conteudo(&json!({})).is_err());
    }

    #[test]
    fn le_o_total_de_tokens_quando_o_gateway_informa() {
        assert_eq!(extrair_tokens(&json!({"usage": {"total_tokens": 321}})), Some(321));
        assert_eq!(extrair_tokens(&json!({"usage": {}})), None);
        assert_eq!(extrair_tokens(&json!({})), None);
    }

    #[test]
    fn reconhece_erro_de_cota() {
        assert!(eh_cota("HTTP 429: Individual quota reached"));
        assert!(eh_cota("limite de Quota"));
        assert!(!eh_cota("HTTP 502: Bad Gateway"));
    }

    #[test]
    fn configuracao_sem_chave_ou_sem_modelo_e_recusada() {
        assert!(matches!(OmniRoute::novo("http://x", " ", vec!["m".into()]), Err(ErroGeracao::ConfiguracaoInvalida(_))));
        assert!(matches!(OmniRoute::novo("http://x", "k", vec![]), Err(ErroGeracao::ConfiguracaoInvalida(_))));
    }

    #[test]
    fn cadeia_esgotada_lista_cada_tentativa_e_marca_a_cota() {
        // servidor que fecha a conexao: toda tentativa falha por rede, sem depender do gateway real. Orcamento de 1 s
        // para a retentativa do gateway nao dar uma 2a volta pelo pool.
        let _trava = AMBIENTE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("QA_OMNIROUTE_ORCAMENTO_S", "1");
        let g = OmniRoute::novo(&servidor_que_fecha(), "k", vec!["a".into(), "b".into()]).unwrap();
        let resultado = g.gerar("s", "u");
        std::env::remove_var("QA_OMNIROUTE_ORCAMENTO_S");
        match resultado {
            Err(ErroGeracao::CadeiaEsgotada { tentativas, cota }) => {
                assert_eq!(tentativas.len(), 2);
                // o rodizio escolhe por onde comecar: o que importa e que cada modelo foi tentado uma vez
                let mut modelos: Vec<&str> = tentativas.iter().map(|t| t.modelo.as_str()).collect();
                modelos.sort_unstable();
                assert_eq!(modelos, vec!["a", "b"]);
                assert!(!cota);
            }
            outro => panic!("esperava cadeia esgotada, veio {outro:?}"),
        }
    }
}
