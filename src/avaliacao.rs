use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Veredito {
    pub caso: String,
    pub passou: bool,
    pub motivo: String,
}

type Regra = Box<dyn Fn(&str) -> bool>;

/// Passo 10: assertions, gabarito de negocio e analise de respostas.
#[derive(Default)]
pub struct EvaluationEngine {
    gabarito: HashMap<String, Regra>,
}

impl EvaluationEngine {
    pub fn gabarito(&mut self, caso: &str, regra: impl Fn(&str) -> bool + 'static) {
        self.gabarito.insert(caso.to_string(), Box::new(regra));
    }

    pub fn avaliar(&self, caso: &str, resposta: &str) -> Veredito {
        let (passou, motivo) = match self.gabarito.get(caso) {
            None => (false, "sem gabarito para o caso"),
            Some(regra) if regra(resposta) => (true, ""),
            Some(_) => (false, "resposta fora do gabarito"),
        };
        Veredito { caso: caso.to_string(), passou, motivo: motivo.to_string() }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn aprova_resposta_que_cumpre_o_gabarito() {
        let mut e = EvaluationEngine::default();
        e.gabarito("saldo", |r| r.contains("R$ 10,00"));
        assert!(e.avaliar("saldo", "Seu saldo e R$ 10,00").passou);
    }

    #[test]
    fn reprova_resposta_fora_do_gabarito() {
        let mut e = EvaluationEngine::default();
        e.gabarito("saldo", |r| r.contains("R$ 10,00"));
        let v = e.avaliar("saldo", "nao sei");
        assert!(!v.passou);
        assert_eq!(v.motivo, "resposta fora do gabarito");
    }

    #[test]
    fn caso_sem_gabarito_reprova_em_vez_de_passar_em_silencio() {
        let v = EvaluationEngine::default().avaliar("desconhecido", "qualquer coisa");
        assert!(!v.passou);
    }
}
