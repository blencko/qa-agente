use crate::avaliacao::Veredito;
use crate::observacao::ResumoBackend;
use crate::regressao::ResultadoCaminho;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Evidencias {
    pub screenshots: Vec<String>,
    pub logs: Vec<String>,
    pub trace: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Relatorio {
    pub versao: String,
    pub caminhos: usize,
    pub vereditos: usize,
    pub falhas: Vec<String>,
    pub passou: bool,
    pub evidencias: Evidencias,
    pub backend: ResumoBackend,
}

/// Passo 11: resultado consolidado com as evidencias, sem observacao do backend.
pub fn consolidar(
    versao: &str,
    caminhos: &[ResultadoCaminho],
    vereditos: &[Veredito],
    evidencias: Evidencias,
) -> Relatorio {
    consolidar_com_backend(versao, caminhos, vereditos, evidencias, ResumoBackend::default())
}

/// Passo 11 olhando tambem o backend: erro 5xx ou requisicao que nem chegou reprova o relatorio, mesmo
/// com todos os caminhos de tela verdes.
pub fn consolidar_com_backend(
    versao: &str,
    caminhos: &[ResultadoCaminho],
    vereditos: &[Veredito],
    evidencias: Evidencias,
    backend: ResumoBackend,
) -> Relatorio {
    let mut falhas: Vec<String> = caminhos
        .iter()
        .filter(|c| !c.passou)
        .map(|c| c.nome.clone())
        .chain(vereditos.iter().filter(|v| !v.passou).map(|v| v.caso.clone()))
        .collect();
    falhas.extend(backend.erros_5xx.iter().map(|e| format!("backend 5xx: {e}")));
    falhas.extend(backend.falhas_de_rede.iter().map(|e| format!("requisicao sem resposta: {e}")));
    falhas.extend(backend.erros_ws.iter().map(|e| format!("websocket: {e}")));
    falhas.extend(backend.log_graves.iter().map(|e| format!("log do servidor: {e}")));
    Relatorio {
        versao: versao.to_string(),
        caminhos: caminhos.len(),
        vereditos: vereditos.len(),
        passou: falhas.is_empty(),
        falhas,
        evidencias,
        backend,
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn caminho(nome: &str, passou: bool) -> ResultadoCaminho {
        ResultadoCaminho { nome: nome.into(), passou, detalhe: String::new() }
    }

    fn veredito(caso: &str, passou: bool) -> Veredito {
        Veredito { caso: caso.into(), passou, motivo: String::new() }
    }

    #[test]
    fn relatorio_passa_so_sem_nenhuma_falha() {
        let r = consolidar("v1", &[caminho("a", true)], &[veredito("x", true)], Evidencias::default());
        assert!(r.passou);
        assert!(r.falhas.is_empty());
    }

    #[test]
    fn erro_5xx_do_backend_reprova_mesmo_com_as_telas_verdes() {
        let backend = ResumoBackend { erros_5xx: vec!["500 http://a/api/turno".into()], ..ResumoBackend::default() };
        let r = consolidar_com_backend("v1", &[caminho("a", true)], &[], Evidencias::default(), backend);
        assert!(!r.passou);
        assert_eq!(r.falhas, vec!["backend 5xx: 500 http://a/api/turno".to_string()]);
    }

    #[test]
    fn so_4xx_e_erro_de_console_nao_reprovam() {
        let backend = ResumoBackend {
            erros_4xx: vec!["404 http://a/x".into()],
            erros_console: vec!["aviso".into()],
            ..ResumoBackend::default()
        };
        let r = consolidar_com_backend("v1", &[caminho("a", true)], &[], Evidencias::default(), backend);
        assert!(r.passou);
    }

    #[test]
    fn falha_de_caminho_ou_de_veredito_reprova_o_relatorio() {
        let r = consolidar(
            "v1",
            &[caminho("a", false)],
            &[veredito("x", false), veredito("y", true)],
            Evidencias::default(),
        );
        assert!(!r.passou);
        assert_eq!(r.falhas, vec!["a".to_string(), "x".to_string()]);
    }
}
