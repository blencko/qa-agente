#[derive(Debug, Clone, PartialEq)]
pub struct ResultadoCaminho {
    pub nome: String,
    pub passou: bool,
    pub detalhe: String,
}

type Caminho = Box<dyn Fn() -> Result<(), String>>;

/// Passo 9: executa os happy paths e valida as transicoes criticas, de forma deterministica.
#[derive(Default)]
pub struct RegressionRunner {
    caminhos: Vec<(String, Caminho)>,
}

impl RegressionRunner {
    pub fn adicionar(&mut self, nome: &str, caminho: impl Fn() -> Result<(), String> + 'static) {
        self.caminhos.push((nome.to_string(), Box::new(caminho)));
    }

    pub fn executar(&self) -> Vec<ResultadoCaminho> {
        self.caminhos
            .iter()
            .map(|(nome, caminho)| match caminho() {
                Ok(()) => ResultadoCaminho { nome: nome.clone(), passou: true, detalhe: String::new() },
                Err(detalhe) => ResultadoCaminho { nome: nome.clone(), passou: false, detalhe },
            })
            .collect()
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn separa_caminhos_que_passam_dos_que_falham() {
        let mut r = RegressionRunner::default();
        r.adicionar("login", || Ok(()));
        r.adicionar("pagamento", || Err("saldo nao confere".into()));
        let saida = r.executar();
        assert_eq!(saida.len(), 2);
        assert!(saida[0].passou);
        assert!(!saida[1].passou);
        assert_eq!(saida[1].detalhe, "saldo nao confere");
    }

    #[test]
    fn sem_caminhos_nao_ha_resultado() {
        assert!(RegressionRunner::default().executar().is_empty());
    }
}
