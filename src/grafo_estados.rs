use std::collections::{BTreeMap, BTreeSet};

type Chave = (String, String);

/// Passos 7 e 8: maquina de estados por versao, com reuso do conhecimento das versoes anteriores.
#[derive(Default)]
pub struct VersionedStateGraph {
    transicoes: BTreeMap<String, BTreeMap<Chave, String>>,
    ordem: Vec<String>,
}

impl VersionedStateGraph {
    pub fn registrar(&mut self, versao: &str, origem: &str, acao: &str, destino: &str) -> Result<(), String> {
        if !self.ordem.iter().any(|v| v == versao) {
            self.ordem.push(versao.to_string());
        }
        let da_versao = self.transicoes.entry(versao.to_string()).or_default();
        let chave = (origem.to_string(), acao.to_string());
        match da_versao.get(&chave) {
            Some(atual) if atual != destino => Err(format!(
                "transicao ambigua em {versao}: {origem} --{acao}--> {atual} e {destino}"
            )),
            _ => {
                da_versao.insert(chave, destino.to_string());
                Ok(())
            }
        }
    }

    pub fn destino(&self, versao: &str, origem: &str, acao: &str) -> Option<&str> {
        self.transicoes
            .get(versao)?
            .get(&(origem.to_string(), acao.to_string()))
            .map(String::as_str)
    }

    pub fn conhecido_na_anterior(&self, versao: &str, origem: &str, acao: &str) -> Option<&str> {
        let indice = self.ordem.iter().position(|v| v == versao)?;
        self.ordem[..indice]
            .iter()
            .rev()
            .find_map(|anterior| self.destino(anterior, origem, acao))
    }

    pub fn novas_em(&self, versao: &str) -> BTreeSet<(String, String, String)> {
        let Some(da_versao) = self.transicoes.get(versao) else { return BTreeSet::new() };
        da_versao
            .iter()
            .filter(|((o, a), d)| self.conhecido_na_anterior(versao, o, a) != Some(d.as_str()))
            .map(|((o, a), d)| (o.clone(), a.clone(), d.clone()))
            .collect()
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn reaproveita_transicao_da_versao_anterior() {
        let mut g = VersionedStateGraph::default();
        g.registrar("v1", "inicio", "abrir_chat", "chat").unwrap();
        g.registrar("v2", "inicio", "abrir_chat", "chat").unwrap();
        assert_eq!(g.conhecido_na_anterior("v2", "inicio", "abrir_chat"), Some("chat"));
        assert!(g.novas_em("v2").is_empty());
    }

    #[test]
    fn transicao_que_mudou_de_destino_conta_como_nova() {
        let mut g = VersionedStateGraph::default();
        g.registrar("v1", "inicio", "abrir_chat", "chat").unwrap();
        g.registrar("v2", "inicio", "abrir_chat", "chat_novo").unwrap();
        assert_eq!(g.novas_em("v2").len(), 1);
    }

    #[test]
    fn recusa_destino_ambiguo_na_mesma_versao() {
        let mut g = VersionedStateGraph::default();
        g.registrar("v1", "a", "x", "b").unwrap();
        assert!(g.registrar("v1", "a", "x", "c").is_err());
        assert_eq!(g.destino("v1", "a", "x"), Some("b"));
    }

    #[test]
    fn versao_sem_anterior_nao_tem_conhecimento_herdado() {
        let mut g = VersionedStateGraph::default();
        g.registrar("v1", "a", "x", "b").unwrap();
        assert_eq!(g.conhecido_na_anterior("v1", "a", "x"), None);
        assert_eq!(g.novas_em("v1").len(), 1);
    }
}
