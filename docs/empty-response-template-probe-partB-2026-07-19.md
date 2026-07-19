# Probe de plantilla, Parte B (conductual) — resultados (2026-07-19)

Diseño pre-registrado:
`docs/empty-response-template-probe-design-2026-07-18.md` § Parte B.
Datos crudos: `docs/empty-response-template-probe-partB-2026-07-19.json`
(80 requests `/api/chat` directos contra Nitro, Ollama 0.30.7, sin
braze en el medio; plan REAL de gemma4:e4b tomado de una transcripción
preservada del re-run Bloque 1, tarea `multi_step_read_count_write`,
con su user task original; `temperature=0.2`, `num_predict` default,
n=10 por celda).

## Tabla (vacíos / n, con rango de `eval_count`)

| Modelo | Plan | A-last | U-last |
|---|---|---|---|
| llama3.2:1b | terminado | **10/10** (eval=1 en los 10) | 0/10 (277–371) |
| llama3.2:1b | cortado | 0/10 (16–253) | 0/10 (118–420) |
| qwen3.5-coder | terminado | 0/10 (70–301) | — sin dato (10/10 timeout del probe, cap 300s) |
| qwen3.5-coder | cortado | 0/10 (117–263) | 0/10 (339–956) |

## Lecturas (contra la tabla pre-declarada del diseño)

**llama3.2:1b — mecanismo de plantilla CONFIRMADO.** Las tres firmas
pre-declaradas, simultáneas: (1) A-last con plan terminado produce
vacíos (10/10) y U-last no (0/10); (2) dentro de A-last, el plan
terminado produce vacíos y el cortado no (0/10 — el modelo *continúa*
el plan incompleto, 147–253 tokens) — la firma de continuación/EOS;
(3) `eval_count=1` en los 10 vacíos: el modelo emite EOS inmediato,
exactamente lo que la rama de plantilla de la Parte A predice (el
prompt termina en el plan sin header de generación nuevo → "continuar
un mensaje que se ve terminado" → fin de turno inmediato).

**qwen3.5-coder — divergente; se reporta por modelo, sin
generalización** (lectura pre-declarada 4). En el probe aislado, el rol
por sí solo NO reproduce el colapso: A-last con plan terminado da 0/10
vacíos. Coherente con dos cosas que el paper ya dice: su plantilla es
de renderer built-in no inspeccionable (la evidencia documental de la
Parte A cubre 2 de 3 executors afectados, no éste), y el probe minimal
no reproduce el prompt completo de braze (system prompt + inventario de
tools + historial — la amenaza declarada en el propio diseño). El
colapso del coder en el sweep (35/48 vacíos) queda como observación
real cuyo mecanismo NO está aislado por rol en prompt desnudo.

**La pregunta "¿dónde van los 44–619 tokens?" queda respondida solo
para el 1B en el setting minimal**: no van a ninguna parte —
`eval_count=1`. Los 47–594 tokens de los vacíos del 1B en el sweep real
pertenecen entonces al setting de prompt completo, no al efecto de rol
puro; el probe acota el mecanismo, no cierra esa contabilidad.

**Celda perdida**: qwen3.5-coder / terminado / U-last — 10/10 timeouts
del cap de 300s del script del probe (el thinking model piensa largo
con tarea+plan como user). Limitación del harness del probe, no
evidencia sobre vacíos. Se disclosa; no se re-corre (ninguna lectura
pre-declarada depende de esa celda).

## Qué cambia en el paper

Según la tabla del diseño: para llama3.2:1b el hallazgo se reescribe
como **afirmación de interfaz** (inyectar contexto en rol assistant
colisiona con la convención de prefill de la plantilla — confirmado
conductualmente, no solo documentalmente); para qwen3.5-coder se
mantiene "consistent with" acotado y la divergencia se reporta. El
párrafo "pending" de §5.5 (mecanismo) se reemplaza por estos
resultados; § Threats actualiza la nota de mecanismo (una de las dos
incompletitudes declaradas — el probe conductual — deja de estar
pendiente; la otra — plantillas built-in no inspeccionables — persiste
y la divergencia del coder la vuelve más relevante).
