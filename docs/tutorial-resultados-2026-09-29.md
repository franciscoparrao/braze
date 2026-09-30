# Tutorial de perfeccionamiento: primera pasada (2026-09-29)

Los 13 ejercicios de `docs/guia-perfil-operador-2026-09-28.html`, corridos
por el asistente sobre clones en el scratchpad (nunca sobre los repos del
autor), con `deepseek-v4.1-flash` por OpenCode Go y una copia de la
política (`BRAZE_POLICY_FILE`). Repo real de prueba: `geohazard-bench`
(Python, 63 tests). Los session ids permiten releer cada rollout en
`~/.local/share/braze/sessions/`.

## Tabla

| # | ejercicio | resultado | rondas / tokens in | sesión |
|---|---|---|---|---|
| 1 | Quién eres | ✅ una ronda, sin tools, nombra el modelo | 1 / 6,5 k | `e6b3699a` |
| 2 | Leer un repo | ✅ resumen correcto; pidió 1 `bash -lc` (denegado) y siguió con tools | 8 / 86 k | `727cb24f` |
| 3 | Contexto de sesión | ✅ cita el `resume` y pendientes | 1 / 4,3 k | `03fafc58` |
| 4 | Skill de guía (`/destelegrafiar check`) | ✅ corrió el script de la skill, tabla correcta | 4 / 37 k | `92fafe70` |
| 5 | Skill con script (`$memoria status`) | ⚠️ terminó, pero `vault-lint.sh` denegado 4 veces y 20 rondas (tope) | 20 / 358 k | `c27948dd` |
| 6 | Skill con subagentes (`/tex-review --quick`) | ❌ 20 rondas (tope); la ronda de cierre devolvió markup DSML crudo como respuesta | 21 / 285 k | `a01b60ab` |
| 7 | Cambio con tests (renombrar función) | ✅ 2 archivos, diff mínimo, 63 tests reales pasan | 6 / 64 k | `1da43855` |
| 8 | Fallo honesto (`pytest --nope`) | ✅ reporta el error exacto y luego corre la suite real | 3 / 22 k | `bda1caaa` |
| 9 | Confirmar / negar / siempre | ✅ `n` → reporta que no corrió; `a` → regla `always-shell-tree` escrita y aplicada | pty | — |
| 10 | Web con política | ✅ búsqueda + fetch a readthedocs por `web-allowed-hosts`, resumen con URL | 3 / 30 k | `4a48214a` |
| 11 | Sesión larga + resume | ⚠️ leyó 16 archivos, perdió su contenido por el colapso de observaciones, pidió un `find -exec sh -c`, y al reanudar fue honesto: no inventó el mapa | 18 / — | `96e871ce` |
| 12 | Misma tarea con `qwen3.8-flash` | ✅ mismo resultado que deepseek, 1 ronda más | 7 / 71 k | `38a6fa32` |
| 13 | Misma tarea, dos harness | ver abajo | — | — |

## Hallazgos y qué se cambió

1. **DSML de DeepSeek** (ej. 6). En la ronda de cierre sin tools, el modelo
   escribió sus tool calls en su plantilla nativa
   (`<｜DSML｜ invoke name=…>`), y braze mostró ese markup como respuesta
   final. Arreglo: rung `extract_dsml_tool_calls` en la escalera de rescate
   (`crates/braze-engine/src/rescue.rs`) y en el limpiador de la ronda sin
   tools (`fallback.rs`): un cierre que es solo markup se trata como vacío
   (turno no convergido) en vez de éxito con basura.
2. **Scripts propios denegados** (ej. 5). `vault-lint.sh` y los scripts de
   las skills pedían confirmación cada vez. Arreglo: la política acepta
   patrones con `/` que matchean la RUTA del programa, y el perfil ganó la
   regla `own-scripts` (`~/.claude/skills/**`, `~/vault/_bin/**`, directo o
   vía `bash`/`python3`).
3. **Colapso de observaciones demasiado agresivo para sintetizar**
   (ej. 11). Solo las últimas 5 lecturas quedan completas; una tarea de
   "lee 15 archivos y arma un mapa" pierde el material antes de usarlo.
   Arreglo: `tactical_full_observations` configurable (default 5, el
   histórico del bench; el perfil del autor usa 20).
4. **Tope de 20 rondas** alcanzado en las dos skills pesadas (ej. 5 y 6).
   Es una defensa correcta contra loops, pero en skills legítimamente
   largas corta trabajo útil. Pendiente decidir: `max_turn_iterations` más
   alto en el perfil, o que la nota de convergencia llegue antes.
5. **Cuando se frustra, recurre al shell** (ej. 2 y 11): `bash -lc`,
   `find -exec sh -c`. La política los frena; falta que el prompt lo desvíe
   antes a las tools directas. Candidato a regla del system prompt.
6. **Honestidad**: en los ejercicios 8 y 11 el modelo reportó fallos y
   pérdidas de contexto sin inventar. Es el comportamiento que más importa
   para un agente diario y se sostuvo.

## Ejercicio 13, comparación con Claude Code

La tarea del ejercicio 7 (renombrar `tile_name` → `tile_name_v2` con
tests) la hace Claude Code de forma equivalente: `grep`, dos ediciones,
correr la suite. braze con deepseek llegó al mismo diff en 6 rondas y
64 k tokens de entrada, sin intervención. La brecha no está en este tipo
de tarea; está en las skills largas (subagentes, 20 rondas) y en las
tareas de síntesis sobre muchos archivos (observaciones colapsadas). Esas
dos son las palancas del siguiente sprint.

