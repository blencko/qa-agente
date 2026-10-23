//! O que UM passo do agente causou no backend: chamadas de API, eventos de WebSocket e erros. E a metade "back" do
//! sinal de aprendizado: a politica nao ve so a tela mudar, ve o servidor responder (ou falhar).

use serde::{Deserialize, Serialize};

use crate::observacao::{url_limpa, Observacao};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Efeito {
    /// "2xx /api/campanhas/{id}": classe de status + caminho sem host, query nem ids.
    #[serde(default)]
    pub endpoints: Vec<String>,
    /// "POST /api/x": o servidor GRAVOU algo (POST, PUT, PATCH ou DELETE com 2xx).
    #[serde(default)]
    pub escritas: Vec<String>,
    #[serde(default)]
    pub erros_5xx: Vec<String>,
    #[serde(default)]
    pub erros_4xx: Vec<String>,
    /// "↑turno" (enviado) e "↓narracao" (recebido).
    #[serde(default)]
    pub eventos_ws: Vec<String>,
    /// Falhas de rede, erros de console e erros de WebSocket.
    #[serde(default)]
    pub falhas: Vec<String>,
}

/// Caminho da URL sem host, query e ids: numeros longos e UUIDs viram `{id}` para a mesma rota contar uma vez so.
pub fn rota(url: &str) -> String {
    let limpa = url_limpa(url);
    let sem_host = match limpa.split_once("://") {
        Some((_, resto)) => resto.split_once('/').map(|(_, p)| format!("/{p}")).unwrap_or_else(|| "/".into()),
        None => limpa,
    };
    sem_host
        .split('/')
        .map(|seg| {
            let numero = !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit());
            let uuid = seg.len() >= 32 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
            if numero || uuid { "{id}" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn unico(v: &mut Vec<String>, item: String) {
    if !v.contains(&item) {
        v.push(item);
    }
}

fn eh_escrita(metodo: &str) -> bool {
    matches!(metodo.to_ascii_uppercase().as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
}

fn eh_api(mime: &str, status: u32, metodo: &str) -> bool {
    // arquivos estaticos (js, css, imagem, fonte) nao dizem nada sobre o backend, a menos que falhem. Pedido que ESCREVE
    // (POST, PUT...) e sempre do backend, mesmo respondendo HTML (formulario tradicional, SSR, htmx).
    status >= 400
        || mime.contains("json")
        || mime.contains("event-stream")
        || mime.contains("text/plain")
        || eh_escrita(metodo)
}

impl Efeito {
    /// So o que chegou entre as duas fotografias da observacao (a de `depois` so cresce).
    pub fn de_delta(antes: &Observacao, depois: &Observacao) -> Efeito {
        let mut e = Efeito::default();
        for r in depois.respostas.iter().skip(antes.respostas.len()) {
            if !eh_api(&r.mime, r.status, &r.metodo) {
                continue;
            }
            // com o verbo: o POST de aprovar e o GET da lista na mesma rota deixam de ser a mesma "rota"
            let pedido = if r.metodo.is_empty() {
                rota(&r.url)
            } else {
                format!("{} {}", r.metodo.to_ascii_uppercase(), rota(&r.url))
            };
            let item = format!("{}xx {}", r.status / 100, pedido);
            match r.status {
                500..=599 => unico(&mut e.erros_5xx, item.clone()),
                400..=499 => unico(&mut e.erros_4xx, item.clone()),
                200..=299 if eh_escrita(&r.metodo) => unico(&mut e.escritas, pedido),
                _ => {}
            }
            unico(&mut e.endpoints, item);
        }
        for f in depois.frames_ws.iter().skip(antes.frames_ws.len()) {
            if let Some(ev) = &f.evento {
                unico(&mut e.eventos_ws, format!("{}{}", if f.enviado { "↑" } else { "↓" }, ev));
            }
        }
        for f in depois.falhas_de_rede.iter().skip(antes.falhas_de_rede.len()) {
            unico(&mut e.falhas, format!("rede: {f}"));
        }
        for f in depois.erros_console.iter().skip(antes.erros_console.len()) {
            unico(&mut e.falhas, format!("console: {f}"));
        }
        for f in depois.erros_ws.iter().skip(antes.erros_ws.len()) {
            unico(&mut e.falhas, format!("ws: {f}"));
        }
        e
    }

    pub fn vazio(&self) -> bool {
        self == &Efeito::default()
    }

    /// Resumo curto para o historico do prompt e para o log: o que o servidor fez neste passo.
    pub fn resumo(&self) -> String {
        if self.vazio() {
            return String::new();
        }
        let mut partes: Vec<String> = Vec::new();
        for g in self.erros_5xx.iter().chain(&self.falhas).take(2) {
            partes.push(format!("ERRO {g}"));
        }
        for g in self.endpoints.iter().filter(|g| !self.erros_5xx.contains(g)).take(2) {
            partes.push(g.clone());
        }
        for g in self.eventos_ws.iter().take(2) {
            partes.push(format!("ws {g}"));
        }
        let texto: String = partes.join(", ").chars().take(160).collect();
        format!("[back: {texto}]")
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::observacao::{FrameWs, RespostaHttp};

    fn resp(url: &str, status: u32, mime: &str) -> RespostaHttp {
        RespostaHttp { url: url.into(), status, mime: mime.into(), metodo: String::new() }
    }

    fn resp_m(url: &str, status: u32, mime: &str, metodo: &str) -> RespostaHttp {
        RespostaHttp { url: url.into(), status, mime: mime.into(), metodo: metodo.into() }
    }

    #[test]
    fn o_verbo_separa_a_escrita_da_leitura_e_post_em_html_conta() {
        let antes = Observacao::default();
        let depois = Observacao {
            respostas: vec![
                resp_m("http://h/api/casos", 200, "application/json", "GET"),
                resp_m("http://h/api/casos", 200, "application/json", "POST"),
                resp_m("http://h/form", 200, "text/html", "POST"),
                resp_m("http://h/pagina", 200, "text/html", "GET"),
            ],
            ..Default::default()
        };
        let e = Efeito::de_delta(&antes, &depois);
        assert_eq!(e.endpoints, vec!["2xx GET /api/casos", "2xx POST /api/casos", "2xx POST /form"]);
        assert_eq!(e.escritas, vec!["POST /api/casos", "POST /form"], "so POST/PUT/PATCH/DELETE com 2xx e a pagina HTML (GET) fica de fora");
    }

    #[test]
    fn rota_perde_host_query_e_ids() {
        assert_eq!(rota("http://127.0.0.1:5000/api/campanhas/42/personagens?x=1"), "/api/campanhas/{id}/personagens");
        assert_eq!(rota("http://h/api/p/3f2504e0-4f89-11d3-9a0c-0305e82c3301"), "/api/p/{id}");
    }

    #[test]
    fn delta_separa_api_de_estatico_e_classifica_os_erros() {
        let antes = Observacao { respostas: vec![resp("http://h/api/a", 200, "application/json")], ..Default::default() };
        let mut depois = antes.clone();
        depois.respostas.push(resp("http://h/app.js", 200, "text/javascript"));
        depois.respostas.push(resp("http://h/api/personagens/7", 201, "application/json"));
        depois.respostas.push(resp("http://h/api/mestre", 500, "application/json"));
        depois.respostas.push(resp("http://h/api/x", 404, "text/html"));
        depois.frames_ws.push(FrameWs { enviado: true, evento: Some("turno".into()), tamanho: 3 });
        depois.erros_ws.push("caiu".into());
        let e = Efeito::de_delta(&antes, &depois);
        assert_eq!(e.endpoints, vec!["2xx /api/personagens/{id}", "5xx /api/mestre", "4xx /api/x"]);
        assert_eq!(e.erros_5xx, vec!["5xx /api/mestre"]);
        assert_eq!(e.erros_4xx, vec!["4xx /api/x"]);
        assert_eq!(e.eventos_ws, vec!["↑turno"]);
        assert_eq!(e.falhas, vec!["ws: caiu"]);
        assert!(e.resumo().starts_with("[back: ERRO 5xx /api/mestre"), "{}", e.resumo());
        assert_eq!(Efeito::de_delta(&depois, &depois), Efeito::default());
        assert_eq!(Efeito::default().resumo(), "");
    }
}
