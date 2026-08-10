# Diseño pre-registrado: A/B del impuesto JSON — edición como tool-arg vs SEARCH/REPLACE textual

Fecha: 2026-08-10
Estado: **CERRADO — RECHAZADO el mismo día, por el criterio exacto de
abajo.** B ≤ A en los tres débiles (−4,2 / −8,4 / −1,1pp); mecanismo
limpio pero revelador: llama3.2:1b y qwen2.5:3b emitieron CERO fences
válidos en 190 corridas del brazo B — para SLM tool-tuned el JSON es
la modalidad entrenada, no un impuesto, y quitarles `edit_file` les
quitó capacidad (−6/15 en tareas `edit` cada uno). La cláusula de
iteración única no se disparó. Tercer nulo de la familia sintáctica
(constrained decoding, stencil, edit-fence). Detalle completo:
`docs/sweep-json-tax-edit-fence-2026-08-10.md`. Ninguna regla se
modificó después de correr el sweep; el análisis quedó commiteado
(`92850d9`) antes de que el JSON existiera.
Ninguna regla de este documento se modifica después de correr ningún
sweep (disciplina de `docs/constrained-decoding-ab-design.md`, el
template estructural de este diseño). Origen: survey de referencia
(`docs/reference-agents-survey-2026-08-10.md` ítem 10 Tier-2 y
convergencia #2) — la señal cross-repo más fuerte de los 5 repos
revisados: aider midió que envolver código en JSON degrada sus
benchmarks de edición, y SWE-agent mantiene un parser de texto plano
precisamente para los modelos débiles. braze entrega hoy TODAS sus
ediciones como tool calls JSON.

## La hipótesis

Para los modelos chicos, serializar una edición de código dentro de un
string JSON (`edit_file {"old_string": "...", "new_string": "..."}`)
impone un **impuesto de transporte**: el código viaja con una capa de
escapes encima — comillas anidadas, backslashes, newlines literales,
unicode — y cada error de esa capa corrompe o pierde la edición. El
proyecto ya observó la clase en vivo: el hallazgo del 2026-07-28
(caracteres que el modelo entiende y no puede emitir; comillas anidadas
en `format!` rotas dentro de args JSON) vive exactamente en este canal.
La escalera de reparación absorbe parte gastando rondas; lo que no
absorbe termina en `schema_fail` o en ediciones silenciosamente
truncadas.

Emitir la MISMA edición como bloque SEARCH/REPLACE en texto plano
elimina la capa de escapes: el código se cita verbatim, sin
serialización. Es la modalidad con la que aider gana sus benchmarks y
la que SWE-agent adopta para débiles.

Predicción si la hipótesis es cierta: en los executors débiles, el
brazo fence mejora el pass rate agregado, con mecanismo verificable —
`fence_edits > 0`, `schema_fail ≈ 0` y menos rondas quemadas en
reparación en las tareas que editan.

Predicción alternativa (la que mataría la palanca): el cuello de los
débiles es **semántico** (el `old_string` citado no matchea el archivo
— indiferente al transporte) y el schema JSON además le da al modelo un
andamiaje que el texto libre no da. Entonces fence ≤ baseline y la
conclusión publicable es el nulo espejo del stencil: **la escalera de
reparación río abajo ya subsume el impuesto JSON a esta escala** — dos
nulos independientes apuntando al mismo mecanismo (el harness absorbe
la clase sintáctica) se refuerzan mutuamente.

Expectativa pre-declarada para el control saturado (gpt-oss:20b): si
fence lo EMPEORA, eso es el costo de quitarle la modalidad nativa a un
modelo fuerte en tool-calling — hallazgo, no fallo del A/B (mismo rol
que qwen2.5:3b en el A/B de constrained decoding).

## Mecanismo (implementado con este diseño)

Lever `edit_fence_enabled` en el engine, off por default en todos los
composition roots. Efectos, todos condicionados al lever:

1. **`edit_file` sale del inventario** de tool stubs del request (misma
   mecánica opt-in/opt-out que `explore`/`editor` en `turn.rs`). El
   resto de las tools sigue JSON nativo — el A/B aísla SOLO el
   transporte de la edición, no la modalidad completa (eso ya se midió
   en el A/B de prompt-tools y perdió).
2. **Addendum al system prompt** con la gramática del bloque:
   ```
   path/al/archivo.rs
   <<<<<<< SEARCH
   texto exacto actual
   =======
   texto de reemplazo
   >>>>>>> REPLACE
   ```
   Path en la línea inmediatamente anterior; varios bloques por
   respuesta permitidos; fence de código envolvente opcional.
3. **Parser primario, NO rescate**: los bloques se parsean en
   `complete_once_with` ANTES del envelope y de la escalera, sin la
   condición `tool_calls.is_empty()` (una respuesta puede llamar
   `read_file` nativo Y emitir un fence). Cada bloque se sintetiza como
   `ToolCall { name: "edit_file", ... }` (id `fence-<uuid>`). Mismo
   precedente que el envelope: canal instruido → no contamina
   `rescued_tool_calls`, que sigue siendo métrica de mecanismo limpia.
4. **La aplicación reusa `edit_file` entera**: escalera fuzzy, gate
   sintáctico pre-aplicación, post-edit `cargo check`, y sus mensajes
   de error pedagógicos vuelven como tool result normal. Se mide
   transporte, no semántica de aplicación.
5. **Evento `AgentEvent::EditFenceApplied { blocks }`** + contador
   `fence_edits` en el `TaskResult` del bench — la verificación del
   mecanismo del brazo B.
6. **Gates**: `+ablate:edit-fence` (enabling key, misma excepción
   documentada que `task-list`) y `Config::enable_edit_fence` /
   `BRAZE_ENABLE_EDIT_FENCE` para verificación en vivo. Backend-
   agnóstico (opera sobre inventario + prompt + texto): sin warning de
   executor incompatible.
7. **`write_file` queda JSON en ambos brazos.** Su payload también es
   código y también paga el impuesto — se anota como extensión, no se
   mezcla en este A/B (un factor a la vez).

## Brazos y executors

Por executor, 2 brazos sobre `suites/default.toml` (19 tareas, 5 reps,
temp 0.2, Nitro, `--no-ollama-stop`, seeds pareados):

| Brazo | Qué aísla |
|---|---|
| A: baseline | edición como tool call JSON + escalera de reparación (el default) |
| B: `+ablate:edit-fence` | la misma edición como SEARCH/REPLACE textual |

Executors (gradiente débil→fuerte, vía Ollama en Nitro):

- **llama3.2:1b** — el débil histórico (18% en 2026-07-04).
- **qwen2.5:3b** — chico tuneado para function calling (62-64%): donde
  "el schema como andamiaje" jugaría a favor del brazo A.
- **gemma4:e4b** — driver diario, 3 fallos sistemáticos conocidos.
- **gpt-oss:20b** — control saturado (98.9% vía Ollama): mide el costo
  de la modalidad en un modelo fuerte, fuera del criterio de adopción.

Total: 4 × 2 × 19 × 5 = **760 corridas** (~2-3h de Nitro).

## Criterio pre-registrado

- **Adoptar** (recomendar `enable_edit_fence` para modelos débiles +
  sección en el paper) si, en al menos un executor débil (llama3.2:1b,
  qwen2.5:3b o gemma4:e4b):
  - B − A ≥ **+10pp** agregado, fuera del ruido (Newcombe 95% sobre el
    delta within-sweep; McNemar exacto pareado por (tarea, repetición)
    como confirmación), **Y**
  - el mecanismo verifica: `fence_edits > 0` en B y el delta se
    concentra en tareas que editan archivos, **Y**
  - sin daño fuera del ruido en las tareas que NO editan (el addendum
    viaja en el prompt de todas — su costo distractor se mide, no se
    asume nulo).
- **Rechazar** (documentar como nulo en la discusión: la escalera de
  reparación subsume el impuesto JSON a esta escala) si B ≤ A en los
  tres débiles.
- **Iterar UNA vez**, pre-declarada única: si el modo de falla
  dominante del brazo B es creación/reescritura de archivos vía
  `write_file` JSON rota (el canal que este A/B dejó fuera), la
  iteración es extender el fence a `write_file` (payload completo
  textual). Nada más se itera.
- La fila gpt-oss:20b se REPORTA siempre (costo de modalidad en
  fuertes) pero no participa del criterio.

### Contaminación medible, no supuesta

Un modelo puede llamar `edit_file` por nombre memorizado aunque no
esté en el inventario del brazo B — dispatch lo ejecutará igual (la
tool existe en el provider). Se reporta la fracción de ediciones que
llegaron por fence (`fence_edits`) vs por tool call nativa filtrada;
si la fuga domina, el brazo B no midió lo que dice y el sweep se
declara inválido ANTES de mirar pass rates.

## Riesgos anotados

- **El addendum toca todas las tareas del brazo B**, también las que no
  editan — por eso el criterio exige explícitamente no-daño en
  no-edición en vez de promediarlo.
- **Fence truncado por `max_tokens`**: no parsea → ronda sin efecto y
  reintento. Mismo modo de falla que un JSON truncado (N-24); neutral
  al A/B, no se corrige.
- **Bloques citados como ejemplo en prosa**: el parser los consumiría.
  Riesgo real solo con el lever ON (off en producción por default —
  postura N-15); en el bench, las tareas no piden mostrar ejemplos de
  fences. Se anota, no se mitiga.
- **Interacción con `is_inside_code_fence` del rescate**: el parser de
  fences corre antes y consume sus bloques; el guard del rescate opera
  sobre el texto restante. Orden fijado en el código, con test.
- **SEARCH vacío** (intento de crear archivo): se sintetiza igual y
  `edit_file` responde su error pedagógico ("use write_file") — el loop
  de feedback existente hace la corrección, como con cualquier arg
  inválido.

## Conexión con el paper

Entra a § ablations como la ablación de **transporte de edición** — el
tercer punto del espectro que el paper ya recorre: constrained decoding
(prevención en el decoder, RECHAZADO), escalera de reparación
(recuperación en el harness, la contribución), y ahora evitar la
serialización de una vez (el canal que no necesita ni prevención ni
recuperación). Citas: aider (benchmarks de formato de edición),
SWE-agent (ACI, parser textual para débiles). Si sale nulo, se cita
junto al nulo del stencil como evidencia doble de que el harness río
abajo absorbe la clase sintáctica.

## Qué NO es este documento

No es un compromiso de correr el sweep hoy: mecanismo ~medio día (ya
ejecutado junto con este pre-registro), sweep ~2-3h de Nitro cuando la
cola lo permita. Este diseño garantiza que, si se corre, se corre
pre-registrado.