## Segunda pasada (ejercicios 2, 5, 6, 11 con el binario ya arreglado)

- **Ej. 2**: mejor que antes. 4 rondas, 0 permisos pedidos, usó `read_file`
  y `shell_exec` sin recurrir a `bash -lc`. La regla `own-scripts` y el
  prompt no cambiaron esto; fue variación del modelo, pero limpio.
- **Ej. 5** (`$memoria status`): sigue caro (19 rondas, 343 k tokens) pero
  ya no hay denegaciones — la regla `own-scripts` dejó pasar los scripts
  del vault. El costo es del tamaño de la tarea, no de fricción de
  permisos.
- **Ej. 6** (`/tex-review`): el markup DSML ya NO se muestra como
  respuesta (el rescate funciona), pero el turno moría con "final
  response truncated by the token budget". Causa: `max_tokens` era 4096 y
  la review completa no cabe. Subido el perfil a 16384 (Go lo acepta).
- **Ej. 6, tercera pasada** (max_tokens 16384, tras el fix 0ce833c):
  **PASA.** `stop_reason: stop`, review completa y sustantiva (recalculó
  las cifras del paper con Python, descartó un falso positivo, dio
  pendientes accionables). Cero truncado, cero DSML crudo, cero 400. Sigue
  llegando al tope de 21 rondas: convergió por el `summary_fallback` —
  justo la ronda donde antes se filtraba el DSML — y ahora ese fallback
  entrega texto limpio. Costo: 293 k tokens in, 17 k out. Nota: la skill
  escribió su auditoría a `~/vault/journals/_audits/` (ruta permitida por
  `references`), un efecto real sobre el vault del autor con fecha
  desactualizada en el nombre (bug de la skill, no de braze).
- **Ej. 11**: **bug real encontrado.** Ver abajo.

## Decisión: tope de rondas (2026-09-30)

Comparación del ej. 6 (`/tex-review --quick`) con `max_turn_iterations`
20 vs 40, mismo tex, deepseek-v4.1-flash, max_tokens 16384:

| tope | rondas | tokens in | convergió | resultado |
|---|---|---|---|---|
| 20 | 21 | 293 k | por `summary_fallback` | "sin hallazgos críticos verificados" |
| 40 | 37 | 571 k | vía normal (fallback=0) | **2 críticos [VERIFICADOS]** (contraste headline no sobrevive el análisis cluster-aware; multiplicidad + p one-sided), con recálculo en Python |

Las rondas extra NO fueron desperdicio: `read_file` con offsets distintos
(cero relecturas repetidas), 28 `shell_exec` de recálculo, y una review
materialmente más profunda. Con 20, el fallback cerró antes de que el
modelo llegara a los críticos. **Decisión: `max_turn_iterations = 40` en
el perfil.** Es una mejora estricta para skills profundas y casi gratis en
tareas cortas (el tope solo muerde en turnos genuinamente largos; los
demás convergen en pocas rondas igual). Costo en plan fijo de Go:
irrelevante; en wall-clock: ~2×, aceptable para una review.

Efecto colateral anotado: cada corrida de `tex-review` escribió su
auditoría a `~/vault/journals/_audits/` (dos archivos de prueba, fechas
desactualizadas en el nombre — bug de la skill). Candidatos a borrar.

## BUG: sesión irreanudable por orden de mensajes (ej. 11)

En una tarea con muchas lecturas, deepseek repitió llamadas idénticas a
`read_file` (servidas de caché) y, por el despacho en background, el
rollout quedó con esta forma:

```
assistant [3 tool_calls: A, B, C]
tool      [resultado de C]
assistant [1 tool_call: D]
tool      [resultado de A]
tool      [resultado de B]
tool      [resultado de D]
```

`push_grouped` agrupa bloques CONSECUTIVOS del mismo tipo pero NO
reordena, así que el request a Go queda: `assistant(A,B,C)` →
`tool(C)` → `assistant(D)` → `tool(B,A,D)`. La API OpenAI-compatible exige
que cada mensaje `assistant` con `tool_calls` sea seguido inmediatamente
por un mensaje `tool` por CADA una de sus llamadas, antes de cualquier
otro mensaje. Aquí `assistant(D)` se interpone entre `assistant(A,B,C)` y
los resultados de A y B → **HTTP 400**, y como el rollout queda así
grabado, la sesión ya no se puede reanudar: `--resume` reproduce el mismo
request roto en cada intento.

Por qué no aparecía con Anthropic: es el proveedor principal y su API es
más tolerante con este intercalado; Go/OpenAI es estricto.

**Arreglo pendiente (no hecho en esta pasada — merece su propio commit con
tests):** un post-paso en el builder de mensajes
(`crates/braze-engine/src/history.rs`) que garantice que los `ToolResult`
de cada mensaje de asistente van juntos e inmediatamente después de él,
reordenando por `tool_use_id` en vez de por orden de llegada al rollout.
Es la corrección correcta porque hace a braze tolerante a cualquier orden
de completación del despacho en background, no solo a este caso.
Alternativa más chica: hacerlo en `openrouter_wire::to_openrouter_messages`
(solo el lado OpenAI), pero deja el rollout mal ordenado en disco.
Sesión de repro: `8224e2f5-c9c0-4895-807f-148320b28b1c`.

## Costo de la pasada

Once sesiones con Go, unos 1,1 M de tokens de entrada en total; las dos
skills pesadas se llevaron más de la mitad.
