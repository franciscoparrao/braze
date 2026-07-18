# Probe de plantilla, Parte A (estática) — resultados (2026-07-18)

Ejecución de la Parte A de
`docs/empty-response-template-probe-design-2026-07-18.md`, pre-registrado
antes de mirar. Costo: cuatro llamadas de metadata a `/api/show` (sin
inferencia, sin tocar el sweep en curso).

**Resultado: la hipótesis de prefill queda CONFIRMADA documentalmente
para dos de los tres executors que exhibieron el colapso del planner.**

## Qué se buscaba

En el diseño original del planner, braze envía a `/api/chat` la
secuencia `[user: tarea, assistant: PLAN]` — el último mensaje es del
assistant (confirmado en `braze-engine/src/engine.rs`, doc del test
`a_planned_turn_shape_renders_protocol_valid_messages`). La pregunta:
¿la plantilla del modelo abre un turno nuevo de assistant (respuesta
fresca) o deja el turno anterior abierto (continuación/prefill)?

## Evidencia por modelo

### `llama3.2:1b` — PREFILL CONFIRMADO

Rama assistant de su plantilla Go:

```
{{- else if eq .Role "assistant" }}<|start_header_id|>assistant<|end_header_id|>
...
{{ .Content }}
{{- end }}{{ if not $last }}<|eot_id|>{{ end }}
```

El token de fin de turno `<|eot_id|>` se emite **solo si el mensaje NO
es el último**. El header de generación aparece únicamente dentro de las
ramas `user` y `tool`, y solo cuando esas son `$last`.

⇒ Con el plan como último mensaje, el prompt termina en
`<|start_header_id|>assistant<|end_header_id|>\n\n<PLAN>` **sin cerrar**
y sin header nuevo: al modelo se le pide *continuar el plan*, no
responder.

### `qwen2.5:3b` — PREFILL CONFIRMADO

Mismo patrón en ChatML, y con la condición hecha explícita:

```
{{ .Content }}
{{- end }}{{ if not $last }}<|im_end|>{{ end }}
...
{{- if and (ne .Role "assistant") $last }}<|im_start|>assistant
{{ end }}
```

La segunda línea es inequívoca: **el header de generación se emite si y
solo si el rol del último mensaje NO es assistant**.

⇒ Entrega assistant-role: `<|im_start|>assistant\n<PLAN>` abierto,
modo continuación.
⇒ Entrega user-role (la corrección del paper):
`<|im_start|>user\n<PLAN><|im_end|>\n<|im_start|>assistant\n` — turno
nuevo, limpio. **La plantilla explica por qué el arreglo funcionó.**

### `qwen3.5-coder` y `gemma4:e4b` — NO INSPECCIONABLES ESTÁTICAMENTE

Ambos declaran `TEMPLATE {{ .Prompt }}` con `RENDERER qwen3.5` /
`RENDERER gemma4`: el renderizado lo hace un renderer interno de Ollama,
no una plantilla Go legible desde el Modelfile. Su comportamiento de
prefill requiere la Parte B (empírica). Se declara como límite; no se
extrapola desde los otros dos.

## Qué queda establecido y qué no

**Establecido (documental, verificable por cualquiera con
`ollama show --template`)**: para llama3.2:1b y qwen2.5:3b —dos de los
tres executors donde el paper reporta que el plan en rol assistant
*daña*— las dos entregas que el paper compara **no son la misma
pregunta al modelo**. Una pide continuar un texto ya completo; la otra
abre un turno nuevo. La diferencia es de interfaz harness↔serving y
existe con independencia de la capacidad del modelo.

**No establecido**: la cadena causal completa desde el prefill hasta el
error específico observado (`no text and no tool calls` con 47–594
tokens generados). Que el modelo esté en modo continuación no explica
por sí solo dónde fueron esos tokens; hipótesis plausibles (continuación
que el parser convierte en tool-call malformada y se descarta; EOS
inmediato con tokens contados de otra forma) requieren la Parte B, que
mide el comportamiento en vez de inferirlo.

## Consecuencia para el manuscrito

§planner debe reencuadrarse. Hoy dice, con hedge, que el modelo "deja de
producir salida usable" al recibir su propio plan como texto — una
afirmación sobre cognición de modelos chicos que además no aplicaría al
ceiling (un modelo capaz). La lectura correcta y más útil:

> Inyectar contexto como mensaje assistant final colisiona con la
> convención de *prefill* de las plantillas de chat: el modelo no
> recibe un turno nuevo sino la instrucción implícita de continuar un
> texto ya terminado. El daño no es una propiedad del modelo sino de la
> interfaz, lo que explica que aparezca en ambos extremos de la escala
> y que se repare cambiando el rol de entrega.

Esto **fortalece** la contribución: pasa de una anécdota sobre modelos
chicos a una advertencia de diseño verificable para cualquier harness
que inyecte contexto, sobre cualquier modelo cuya plantilla siga esta
convención (que es la mayoritaria).

Pendiente para cerrar: Parte B sobre los cuatro modelos (los dos con
renderer interno, obligatorio; los dos confirmados, como validación
conductual de la lectura estática).
