use std::process::Command;
use std::time::Duration;

use serde_json::json;

pub const URL_MOTOR: &str = "http://127.0.0.1:8090/v1/opiniao";
/// Programa que registra o desfecho de uma decisao no motor (so o rotulo em modo de ensaio o usa). `QA_MOTOR_ALIMENTAR` troca.
pub const ALIMENTAR: &str = "alimentar.mjs";

#[derive(Debug, Clone, PartialEq)]
pub struct Opiniao {
    pub ha_veredito: bool,
    pub preferida: Option<String>,
    pub forca: String,
    pub resumo: String,
}

#[derive(Debug, PartialEq)]
pub struct MotorIndisponivel(pub String);

pub trait ConsultaMotor {
    fn opiniao(
        &self,
        tipo: &str,
        balde: &str,
        opcoes: &[&str],
        pretendida: &str,
    ) -> Result<Opiniao, MotorIndisponivel>;
}

pub struct Motor {
    url: String,
    tempo: Duration,
}

impl Default for Motor {
    fn default() -> Self {
        Motor {
            url: URL_MOTOR.to_string(),
            tempo: Duration::from_secs(5),
        }
    }
}

impl ConsultaMotor for Motor {
    fn opiniao(
        &self,
        tipo: &str,
        balde: &str,
        opcoes: &[&str],
        pretendida: &str,
    ) -> Result<Opiniao, MotorIndisponivel> {
        let corpo = json!({"tipo": tipo, "balde": balde, "opcoes": opcoes, "pretendida": pretendida});
        let dados: serde_json::Value = ureq::post(&self.url)
            .timeout(self.tempo)
            .send_json(corpo)
            .map_err(|e| MotorIndisponivel(e.to_string()))?
            .into_json()
            .map_err(|e| MotorIndisponivel(e.to_string()))?;
        Ok(Opiniao {
            ha_veredito: dados["opiniao"].as_bool().unwrap_or(false),
            preferida: dados["preferida"].as_str().map(String::from),
            forca: dados["forca"].as_str().unwrap_or("nenhuma").to_string(),
            resumo: dados["resumo"].as_str().unwrap_or("").to_string(),
        })
    }
}

/// Monta (e, fora do ensaio, executa) o comando de rotulo do motor. O modo de ensaio e o padrao seguro.
pub fn rotular(
    tipo: &str,
    balde: &str,
    opcoes: &[&str],
    escolhida: &str,
    resultado: f64,
    id_estavel: &str,
    ensaio: bool,
) -> Result<Vec<String>, String> {
    if !(0.0..=1.0).contains(&resultado) {
        return Err("resultado fora de [0, 1]".into());
    }
    let args: Vec<String> = vec![
        std::env::var("QA_MOTOR_ALIMENTAR").unwrap_or_else(|_| ALIMENTAR.into()),
        "--tipo".into(),
        tipo.into(),
        "--balde".into(),
        balde.into(),
        "--opcoes".into(),
        opcoes.join(","),
        "--escolhida".into(),
        escolhida.into(),
        "--resultado".into(),
        resultado.to_string(),
        "--id".into(),
        id_estavel.into(),
    ];
    if !ensaio {
        let status = Command::new("node").args(&args).status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("alimentar saiu com {status}"));
        }
    }
    Ok(args)
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn rotular_em_ensaio_so_monta_o_comando() {
        let args = rotular("t", "b", &["a", "c"], "a", 0.5, "id-1", true).unwrap();
        assert!(args.contains(&"a,c".to_string()));
    }

    #[test]
    fn rotular_recusa_resultado_fora_da_escala() {
        assert!(rotular("t", "b", &["a"], "a", 1.5, "id-1", true).is_err());
        assert!(rotular("t", "b", &["a"], "a", -0.1, "id-1", true).is_err());
    }
}
