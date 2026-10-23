use std::time::Duration;

/// Checagem somente-leitura do estado do backend depois da jornada: GET numa rota de leitura, status esperado e
/// um trecho que o corpo precisa conter (por exemplo, o nome da campanha que o agente criou). Nunca outro verbo.
#[derive(Debug, Clone, PartialEq)]
pub struct Verificacao {
    pub nome: String,
    pub url: String,
    pub status_esperado: u16,
    pub contem: String,
    /// Trecho que o corpo NAO pode conter (vazio = nao confere).
    pub nao_contem: String,
    /// Sessao (nome, valor) usada SO nesta checagem, no lugar da sessao do alvo: serve para provar que um usuario fora da
    /// hierarquia e recusado (status esperado 401/403) ou que o dono enxerga o proprio dado.
    pub cookie: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultadoVerificacao {
    pub nome: String,
    pub ok: bool,
    pub detalhe: String,
}

/// `cookie` = (nome, valor) da sessao de teste. O corpo da resposta nunca entra no resultado: pode ter dado do usuario.
pub fn executar(verificacoes: &[Verificacao], cookie: Option<(&str, &str)>) -> Vec<ResultadoVerificacao> {
    verificacoes.iter().map(|v| verificar(v, cookie)).collect()
}

fn verificar(v: &Verificacao, cookie: Option<(&str, &str)>) -> ResultadoVerificacao {
    let mut pedido = ureq::get(&v.url).timeout(Duration::from_secs(10));
    let cookie = v.cookie.as_ref().map(|(n, c)| (n.as_str(), c.as_str())).or(cookie);
    if let Some((nome, valor)) = cookie {
        pedido = pedido.set("Cookie", &format!("{nome}={valor}"));
    }
    let (status, corpo) = match pedido.call() {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(codigo, r)) => (codigo, r.into_string().unwrap_or_default()),
        Err(e) => {
            return ResultadoVerificacao { nome: v.nome.clone(), ok: false, detalhe: format!("sem resposta: {e}") }
        }
    };
    let detalhe = if status != v.status_esperado {
        format!("esperava {} e veio {status}", v.status_esperado)
    } else if !v.contem.is_empty() && !corpo.contains(&v.contem) {
        format!("a resposta nao contem {:?}", v.contem)
    } else if !v.nao_contem.is_empty() && corpo.contains(&v.nao_contem) {
        format!("a resposta contem {:?}, que nao podia aparecer", v.nao_contem)
    } else {
        String::new()
    };
    ResultadoVerificacao { nome: v.nome.clone(), ok: detalhe.is_empty(), detalhe }
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Servidor de uma resposta so, em porta efemera: 200 com o corpo so se o pedido trouxer o cookie esperado.
    fn servidor(corpo: &'static str, cookie_esperado: &'static str) -> String {
        let escuta = TcpListener::bind("127.0.0.1:0").unwrap();
        let porta = escuta.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = escuta.accept() {
                let mut buf = [0u8; 2048];
                let n = c.read(&mut buf).unwrap_or(0);
                let pedido = String::from_utf8_lossy(&buf[..n]).to_string();
                let (linha, corpo) = if pedido.contains(cookie_esperado) {
                    ("200 OK", corpo)
                } else {
                    ("401 Unauthorized", "nao autorizado")
                };
                let _ = write!(c, "HTTP/1.1 {linha}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{corpo}", corpo.len());
            }
        });
        format!("http://127.0.0.1:{porta}/api/campanhas")
    }

    fn verificacao(url: String, contem: &str) -> Verificacao {
        Verificacao { nome: "campanha".into(), url, status_esperado: 200, contem: contem.into(), nao_contem: String::new(), cookie: None }
    }

    #[test]
    fn cookie_proprio_da_checagem_vale_mais_que_o_do_alvo() {
        let url = servidor("[]", "sessao=outro");
        let v = Verificacao { cookie: Some(("sessao".into(), "outro".into())), ..verificacao(url, "") };
        let r = executar(&[v], Some(("sessao", "dono")));
        assert!(r[0].ok, "a checagem usa a sessao dela, nao a do alvo: {r:?}");
    }

    #[test]
    fn corpo_com_trecho_proibido_reprova_sem_vazar_o_corpo() {
        let url = servidor("{\"eventos\":[1]}", "sessao=abc");
        let v = Verificacao { nao_contem: "eventos".into(), ..verificacao(url, "") };
        let r = executar(&[v], Some(("sessao", "abc")));
        assert!(!r[0].ok);
        assert_eq!(r[0].detalhe, "a resposta contem \"eventos\", que nao podia aparecer");
    }

    #[test]
    fn aprova_quando_o_estado_tem_o_que_o_agente_criou() {
        let url = servidor("[{\"nome\":\"E2E Sombras\"}]", "sessao=abc");
        let r = executar(&[verificacao(url, "E2E Sombras")], Some(("sessao", "abc")));
        assert!(r[0].ok, "{r:?}");
    }

    #[test]
    fn reprova_quando_o_estado_nao_tem_o_trecho_e_nao_vaza_o_corpo() {
        let url = servidor("[{\"nome\":\"outra\"}]", "sessao=abc");
        let r = executar(&[verificacao(url, "E2E Sombras")], Some(("sessao", "abc")));
        assert!(!r[0].ok);
        assert_eq!(r[0].detalhe, "a resposta nao contem \"E2E Sombras\"");
        assert!(!r[0].detalhe.contains("outra"));
    }

    #[test]
    fn sem_o_cookie_da_sessao_o_status_denuncia() {
        let url = servidor("[]", "sessao=abc");
        let r = executar(&[verificacao(url, "")], None);
        assert!(!r[0].ok);
        assert_eq!(r[0].detalhe, "esperava 200 e veio 401");
    }

    #[test]
    fn porta_fechada_reprova_sem_resposta() {
        let r = executar(&[verificacao("http://127.0.0.1:9/x".into(), "")], None);
        assert!(!r[0].ok);
        assert!(r[0].detalhe.starts_with("sem resposta"));
    }
}
