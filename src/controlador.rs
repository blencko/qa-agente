use std::cell::Cell;

use crate::modelos::{Acao, Decisao, Gatilho, Orcamento, OrigemGatilho, ResultadoValido};
use crate::motor::{ConsultaMotor, MotorIndisponivel};

pub const TIPO: &str = "decisao-de-execucao-qa";

/// Passos 2 e 3: decide executar, pular ou bloquear. A regra decide; o motor e contraponto.
pub struct ExecutionController<'a> {
    resultados: Vec<ResultadoValido>,
    custo_execucao: f64,
    motor: Option<&'a dyn ConsultaMotor>,
    auditar_a_cada: Option<usize>,
    pulos_decididos: Cell<usize>,
}

impl<'a> ExecutionController<'a> {
    pub fn new(
        resultados: Vec<ResultadoValido>,
        custo_execucao: f64,
        motor: Option<&'a dyn ConsultaMotor>,
    ) -> Self {
        ExecutionController { resultados, custo_execucao, motor, auditar_a_cada: None, pulos_decididos: Cell::new(0) }
    }

    /// Pular um teste que falharia e o erro caro da selecao de testes, e a taxa de pulo medida sem checagem engana
    /// (um benchmark deu 73% de pulos quando o valor real era cerca de 21%). Com a auditoria ligada, todo
    /// `a_cada`-esimo pulo vira execucao: comparar esse resultado com o que o pulo teria assumido mede o falso-pular.
    /// `a_cada` 0 desliga.
    pub fn auditando_o_pular(mut self, a_cada: usize) -> Self {
        self.auditar_a_cada = (a_cada > 0).then_some(a_cada);
        self
    }

    pub fn decidir(&self, gatilho: &Gatilho, orcamento: &Orcamento) -> Decisao {
        let mut decisao = self.regra(gatilho, orcamento);
        let Some(motor) = self.motor else { return decisao };
        match motor.opiniao(TIPO, "geral", &Acao::TODAS, decisao.acao.como_str()) {
            Err(MotorIndisponivel(_)) => decisao.motor_indisponivel = true,
            Ok(op) if op.ha_veredito => {
                decisao.divergiu_do_motor = op.preferida.as_deref() != Some(decisao.acao.como_str());
                decisao.opiniao_motor = op.preferida;
            }
            Ok(_) => {}
        }
        decisao
    }

    fn regra(&self, gatilho: &Gatilho, orcamento: &Orcamento) -> Decisao {
        if gatilho.origem == OrigemGatilho::Manual {
            return self.por_orcamento(orcamento, "execucao manual pedida");
        }
        if self.reaproveitavel(gatilho) {
            let n = self.pulos_decididos.get() + 1;
            self.pulos_decididos.set(n);
            if self.auditar_a_cada.is_some_and(|a| n % a == 0) {
                let mut d = self.por_orcamento(
                    orcamento,
                    "auditoria do pular: havia resultado valido, mas executa para medir o falso-pular",
                );
                d.auditoria_do_pular = d.acao == Acao::Executar;
                return d;
            }
            return Decisao::nova(Acao::Pular, "resultado valido para a mesma versao e o mesmo impacto");
        }
        self.por_orcamento(orcamento, "sem resultado valido para esta versao e impacto")
    }

    fn reaproveitavel(&self, gatilho: &Gatilho) -> bool {
        self.resultados.iter().any(|r| {
            r.passou && r.versao == gatilho.versao && gatilho.impacto.is_subset(&r.impacto)
        })
    }

