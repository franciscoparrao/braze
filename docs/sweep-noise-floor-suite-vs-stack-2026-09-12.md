# Resultado: el piso de ruido es de la suite, no del stack

Fecha: 2026-09-12
Pre-registro: `docs/hypothesis-2026-09-12-noise-floor-suite-vs-stack.md`
(committeado en `69078b8` ANTES de la corrida válida).
Estado: **CERRADO. Veredicto H_suite.**

## Qué se midió

El flip rate same-prompt (fracción de rep-pairs que voltean pass/fail bajo
prompt byte-idéntico, solo cambia el seed de sampling) sobre la suite
**saturada** `default.toml`, para separar las dos causas que §6.3 del
Paper 2 atribuye al piso de ruido de la suite discriminante:
marginalidad-de-suite vs no-determinismo-de-stack.

Lógica del test: en una suite saturada (gpt-oss ~99%) la capacidad no es
el limitante, así que el flip rate solo puede subir si el stack es
genuinamente no-determinista. Efecto techo a propósito.

## Resultado

| | default.toml (saturada) | discriminante (Study 2) |
|---|---|---|
| pass rate | **95/95 = 100%** | ~58-70% |
| flip rate same-prompt | **0/190 = 0,0%** | 15,7% within-baseline / 20,6% cross-arm |
| tareas inestables | **0/19** | 8/34 (24%) |

Cero flips en 190 rep-pairs. El piso de ~16-20% de la suite discriminante
**desaparece** cuando las tareas no son marginales.

## Veredicto (criterio comprometido antes de correr)

**H_suite** (flip ≤5%). El piso de ruido es **marginalidad de la suite
cerca de la frontera**, no no-determinismo genérico del stack. El término
de stack —el que voltearía outcomes independientemente de la dificultad—
queda **acotado por abajo en ≈0** para outcomes no marginales.

El caveat de stack del pre-registro refuerza la conclusión: esta corrida
usó **Ollama** (parser Harmony server-side, históricamente el stack MÁS
frágil), no el LocalBackend in-process de Study 2. Un flip de 0% en el
stack más frágil sobre tareas saturadas acota el ruido de stack por abajo
*a fortiori*: el stack menos frágil no voltearía más.

## Consecuencia para el Paper 2 (§6.3, §5.3, §7)

La redacción de Tier 2 dejaba las dos causas "sin repartir". Este dato
permite ahora acotar: el término de stack es ≈0 para outcomes no
marginales, así que el piso de ~16-20% de la discriminante es
sustancialmente marginalidad-de-suite. La aritmética de MDE caracteriza a
la SUITE (no al stack), y la lección "todo A/B necesita un control
same-prompt in-sweep" se sostiene porque el ruido vive donde las tareas
son marginales — exactamente donde corre un A/B discriminante.

## Procedencia

- Datos: `docs/sweep-noise-floor-default-2026-09-12.json` (95 corridas),
  `.log`.
- suite_fingerprint `6694b4c073476ffa`, braze_git_commit `93fa19e`,
  gpt-oss:20b digest `17052f91a42e`, Ollama 0.32.1, temp 0.2, seed 42,
  5 reps, grading `functional-primary+strict-secondary/2026-08-12`.

## Desviación del pre-registro (operativa, declarada)

El pre-registro decía `BRAZE_OLLAMA_NUM_CTX=32768` (cargo-culteado del
requisito de la suite discriminante). La primera corrida abortó a los 90s
con **CUDA OOM**: a 32k los buffers de cómputo de gpt-oss (~4.5GB) no caben
en la VRAM libre de la RTX 3050 de Nitro, porque un `llama-server` de
Spark-X2.5 del autor ocupaba 3.2GB (el circuit breaker abrió y el bench
hizo fail-fast correctamente; evidencia en el archivo v1 descartado). Se
corrigió a **`NUM_CTX=8192`**, de sobra para las tareas single-tool de
`default.toml` (no trunca) y sin tocar el server del autor. Es un parámetro
operativo: NO cambia arms, métrica ni criterio del pre-registro. Nota:
`NUM_CTX` es capa env-only y no queda en `metadata` del JSON — misma
laguna de procedencia que el pendiente `zen_base_url`.
