use std::path::Path;

use serde::Deserialize;

use crate::navegador_chrome::EVITAR_PADRAO;
use crate::sondas::Sonda;
use crate::verificacoes::Verificacao;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SondaAlvo {
    pub nome: String,
    pub url: String,
    #[serde(default = "status_padrao")]
    pub status: u16,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CookieAlvo {
    pub nome: String,
    pub valor: String,
    pub dominio: String,
    /// `false` quando o front le a sessao por JavaScript (`document.cookie`). Padrao: httpOnly.
    #[serde(default = "verdadeiro")]
    pub http_only: bool,
}

fn verdadeiro() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct VerificacaoAlvo {
    pub nome: String,
    pub url: String,
    #[serde(default = "status_padrao")]
    pub status: u16,
    /// Trecho que o corpo da resposta precisa conter (vazio = so confere o status).
    #[serde(default)]
    pub contem: String,
}

/// Um projeto a testar. Um arquivo JSON por alvo: o mesmo `qa-agente` serve ao jogo, ao painel do Vigia,
/// a um painel interno e ao proprio motor de decisao.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Alvo {
    pub nome: String,
    pub url: String,
    pub objetivo: String,
    /// `generico` (le titulo e botoes do DOM) ou `data-acao` (app que expoe `data-estado` e `data-acao`).
    #[serde(default = "modo_padrao")]
    pub modo: String,
    #[serde(default = "evitar_padrao")]
    pub evitar: String,
    #[serde(default = "limite_padrao")]
    pub limite_passos: usize,
    /// Sondas de saude do backend, sempre GET.
    #[serde(default)]
    pub sondas: Vec<SondaAlvo>,
    /// Sessao de teste ja emitida pelo ambiente descartavel do projeto (nunca credencial real).
    #[serde(default)]
    pub cookie: Option<CookieAlvo>,
    /// Manda o texto visivel de cada tela ao LLM como contexto do agente. Desligado por padrao: o texto da aplicacao
    /// passa pelo gateway de LLM, entao so ligue em alvo de teste com dados ficticios.
    #[serde(default)]
    pub contexto_ao_llm: bool,
    /// Arquivo de log do servidor do projeto: so as linhas gravadas durante o QA sao lidas.
    #[serde(default)]
    pub log_servidor: Option<String>,
    /// Checagens GET somente-leitura do estado do backend depois da jornada (usam o cookie do alvo).
    #[serde(default)]
    pub verificacoes: Vec<VerificacaoAlvo>,
    /// Rotulos (separados por `|`) que contam como ACAO DE ESCRITA, ex.: `aprovar|reprovar|salvar`. Com `max_escritas`,
    /// vale para qualquer escolhedor (LLM, politica, fallback): o objetivo sozinho so restringe o LLM.
    #[serde(default)]
    pub acoes_de_escrita: String,
    /// Maximo de acoes de escrita por rodada (0 = sem limite).
    #[serde(default)]
    pub max_escritas: usize,
    /// Logs do projeto (back, BFF, front...) lidos a CADA passo: erro grave novo vira achado ligado ao passo que o causou.
    /// `log_servidor` continua valendo para o relatorio final.
    #[serde(default)]
    pub logs: Vec<LogAlvo>,
    /// Consultas SQL somente-leitura rodadas antes e depois da jornada (estado gravado, por criterio de aceite).
    #[serde(default)]
    pub consultas_sql: Vec<ConsultaSqlAlvo>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LogAlvo {
    pub nome: String,
    pub caminho: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ConsultaSqlAlvo {
    pub nome: String,
    pub sql: String,
    #[serde(default = "container_padrao")]
    pub container: String,
    #[serde(default)]
    pub banco: String,
    /// Trecho que o resultado DEPOIS da jornada precisa conter (vazio = so registra antes e depois).
    #[serde(default)]
    pub depois_contem: String,
}

fn container_padrao() -> String {
    "mysql-db".into()
}

fn status_padrao() -> u16 {
    200
}
fn modo_padrao() -> String {
    "generico".into()
}
fn evitar_padrao() -> String {
    EVITAR_PADRAO.into()
}
fn limite_padrao() -> usize {
    15
}

impl Alvo {
    pub fn ler(caminho: &Path) -> Result<Alvo, String> {
        let texto = std::fs::read_to_string(caminho).map_err(|e| format!("{}: {e}", caminho.display()))?;
        Self::de_json(&texto)
    }

    pub fn de_json(texto: &str) -> Result<Alvo, String> {
        let alvo: Alvo = serde_json::from_str(texto).map_err(|e| format!("alvo invalido: {e}"))?;
        if !matches!(alvo.modo.as_str(), "generico" | "data-acao") {
            return Err(format!("modo desconhecido: {:?} (use generico ou data-acao)", alvo.modo));
        }
        if alvo.nome.trim().is_empty() || !alvo.nome.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("nome do alvo so aceita letras, numeros, - e _ (vira nome de pasta)".into());
        }
        // consulta que escreve recusa o alvo inteiro, antes de qualquer coisa rodar
        for c in &alvo.consultas_sql {
            crate::consultas_sql::somente_leitura(&c.sql).map_err(|e| format!("consulta {:?}: {e}", c.nome))?;
        }
        Ok(alvo)
    }

    pub fn consultas(&self) -> Vec<crate::consultas_sql::ConsultaSql> {
        self.consultas_sql
            .iter()
            .map(|c| crate::consultas_sql::ConsultaSql {
                nome: c.nome.clone(),
                sql: c.sql.clone(),
                container: c.container.clone(),
                banco: c.banco.clone(),
                depois_contem: c.depois_contem.clone(),
            })
            .collect()
    }

    pub fn sondas(&self) -> Vec<Sonda> {
        self.sondas
            .iter()
            .map(|s| Sonda { nome: s.nome.clone(), url: s.url.clone(), status_esperado: s.status })
            .collect()
    }

    pub fn verificacoes(&self) -> Vec<Verificacao> {
        self.verificacoes
            .iter()
            .map(|v| Verificacao {
                nome: v.nome.clone(),
                url: v.url.clone(),
                status_esperado: v.status,
                contem: v.contem.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    const MINIMO: &str = r#"{"nome":"jogo","url":"http://127.0.0.1:1/","objetivo":"explorar"}"#;

    #[test]
    fn alvo_com_consulta_que_escreve_e_recusado_e_logs_e_sql_tem_padroes() {
        let ruim = r#"{"nome":"a","url":"u","objetivo":"o","consultas_sql":[{"nome":"x","sql":"delete from t"}]}"#;
        assert!(Alvo::de_json(ruim).unwrap_err().contains("consulta"), "deveria recusar o alvo inteiro");
        let bom = r#"{"nome":"a","url":"u","objetivo":"o","logs":[{"nome":"back","caminho":"/x.log"}],
            "consultas_sql":[{"nome":"q","sql":"select 1","depois_contem":"1"}]}"#;
        let a = Alvo::de_json(bom).unwrap();
        assert_eq!(a.logs[0].nome, "back");
        assert_eq!(a.consultas()[0].container, "mysql-db");
        assert_eq!(a.consultas()[0].depois_contem, "1");
    }

    #[test]
    fn alvo_minimo_ganha_os_padroes() {
        let a = Alvo::de_json(MINIMO).unwrap();
        assert_eq!(a.modo, "generico");
        assert_eq!(a.limite_passos, 15);
        assert_eq!(a.evitar, EVITAR_PADRAO);
        assert!(a.sondas.is_empty() && a.cookie.is_none());
        assert!(a.log_servidor.is_none() && a.verificacoes.is_empty());
    }

    #[test]
    fn verificacao_e_log_do_servidor_vem_do_json() {
        let a = Alvo::de_json(
            r#"{"nome":"jogo","url":"http://x/","objetivo":"o","log_servidor":"C:/logs/servidor.log",
                "verificacoes":[{"nome":"campanha criada","url":"http://x/api/campanhas","contem":"E2E"}]}"#,
        )
        .unwrap();
        assert_eq!(a.log_servidor.as_deref(), Some("C:/logs/servidor.log"));
        let v = a.verificacoes();
        assert_eq!((v[0].status_esperado, v[0].contem.as_str()), (200, "E2E"));
    }

    #[test]
    fn sondas_viram_get_com_status_esperado() {
        let a = Alvo::de_json(
            r#"{"nome":"api","url":"http://x/","objetivo":"o","sondas":[{"nome":"saude","url":"http://x/saude"},{"nome":"v","url":"http://x/v","status":204}]}"#,
        )
        .unwrap();
        let s = a.sondas();
        assert_eq!((s[0].status_esperado, s[1].status_esperado), (200, 204));
    }

    #[test]
    fn recusa_modo_desconhecido_nome_perigoso_e_limite_zero_vale_sem_limite() {
        assert!(Alvo::de_json(r#"{"nome":"a","url":"u","objetivo":"o","modo":"magico"}"#).is_err());
        assert!(Alvo::de_json(r#"{"nome":"../fora","url":"u","objetivo":"o"}"#).is_err());
        // 0 = sem limite de passos (o agente decide quando parar)
        assert_eq!(Alvo::de_json(r#"{"nome":"a","url":"u","objetivo":"o","limite_passos":0}"#).unwrap().limite_passos, 0);
    }

    #[test]
    fn campo_obrigatorio_ausente_e_erro() {
        assert!(Alvo::de_json(r#"{"nome":"a","url":"u"}"#).is_err());
    }
}
