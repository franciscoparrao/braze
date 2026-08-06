# Hipótesis: A/B de `enable_project_memory` (lado prompt, dentro del bench)

Fecha: 2026-08-04
Estado: proposed — este documento se commitea ANTES de lanzar el sweep
(registro git-only, convención del proyecto)
Línea: paper2-learning (groundwork; ver "qué puede y qué no puede concluir")

## Pregunta

¿La sección de memoria de proyecto que `enable_project_memory` inyecta al
system prompt (archivos tocados en sesiones previas, V1 determinística)
cambia el pass rate de un modelo local en tareas de edición reales?

## Lo que este A/B puede y no puede concluir — declarado de entrada

La palanca es **cross-sesión** y el bench abre sesión fresca por repetición:
el brazo simple (`+ablate:project-memory`) corre siempre con memoria vacía y
su render es `None` → prompt idéntico al baseline **por construcción**. Por
eso existe el brazo nuevo `+ablate:project-memory-seeded`: el bench sintetiza
la memoria que una sesión previa habría dejado (los `setup_files` de la
propia tarea como archivos tocados — cero contenido escrito por el
experimentador; `project_key` = sandbox real, así que K-7 queda intacto).

- Este A/B **sí** mide: el efecto del contenido inyectado en el prompt
  (¿ayuda, estorba o nada?) y el costo de la plomería del hook.
- Este A/B **no** mide: el valor cross-sesión real (necesita el bench
  multi-sesión, Gate 0 del backlog). Por lo tanto **puede RECHAZAR la
  promoción o aportar a favor, pero no bastar para promover**.

## Hipótesis principal

H1: el brazo seeded difiere del baseline en pass rate más allá del ruido.

## Hipótesis nula

H0: pass rate indistinguible entre los tres brazos; el único efecto medible
es el costo en input tokens de la sección inyectada.

**Prior honesto** (del tipo de cambio medido en la línea Paper 2): nulo o
levemente negativo. La lista de archivos tocados agrega poco que el prompt de
cada tarea no diga ya, y cuesta tokens por ronda. Un positivo sería noticia.

## Diseño

| | |
|---|---|
| Suite | `discriminating.toml` (34 tareas, oráculo `cargo check`, ~2,9 pp/ítem) |
| Ejecutor | `gpt-oss:20b` GGUF canónico, LocalBackend/Harmony, Nitro |
| Brazos | `baseline` · `+ablate:project-memory` (vacío) · `+ablate:project-memory-seeded` |
| Repeticiones | 3, `--seed 42` (convención del piso de ruido), temp 0.2 (default del bench) |
| Total | 306 corridas |
| Timeout | 900 s/tarea (el hallazgo 300s-vs-900s: el tope que muerde binariza ruido) |
| Env | `source ~/.cargo/env`, `BRAZE_OLLAMA_NUM_CTX=32768`, `BRAZE_MAX_TOKENS=12288`, `BRAZE_LOCAL_FAMILY=harmony` — todo el tier queda además en `metadata.local_env` (v9 L-1) |

**El brazo vacío es un control del mismo prompt**: como su prompt es idéntico
al baseline, cualquier discordancia pareada entre ambos es piso de ruido
in-sweep (plomería + no-determinismo), medido dentro del propio experimento.

## Métricas

Primaria: pass rate por brazo; McNemar exacto sobre pares (tarea, repetición),
Holm entre los dos contrastes contra baseline. Secundarias: input_tokens
(costo de la sección), rondas, walltime, `schema_validation_failures`.

## Criterios de decisión, pre-registrados

1. **Gate de plomería** (se evalúa primero): si `|project-memory − baseline|`
   supera 2 celdas discordantes pareadas, investigar antes de leer el brazo
   seeded — con prompt idéntico, más que eso no es la palanca, es la
   infraestructura.
2. **Señal a favor**: `seeded − baseline` ≥ +3 tareas (≈9 pp) con McNemar
   p<0.05 **y** `seeded ≥ vacío`. Abre la discusión de promoción (que además
   exige el valor cross-sesión, fuera de alcance aquí).
3. **Daño**: `seeded − baseline` ≤ −3 con p<0.05 → negativo documentado; la
   palanca queda off y el dato alimenta round-economics como palanca de
   contexto con su tipo de cambio.
4. **Nulo** (lo esperado): la palanca queda off por default, sin cambio de
   estado; se reporta el costo de tokens medido.
5. **Sin iteración permitida** — es una medición simple, no una decisión de
   harness con cláusula de rescate.

## Riesgos anotados

- `expect_max_rounds` bajos en la suite (higiene pendiente desde el 28-jul):
  afecta a los tres brazos por igual; el diseño pareado lo cancela para los
  contrastes, pero deprime el pass rate absoluto — no comparar contra otras
  suites.
- Tareas sin `setup_files` (si las hubiera): el seed queda vacío y esa celda
  degenera al brazo vacío — cuenta como celda nula, no como señal.
- El seed dice qué archivos existen, y varias tareas ya lo dicen en el
  prompt: colinealidad esperada, va en el prior.

## Resultados

**2026-08-06 — GATE DE PLOMERÍA (criterio 1): FALLÓ.** baseline 59/102,
empty 68/102, **21 celdas discordantes pareadas** contra el umbral
pre-registrado de ≤2. La discordancia además es asimétrica (15 flips a
favor de empty vs 6, binomial p≈0.08) e incluye un volteo de tarea
completa (`tres_archivos_coordinados` 0/3 → 3/3) — improbable como ruido
de punto flotante con semillas idénticas. Conforme al criterio, **ningún
contraste se interpreta** hasta diagnosticar.

Hipótesis mecánica a probar: el hook escribe `.braze/memory.json` DENTRO
del sandbox durante la corrida (wiring de producción) → el brazo con hook
tiene un filesystem observable distinto a mitad de corrida, y las tareas
de esta suite están por diseño en la frontera del modelo. Si se
confirma, es un hallazgo sobre la palanca (también aplica en
producción), no solo plomería del bench.

Diagnóstico encolado tras el brazo seeded: las 4 tareas con más flips ×
{baseline, empty} × 3 reps con `BRAZE_BENCH_KEEP_SESSIONS=1` — permite
comparar el input de la ronda 1 (¿prompt idéntico?) y buscar
observaciones que toquen `.braze/` en las trayectorias.

Notas del barrido: `cambio_coordinado_dos_archivos` = timeout 900s en
los 6 intentos de ambos brazos (la tarea no cabe; independiente de la
suspensión del 05-ago, que congeló ~16.7h de reloj sin corromper datos —
timeouts de tokio usan reloj monotónico). `metadata.local_env` idéntico
en ambos brazos. El brazo seeded corre al escribir esto; sus datos se
leerán solo después del diagnóstico.

## Decisión

(pendiente)
