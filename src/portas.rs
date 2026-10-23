/// Passos 5 e 6: abre, clica, preenche e observa a aplicacao (HTTP e WebSocket).
///
/// Duas formas de a aplicacao expor o que o agente precisa (ver `navegador_chrome::Modo`): atributos
/// `data-estado` no `body` e `data-acao` nos clicaveis, ou o modo generico, que le titulo e botoes do DOM.
pub trait Navegador {
    fn abrir(&mut self, url: &str) -> Result<(), String>;
    fn clicar(&mut self, acao: &str) -> Result<(), String>;
    fn preencher(&mut self, seletor: &str, texto: &str) -> Result<(), String>;
    fn estado(&mut self) -> Result<String, String>;
    fn clicaveis(&mut self) -> Result<Vec<String>, String>;
    fn texto(&mut self, seletor: &str) -> Result<String, String>;
    fn captura(&mut self) -> Result<Vec<u8>, String>;
    /// Texto visivel da tela para dar contexto ao agente. Vazio quando o navegador nao o expoe: o texto da
    /// aplicacao vai para um LLM (via OmniRoute), entao so sai se o alvo pedir.
    fn contexto(&mut self) -> String {
        String::new()
    }
    /// Sobe de novo um navegador que caiu (mantendo sessao e o que ja foi observado do backend). A tela volta ao
    /// zero: quem chama reabre a URL inicial. Quem nao sabe reiniciar devolve erro.
    fn reiniciar(&mut self) -> Result<(), String> {
        Err("este navegador nao sabe reiniciar".into())
    }
    /// O que o navegador viu do backend e do console ate agora. Quem nao observa devolve vazio.
    fn observacao(&mut self) -> crate::observacao::Observacao {
        crate::observacao::Observacao::default()
    }
}

/// Consumo do LLM numa escolha: o ultimo modelo que respondeu e os tokens somados (incluindo a segunda tentativa).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Uso {
    pub modelo: Option<String>,
    pub tokens: Option<u64>,
}

/// Passo 4: planeja e escolhe a proxima acao da exploracao. O texto da aplicacao e dado nao confiavel:
/// a acao devolvida tem de ser uma das opcoes oferecidas, nunca um comando livre.
pub trait AgenteQA {
    fn planejar(&self, objetivo: &str) -> Result<Vec<String>, String>;
    /// Consumo acumulado desde a ultima leitura (le e zera). Quem nao usa LLM devolve vazio.
    fn uso_do_passo(&self) -> Uso {
        Uso::default()
    }

    /// Igual a `proxima_acao`, com o texto visivel da tela como contexto (ex.: "Falta: nome, classe"). Quem nao usa
    /// contexto herda a versao sem ele.
    fn proxima_acao_com_contexto(
        &self,
        objetivo: &str,
        estado: &str,
        opcoes: &[String],
        historico: &[String],
        _contexto: &str,
    ) -> Result<Option<String>, String> {
        self.proxima_acao(objetivo, estado, opcoes, historico)
    }

    /// `Ok(None)` quando o agente considera o objetivo cumprido.
    fn proxima_acao(
        &self,
        objetivo: &str,
        estado: &str,
        opcoes: &[String],
        historico: &[String],
    ) -> Result<Option<String>, String>;
}
