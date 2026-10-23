use crate::evidencias::Relatorio;
use crate::motor::Opiniao;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encaminhamento {
    GoSugerido,
    NoGoSugerido,
    HumanoSemBase,
}

/// Passo 12: a decisao final e sempre do humano; aqui so se define o que chega a ele e com que marca.
/// Sem veredito do motor o relatorio segue marcado como "sem base", em vez de sugerir GO ou NO-GO.
pub fn encaminhar(relatorio: &Relatorio, opiniao: Option<&Opiniao>) -> Encaminhamento {
    match opiniao {
        Some(op) if op.ha_veredito => {
            if relatorio.passou {
                Encaminhamento::GoSugerido
            } else {
                Encaminhamento::NoGoSugerido
            }
        }
        _ => Encaminhamento::HumanoSemBase,
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::evidencias::Evidencias;

    fn relatorio(passou: bool) -> Relatorio {
        Relatorio {
            versao: "v1".into(),
            caminhos: 1,
            vereditos: 0,
            falhas: if passou { vec![] } else { vec!["a".into()] },
            passou,
            evidencias: Evidencias::default(),
            backend: Default::default(),
        }
    }

    fn opiniao(ha_veredito: bool) -> Opiniao {
        Opiniao { ha_veredito, preferida: None, forca: String::new(), resumo: String::new() }
    }

    #[test]
    fn com_veredito_sugere_go_ou_no_go_pelo_relatorio() {
        assert_eq!(encaminhar(&relatorio(true), Some(&opiniao(true))), Encaminhamento::GoSugerido);
        assert_eq!(encaminhar(&relatorio(false), Some(&opiniao(true))), Encaminhamento::NoGoSugerido);
    }

    #[test]
    fn sem_veredito_ou_sem_motor_vai_ao_humano_sem_base() {
        assert_eq!(encaminhar(&relatorio(true), Some(&opiniao(false))), Encaminhamento::HumanoSemBase);
        assert_eq!(encaminhar(&relatorio(true), None), Encaminhamento::HumanoSemBase);
    }
}
