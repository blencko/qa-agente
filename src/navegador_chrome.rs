use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use headless_chrome::protocol::cdp::types::Event;
use headless_chrome::protocol::cdp::Log::LogEntryLevel;
use headless_chrome::protocol::cdp::Network::CookieParam;
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::{Browser, LaunchOptions, Tab};

use crate::observacao::{evento_socketio, Coletor, FrameWs, Observacao, RespostaHttp};
use crate::portas::Navegador;

/// Janela de desktop. A padrao do Chrome headless e de celular (cerca de 800x600): os apps responsivos trocam o menu por
/// um hamburguer e escondem abas, e o agente passa a testar um layout que ninguem usa no desktop.
pub const JANELA_PADRAO: (u32, u32) = (1366, 900);

/// Rotulos que o modo generico nunca clica: acoes destrutivas ou que encerram a sessao.
pub const EVITAR_PADRAO: &str = "excluir|apagar|deletar|remover|sair|logout|encerrar|delete|remove";

/// Como a aplicacao expoe estado e acoes ao agente.
#[derive(Debug, Clone)]
pub enum Modo {
    /// `data-estado` no `body` e `data-acao` nos clicaveis (aplicacao feita para ser testada).
    DataAcao,
    /// Aplicacao qualquer: estado = caminho + primeiro titulo; acoes = botoes, links e abas visiveis, por rotulo.
    Generico { evitar: String },
}

/// Passos 5 e 6 com Chrome de verdade (CDP). O navegador sobe em modo headless e morre com a struct.
pub struct ChromeNavegador {
    _navegador: Browser,
    aba: Arc<Tab>,
    modo: Modo,
    coletor: Coletor,
    enviar_contexto: bool,
    executavel: Option<PathBuf>,
    /// Cookie de sessao a reaplicar quando o navegador reinicia.
    cookie: Option<(String, String, String, bool)>,
}

