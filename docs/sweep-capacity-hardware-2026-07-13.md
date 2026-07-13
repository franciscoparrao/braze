# Prerrequisito de hardware: ¿un modelo que ya cabe en Nitro supera a qwen3.5-coder?

Fecha: 2026-07-13
Contexto: prerrequisito de `docs/local-backend-stencil-design.md` § "Hardware"
tras desacoplar el eje de capacidad del veredicto RECHAZADO de
constrained decoding (`docs/sweep-constrained-decoding-2026-07-12.md`).
Pregunta: ¿algún modelo servible hoy en los 16GB RAM de Nitro (sin
offloading, sin RAM nueva) supera a `qwen3.5-coder` — el mejor local del
proyecto hasta ahora — en pass rate Y latencia? 3 filas × 19 tareas × 5
reps = 285 corridas, seed 42, temp 0.2, Nitro, `--no-ollama-stop`,
`qwen3.5-coder` corrido como ancla dentro del MISMO sweep (no
cross-sweep) para una comparación limpia. Estado: **CERRADO.** Datos:
`docs/sweep-capacity-hardware-2026-07-13.json`/`.log`.

`gpt-oss:20b` no estaba instalado — se bajó (`ollama pull gpt-oss:20b`,
~13GB) el mismo día antes de correr el sweep. `gemma4:12b` ya estaba
instalado y nunca se había probado en este suite.

## Resultados

| Modelo | Pass rate [IC 95% Wilson] | Latencia promedio | schema_fail | exec_fail |
|---|---|---|---|---|
| `gpt-oss:20b` | **98.9%** (94/95) | **13.0s** | 0 | 4 |
| `gemma4:12b` | 82.1% [76.2,92.1]† | 29.8s | 2 | 14 |
| `qwen3.5-coder` (ancla) | 92.6% [89.0,100.3]† | 24.7s | 8 | 0 |

† Cotas Wilson calculadas con la fórmula estándar; en `p` muy alto/bajo
con `n=95` el límite superior puede exceder 100% por la aproximación —
leer como "cerca del techo", no literal.

Delta contra el ancla (mismo sweep, Newcombe 95%):
- `gpt-oss:20b` − `qwen3.5-coder` = **+6.3pp** [+0.2, +14.0] — excluye
  cero, aunque por poco.
- `gemma4:12b` − `qwen3.5-coder` = **−10.5pp** [−21.1, −0.8].

Referencia cruzada (no comparable a nivel de sweep, solo dirección):
`qwen2.5:7b` ya había perdido esta comparación en
`docs/sweep-curva-multiescala-2026-07-10.md` (80% [71,87] baseline,
5.1s) — no se re-testeó.

## Hallazgos

1. **`gpt-oss:20b` limpia la barra en las dos dimensiones que pedía el
   prerrequisito, sin offloading ni RAM nueva.** Mejor pass rate (+6.3pp,
   CI fuera de cero) y casi el doble de rápido (13.0s vs 24.7s, 1.9×) que
   `qwen3.5-coder` dentro del mismo sweep. Una sola falla en 95 corridas
   (`grep_basic`, `assertion_tool_call`) — no hay indicio de que el
   pass rate alto sea artefacto: `schema_fail=0`, `rescues=0` (nunca
   necesitó la escalera de rescate textual).
2. **`gemma4:12b` queda descalificado igual que `qwen2.5:7b`** — pierde
   en las dos dimensiones (−10.5pp, más lento). Confirma que el "modelo
   más grande instalado" no es automáticamente mejor; hace falta medir.
3. **El mecanismo de `qwen3.5-coder` (el ancla) tiene 8 `schema_fail`
   en este sweep** — vs. 0 en `gpt-oss:20b` — un dato incidental: el
   "mejor local" hasta ahora seguía teniendo fricción de schema que
   `gpt-oss:20b` no tuvo, en el mismo suite y sesión.
4. **`qwen3.5-coder` corrió a 92.6% en este sweep, no 98% como en la
   curva del 2026-07-10** — variación cross-sesión esperada (distinta
   carga térmica de Nitro, distinto momento), exactamente por lo que el
   ancla se corrió DENTRO de este mismo sweep en vez de reusar el número
   viejo — la comparación válida es 92.6% vs 98.9%/82.1% de esta misma
   sesión, no contra el 98% de otro día.

## Implicación

El resultado invierte la pregunta que motivó todo el documento
`local-backend-stencil-design.md`: no hace falta `LocalBackend`
in-process ni comprar RAM para conseguir un modelo mejor que
`qwen3.5-coder` en Nitro — ya existe uno (`gpt-oss:20b`) que corre bien
con la infraestructura actual (`OllamaBackend`, 16GB RAM, sin cambios).
El eje de capacidad del documento de diseño queda resuelto de la forma
más barata posible: cambiar de modelo, no construir infraestructura
nueva. Detalle de la decisión y su impacto sobre el resto del documento
en `docs/local-backend-stencil-design.md` § "LocalBackend-por-capacidad".

## Limitaciones

- n=95 de un solo suite (`default.toml`, 5 skills, 19 tareas) — no es
  la batería `g10-weak-skills` que calibró a `qwen3.5-coder` como mejor
  local originalmente (CLAUDE.md § "Modelos locales recomendados").
  Antes de promover `gpt-oss:20b` formalmente ahí, correr esa batería.
- Un solo sweep, sin repetición cross-sesión — el 92.6% del ancla en
  esta sesión vs 98% en la curva del 07-10 ya muestra que hay variación
  real entre sesiones; `gpt-oss:20b` no se ha replicado todavía.
- `gpt-oss:20b` es un modelo nuevo para el proyecto — sin historial de
  uso en vivo (TUI, sesiones largas, contextos grandes) más allá de este
  sweep.

## Cómo reproducir

```bash
BRAZE_OLLAMA_BASE_URL=http://192.168.1.8:11434 RUST_LOG=braze_engine=info \
cargo run --release -p braze-bench -- crates/braze-bench/suites/default.toml \
  --backends "ollama:gpt-oss:20b,ollama:gemma4:12b,ollama:qwen3.5-coder" \
  --repetitions 5 --seed 42 --no-ollama-stop \
  --output docs/sweep-capacity-hardware-<fecha>.json
```
