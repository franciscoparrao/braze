# Síntesis: condición de amortización confirmada en 3 tareas B independientes

Fecha: 2026-07-17
Contexto: cierre del ciclo abierto por `docs/paper2-memory-distillation-protocol-2026-07-16.md`
§ "Hallazgo del piloto M1" — con solo la tarea `loop` no se podía saber si la ausencia
de `round_reduction` era la norma o un caso atípico. Se diseñó y corrió una tercera
tarea B (`move`, bug E0382) para responder eso. Los tres pares corrieron a n=20 con el
mismo backend (`ollama:gpt-oss:20b`/Nitro), mismo playbook humano genérico, mismo rango
de seeds (42-61, en 4 tandas de 5 para tolerar cortes del proceso). Datos:
`docs/sweep-memory-distillation-r20-moveB-2026-07-17.json` (140 corridas, 7 tareas × 20).
Estado: **CERRADO**.

## Los tres pares, lado a lado

| Tarea | Pass (none→pb) | Rounds (none→pb) | round_reduction | tokens/ronda (none→pb) | net_token_delta | wall_s (none→pb) |
|---|---|---|---|---|---|---|
| ORIGINAL (bug canónico, saturada) | 14/20→16/20 | 6.70→5.75 | **+0.95** | 1406→1590 (+184) | **-304** | 57.4→40.6 (p=0.0026) |
| LOOP (E0502 por iteración) | 15/20→14/20 | 5.60→5.45 | +0.15 | 1383→1625 (+243) | **+1076** | 49.5→51.2 (p=0.55) |
| MOVE (E0382, nueva) | 18/20→15/20 | 6.40→6.05 | +0.35 (p=0.035) | 1394→1662 (+268) | **+1132** | 48.5→47.0 (p=0.62) |

`round_reduction = rounds(none) - rounds(playbook)`; positivo significa que el playbook
acorta el turno. `net_token_delta = input_tokens(playbook) - input_tokens(none)`;
negativo significa que el playbook ahorra tokens netos pese a su costo fijo por ronda.

## Veredicto: la condición de amortización es la norma, no el caso atípico

De 3 tareas B de la misma familia (`rust_compile_repair`), con el mismo playbook
genérico, **solo 1 de 3 ahorra tokens netos** (la tarea original, saturada/memorizada).
Las otras 2 (`loop`, `move`) cuestan tokens netos de forma consistente
(+1076, +1132 respectivamente) porque el `round_reduction` que producen (+0.15, +0.35)
es demasiado chico para pagar el costo fijo de ~200-270 tokens/ronda que el playbook
agrega en cada turno.

El patrón que se ve en las 3 filas es coherente: el playbook ahorra rondas
**precisamente en la tarea que el modelo ya tiene memorizada** (donde, irónicamente,
menos la necesita para acertar) y **no ahorra rondas en las dos tareas donde el modelo
tiene que razonar de verdad** (donde, en teoría, más se esperaría que una metodología
explícita ayudara). Esto es lo opuesto de lo que predice la hipótesis central del
Paper 2 (`docs/hypothesis-2026-07-16-memory-distillation.md`: "un playbook procedimental
... aumenta el success rate ... en tareas futuras relacionadas") — al menos para este
playbook genérico y esta familia de bugs.

## Pass rate: sin señal consistente en ninguna tarea

Ninguno de los tres pares alcanza significancia en pass rate (Fisher p=0.72, 1.00, 0.41).
Más notable: la dirección del punto estimado es negativa en 2 de 3 (`loop` 15→14,
`move` 18→15) y solo positiva en la tarea saturada (`original` 14→16). No hay evidencia,
en ninguna de las tres tareas piloteadas hasta ahora, de que este playbook mejore
`success_rate` de forma confiable — el hallazgo de eficiencia de la tarea original sigue
siendo el único resultado positivo de todo el piloto M1, y ahora sabemos que no
generaliza ni siquiera dentro de la misma familia de bugs.

## Nota de reproducibilidad

La tarea `loop` había mostrado antes (sweep de un solo tiro, n=20, seed base 42) un
aumento de wall time *significativo* con playbook (77.9s→93.5s, p=0.008). Esta
repetición independiente (mismos seeds 42-61, pero ejecución nueva — Ollama no es
bit-exacto con seed fijo, ya documentado en `docs/sweep-memory-distillation-pilot-
2026-07-16.md`) da wall time *sin diferencia significativa* (49.5s→51.2s, p=0.55). El
`net_token_delta` positivo sí replica en ambas corridas (dirección y orden de magnitud
consistentes), pero el efecto de wall time específico no fue tan estable — otro
recordatorio de que un solo sweep, incluso a n=20, puede sobre-representar la
magnitud de un efecto secundario aunque el efecto primario (tokens) sea real.

## Decisión para el protocolo

Actualiza `docs/paper2-memory-distillation-protocol-2026-07-16.md` § "Hallazgo del
piloto M1": la condición de amortización no es una curiosidad de una tarea diseñada
apresuradamente, es el comportamiento típico de este playbook fuera de la tarea
para la que fue escrito. Esto refuerza, con evidencia más fuerte, la recomendación ya
anotada ahí: el lifecycle de `LearnedPlaybook` debería exigir `round_reduction`
medido (no solo corrección semántica ni `applies_when` matching) antes de promover
`candidate -> validated`. Un playbook "correcto" que no ahorra rondas en la tarea
concreta que enfrenta es, en términos netos de tokens, un costo, no un beneficio.
