# Probe de plantilla para la degeneración empty-response — diseño pre-registrado (2026-07-18)

**Estado**: comprometido antes de ejecutar. Sin cláusula de adopción ni
de iteración (es una medición diagnóstica, no una decisión de harness),
así que el registro auto-alojado se considera suficiente y se disclosa
como tal.

**Reemplaza** al experimento de sensibilidad `num_predict` que estaba
encolado: la auditoría `docs/curve-transport-audit-2026-07-18.md`
§ Hallazgo 3 ya descartó el agotamiento de presupuesto con datos
commiteados (0 y 3 truncaciones registradas por el WARN del engine en
los slices qwen y 1B, contra empties de 44–619 tokens sobre un
presupuesto de 4096). Variar `num_predict` mediría una hipótesis ya
refutada.

## La hipótesis viva

El reviewer blind (Issue 6) nombró dos explicaciones alternativas al
relato cognitivo del paper ("el modelo, al recibir su propio plan como
texto de assistant, deja de producir salida usable"). Una (presupuesto)
está descartada. Queda la de **plantilla/serving**:

En el diseño original, un turno planificado renderiza
`UserMessage → PlanCreated (rol assistant) → generación` — es decir, el
**último** mensaje antes de generar es del assistant (confirmado en
`braze-engine/src/engine.rs`, doc del test
`a_planned_turn_shape_renders_protocol_valid_messages`). Muchas
plantillas de chat (Jinja/Go de llama.cpp/Ollama) tratan un mensaje
assistant final como **prefill**: no abren un turno nuevo, sino que
piden al modelo *continuar* ese mensaje. Un plan que "se ve terminado"
invita entonces a un EOS inmediato → contenido vacío con pocos tokens
generados, exactamente la firma observada.

Esto importa porque, de ser cierto, el hallazgo del paper cambia de
naturaleza: no sería "los modelos chicos se callan al recibir su propio
plan" (afirmación sobre modelos) sino "inyectar contexto en rol
assistant colisiona con la convención de prefill de las plantillas de
chat" (afirmación sobre la interfaz harness↔serving). La segunda es más
accionable para quien construye harnesses y, además, explica por qué el
arreglo (re-render como user) funcionó a ambas escalas.

## Diseño

### Parte A — inspección estática de plantillas (costo cero, sin sweep)

`ollama show --template <modelo>` para `qwen3.5-coder`, `llama3.2:1b`,
`qwen2.5:3b` y `gemma4:e4b`. Se lee un solo hecho: **¿la plantilla
agrega el encabezado de generación (`<|im_start|>assistant` o
equivalente) incondicionalmente, o solo cuando el último mensaje no es
del assistant?**

- Si NO lo agrega con assistant final ⇒ evidencia documental directa de
  prefill: el modelo estaba siendo invitado a continuar el plan, no a
  responder.
- Si lo agrega siempre ⇒ el modelo recibía un turno nuevo legítimo y el
  relato cognitivo se refuerza.

### Parte B — pares emparejados contra Ollama (80 requests, ~5 min)

Contra Nitro, `/api/chat` directo (sin braze en el medio, para aislar
la interfaz), dos condiciones idénticas salvo el rol del último mensaje:

- **A-last**: `[user: tarea, assistant: PLAN]`
- **U-last**: `[user: tarea, user: PLAN]`

Con el mismo plan textual (tomado de una transcripción real de un run
degenerado), `temperature=0.2`, `num_predict` default, n=10 por
condición × 2 modelos (`qwen3.5-coder`, `llama3.2:1b`) × 2 planes
(uno "terminado", uno cortado a media frase) = 80 requests.

Se registra por request: `content` vacío o no, `eval_count` (tokens
generados), `done_reason`.

### Lecturas pre-declaradas

| Patrón | Lectura |
|---|---|
| A-last con tasa de vacíos alta y U-last baja, en ambos modelos | **Mecanismo de plantilla confirmado**: el paper reescribe §planner como hallazgo de interfaz, no de cognición del modelo |
| Tasas similares en A-last y U-last | Plantilla descartada; el relato cognitivo queda como la mejor explicación disponible y el paper puede subir de "consistent with" a afirmación acotada |
| El plan "terminado" produce más vacíos que el cortado (dentro de A-last) | Firma de continuación/EOS — refuerza plantilla aunque las tasas globales sean parecidas |
| Resultados divergentes entre modelos | Se reporta por modelo; ninguna generalización a "modelos chicos" |

Ninguna lectura gatilla cambios de harness: el arreglo (entrega
user-role) ya está implementado y medido. Lo que está en juego es
**qué afirma el paper sobre el mecanismo**, y cualquiera de los cuatro
patrones es reportable.

## Amenaza a la validez que este probe NO resuelve

`/api/chat` directo no reproduce el prompt exacto de braze (system
prompt, inventario de tools, historial). El probe aísla la variable de
rol; no demuestra que el turno degenerado del sweep tuviera esa y solo
esa causa. Se reporta como evidencia mecanística convergente, no como
replicación del fallo original.
