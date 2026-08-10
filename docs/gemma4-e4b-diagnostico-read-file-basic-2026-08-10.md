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
3. **Decisión de banco abierta (del autor)**: `read_file_basic` mide
   hoy "¿elige la tool canónica?" y no "¿puede leer un archivo?". Las
   opciones — (a) dejarlo así (mide selección de tool, que es una
   habilidad real y el resto de la familia single_tool lo corrobora),
   (b) aceptar equivalencia funcional en el grader, o (c) endurecer el
   prompt ("usa read_file") — cambian la comparabilidad histórica de
   TODOS los sweeps previos, así que no se toca sin decidirlo
   explícitamente. Hasta entonces, leer los números de e4b en
   `default.toml` sabiendo que ~1 tarea (≈5,3% del banco) es esta
   clase.
