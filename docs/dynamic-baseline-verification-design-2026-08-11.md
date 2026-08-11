# Diseño: Dynamic Baseline Verification (DBV) — `--baseline-ref`

Fecha: 2026-08-11
Origen: survey de referencia (`docs/reference-agents-survey-2026-08-10.md`
§ Bench, "Dynamic Baseline Verification: ante un fallo de A/B, re-corre el
baseline para distinguir regresión de fallo preexistente"). Último ítem
del backlog del survey.

## Qué YA está cubierto (no reconstruir)

El caso **intra-sweep** ya lo hace McNemar. En una invocación con ≥2
brazos, `report::paired_comparisons` (report.rs:282) empareja por
`(task_id, repetition)` y solo cuenta como regresión un par DISCORDANTE
donde el brazo falla lo que el baseline (primer brazo) pasó; un fallo que
**ambos** brazos comparten es un par concordante y se ignora
(report.rs:320 `_ => {}`) — eso es exactamente "distinguir regresión de
fallo preexistente". Los fallos de infraestructura (OOM, circuit breaker
→ `HarnessError`) ya salen del denominador y del pareo (report.rs:297).

## El gap real: cross-invocación

La doctrina del proyecto parte el sweep en **invocaciones secuenciales
por brazo** (CLAUDE.md: "3 invocaciones, no monolítico") con `--seed 42`
fijo. Pero `paired_comparisons` solo puede parear brazos presentes en el
MISMO `Vec<TaskResult>` en memoria — no hay tooling que cargue el JSON de
una invocación previa como baseline. Y la metadata que detectaría **drift
ambiental** entre invocaciones (git commit, suite fingerprint, digests de
Ollama, versión del server, `local_env`, sampling) se **graba** en
`RunMetadata` (metadata.rs:15) pero **nunca se compara** — queda al ojo
humano.

El incidente OOM del 2026-08-10 es este gap en vivo: el sweep de
edit-fence murió a mitad, se re-corrieron 4 brazos, y se fusionaron los
JSON A MANO en Python — sin ninguna verificación de que Ollama reiniciado
(0.30.7→0.32.1 era la clase) o `KEEP_ALIVE` cambiado no invalidara
comparar contra los brazos pre-OOM.

## DBV para braze: fingerprint > re-run

gemini-cli hace DBV **re-corriendo** el baseline (caro, no-determinista).
braze tiene algo mejor por construcción: ya captura un fingerprint
determinístico del entorno. Así que DBV en braze es **verificar las
condiciones de reproducibilidad comparando fingerprints**, no re-ejecutar
— mismo objetivo, mecanismo más barato y exacto. (El paper lo puede
contrastar: re-run no-determinista vs fingerprint determinista.)

`--baseline-ref <sweep-previo.json>` hace dos cosas:

### 1. Drift check (verificación de entorno)

Carga el `RunMetadata` del ref y lo compara contra el de la corrida
actual, campo por campo:

- `suite_fingerprint` — ¿la suite cambió? (comparar peras con peras)
- `braze_git_commit` — ¿el harness cambió entre las dos corridas?
- `ollama_model_digests` — ¿un modelo se re-pulleó? (la clase gemma4
  stealth-refresh)
- `ollama_server_version` — ¿el serving layer cambió? (la clase OOM/
  0.30.7→0.32.1, que reveló mover el chat-template rendering)
- `local_env` — ¿cambió un `BRAZE_LOCAL_*` (capas GPU, KV type) o el
  `BRAZE_VERIFY_COMMAND`?
- `sampling` (seed, temperatura) + `repetitions` — ¿el régimen de
  muestreo es comparable? Sin el mismo `seed`, el pareo por
  `(task_id, repetition)` no comparte semilla derivada y NO es válido.

Cada divergencia se reporta con su antes→después. Si hay CUALQUIER drift,
la comparación cross-invocación de abajo se marca **INVÁLIDA** (se
imprime igual, con un banner, para que el operador vea los números pero
sepa que no son de fiar). No aborta por default — un warning prominente,
misma postura que `WriteSandboxStatus::NotEnforced`; un flag futuro
`--baseline-strict` podría abortar.

### 2. Pareo cross-invocación

Extrae las celdas del PRIMER brazo del ref (su baseline) como
`control_outcomes` —mismo filtro `HarnessError` que `outcomes_for`— y
compara cada brazo de la corrida ACTUAL contra ellas con la maquinaria
existente (McNemar exacto + Holm, `mcnemar_exact_p`, `holm_adjust`).
Reutiliza todo salvo el origen del control: en vez de
`backend_order.split_first()` (primer brazo de esta corrida), el control
viene del JSON externo. Hace first-class el flujo "3 invocaciones":
invocación 1 corre y guarda el brazo A; invocación 2 corre el brazo B con
`--baseline-ref A.json` y obtiene el McNemar B-vs-A automáticamente,
gateado por el drift check.

## Alcance MVP y decisiones

ENTRA: carga del JSON previo (Deserialize en `RunMetadata`, `TaskResult`,
etc.), drift check completo, pareo cross-invocación, banner de
invalidez. Flag `--baseline-ref <path>`.

DECISIONES:
- **Fingerprint, no re-run**: braze ya tiene la metadata; re-ejecutar
  sería más caro y menos determinista. Es la adaptación correcta a
  braze, no una copia de gemini-cli.
- **Warn, no abort** (por default): el operador ve los números marcados;
  la doctrina del proyecto es sobre-informar, no bloquear
  (`--baseline-strict` diferido).
- **El e-process/SPRT secuencial NO se cablea al baseline externo en el
  MVP**: el corte secuencial (`--sequential-stop`) monitorea el primer
  brazo EN MEMORIA. Cablearlo al baseline externo es una segunda pasada
  (el sequential se alimenta online, el ref es batch) — el pareo
  cross-invocación batch (McNemar/Holm) es el 90% del valor. Anotado.

DIFERIDO: `--baseline-strict` (abortar en drift), sequential contra ref
externo, y el re-run dinámico de una muestra (si algún día se quiere
detectar drift que la metadata NO captura — thermal, disco — a costa de
re-ejecución).

## Verificación

- **Unit**: `drift_report` detecta cada clase de divergencia (suite,
  commit, digest, server, env, sampling) y devuelve vacío cuando todo
  coincide; el pareo cross-invocación reproduce el McNemar de un pareo
  intra-sweep equivalente (mismo control, mismos brazos → mismo p).
- **Integración**: dos JSON sintéticos (baseline + actual) con y sin
  drift; con drift → banner INVÁLIDA; sin drift → comparación limpia.
  Más un round-trip: un `results.json` REAL del repo deserializa a
  `BaselineRef` sin pérdida (pinea que el Deserialize agregado matchea lo
  que `write_json` serializa).
- **En vivo VERIFICADO (2026-08-11)** con el binario real, dos
  invocaciones sobre `fast-core.toml` / deepseek-v4-flash:
  - Invocación 1 (`--output baseline.json`) guardó el brazo A.
  - Invocación 2 (`--baseline-ref baseline.json`), mismo seed →
    *"entorno reproducible: fingerprints coinciden (comparación
    válida)"* + McNemar cross-invocación limpio (13 pares).
  - Invocación 2 con `--seed 7` (baseline usó 42) → *"DRIFT DE ENTORNO
    detectado: sampling seed 42 → 7"* y la comparación marcada
    *"[INVÁLIDA — drift de entorno]"*. Cazó exactamente la clase de
    cambio que invalidaría un pareo en silencio.