    fn por_orcamento(&self, orcamento: &Orcamento, motivo: &str) -> Decisao {
        if orcamento.restante() < self.custo_execucao {
            return Decisao::nova(
                Acao::Bloquear,
                format!("{motivo}, mas o orcamento restante nao cobre o custo"),
            );
        }
        Decisao::nova(Acao::Executar, motivo)
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::motor::Opiniao;
    use std::collections::BTreeSet;

    fn conjunto(itens: &[&str]) -> BTreeSet<String> {
        itens.iter().map(|s| s.to_string()).collect()
    }

    fn gatilho(origem: OrigemGatilho, versao: &str, impacto: &[&str]) -> Gatilho {
        Gatilho { origem, versao: versao.into(), impacto: conjunto(impacto) }
    }

    fn resultado(versao: &str, impacto: &[&str], passou: bool) -> ResultadoValido {
        ResultadoValido { versao: versao.into(), impacto: conjunto(impacto), passou }
    }

    const FOLGA: Orcamento = Orcamento { limite: 10.0, gasto: 0.0 };

    struct MotorFalso(Result<Opiniao, ()>);

    impl ConsultaMotor for MotorFalso {
        fn opiniao(&self, _: &str, _: &str, _: &[&str], _: &str) -> Result<Opiniao, MotorIndisponivel> {
            self.0.clone().map_err(|_| MotorIndisponivel("fora".into()))
        }
    }

    fn opiniao(preferida: Option<&str>) -> Opiniao {
        Opiniao {
            ha_veredito: preferida.is_some(),
            preferida: preferida.map(String::from),
            forca: "forte".into(),
            resumo: String::new(),
        }
    }

    #[test]
    fn pula_quando_ha_resultado_valido_que_cobre_o_impacto() {
        let c = ExecutionController::new(vec![resultado("v1", &["login", "chat"], true)], 1.0, None);
        let d = c.decidir(&gatilho(OrigemGatilho::Deploy, "v1", &["login"]), &FOLGA);
        assert_eq!(d.acao, Acao::Pular);
    }

    #[test]
    fn auditoria_transforma_todo_enesimo_pulo_em_execucao() {
        let c = ExecutionController::new(vec![resultado("v1", &["login"], true)], 1.0, None).auditando_o_pular(3);
        let g = gatilho(OrigemGatilho::Deploy, "v1", &["login"]);
        let acoes: Vec<(Acao, bool)> = (0..6)
            .map(|_| {
                let d = c.decidir(&g, &FOLGA);
                (d.acao, d.auditoria_do_pular)
            })
            .collect();
        assert_eq!(
            acoes,
            vec![
                (Acao::Pular, false),
                (Acao::Pular, false),
                (Acao::Executar, true),
                (Acao::Pular, false),
                (Acao::Pular, false),
                (Acao::Executar, true),
            ]
        );
    }

    #[test]
    fn auditoria_zero_desliga_e_sem_orcamento_nao_marca_auditoria() {
        let sem = ExecutionController::new(vec![resultado("v1", &["x"], true)], 1.0, None).auditando_o_pular(0);
        let g = gatilho(OrigemGatilho::Deploy, "v1", &["x"]);
        assert!((0..5).all(|_| sem.decidir(&g, &FOLGA).acao == Acao::Pular));
        let curto = Orcamento { limite: 1.0, gasto: 1.0 };
        let c = ExecutionController::new(vec![resultado("v1", &["x"], true)], 1.0, None).auditando_o_pular(1);
        let d = c.decidir(&g, &curto);
        assert_eq!(d.acao, Acao::Bloquear);
        assert!(!d.auditoria_do_pular);
    }

    #[test]
    fn executa_quando_o_resultado_anterior_falhou() {
        let c = ExecutionController::new(vec![resultado("v1", &["login"], false)], 1.0, None);
        let d = c.decidir(&gatilho(OrigemGatilho::Deploy, "v1", &["login"]), &FOLGA);
        assert_eq!(d.acao, Acao::Executar);
    }

    #[test]
    fn executa_quando_a_versao_mudou() {
        let c = ExecutionController::new(vec![resultado("v1", &["login"], true)], 1.0, None);
        let d = c.decidir(&gatilho(OrigemGatilho::Mudanca, "v2", &["login"]), &FOLGA);
        assert_eq!(d.acao, Acao::Executar);
    }

    #[test]
    fn executa_quando_o_impacto_nao_esta_coberto() {
        let c = ExecutionController::new(vec![resultado("v1", &["login"], true)], 1.0, None);
        let d = c.decidir(&gatilho(OrigemGatilho::Mudanca, "v1", &["login", "pagamento"]), &FOLGA);
        assert_eq!(d.acao, Acao::Executar);
    }

    #[test]
    fn execucao_manual_ignora_o_cache() {
        let c = ExecutionController::new(vec![resultado("v1", &["login"], true)], 1.0, None);
        let d = c.decidir(&gatilho(OrigemGatilho::Manual, "v1", &["login"]), &FOLGA);
        assert_eq!(d.acao, Acao::Executar);
    }

    #[test]
    fn bloqueia_quando_o_orcamento_nao_cobre_o_custo() {
        let c = ExecutionController::new(vec![], 5.0, None);
        let curto = Orcamento { limite: 10.0, gasto: 8.0 };
        let d = c.decidir(&gatilho(OrigemGatilho::Deploy, "v1", &[]), &curto);
        assert_eq!(d.acao, Acao::Bloquear);
    }

    #[test]
    fn registra_divergencia_do_motor_sem_trocar_a_decisao() {
        let motor = MotorFalso(Ok(opiniao(Some("pular"))));
        let c = ExecutionController::new(vec![], 1.0, Some(&motor));
        let d = c.decidir(&gatilho(OrigemGatilho::Deploy, "v1", &[]), &FOLGA);
        assert_eq!(d.acao, Acao::Executar);
        assert!(d.divergiu_do_motor);
        assert_eq!(d.opiniao_motor.as_deref(), Some("pular"));
    }

    #[test]
    fn motor_sem_veredito_nao_diverge() {
        let motor = MotorFalso(Ok(opiniao(None)));
        let c = ExecutionController::new(vec![], 1.0, Some(&motor));
        let d = c.decidir(&gatilho(OrigemGatilho::Deploy, "v1", &[]), &FOLGA);
        assert!(!d.divergiu_do_motor);
        assert!(d.opiniao_motor.is_none());
    }

    #[test]
    fn motor_fora_do_ar_nao_derruba_a_decisao() {
        let motor = MotorFalso(Err(()));
        let c = ExecutionController::new(vec![], 1.0, Some(&motor));
        let d = c.decidir(&gatilho(OrigemGatilho::Deploy, "v1", &[]), &FOLGA);
        assert_eq!(d.acao, Acao::Executar);
        assert!(d.motor_indisponivel);
    }
}
