# Pre-registro: ¿el piso de ruido de ~16-20% es de la suite o del stack?

Fecha: 2026-09-12
Estado: **DISEÑO. Ninguna corrida lanzada al escribir esto.**
Disparador: Issue 4 de la revisión IST del Paper 2 (Tier 2). §6.3 del
manuscrito ahora afirma que el piso de ruido same-prompt (~20% de celdas
pareadas que voltean bajo prompts byte-idénticos, temp 0.2, suite
discriminante) tiene DOS causas de generalidad distinta —marginalidad de
la suite cerca de la frontera vs no-determinismo del serving stack— y que
los datos no las reparten. Este pre-registro mide el término barato de esa
descomposición.

## Dato previo (sin compute nuevo, computado 2026-09-12)

Sobre los JSONs de Study 2 (`docs/pm-ab/`, gpt-oss:20b, LocalBackend/
Harmony, temp 0.2, suite discriminante de 34 tareas):
- Flip rate same-prompt DENTRO del brazo baseline (3 reps, prompt
  idéntico, solo cambia seed+rep): **16/102 rep-pairs = 15,7%**; 8/34
  tareas voltean entre reps.
- Cross-arm baseline vs empty (el control del paper): 21/102 = 20,6%.

El piso es robusto a cómo se mida, pero todo es a temp 0.2 sobre la suite
discriminante: no separa las dos causas.

## Pregunta

A régimen comparable (mismo modelo, misma temp, mismo tipo de oráculo),
¿el piso de ruido same-prompt es una propiedad de la SUITE (requiere
tareas marginales cerca de la frontera para voltear) o del STACK (voltea
resultados independientemente de la dificultad de la tarea)?

**Palanca barata (esta corrida):** medir el flip rate same-prompt sobre
la suite SATURADA `default.toml` (19 tareas; gpt-oss:20b ~98,9% histórico
→ lejos de la frontera). Efecto techo a propósito: en una suite saturada
un modelo capaz solo puede voltear pass/fail si el stack es genuinamente
no-determinista, porque la capacidad no es el limitante. Es el test
directo de "¿el ruido necesita tareas marginales?".

## Hipótesis y criterio comprometido antes de correr

- **H_suite:** flip rate en `default.toml` ≈ 0 (≤ ~5%, es decir ≤ ~1 de
  las 19 tareas inestable). → El piso de ~16-20% de la discriminante es
  marginalidad de la suite; el no-determinismo del stack que voltea
  outcomes está acotado por abajo. Refuerza la atribución primaria del
  paper (§6.3) y permite escribir "el término de stack es pequeño para
  outcomes no marginales", en vez de dejar las dos causas sin repartir.
- **H_stack:** flip rate en `default.toml` sustancial (≳ ~15%, comparable
  al de la discriminante). → El piso es propiedad del stack, independiente
  de la suite; el no-determinismo de serving es la causa dominante y la
  redacción de §6.3 debe inclinarse hacia el stack (y la aritmética de MDE
  caracteriza al stack, no a la suite).
- **Intermedio (5-15%):** el stack contribuye pero la marginalidad
  amplifica; se reporta el número y se mantiene "las dos causas, sin
  repartir exacto" con la cota medida.

Sin iteración: es una medición, no un loop de tuning. Si sale intermedio
no se re-corre con más reps "para desambiguar" sin un pre-registro nuevo.

## Caveat de stack (declarado antes)

Study 2 midió el 15,7-20,6% con **LocalBackend in-process**; esta corrida
usa **Ollama** (gpt-oss:20b en Nitro) por costo/simplicidad, no LocalBackend.
Es un stack distinto. Pero el argumento es conservador en la dirección que
importa: Ollama parsea Harmony server-side y ha sido históricamente MÁS
frágil/no-determinista que el LocalBackend in-process (ver incidente #1 del
testbed roam, CLAUDE.md). Por lo tanto un flip ≈ 0 en Ollama-fácil acota el
ruido de stack por abajo *a fortiori*: si el stack más frágil no voltea
tareas saturadas, el menos frágil tampoco. Un flip alto en Ollama-fácil,
en cambio, no distinguiría "stack Ollama" de "stack en general" —en ese
caso se re-corre con LocalBackend antes de concluir H_stack.

## Diseño

- Suite: `crates/braze-bench/suites/default.toml` (19 tareas, saturada).
- Backend: `ollama:gpt-oss:20b`, servido por Ollama 0.32.1 local en Nitro.
- Un solo brazo (el "same-prompt control" son las repeticiones: prompt
  byte-idéntico cada rep, solo cambia el seed de sampling).
- 5 repeticiones, `--temperature 0.2` (paridad con Study 2), `--seed 42`
  (cada rep usa seed+rep → varianza real, no copias).
- Contexto/output en paridad: `BRAZE_OLLAMA_NUM_CTX=32768`,
  `BRAZE_MAX_TOKENS=12288`.
- Ejecutado EN NITRO (inferencia y oráculo `cargo check` en el mismo nodo).
- Binario: braze-bench de `~/proyectos/braze` @ `93fa19e` (los commits
  locales posteriores son solo `paper2/*` y no tocan el engine).

## Métrica

- Primaria: flip rate same-prompt = fracción de rep-pairs discordantes en
  pass/fail, agregado sobre las 19 tareas (mismo estimador que el 15,7%
  de Study 2); y nº de tareas inestables / 19.
- Control de sanidad: pass rate global ≈ 99% (si sale bajo, el grading o
  el cargo del PATH está roto → abortar, no interpretar).

## Salvaguardas operativas (lecciones del CLAUDE.md)

- NO exportar `BRAZE_LOCAL_FAMILY` (causó 13 JSONs vacíos sobre gemma).
- NO redirigir la salida a `/dev/null`; capturar JSON a archivo con nombre
  y `tee` del log. Verificar que el JSON tiene 95 resultados y pass rate
  sano ANTES de declarar éxito.
- Un solo modelo → sin `--no-ollama-stop`.
- `source ~/.cargo/env` antes de lanzar (sin cargo, el oráculo califica
  todo como fallo en silencio).
