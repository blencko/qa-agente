use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrigemGatilho {
    Deploy,
    Manual,
    Mudanca,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acao {
    Executar,
    Pular,
    Bloquear,
}

impl Acao {
    pub const TODAS: [&'static str; 3] = ["executar", "pular", "bloquear"];

    pub fn como_str(self) -> &'static str {
        match self {
            Acao::Executar => "executar",
            Acao::Pular => "pular",
            Acao::Bloquear => "bloquear",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Gatilho {
    pub origem: OrigemGatilho,
    pub versao: String,
    pub impacto: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct ResultadoValido {
    pub versao: String,
    pub impacto: BTreeSet<String>,
    pub passou: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Orcamento {
    pub limite: f64,
    pub gasto: f64,
}

impl Orcamento {
    pub fn restante(&self) -> f64 {
        self.limite - self.gasto
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decisao {
    pub acao: Acao,
    pub motivo: String,
    pub opiniao_motor: Option<String>,
    pub divergiu_do_motor: bool,
    pub motor_indisponivel: bool,
    /// Havia resultado valido para pular, mas a auditoria mandou executar para medir o falso-pular.
    pub auditoria_do_pular: bool,
}

impl Decisao {
    pub fn nova(acao: Acao, motivo: impl Into<String>) -> Self {
        Decisao {
            acao,
            motivo: motivo.into(),
            opiniao_motor: None,
            divergiu_do_motor: false,
            motor_indisponivel: false,
            auditoria_do_pular: false,
        }
    }
}
