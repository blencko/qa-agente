#!/usr/bin/env bash
# Instancia SOLO: ambiente proprio, sem limite de passos (usize::MAX) e sem roteiro; o agente decide o que testar e
# quando parar (objetivo cumprido, nada novo a clicar, todas as opcoes esgotadas ou cota do gateway).
# Uso: tools/rodar-solo.sh <EXE_DO_TESTE> [id]   (exige DND_TESTE_DATABASE_URL e QA_OMNIROUTE_CHAVE no ambiente)
set -u
EXE="${1:?executavel do teste jogo_dnd_mestre}"
ID="${2:-solo}"
RAIZ="$(cd "$(dirname "$0")/.." && pwd)"
TMP="${TEMP:-/tmp}"
base="$TMP/dnd-e2e-$ID"
parada="$TMP/qa-parada-$ID"
rm -f "$parada"
export E2E_BASE="$base"
cp "$RAIZ/qa-politica.json" "$TMP/qa-politica-$ID.json" 2>/dev/null && export QA_POLITICA="$TMP/qa-politica-$ID.json"
cd "$RAIZ"
node tools/jogo-ambiente.mjs "$parada" "$TMP/qa-jogo-$ID.log" > "$TMP/qa-amb-$ID.out" 2>&1 &
amb=$!
for _ in $(seq 1 90); do [ -f "$base/estado/estado.json" ] && break; sleep 3; done
if [ -f "$base/estado/estado.json" ]; then
  QA_LIMITE_PASSOS="${QA_LIMITE_PASSOS:-18446744073709551615}" QA_EXECUCAO_ID="$ID-$(date +%s)" \
  QA_OBJETIVO="Voce e o QA deste jogo e vai sozinho: explore e use o jogo inteiro como um jogador novo, decidindo por conta propria o que testar (campanha chamada E2E Solo, personagem completo, jogar com o mestre, inventario, diario, magias, perfil, temas). Pode criar e editar dados de teste, mas nao apagar nada. Encerre (null) so quando considerar que cobriu o jogo." \
    "$EXE" --ignored --nocapture > "$TMP/qa-rodada-$ID.out" 2>&1
else
  echo "ambiente $ID nao subiu" > "$TMP/qa-rodada-$ID.out"
fi
: > "$parada"
wait "$amb"
echo "instancia $ID terminou"
