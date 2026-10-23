//! `qa-agente <alvo.json>`: roda o agente de QA contra um projeto descrito por um arquivo de alvo.
//! Exige `QA_OMNIROUTE_CHAVE` (todo texto gerado vai pelo OmniRoute). Saida em `qa-saida/<nome-do-alvo>/`.
//! Codigo de saida: 0 relatorio verde, 1 relatorio com falhas, 2 erro de uso, configuracao ou agente.
//!
//! Front e back no mesmo relatorio: telas e capturas de cada passo; status HTTP, requisicoes sem resposta,
//! WebSocket, erros de console, log do servidor, sondas de saude e estado gravado no backend.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use qa_agente::agente_llm::AgenteLlm;
use qa_agente::alvo::Alvo;
use qa_agente::avaliacao::Veredito;
use qa_agente::evidencias::{consolidar_com_backend, Evidencias};
use qa_agente::exploracao::jornada_guiada_observada;
use qa_agente::gate_humano::encaminhar;
use qa_agente::grafo_estados::VersionedStateGraph;
use qa_agente::insumos;
use qa_agente::log_servidor;
use qa_agente::motor::{ConsultaMotor, Motor};
use qa_agente::navegador_chrome::ChromeNavegador;
use qa_agente::omniroute::OmniRoute;
use qa_agente::portas::Navegador;
use qa_agente::regressao::ResultadoCaminho;
use qa_agente::{sondas, verificacoes};
use serde_json::json;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [caminho] = args.as_slice() else {
        eprintln!("uso: qa-agente <alvo.json>   (defina QA_OMNIROUTE_CHAVE)");
        return ExitCode::from(2);
    };
    match rodar(Path::new(caminho)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(erro) => {
            eprintln!("erro: {erro}");
            ExitCode::from(2)
        }
    }
}

