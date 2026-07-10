# A/B de 3 brazos: lead proactivo vs escalación reactiva pura

Fecha: 2026-07-10
Contexto: la CORRECCIÓN de `docs/sweep-si2-lead-ab-2026-07-09.md` (hallazgo
I-1 de `docs/AUDITORIA-2026-07-v6.md`) estableció que el A/B original de
SI-2 midió "lead proactivo los primeros 3 turnos" y no "escalación
reactiva" — pero no podía separar los mecanismos porque los knobs del
`EscalatingBackend` eran inalcanzables. Con I-1 cerrado (`9aff6aa`,
`+ablate:lead-turns=0`), este sweep es la separación prometida: mismo
lead, mismos modelos, un brazo por mecanismo.
Estado: **CERRADO**. Datos crudos en
`docs/sweep-lead-3brazos-2026-07-10.json`/`.log`.

## Diseño

`crates/braze-bench/suites/default.toml` (19 tareas, 5 skills) × 3 brazos
× 5 repeticiones = 285 corridas, contra Nitro
(`BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434`, `--no-ollama-stop`,
temp 0.2, sin seed):

- **`ollama:qwen2.5:3b`** — baseline, sin lead.
- **`ollama:qwen2.5:3b+lead:ollama:gemma4:e4b`** — lead **proactivo**
  (default `DEFAULT_LEAD_TURNS = 3`: el lead maneja los primeros 3
  rounds de cada turno; la escalación reactiva también está armada pero
  casi nunca alcanza a dispararse).
- **`ollama:qwen2.5:3b+lead:ollama:gemma4:e4b+ablate:lead-turns=0`** —
  escalación **puramente reactiva**: el worker maneja todo salvo cuando
  acumula `failure_threshold = 2` observaciones de tool falladas
  consecutivas, que le pasan el control al lead por
  `escalation_turns = 3` rounds.

Nota de trazabilidad: `braze_git_commit: 8b720f1` en el metadata se lee
al escribir el output (01:59); el binario se compiló a las 00:41, antes
de que el código del Paquete 3 (pricing) existiera en el árbol — por eso
las filas no traen `estimated_cost_usd`. Verificado después con binario
fresco: un smoke de 1 tarea contra Nitro reporta `estimated_cost_usd:
0.0` (presente, no `None`) para un backend Ollama, como diseña el
Paquete 3. Los mecanismos que este sweep mide (I-1/ablations) son
anteriores al binario y no están afectados.

## Resultados — comparación por brazo

| Brazo | Pass rate (±95% Wilson) | avg rounds | avg s | avg tok_out | schema_fail | exec_fail | run_error | escalaciones |
|---|---|---|---|---|---|---|---|---|
| baseline | 64/95 (67.4%, IC [57.4, 76.0]) | 2.2 | 5.5 | 86 | 19 | 25 | 5 | — |
| lead proactivo | 88/95 (92.6%, IC [85.6, 96.4]) | 2.5 | 32.2 | 353 | 0 | 15 | 0 | 1 |
| reactivo puro (`lead-turns=0`) | 72/95 (75.8%, IC [66.3, 83.3]) | 2.2 | 11.0 | 114 | 12 | 23 | 8 | 7 |

## Resultados — comparación por skill

| Skill | baseline | reactivo puro | lead proactivo |
|---|---|---|---|
| single_tool | 26/35 (74%) | 28/35 (80%) | 30/35 (86%) |
| no_tool | 15/15 (100%) | 15/15 (100%) | 15/15 (100%) |
| multi_step | 10/15 (67%) | 13/15 (87%) | 15/15 (100%) |
| **error_recovery** | **0/15 (0%)** | **2/15 (13%)** | **14/15 (93%)** |
| distractor_selection | 13/15 (87%) | 14/15 (93%) | 14/15 (93%) |

## Hallazgos

1. **La atribución causal queda confirmada: el mecanismo que mueve el
   pass rate es la apertura proactiva, no la escalación reactiva.**
   Proactivo 92.6% [85.6, 96.4] vs reactivo puro 75.8% [66.3, 83.3] —
   los intervalos no se solapan. Reactivo vs baseline (+8.4pp, IC
   [57.4, 76.0] vs [66.3, 83.3]) se solapan ampliamente: con n=95 por
   brazo, la mejora del mecanismo reactivo solo **no es distinguible de
   ruido**. La conjetura de la CORRECCIÓN del 2026-07-10 pasa de
   inferencia (contar escalaciones en el brazo proactivo) a resultado
   experimental directo.

