#!/usr/bin/env bash
# Sobe N instancias do QA contra o jogo, cada uma com o seu ambiente E2E descartavel (E2E_BASE propria) e o seu objetivo.
# Uso: tools/rodar-varios.sh <N> <EXE_DO_TESTE>   (exige DND_TESTE_DATABASE_URL e QA_OMNIROUTE_CHAVE no ambiente)
set -u
N="${1:?numero de instancias}"
EXE="${2:?executavel do teste jogo_dnd_mestre}"
RAIZ="$(cd "$(dirname "$0")/.." && pwd)"
TMP="${TEMP:-/tmp}"
OBJ=(
  "Criar uma campanha chamada E2E Alfa, criar um personagem completo ate o botao final de criar e jogar um turno com o mestre. Nao apagar nada."
  "Criar uma campanha chamada E2E Beta, criar um personagem guerreiro anao completo e abrir o diario e o inventario dele. Nao apagar nada."
  "Criar uma campanha chamada E2E Gama, criar um personagem mago completo e conjurar ou preparar uma magia na ficha. Nao apagar nada."
  "Criar uma campanha chamada E2E Delta, abrir cada area do menu e do perfil, trocar o tema claro e escuro e voltar. Nao apagar nada."
  "Criar uma campanha chamada E2E Epsilon, criar dois personagens e alternar entre eles na mesa. Nao apagar nada."
)
for i in $(seq 1 "$N"); do
  base="$TMP/dnd-e2e-i$i"
  parada="$TMP/qa-parada-i$i"
  rm -f "$parada"
  (
    export E2E_BASE="$base"
    cp "$RAIZ/qa-politica.json" "$TMP/qa-politica-i$i.json" 2>/dev/null && export QA_POLITICA="$TMP/qa-politica-i$i.json"
    cd "$RAIZ"
    node tools/jogo-ambiente.mjs "$parada" "$TMP/qa-jogo-i$i.log" > "$TMP/qa-amb-i$i.out" 2>&1 &
    amb=$!
    for _ in $(seq 1 90); do [ -f "$base/estado/estado.json" ] && break; sleep 3; done
    if [ -f "$base/estado/estado.json" ]; then
      QA_LIMITE_PASSOS=70 QA_OBJETIVO="${OBJ[$(( (i - 1) % ${#OBJ[@]} ))]}" \
        "$EXE" --ignored --nocapture > "$TMP/qa-rodada-i$i.out" 2>&1
    else
      echo "ambiente $i nao subiu" > "$TMP/qa-rodada-i$i.out"
    fi
    : > "$parada"       # pede o teardown do ambiente
    wait "$amb"
  ) &
  sleep 20             # escalona a subida: banco, build e Chrome nao disputam tudo ao mesmo tempo
done
wait
echo "todas as instancias terminaram"