fn erro(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Liga a coleta do que o backend respondeu (todo status HTTP), do que nem chegou a responder e dos erros de console.
fn observar(aba: &Arc<Tab>, coletor: &Coletor) -> Result<(), String> {
    let respostas = Arc::clone(coletor);
    aba.register_response_handling(
        "qa-agente",
        Box::new(move |evento, _| {
            if let Ok(mut o) = respostas.lock() {
                o.respostas.push(RespostaHttp {
                    url: evento.response.url.clone(),
                    status: evento.response.status as u32,
                    mime: evento.response.mime_type.clone(),
                });
            }
        }),
    )
    .map_err(erro)?;
    aba.enable_log().map_err(erro)?;
    let eventos = Arc::clone(coletor);
    aba.add_event_listener(Arc::new(move |evento: &Event| {
        let Ok(mut o) = eventos.lock() else { return };
        match evento {
            Event::NetworkLoadingFailed(f) if f.params.canceled != Some(true) => {
                o.falhas_de_rede.push(f.params.error_text.clone());
            }
            Event::LogEntryAdded(e) if e.params.entry.level == LogEntryLevel::Error => {
                o.erros_console.push(e.params.entry.text.clone());
            }
            Event::RuntimeExceptionThrown(e) => o.erros_console.push(e.params.exception_details.text.clone()),
            Event::NetworkWebSocketFrameSent(f) => o.frames_ws.push(frame_ws(true, &f.params.response.payload_data)),
            Event::NetworkWebSocketFrameReceived(f) => o.frames_ws.push(frame_ws(false, &f.params.response.payload_data)),
            Event::NetworkWebSocketFrameError(f) => o.erros_ws.push(f.params.error_message.clone()),
            _ => {}
        }
    }))
    .map_err(erro)?;
    Ok(())
}

fn literal(texto: &str) -> Result<String, String> {
    serde_json::to_string(texto).map_err(erro)
}

const ROTULO_JS: &str = "const rotulo = e => (e.getAttribute('aria-label') || e.textContent || '')\
    .replace(/\\s+/g, ' ').trim().slice(0, 60);\
    const visivel = e => e.offsetParent !== null && !e.disabled;\
    const alvos = () => document.querySelectorAll('button, a[href], [role=button], [role=tab], [role=checkbox],\
        [role=radio], [role=option], [role=switch], [role=menuitem], summary');\
    const aparece = e => e.getClientRects().length > 0 && !e.disabled;\
    const marcaveis = () => [...document.querySelectorAll('input[type=checkbox], input[type=radio]')].filter(aparece);\
    const listas = () => [...document.querySelectorAll('select')].filter(aparece);\
    const camposVisiveis = () => [...document.querySelectorAll('input, textarea')].filter(e => {\
        const t = (e.type || '').toLowerCase();\
        return !['hidden','checkbox','radio','submit','button','file','password'].includes(t) && visivel(e) && !e.readOnly; });\
    const semControles = l => { const c = l.cloneNode(true);\
        c.querySelectorAll('select, input, textarea, option, button').forEach(x => x.remove()); return c.textContent; };\
    const rotuloCampo = (e, i) => ((e.getAttribute('aria-label') || e.placeholder\
        || (e.labels && e.labels[0] && semControles(e.labels[0])) || e.getAttribute('title') || e.name || e.id\
        || (e.closest('label') && semControles(e.closest('label')))\
        || (e.previousElementSibling && semControles(e.previousElementSibling)) || '')\
        .replace(/\\s+/g, ' ').trim().slice(0, 60)) || ('campo ' + (i + 1));";

impl ChromeNavegador {
    pub fn novo(executavel: Option<PathBuf>) -> Result<Self, String> {
        Self::subir(executavel, Modo::DataAcao)
    }

    pub fn novo_generico(executavel: Option<PathBuf>, evitar: &str) -> Result<Self, String> {
        Self::subir(executavel, Modo::Generico { evitar: evitar.to_string() })
    }

    /// Liga o envio do texto visivel da tela ao agente (e, por ele, ao LLM). Desligado por padrao: so para alvo
    /// cujo conteudo pode ir ao gateway de LLM.
    pub fn com_contexto(mut self, ligado: bool) -> Self {
        self.enviar_contexto = ligado;
        self
    }

    /// Injeta um cookie de sessao (ex.: a sessao de teste do ambiente E2E do jogo).
    pub fn definir_cookie(&mut self, nome: &str, valor: &str, dominio: &str) -> Result<(), String> {
        self.definir_cookie_com(nome, valor, dominio, true)
    }

    /// Igual a `definir_cookie`; `http_only = false` deixa o cookie visivel ao JavaScript da pagina, que e como
    /// alguns fronts leem a sessao (`document.cookie`).
    pub fn definir_cookie_com(&mut self, nome: &str, valor: &str, dominio: &str, http_only: bool) -> Result<(), String> {
        self.cookie = Some((nome.to_string(), valor.to_string(), dominio.to_string(), http_only));
        self.aplicar_cookie()
    }

    fn aplicar_cookie(&self) -> Result<(), String> {
        let Some((nome, valor, dominio, http_only)) = &self.cookie else { return Ok(()) };
        let cookie = CookieParam {
            name: nome.to_string(),
            value: valor.to_string(),
            domain: Some(dominio.to_string()),
            path: Some("/".to_string()),
            secure: Some(false),
            http_only: Some(*http_only),
            url: None,
            same_site: None,
            expires: None,
            priority: None,
            same_party: None,
            source_scheme: None,
            source_port: None,
            partition_key: None,
        };
        self.aba.set_cookies(vec![cookie]).map_err(erro)
    }

    fn subir(executavel: Option<PathBuf>, modo: Modo) -> Result<Self, String> {
        let opcoes = LaunchOptions::default_builder()
            .headless(true)
            .window_size(Some(JANELA_PADRAO))
            .idle_browser_timeout(Duration::from_secs(900))
            .path(executavel.clone())
            .build()
            .map_err(erro)?;
        let navegador = Browser::new(opcoes).map_err(erro)?;
        let aba = navegador.new_tab().map_err(erro)?;
        let coletor: Coletor = Arc::new(Mutex::new(Observacao::default()));
        observar(&aba, &coletor)?;
        Ok(ChromeNavegador { _navegador: navegador, aba, modo, coletor, enviar_contexto: false, executavel, cookie: None })
    }

    /// Sobe um Chrome novo no lugar do que caiu. Mantem o mesmo coletor de backend (o relatorio nao perde o que ja
    /// foi visto), a sessao (cookie) e o modo. A tela volta ao zero: quem chama reabre a URL inicial.
    fn recriar(&mut self) -> Result<(), String> {
        let opcoes = LaunchOptions::default_builder()
            .headless(true)
            .window_size(Some(JANELA_PADRAO))
            .idle_browser_timeout(Duration::from_secs(900))
            .path(self.executavel.clone())
            .build()
            .map_err(erro)?;
        let navegador = Browser::new(opcoes).map_err(erro)?;
        let aba = navegador.new_tab().map_err(erro)?;
        observar(&aba, &self.coletor)?;
        self._navegador = navegador; // o Chrome antigo morre aqui, ao ser solto
        self.aba = aba;
        self.aplicar_cookie()
    }

    fn avaliar_texto(&self, js: &str) -> Result<String, String> {
        let objeto = self.aba.evaluate(js, false).map_err(erro)?;
        objeto
            .value
            .and_then(|v| v.as_str().map(String::from))
            .ok_or_else(|| "o script nao devolveu texto".to_string())
    }
}

impl Navegador for ChromeNavegador {
    fn abrir(&mut self, url: &str) -> Result<(), String> {
        self.aba.navigate_to(url).map_err(erro)?;
        self.aba.wait_until_navigated().map_err(erro)?;
        Ok(())
    }

    fn clicar(&mut self, acao: &str) -> Result<(), String> {
        match &self.modo {
            Modo::DataAcao => {
                let seletor = format!("[data-acao=\"{acao}\"]");
                self.aba.wait_for_element(&seletor).map_err(erro)?.click().map_err(erro)?;
            }
            Modo::Generico { .. } if acao.starts_with("Preencher: ") => {
                let (rotulo, texto) = acao["Preencher: ".len()..]
                    .split_once(" = ")
                    .ok_or_else(|| format!("preenchimento sem texto: {acao:?}"))?;
                let (rotulo, texto) = (literal(rotulo)?, literal(texto)?);
                let achou = self.avaliar_texto(&format!(
                    "(() => {{ {ROTULO_JS} const campos = camposVisiveis(); for (let i = 0; i < campos.length; i++) {{ \
                     const e = campos[i]; if (rotuloCampo(e, i) !== {rotulo}) continue; e.focus(); \
                     const setter = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set; \
                     setter.call(e, {texto}); e.dispatchEvent(new Event('input', {{ bubbles: true }})); \
                     e.dispatchEvent(new Event('change', {{ bubbles: true }})); return 'ok'; }} return 'nao'; }})()"
                ))?;
                if achou != "ok" {
                    return Err(format!("nao achei o campo para {acao:?}"));
                }
                sleep(Duration::from_millis(300));
            }
            Modo::Generico { .. } if acao.starts_with("Marcar: ") => {
                let rotulo = literal(&acao["Marcar: ".len()..])?;
                let achou = self.avaliar_texto(&format!(
                    "(() => {{ {ROTULO_JS} const caixas = marcaveis(); for (let i = 0; i < caixas.length; i++) {{ \
                     if (rotuloCampo(caixas[i], i) !== {rotulo}) continue; caixas[i].click(); return 'ok'; }} return 'nao'; }})()"
                ))?;
                if achou != "ok" {
                    return Err(format!("nao achei a caixa para {acao:?}"));
                }
                sleep(Duration::from_millis(300));
            }
            Modo::Generico { .. } if acao.starts_with("Selecionar: ") => {
                let (rotulo, opcao) = acao["Selecionar: ".len()..]
                    .split_once(" = ")
                    .ok_or_else(|| format!("selecao sem opcao: {acao:?}"))?;
                let (rotulo, opcao) = (literal(rotulo)?, literal(opcao)?);
                let achou = self.avaliar_texto(&format!(
                    "(() => {{ {ROTULO_JS} const todas = listas(); for (let i = 0; i < todas.length; i++) {{ \
                     if (rotuloCampo(todas[i], i) !== {rotulo}) continue; \
                     const o = [...todas[i].options].find(x => (x.textContent || '').trim().slice(0, 40) === {opcao}); \
                     if (!o) continue; todas[i].value = o.value; \
                     todas[i].dispatchEvent(new Event('input', {{ bubbles: true }})); \
                     todas[i].dispatchEvent(new Event('change', {{ bubbles: true }})); return 'ok'; }} return 'nao'; }})()"
                ))?;
                if achou != "ok" {
                    return Err(format!("nao achei a lista para {acao:?}"));
                }
                sleep(Duration::from_millis(300));
            }
            Modo::Generico { .. } => {
                let alvo = literal(acao)?;
                let achou = self.avaliar_texto(&format!(
                    "(() => {{ {ROTULO_JS} for (const e of alvos()) {{ if (visivel(e) && rotulo(e) === {alvo}) \
                     {{ e.click(); return 'ok'; }} }} return 'nao'; }})()"
                ))?;
                if achou != "ok" {
                    return Err(format!("nao achei o clicavel {acao:?} na tela"));
                }
                sleep(Duration::from_millis(500));
            }
        }
        Ok(())
    }

    fn preencher(&mut self, seletor: &str, texto: &str) -> Result<(), String> {
        self.aba.wait_for_element(seletor).map_err(erro)?.type_into(texto).map_err(erro)?;
        Ok(())
    }

    fn estado(&mut self) -> Result<String, String> {
        match &self.modo {
            Modo::DataAcao => self.avaliar_texto("document.body.dataset.estado || 'sem_estado'"),
            Modo::Generico { .. } => self.avaliar_texto(
                "location.pathname + '|' + [...document.querySelectorAll('h1, h2, h3, [role=heading], \
                 [aria-selected=true], [aria-current=step], [aria-current=page]')].filter(e => e.offsetParent !== null)\
                 .map(e => (e.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 40)).filter(Boolean)\
                 .slice(0, 6).join(' / ')",
            ),
        }
    }

    fn clicaveis(&mut self) -> Result<Vec<String>, String> {
        let json = match &self.modo {
            Modo::DataAcao => self.avaliar_texto(
                "JSON.stringify([...document.querySelectorAll('[data-acao]')]\
                 .filter(e => e.offsetParent !== null).map(e => e.getAttribute('data-acao')))",
            )?,
            Modo::Generico { evitar } => {
                let padrao = literal(evitar)?;
                self.avaliar_texto(&format!(
                    "(() => {{ {ROTULO_JS} const evitar = new RegExp({padrao}, 'i'); const vistos = new Set(); \
                     const saida = []; for (const e of alvos()) {{ const r = rotulo(e); \
                     if (!r || !visivel(e) || evitar.test(r) || vistos.has(r)) continue; vistos.add(r); saida.push(r); \
                     if (saida.length >= 40) break; }} \
                     camposVisiveis().forEach((e, i) => {{ const r = rotuloCampo(e, i); const nome = 'Preencher: ' + r; \
                     if (evitar.test(r) || vistos.has(nome)) return; vistos.add(nome); saida.push(nome); }}); \
                     marcaveis().forEach((e, i) => {{ const r = rotuloCampo(e, i); const nome = 'Marcar: ' + r; \
                     if (evitar.test(r) || vistos.has(nome)) return; vistos.add(nome); saida.push(nome); }}); \
                     listas().forEach((e, i) => {{ const r = rotuloCampo(e, i); \
                     [...e.options].slice(0, 12).forEach(o => {{ const t = (o.textContent || '').trim().slice(0, 40); \
                     const nome = 'Selecionar: ' + r + ' = ' + t; \
                     if (!t || o.disabled || evitar.test(t) || vistos.has(nome)) return; vistos.add(nome); saida.push(nome); }}); }}); \
                     return JSON.stringify(saida); }})()"
                ))?
            }
        };
        serde_json::from_str(&json).map_err(erro)
    }

    fn texto(&mut self, seletor: &str) -> Result<String, String> {
        let alvo = literal(seletor)?;
        self.avaliar_texto(&format!("document.querySelector({alvo})?.textContent ?? ''"))
    }

    fn contexto(&mut self) -> String {
        if !self.enviar_contexto {
            return String::new();
        }
        self.avaliar_texto("(document.body.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 700)")
            .unwrap_or_default()
    }

    fn reiniciar(&mut self) -> Result<(), String> {
        self.recriar()
    }

    fn observacao(&mut self) -> Observacao {
        self.coletor.lock().map(|o| o.clone()).unwrap_or_default()
    }

    fn captura(&mut self) -> Result<Vec<u8>, String> {
        self.aba
            .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(erro)
    }
}

fn frame_ws(enviado: bool, payload: &str) -> FrameWs {
    FrameWs { enviado, evento: evento_socketio(payload), tamanho: payload.len() }
}
