use std::sync::{Arc, Mutex};

const LIMITE_POR_LISTA: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct RespostaHttp {
    pub url: String,
    pub status: u32,
    pub mime: String,
    /// Verbo HTTP do pedido ("GET", "POST"...); vazio quando o pedido nao foi visto (resposta chegou antes).
    pub metodo: String,
}

/// O que o navegador viu do backend e do console enquanto o agente navegava.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observacao {
    pub respostas: Vec<RespostaHttp>,
    pub falhas_de_rede: Vec<String>,
    pub erros_console: Vec<String>,
    pub frames_ws: Vec<FrameWs>,
    pub erros_ws: Vec<String>,
    /// Pedidos enviados, por id do CDP: (verbo, url). Serve para dar verbo as respostas e URL as falhas de rede.
    pub pedidos: std::collections::HashMap<String, (String, String)>,
}

/// Um frame de WebSocket. Guarda so o nome do evento Socket.IO e o tamanho: o conteudo pode ter dado do usuario.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameWs {
    pub enviado: bool,
    pub evento: Option<String>,
    pub tamanho: usize,
}

/// Nome do evento de um pacote Socket.IO de evento (`42["turno",{...}]`, com ou sem id de ack). Outros pacotes
/// (ping, conexao, texto livre) nao tem nome.
pub fn evento_socketio(payload: &str) -> Option<String> {
    let inicio = payload.find('[')?;
    if !payload[..inicio].chars().all(|c| c.is_ascii_digit()) || inicio == 0 {
        return None;
    }
    let valor: serde_json::Value = serde_json::from_str(&payload[inicio..]).ok()?;
    valor.as_array()?.first()?.as_str().map(|s| s.chars().take(60).collect())
}

/// Visao do backend para o relatorio. Erro 5xx e requisicao que nem chegou sao graves; 4xx e informativo
/// (401/404 aparecem em app saudavel), e erro de console nao reprova sozinho.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResumoBackend {
    pub requisicoes: usize,
    pub erros_5xx: Vec<String>,
    pub erros_4xx: Vec<String>,
    pub falhas_de_rede: Vec<String>,
    pub erros_console: Vec<String>,
    pub ws_enviados: usize,
    pub ws_recebidos: usize,
    /// Nomes de evento Socket.IO vistos (sem conteudo), sem repeticao.
    pub ws_eventos: Vec<String>,
    pub erros_ws: Vec<String>,
    /// Linhas do log do servidor com excecao nao tratada, fatal etc. (preenchido por quem le o log).
    pub log_graves: Vec<String>,
    /// Linhas do log do servidor com "error" simples: informativas.
    pub log_avisos: Vec<String>,
}

impl ResumoBackend {
    pub fn grave(&self) -> bool {
        !self.erros_5xx.is_empty()
            || !self.falhas_de_rede.is_empty()
            || !self.erros_ws.is_empty()
            || !self.log_graves.is_empty()
    }
}

pub type Coletor = Arc<Mutex<Observacao>>;

/// Tira a query string (pode carregar token) e limita o tamanho, para o relatorio nunca vazar credencial.
pub fn url_limpa(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or("").chars().take(200).collect()
}

fn unicos(itens: impl Iterator<Item = String>) -> Vec<String> {
    let mut saida: Vec<String> = Vec::new();
    for item in itens {
        if !saida.contains(&item) {
            saida.push(item);
        }
        if saida.len() >= LIMITE_POR_LISTA {
            break;
        }
    }
    saida
}

