use std::time::Duration;

/// Sonda de saude do backend: sempre GET, nunca autenticacao, criacao nem outro verbo.
#[derive(Debug, Clone, PartialEq)]
pub struct Sonda {
    pub nome: String,
    pub url: String,
    pub status_esperado: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultadoSonda {
    pub nome: String,
    pub status: Option<u16>,
    pub ok: bool,
    pub detalhe: String,
}

pub fn executar(sondas: &[Sonda]) -> Vec<ResultadoSonda> {
    sondas.iter().map(sondar).collect()
}

fn sondar(sonda: &Sonda) -> ResultadoSonda {
    let resposta = ureq::get(&sonda.url).timeout(Duration::from_secs(10)).call();
    let (status, detalhe) = match resposta {
        Ok(r) => (Some(r.status()), String::new()),
        // Um 500 tambem prova que o processo esta de pe: o status conta, e o criterio e o esperado.
        Err(ureq::Error::Status(codigo, _)) => (Some(codigo), String::new()),
        Err(e) => (None, e.to_string()),
    };
    let ok = status == Some(sonda.status_esperado);
    let detalhe = match (ok, status) {
        (true, _) => detalhe,
        (false, Some(s)) => format!("esperava {} e veio {s}", sonda.status_esperado),
        (false, None) => detalhe,
    };
    ResultadoSonda { nome: sonda.nome.clone(), status, ok, detalhe }
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Servidor de uma resposta so, em porta efemera (nunca a porta default de um servico real).
    fn servidor(status_linha: &'static str) -> String {
        let escuta = TcpListener::bind("127.0.0.1:0").unwrap();
        let porta = escuta.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut c, _)) = escuta.accept() {
                let mut buf = [0u8; 1024];
                let _ = c.read(&mut buf);
                let _ = write!(c, "HTTP/1.1 {status_linha}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            }
        });
        format!("http://127.0.0.1:{porta}/saude")
    }

    fn sonda(url: String, esperado: u16) -> Sonda {
        Sonda { nome: "saude".into(), url, status_esperado: esperado }
    }

    #[test]
    fn aprova_quando_o_status_e_o_esperado() {
        let r = executar(&[sonda(servidor("200 OK"), 200)]);
        assert!(r[0].ok, "{r:?}");
        assert_eq!(r[0].status, Some(200));
    }

    #[test]
    fn reprova_quando_o_status_difere_e_diz_qual() {
        let r = executar(&[sonda(servidor("503 Service Unavailable"), 200)]);
        assert!(!r[0].ok);
        assert_eq!(r[0].detalhe, "esperava 200 e veio 503");
    }

    #[test]
    fn porta_fechada_reprova_sem_status() {
        let r = executar(&[sonda("http://127.0.0.1:9/saude".into(), 200)]);
        assert!(!r[0].ok);
        assert_eq!(r[0].status, None);
    }
}