fn rodar(caminho: &Path) -> Result<bool, String> {
    let inicio = std::time::Instant::now();
    let alvo = Alvo::ler(caminho)?;
    let pasta = PathBuf::from("qa-saida").join(&alvo.nome);
    std::fs::create_dir_all(&pasta).map_err(|e| e.to_string())?;

    let antes = sondas::executar(&alvo.sondas());
    let marca_do_log = alvo.log_servidor.as_deref().map(|p| log_servidor::marcar(Path::new(p)));

    let mut navegador = match alvo.modo.as_str() {
        "data-acao" => ChromeNavegador::novo(None)?,
        _ => ChromeNavegador::novo_generico(None, &alvo.evitar)?,
    }
    .com_contexto(alvo.contexto_ao_llm);
    if let Some(c) = &alvo.cookie {
        navegador.definir_cookie_com(&c.nome, &c.valor, &c.dominio, c.http_only)?;
    }
    let agente = AgenteLlm::new(OmniRoute::do_ambiente().map_err(|e| format!("{e:?}"))?);
    let mut grafo = VersionedStateGraph::default();
    let pasta_passos = pasta.clone();
    // Rastro ao vivo: cada passo vai na hora para stdout, passos.jsonl, checkpoint.json e o banco do RAG.
    let id_execucao = format!(
        "{}-{}",
        alvo.nome,
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    );
    let mut rastro = qa_agente::rastro::Rastro::iniciar(&id_execucao, &alvo.nome, &alvo.objetivo, &pasta);
    // limite_passos = 0 significa SEM limite: o agente decide quando parar (cumpriu, esgotou as opcoes ou cota)
    let limite = if alvo.limite_passos == 0 { usize::MAX } else { alvo.limite_passos };
    // Politica aprendida (Q-learning): experiencia no prompt, atualizada a cada passo, salva a cada 5 e no fim.
    let caminho_politica = PathBuf::from(std::env::var("QA_POLITICA").unwrap_or_else(|_| "qa-politica.json".into()));
    let politica = qa_agente::aprendizado::Politica::carregar(&caminho_politica).unwrap_or_default();
    println!("[politica] {} telas, {} pares aprendidos em {}", politica.estados(), politica.entradas(), caminho_politica.display());
    // Estado gravado ANTES da jornada (consultas SQL somente-leitura do alvo) e logs lidos a cada passo.
    let consultas = alvo.consultas();
    let sql_antes: Vec<Result<String, String>> = consultas.iter().map(qa_agente::consultas_sql::executar).collect();
    let mut logs_vivos = qa_agente::log_servidor::LogsVivos::novo(
        alvo.logs.iter().map(|l| (l.nome.clone(), PathBuf::from(&l.caminho))).collect(),
    );
    let mut fonte_de_logs = || logs_vivos.novos_achados();
    let jornada = jornada_guiada_observada(
        &agente,
        &mut navegador,
        &alvo.url,
        &alvo.objetivo,
        &alvo.nome,
        &mut grafo,
        limite,
        &mut |n, nav| {
            let png = nav.captura()?;
            std::fs::write(pasta_passos.join(format!("passo-{n:02}.png")), png).map_err(|e| e.to_string())
        },
        &mut |n, p| {
            rastro.passo(n, p);
            if n % 5 == 0 {
                let _ = politica.salvar(&caminho_politica);
            }
        },
        Some(&politica),
        qa_agente::exploracao::Restricao::nova(&alvo.acoes_de_escrita, alvo.max_escritas).as_ref(),
        Some(&mut fonte_de_logs),
    );
    let jornada = match jornada {
        Ok(j) => j,
        Err(e) => {
            rastro.encerrar(0, "", &format!("erro: {e}"));
            return Err(e);
        }
    };
    rastro.encerrar(
        jornada.passos.len(),
        jornada.passos.last().map(|p| p.destino.as_str()).unwrap_or(""),
        &jornada.parou_por,
    );
    let _ = politica.salvar(&caminho_politica);
    let passos = &jornada.passos;

    let depois = sondas::executar(&alvo.sondas());
    // Estado gravado DEPOIS: cada consulta vira um veredito (esperado presente?) e o relatorio mostra o antes e o depois.
    let resultados_sql: Vec<qa_agente::consultas_sql::ResultadoSql> = consultas
        .iter()
        .zip(sql_antes)
        .map(|(c, antes)| qa_agente::consultas_sql::comparar(c, antes, qa_agente::consultas_sql::executar(c)))
        .collect();
    for r in &resultados_sql {
        println!("[banco] {}: {} ({})", r.nome, if r.ok { "ok" } else { "FALHOU" }, r.detalhe);
    }
    let cookie = alvo.cookie.as_ref().map(|c| (c.nome.as_str(), c.valor.as_str()));
    let estado_gravado = verificacoes::executar(&alvo.verificacoes(), cookie);
    let observacao = navegador.observacao();

    let mut resumo = observacao.resumo();
    let mut log_lido = marca_do_log.is_none();
    if let Some(marca) = &marca_do_log {
        let (graves, mut avisos) = log_servidor::classificar(&log_servidor::novas_linhas(marca)?);
        log_lido = log_servidor::existe(marca);
        if !log_lido {
            avisos.insert(0, "log do servidor configurado mas ausente: o QA nao viu o log do back".into());
        }
        resumo.log_graves = graves;
        resumo.log_avisos = avisos;
    }

    let vereditos: Vec<Veredito> = antes
        .iter()
        .map(|s| (format!("sonda {} (antes)", s.nome), s.ok, s.detalhe.clone()))
        .chain(depois.iter().map(|s| (format!("sonda {} (depois)", s.nome), s.ok, s.detalhe.clone())))
        .chain(estado_gravado.iter().map(|v| (format!("estado no backend: {}", v.nome), v.ok, v.detalhe.clone())))
        .chain(resultados_sql.iter().map(|r| (format!("banco: {}", r.nome), r.ok, r.detalhe.clone())))
        .map(|(caso, passou, motivo)| Veredito { caso, passou, motivo })
        .collect();
    let progrediu = jornada.erro.is_none() && insumos::progresso(passos, &jornada.parou_por);
    let caminhos = vec![ResultadoCaminho {
        nome: match (&jornada.erro, progrediu) {
            (Some(_), _) => "jornada_guiada (erro do agente)".into(),
            (None, true) => "jornada_guiada".into(),
            (None, false) => "jornada_guiada (sem progresso)".into(),
        },
        passou: progrediu,
        detalhe: format!(
            "{} passos, {} transicoes novas; parou: {}",
            passos.len(),
            grafo.novas_em(&alvo.nome).len(),
            jornada.parou_por
        ),
    }];
    let evidencias = Evidencias {
        screenshots: (0..=passos.len()).map(|n| pasta.join(format!("passo-{n:02}.png")).display().to_string()).collect(),
        ..Evidencias::default()
    };
    let relatorio = consolidar_com_backend(&alvo.nome, &caminhos, &vereditos, evidencias, resumo);

    let opiniao = Motor::default()
        .opiniao("decisao-de-execucao-qa", "geral", &["executar", "pular", "bloquear"], "executar")
        .ok();
    let rumo = encaminhar(&relatorio, opiniao.as_ref());

    let b = &relatorio.backend;
    let saida = json!({
        "alvo": alvo.nome,
        "passou": relatorio.passou,
        "falhas": relatorio.falhas,
        "encaminhamento": format!("{rumo:?}"),
        "parou_por": jornada.parou_por,
        "erro_do_agente": jornada.erro,
        "objetivo_cumprido": jornada.parou_por == qa_agente::exploracao::OBJETIVO_CUMPRIDO,
        "passos": passos.iter().map(|p| json!({
            "origem": p.origem, "acao": p.acao, "destino": p.destino, "ambigua": p.ambigua,
        })).collect::<Vec<_>>(),
        "front": { "erros_console": b.erros_console },
        "backend": {
            "requisicoes": b.requisicoes,
            "erros_5xx": b.erros_5xx,
            "erros_4xx": b.erros_4xx,
            "falhas_de_rede": b.falhas_de_rede,
            "websocket": {
                "enviados": b.ws_enviados, "recebidos": b.ws_recebidos, "eventos": b.ws_eventos, "erros": b.erros_ws,
            },
            "log_servidor": {
                "configurado": alvo.log_servidor.is_some(), "lido": log_lido,
                "graves": b.log_graves, "avisos": b.log_avisos,
            },
        },
        "sondas": antes.iter().chain(depois.iter()).map(|s| json!({
            "nome": s.nome, "status": s.status, "ok": s.ok, "detalhe": s.detalhe,
        })).collect::<Vec<_>>(),
        "estado_no_backend": estado_gravado.iter().map(|v| json!({
            "nome": v.nome, "ok": v.ok, "detalhe": v.detalhe,
        })).collect::<Vec<_>>(),
    });
    let arquivo = pasta.join("relatorio.json");
    std::fs::write(&arquivo, serde_json::to_string_pretty(&saida).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    // Insumos para o motor de decisao: um arquivo por rodada e um acumulado entre rodadas e alvos.
    let registros = insumos::linhas(&insumos::Execucao {
        alvo: &alvo.nome,
        objetivo: &alvo.objetivo,
        passos,
        parou_por: &jornada.parou_por,
        segundos: inicio.elapsed().as_secs(),
        transicoes_novas: grafo.novas_em(&alvo.nome).len(),
        relatorio: &relatorio,
    });
    let jsonl = insumos::como_jsonl(&registros);
    std::fs::write(pasta.join("insumos.jsonl"), &jsonl).map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        let mut acumulado = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(PathBuf::from("qa-saida").join("insumos-acumulado.jsonl"))
            .map_err(|e| e.to_string())?;
        acumulado.write_all(jsonl.as_bytes()).map_err(|e| e.to_string())?;
    }

    println!(
        "{}: {} ({} passos; {} requisicoes HTTP; websocket {} enviados / {} recebidos)",
        alvo.nome,
        if relatorio.passou { "VERDE" } else { "COM FALHAS" },
        passos.len(),
        b.requisicoes,
        b.ws_enviados,
        b.ws_recebidos
    );
    println!("  parou: {}", jornada.parou_por);
    if jornada.parou_por != qa_agente::exploracao::OBJETIVO_CUMPRIDO {
        println!("  atencao: objetivo NAO cumprido (verde aqui quer dizer sem falha encontrada, nao objetivo atingido)");
    }
    for aviso in b.log_avisos.iter().filter(|a| a.starts_with("log do servidor configurado")) {
        println!("  atencao: {aviso}");
    }
    for falha in &relatorio.falhas {
        println!("  falha: {falha}");
    }
    println!("  encaminhamento: {rumo:?}\n  relatorio: {}", arquivo.display());
    if let Some(e) = &jornada.erro {
        // o relatorio parcial ja foi salvo; o codigo de saida 2 diz que o agente falhou, nao que a app falhou
        return Err(format!("o agente falhou durante a jornada (relatorio parcial salvo em {}): {e}", arquivo.display()));
    }
    Ok(relatorio.passou)
}