impl Observacao {
    pub fn resumo(&self) -> ResumoBackend {
        let com_status = |faixa: std::ops::Range<u32>| {
            unicos(
                self.respostas
                    .iter()
                    .filter(|r| faixa.contains(&r.status))
                    .map(|r| format!("{} {}", r.status, url_limpa(&r.url))),
            )
        };
        ResumoBackend {
            requisicoes: self.respostas.len(),
            erros_5xx: com_status(500..600),
            erros_4xx: com_status(400..500),
            falhas_de_rede: unicos(self.falhas_de_rede.iter().map(|f| url_limpa(f))),
            erros_console: unicos(self.erros_console.iter().map(|e| e.chars().take(200).collect())),
            ws_enviados: self.frames_ws.iter().filter(|f| f.enviado).count(),
            ws_recebidos: self.frames_ws.iter().filter(|f| !f.enviado).count(),
            ws_eventos: unicos(self.frames_ws.iter().filter_map(|f| f.evento.clone())),
            erros_ws: unicos(self.erros_ws.iter().map(|e| e.chars().take(200).collect())),
            ..ResumoBackend::default()
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn resposta(url: &str, status: u32) -> RespostaHttp {
        RespostaHttp { url: url.into(), status, mime: "application/json".into(), metodo: "GET".into() }
    }

    #[test]
    fn separa_5xx_de_4xx_e_conta_as_requisicoes() {
        let o = Observacao {
            respostas: vec![resposta("http://a/api/x", 200), resposta("http://a/api/y", 503), resposta("http://a/api/z", 404)],
            ..Observacao::default()
        };
        let r = o.resumo();
        assert_eq!(r.requisicoes, 3);
        assert_eq!(r.erros_5xx, vec!["503 http://a/api/y".to_string()]);
        assert_eq!(r.erros_4xx, vec!["404 http://a/api/z".to_string()]);
        assert!(r.grave());
    }

    #[test]
    fn so_4xx_nao_e_grave_mas_falha_de_rede_e() {
        let so_4xx = Observacao { respostas: vec![resposta("http://a/b", 401)], ..Observacao::default() };
        assert!(!so_4xx.resumo().grave());
        let falha = Observacao { falhas_de_rede: vec!["http://a/c".into()], ..Observacao::default() };
        assert!(falha.resumo().grave());
    }

    #[test]
    fn url_perde_a_query_string_com_token() {
        assert_eq!(url_limpa("http://a/api?token=segredo&x=1"), "http://a/api");
        assert_eq!(url_limpa("http://a/p#frag"), "http://a/p");
    }

    #[test]
    fn repetidos_aparecem_uma_vez() {
        let o = Observacao {
            respostas: vec![resposta("http://a/y?i=1", 500), resposta("http://a/y?i=2", 500)],
            ..Observacao::default()
        };
        assert_eq!(o.resumo().erros_5xx.len(), 1);
    }

    #[test]
    fn extrai_o_nome_do_evento_socketio_sem_o_conteudo() {
        assert_eq!(evento_socketio("42[\"turno\",{\"texto\":\"segredo\"}]"), Some("turno".into()));
        assert_eq!(evento_socketio("421[\"mensagem\",1]"), Some("mensagem".into()));
        assert_eq!(evento_socketio("2"), None);
        assert_eq!(evento_socketio("0{\"sid\":\"x\"}"), None);
        assert_eq!(evento_socketio("[\"sem_prefixo\"]"), None);
        assert_eq!(evento_socketio("texto livre"), None);
    }

    #[test]
    fn websocket_conta_enviados_recebidos_e_eventos_sem_repetir() {
        let f = |enviado, evento: Option<&str>| FrameWs { enviado, evento: evento.map(String::from), tamanho: 10 };
        let o = Observacao {
            frames_ws: vec![f(true, Some("turno")), f(false, Some("narracao")), f(false, Some("narracao")), f(false, None)],
            ..Observacao::default()
        };
        let r = o.resumo();
        assert_eq!((r.ws_enviados, r.ws_recebidos), (1, 3));
        assert_eq!(r.ws_eventos, vec!["turno".to_string(), "narracao".to_string()]);
        assert!(!r.grave());
    }

    #[test]
    fn erro_de_websocket_e_log_grave_sao_graves_mas_aviso_de_log_nao() {
        let ws = Observacao { erros_ws: vec!["conexao caiu".into()], ..Observacao::default() };
        assert!(ws.resumo().grave());
        let log = ResumoBackend { log_graves: vec!["Uncaught".into()], ..ResumoBackend::default() };
        assert!(log.grave());
        let aviso = ResumoBackend { log_avisos: vec!["error: validacao".into()], ..ResumoBackend::default() };
        assert!(!aviso.grave());
    }

    #[test]
    fn nada_visto_nao_e_grave() {
        assert!(!Observacao::default().resumo().grave());
    }
}