2. **El trigger reactivo es ciego a fallas semánticas — y `error_recovery`
   falla casi solo semánticamente.** Las 13 fallas de `error_recovery`
   del brazo reactivo ocurrieron con **cero escalaciones**: 8
   `assertion_text`, 3 `assertion_files`, 2 `model_backend_error`. El
   trigger cuenta observaciones de tool *falladas* consecutivas
   (`trailing_failed_observations >= 2`,
   `crates/braze-model/src/escalation.rs`), pero el modo de falla del
   worker en esta skill es "ejecutar con éxito la acción equivocada":
   lee el mensaje de error y responde mal, o escribe/edita el archivo
   incorrecto sin que ninguna tool falle. Ninguna señal mecánica se
   acumula, el lead nunca entra, la tarea falla. La skill donde el lead
   proactivo más aporta (0/15 → 14/15) es estructuralmente invisible
   para el reactivo (0/15 → 2/15).

3. **Cuando la escalación reactiva sí dispara, funciona: 7/7 corridas
   con escalación pasaron** (5 `multi_step`, 1 `single_tool` de edición,
   1 `error_recovery`). No es un mecanismo malo — es un mecanismo de
   cobertura estrecha: solo ve la franja de fallas que se manifiestan
   como tool calls rechazadas/erróneas consecutivas. En esa franja,
   pagar 2× de latencia promedio (11.0s vs 5.5s) compra +8pp agregados
   (no significativos); la apertura proactiva paga 5.9× (32.2s) y
   compra +25pp robustos.

4. **El lead proactivo también elimina una clase de falla que el
   reactivo ni siquiera puede ver: las respuestas vacías.** Los 13
   `run_error` del sweep ("model's response had no text and requested
   no tool calls", concentrados en `shell_exec_basic`) ocurren solo en
   los brazos donde qwen2.5:3b abre el turno (5 baseline, 8 reactivo, 0
   proactivo). Un run que aborta por respuesta vacía nunca llega a
   acumular observaciones falladas — otra falla estructuralmente fuera
   del alcance del trigger reactivo.

5. **`rescued_tool_calls = 0` en las 285 corridas** — tercera
   replicación (tras los dos sweeps del 09) de que el rescate textual
   no se activa con estos modelos en este suite; los fallos de
   qwen2.5:3b son de schema/semántica, no de formato textual.

6. **La latencia del brazo proactivo no es estable entre sweeps**: 32.2s
   promedio acá vs 13.9s el 2026-07-09 para el mismo brazo
   (`+lead:gemma4:e4b`). Mismo suite, mismo Nitro — condiciones de carga
   distintas (este sweep corrió ~00:40-02:00). Tratar las latencias
   absolutas como orden de magnitud; los ratios dentro de un mismo
   sweep son lo comparable.

## Implicación de diseño

Para la tesis "el harness compensa la escala del modelo", la palanca
validada es **"un modelo capaz abre el turno y deja el plan encaminado"**
(delegación proactiva), no "rescatar al chico cuando tropieza"
(escalación reactiva). Si se quisiera rehabilitar el mecanismo reactivo,
el dato apunta a que el cuello es el *trigger*, no la escalación en sí
(punto 3): necesitaría señales que capturen falla semántica o
degeneración — p.ej. rounds sin converger, respuestas vacías, o
divergencia respecto del objetivo — no solo tool failures consecutivas.
Queda anotado como hipótesis, no como ítem comprometido.

## Limitaciones

- n=15 por celda de skill — el desglose por skill separa los extremos
  (`error_recovery` 0-2/15 vs 14/15) pero no las diferencias chicas
  (`distractor_selection` 13 vs 14 vs 14).
- 8 de las 23 fallas del brazo reactivo son `run_error` de respuesta
  vacía (degeneración del worker, no decisión equivocada) — el 75.8%
  mezcla ambos modos de falla. El punto 4 lo desagrega.
- Sin seed fijo; sampling no determinístico del proveedor.
- El binario del sweep es pre-Paquete-3 (ver nota de trazabilidad) —
  sin `estimated_cost_usd` en las filas; irrelevante para los brazos
  comparados (todo Ollama, $0).

## Cómo reproducir

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "ollama:qwen2.5:3b,ollama:qwen2.5:3b+lead:ollama:gemma4:e4b,ollama:qwen2.5:3b+lead:ollama:gemma4:e4b+ablate:lead-turns=0" \
  --repetitions 5 --temperature 0.2 --no-ollama-stop \
  --output docs/sweep-lead-3brazos-<fecha>.json
```

## Próximo paso

Con este documento, el eje `+lead:` queda cerrado en sus dos variantes y
con atribución causal limpia. Lo que sigue para la matriz del paper
(pendiente desde el A/B del 09): los brazos `+planner` y
`+planner+lead`.
