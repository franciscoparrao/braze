# El "fallo sistemático" de gemma4:e4b no era un fallo — cierre del A/B Gemma4 runtime

Fecha: 2026-08-10
Cierra: pendiente "A/B Gemma4 de runtime con digest fijo (`c6eb396dbd59`)"
Datos: brazo A de `docs/sweep-json-tax-edit-fence-2026-08-10.json` (95
corridas frescas de e4b baseline, seed 42, Ollama 0.32.1, digest
verificado idéntico al del 13-jul) vs
`docs/sweep-gemma4e4b-vs-gptoss20b-2026-07-13.json`.

## La pregunta pendiente y su respuesta

La hipótesis de CLAUDE.md (nota Gemma 4, 2026-07-18): los 3 fallos
sistemáticos de e4b eran una tarea de `single_tool`; si el fix de tool
calling de Ollama 0.32.1 los reparaba, e4b saltaba a pass^5=100% y
desafiaba a gpt-oss:20b como default por RAM.

Respuesta con digest fijo: **NO — porque no había nada que reparar.**

| sweep | runtime | `read_file_basic` |
|---|---|---|
| 13-jul | pre-0.32.1 | 3/5 fallos |
| 10-ago | 0.32.1 | **5/5 fallos** |

## El mecanismo: aserción de tool, no capacidad

Las 5 repeticiones de hoy son idénticas: `converged=True`, 2 rondas,
`schema_fail=0`, `expected_text_found=True` (el modelo responde "3
líneas" — correcto), `failure_cause=assertion_tool_call`. e4b resuelve
"¿cuántas líneas tiene notas.txt?" con `shell_exec` (un `wc -l`) en vez
del `read_file` que `expect_tool_call` exige. En la taxonomía de Raj et
al. (arXiv:2607.28802): fallo en la arista MODEL—BENCH con el lado
culpable en el **banco** — la intervención correcta es reparar el
grader o el prompt, no el modelo ni el runtime.

El paso de 3/5 (13-jul, sampling sin seed fijo) a 5/5 (hoy, seed 42) es
consistente con una **preferencia estable de política** muestreada con
distinta suerte, no con una regresión: cuando el sampling se fija, la
preferencia por shell se expresa siempre.

## Implicaciones

1. **El A/B formal de runtime queda innecesario** — el mecanismo está
   identificado y no es de runtime. Pendiente cerrado sin gastar Nitro.
2. **gpt-oss:20b retiene el default** por la vía original; el
   desafío de e4b no se materializa vía 0.32.1.
3. **Decisión de banco — RESUELTA (2026-08-11): opción (b), equivalencia
   funcional acotada.** El principio: **el logro de la tarea decide el
   pass; la elección de tool es orientativa donde hay equivalencia
   genuina para el tamaño de la entrada.** Mecanismo: campo aditivo
   `accept_tool_calls: Vec<String>` en `TaskDef` — la aserción de tool
   pasa si el modelo llamó `expect_tool_call` O cualquiera de esos
   equivalentes. Vacío (el default de toda otra tarea) = estricto como
   antes, así que la mayoría del banco no cambia.
   - `read_file_basic`: `accept_tool_calls = ["shell_exec"]` (contar
     líneas con `wc -l` logra la tarea; la respuesta "3" ya se verifica).
   - `grep_basic`: `accept_tool_calls = ["read_file"]` **y** se le AGREGÓ
     el chequeo de respuesta que le faltaba (`expect_text_contains = "2"`)
     — antes su única aserción era la tool, así que un grep con respuesta
     equivocada pasaba. Hueco cerrado de paso.

   Por qué (b) y no (a)/(c): (a) medía flakiness de selección en tareas
   demasiado triviales para que la selección importe (1/5 reps, ruido no
   señal); (c) convertía la tarea en "seguir instrucciones" en vez de
   "elegir tool", degradándola (un usuario real no dice "usa grep"). La
   equivalencia es EXPLÍCITA por-tarea y sólo donde las tools son
   genuinamente intercambiables para ese tamaño — sobre un archivo grande
   `read_file` trunca (y ahora spillea) y `grep` no, así que ahí NO se
   listaría. Toda tarea con `accept_tool_calls` debe verificar la
   respuesta, o la relajación la dejaría sin ningún chequeo.

   **Sobre la comparabilidad histórica** (la razón por la que estaba
   abierta): ya no es un riesgo silencioso. El cambio bumpea
   `braze_git_commit` (grader en código) y `suite_fingerprint` (suite
   TOML), y **Dynamic Baseline Verification** (`--baseline-ref`,
   implementado el 2026-08-11) marca INVÁLIDA cualquier comparación
   cross-invocación contra un baseline pre-cambio. La herramienta que
   hacía este cambio inseguro ahora lo hace auditable — cerrar esta
   decisión y construir DBV eran, sin planearlo, el mismo trabajo.
   docs/dynamic-baseline-verification-design-2026-08-11.md.
