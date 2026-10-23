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
            tempo: Duration::from_secs(120),
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
        for i in ordem_do_rodizio(self.modelos.len(), inicio, &presos) {
            let modelo = &self.modelos[i];
            match self.chamar(modelo, sistema, usuario) {
                Ok((texto, tokens)) => return Ok(Resposta { texto, modelo: modelo.clone(), tokens }),
                Err(erro) => {
                    let pausa = if eh_cota(&erro) { QUARENTENA_COTA } else { QUARENTENA_ERRO };
                    if let Ok(mut q) = self.quarentena.lock() {
                        q[i] = Some(std::time::Instant::now() + pausa);
                    }
                    tentativas.push(Tentativa { modelo: modelo.clone(), erro });
                }
            }
        }
        let cota = tentativas.iter().any(|t| eh_cota(&t.erro));
        Err(ErroGeracao::CadeiaEsgotada { tentativas, cota })
    }
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
        // porta fechada: toda tentativa falha por conexao, sem depender do gateway
        let g = OmniRoute::novo("http://127.0.0.1:9", "k", vec!["a".into(), "b".into()]).unwrap();
        match g.gerar("s", "u") {
            Err(ErroGeracao::CadeiaEsgotada { tentativas, cota }) => {
                assert_eq!(tentativas.len(), 2);
                assert_eq!(tentativas[0].modelo, "a");
                assert!(!cota);
            }
            outro => panic!("esperava cadeia esgotada, veio {outro:?}"),
        }
    }
}
