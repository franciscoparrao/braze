# Ancla repo-level: slice de SWE-bench Lite — diseño pre-registrado (2026-07-19)

**Estado**: comprometido ANTES de implementar el driver y de correr
ningún run. Responde al Issue 1 del review blind b2 (EMSE): el
instrumento del paper mide *tool-calling reliability* (suite autoral +
BFCL single-turn); el framing dice *agentic coding*. Este ancla mide la
palanca central en tareas de ingeniería de software REALES (repos,
issues, tests) o, si el resultado es piso, acota el claim con datos en
vez de con prosa.

## Hipótesis y honestidad previa

A 1B en CPU con `num_ctx` 8192, la expectativa REALISTA es piso (~0%
resolved) en todos los brazos — los modelos frontera resuelven 20-45%
de Lite. El ancla NO está diseñada para producir un número alto: está
diseñada para responder **si la palanca lead mueve ALGO en tareas
repo-level, y si el patrón de techo (composite ≈ lead solo) se
sostiene ahí**. Un piso uniforme es un resultado publicable que acota
el claim del paper ("las palancas compensan confiabilidad de
tool-calling, no competencia de tarea repo-level a 1B"); un
movimiento es un resultado mayor. Ambas lecturas se comprometen abajo.

## Muestra (determinística)

- Dataset: `princeton-nlp/SWE-bench_Lite`, split `test` (300 instancias,
  todas Python).
- Selección: ordenar por `instance_id` ascendente; muestrear **20
  instancias** con `random.Random(42).sample(...)` sobre la lista
  ordenada. La lista resultante se escribe en el JSON de resultados y
  en este repo al primer run (`docs/swebench-lite-sample-2026-07-19.txt`).
- Sin filtro por repo/dificultad — el slice es lo que la semilla dé.

## Brazos (3) y repeticiones

| Brazo | Qué responde |
|---|---|
| `ollama:llama3.2:1b` | piso del executor chico |
| `ollama:llama3.2:1b+lead:ollama:gemma4:e4b` | ¿la palanca central mueve algo repo-level? |
| `ollama:gemma4:e4b` | ¿el patrón de techo (composite ≈ lead solo) replica? |

2 repeticiones × 20 instancias × 3 brazos = **120 runs**. Timeout
600 s/attempt (las tareas repo no son micro-tareas; el cap de 180 s
del bench cortaría exploración legítima). Un brazo a la vez contra
Nitro (Ollama 0.30.7, digests registrados por el driver), después de
que la cola actual (re-run Bloque 2) termine.

## Mecánica (driver, no braze-bench)

`braze-bench` no tiene fixtures de tipo repositorio; se usa un driver
(`tools/swebench_driver.py`, a implementar) que por (instancia, brazo,
rep):

1. Prepara un checkout limpio del repo en el `base_commit` de la
   instancia (cache local de clones; `git worktree` por run).
2. Invoca `braze run --output-format json` EN ese checkout con el
   prompt: `problem_statement` truncado a 4.000 caracteres (marcador
   explícito si se corta — `num_ctx` 8192 es parte del claim de
   deployment chico, no un accidente) + una instrucción fija de una
   línea ("Fix the issue described above by editing the repository.
   Do not run tests.").
3. Registra el JSON de braze (tokens, session id) + `git diff` del
   checkout como `model_patch` + metadata (commit de braze, digests,
   versión de Ollama, wall time).
4. Grading OFFLINE posterior con el harness oficial (`pip install
   swebench`, Docker local en la máquina de trabajo): `resolved` =
   los `FAIL_TO_PASS` pasan y los `PASS_TO_PASS` no se rompen. El
   grader es el de SWE-bench, no uno autoral — ese es el punto del
   ancla.

braze corre con sus tools locales default y la misma postura de
permisos del bench (sandbox = el checkout). El driver NO reintenta
runs fallidos de modelo; los fallos de transporte (request nunca llegó
/ stream muerto <1 s, mismo criterio del paper) se cuentan aparte.

## Regla de validez (idéntica al ancla BFCL)

Ningún brazo puede exceder **2% de fallos de transporte** (sobre 40
runs/brazo). Si excede, el sweep del brazo se descarta y se re-corre;
no hay exclusión analítica sobre datos vistos. Fallos de grading
(imagen Docker de la instancia no construye, etc.) se reportan por
instancia y excluyen esa instancia de TODOS los brazos por igual
(exclusión simétrica, decidida por el grader, no por los resultados).

## Lecturas pre-declaradas

- **E-S1 (piso uniforme)**: resolved ≈ 0 en los tres brazos (ningún
  brazo fuera del ruido de 0 con n=40). El paper agrega el ancla como
  cota honesta: las palancas del harness NO rescatan competencia
  repo-level a estas escalas; el claim del título/abstract se acota a
  tool-calling reliability, y la § Threats reemplaza la amenaza de
  constructo por este dato. (Este es el resultado esperado.)
- **E-S2 (la palanca mueve)**: `1b+lead` > `1b` fuera de cero
  (Newcombe 95%). Resultado mayor: la palanca central transfiere a
  tareas SE reales; se reporta con la misma prominencia que E2 del
  ancla BFCL, junto al patrón de techo (comparación con brazo 3).
- **E-S3 (techo replica)**: `1b+lead` ≈ `e4b` solo (cruza cero) — el
  patrón central del paper en tareas repo-level, cualquiera sea el
  nivel absoluto.
- **Lectura de costo (siempre)**: tokens y wall-time por brazo se
  reportan; a 600 s de timeout el costo de pared es parte del
  resultado, no un incidente.

Cualquier divergencia entre repeticiones se reporta (pass^2 además de
pass rate agregado, misma métrica de confiabilidad del bench).

## Amenazas anotadas de antemano

- `problem_statement` truncado a 4.000 chars: se reporta cuántas
  instancias se truncaron; es una limitación del deployment de 8K que
  el ancla mide, no esconde.
- El driver no es braze-bench: sin suite fingerprint TOML. Se compensa
  registrando en el JSON del driver el commit de braze, la lista exacta
  de instancias, digests y versión del server — los mismos campos de
  `RunMetadata`.
- Contaminación de pesos (los repos de SWE-bench están en el
  pre-entrenamiento de casi todo modelo): afecta el NIVEL absoluto,
  no la comparación entre brazos, que es lo que este ancla mide. Se
  anota en Threats.
- 120 runs × hasta 600 s = hasta ~20 h de pared en el peor caso
  teórico; se corre por brazos en tandas. No hay cláusula de aborto
  por lentitud: si el costo excede lo tolerable, el sweep se pausa
  ENTRE brazos y se retoma, nunca a mitad de un brazo.

## Sin cláusula de adopción

Como los re-runs: es una medición-ancla, no una decisión de harness.
Las tres lecturas E-S1/2/3 son reportables tal cual; el único
compromiso es integrarla al paper (nueva subsección junto al ancla
BFCL, o párrafo en ella) con el resultado que salga.
