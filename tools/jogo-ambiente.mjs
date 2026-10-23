// Cola em Node (excecao declarada a regra "Rust sempre"): sobe o ambiente E2E do dnd-mestre usando o proprio
// globalSetup dele, que e escrito em Node/TypeScript. Banco dnd_teste, schema descartavel, narrador falso, login de teste.
// Uso: DND_TESTE_DATABASE_URL=... node jogo-ambiente.mjs <arquivo-de-parada> [arquivo-de-log-do-servidor]
// Fica de pe ate o <arquivo-de-parada> existir; so entao desmonta (apaga so o schema que criou).
import { existsSync } from "node:fs";
import { pathToFileURL } from "node:url";

import { appendFileSync } from "node:fs";
import { createRequire, syncBuiltinESMExports } from "node:module";

const RAIZ_JOGO = process.env.QA_JOGO_RAIZ ?? "C:/workflow/dnd-mestre";
const arquivoDeParada = process.argv[2];
if (!arquivoDeParada) throw new Error("informe o arquivo de parada como argumento");

// O globalSetup do jogo guarda o log do servidor so em memoria. Com um 2o argumento (arquivo de log), o `fork` do
// servidor passa a copiar stdout/stderr para esse arquivo, sem alterar o jogo: o QA le dali o que o back registrou.
const arquivoDeLog = process.argv[3];
if (arquivoDeLog) {
  const cp = createRequire(import.meta.url)("node:child_process");
  const forkOriginal = cp.fork;
  cp.fork = (...args) => {
    const filho = forkOriginal(...args);
    const gravar = (dado) => {
      try { appendFileSync(arquivoDeLog, dado); } catch { /* o log e um extra: nunca derruba o ambiente */ }
    };
    filho.stdout?.on("data", gravar);
    filho.stderr?.on("data", gravar);
    return filho;
  };
  syncBuiltinESMExports();
}

const { default: globalSetup } = await import(pathToFileURL(`${RAIZ_JOGO}/test/e2e/global-setup.ts`).href);
const desmontar = await globalSetup();

let encerrando = false;
const encerrar = async () => {
  if (encerrando) return;
  encerrando = true;
  try {
    await desmontar();
    console.log("[qa-agente] ambiente do jogo desmontado");
  } catch (e) {
    console.error(`[qa-agente] falha ao desmontar: ${e instanceof Error ? e.message : String(e)}`);
    process.exitCode = 1;
  }
  process.exit();
};
process.on("SIGINT", encerrar);
process.on("SIGTERM", encerrar);

console.log("[qa-agente] ambiente do jogo pronto");
while (!existsSync(arquivoDeParada)) await new Promise((ok) => setTimeout(ok, 1000));
await encerrar();
