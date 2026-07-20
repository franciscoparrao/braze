# Ancla SWE-bench Lite — resultados (2026-07-19)

**Diseño pre-registrado**: `docs/swebench-lite-anchor-design-2026-07-19.md`
(comprometido antes del driver y de los runs). Driver:
`tools/swebench_driver.py`; muestra seed-42 en
`docs/swebench-lite-sample-2026-07-19.txt` (20 instancias: 13 django,
2 matplotlib, 2 sphinx, 3 sympy). 3 brazos × 20 instancias × 2 reps =
120 runs contra Nitro (Ollama 0.30.7, digests en los JSONs de runs).
**Grading: harness OFICIAL de SWE-bench 4.1.0 vía Docker** — no un
grader autoral; reports crudos en `docs/swebench-lite-grading/`.

## Validez

**0 fallos de transporte en los tres brazos** (regla del 2%: cumplida
con margen máximo). 0 timeouts del cap de 600s. 3 runs con exit≠0
(errores de braze registrados, no de red).

## Resultados

| Brazo | parches ≠0B / 40 | resueltas / 40 | wall mediana |
|---|---|---|---|
| llama3.2:1b | **0** | **0** | 4 s |
| llama3.2:1b + lead:gemma4:e4b | 6 | **2** (5.0%) | 50 s |
| gemma4:e4b solo | 14 | **3** (7.5%) | 158 s |

Instancias resueltas (tests FAIL_TO_PASS pasan y PASS_TO_PASS no se
rompen, veredicto del harness oficial):
- `1b+lead`: **django__django-14382 en AMBAS repeticiones** (pass^2).
- `e4b`: django-14382 (rep0 y rep1) + django-11099 (rep0).

## Contra las lecturas pre-declaradas

**E-S1 (piso uniforme) — NO es el resultado.** Solo el 1B solo es piso
absoluto — y de la manera más informativa posible: **cero parches en 40
runs** (mediana 4 s: responde texto sin siquiera intentar editar). Los
otros dos brazos resuelven instancias reales.

**E-S2 (la palanca mueve) — direccional, con una capa significativa.**
Dos capas honestas:
- *Conductual*: el lead convierte al 1B de "nunca edita" a "produce
  parches" — 0/40 → 6/40, Fisher exacto p=0.026, fuera del ruido.
- *Resolución*: 0/40 → 2/40 (p=0.494) — direccional, no separable del
  ruido a n=40. La instancia resuelta (django-14382) lo fue en las DOS
  repeticiones — consistencia, no un golpe de suerte de sampling — pero
  el nivel agregado no da poder estadístico.

**E-S3 (el techo replica) — SÍ.** `1b+lead` (2/40) ≈ `e4b` solo (3/40),
p=1.0 — el patrón central del paper (la composición vive en el techo
del lead, el 1B ni suma ni resta) se sostiene en tareas SE reales, en
el nivel absoluto donde ese techo vive (~5-7.5% en este slice).

**Costo (lectura siempre-reportable)**: mediana de wall 4 s (1b) /
50 s (1b+lead) / 158 s (e4b) — el lead como accelerant del turno
también replica direccionalmente aquí (el compuesto es ~3× más rápido
que el lead solo por run, consistente con §5.1 del paper).

## Qué le da esto al paper (Issue 1 del blind)

El gap de constructo se acota con datos en ambas direcciones: (a) las
palancas del harness NO rescatan competencia repo-level a 1B — el
nivel absoluto es piso con o sin palancas, y el claim del paper queda
correctamente acotado a tool-calling reliability; (b) pero los DOS
patrones centrales — la palanca mueve comportamiento (fuera de ruido) y
el techo-fijado (composite ≈ lead solo) — transfieren a issues reales
de Django graduados por los tests oficiales. Y una nota de nivel: un
8B-MoE cuantizado en CPU resolviendo 2 instancias distintas de
SWE-bench Lite es un dato de referencia útil para la franja "local
pequeño" que el benchmark casi no reporta.

## Amenazas (además de las del diseño)

- n=40/brazo: la capa de resolución es direccional por construcción;
  el diseño lo anticipó ("no busca un número alto").
- 5 instancias truncadas a 4.000 chars (registrado por-run en los
  JSONs) — parte del deployment de 8K medido, no escondido.
- Contaminación de pesos (repos en pre-entrenamiento): afecta niveles,
  no comparaciones entre brazos (los tres comparten pesos base del
  lead donde aplica).
